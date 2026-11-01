//! A reader for `.safetensors` files: an 8-byte little-endian header length, a JSON header
//! naming each tensor's dtype, shape and byte range, then the raw data.
//!
//! The file is treated as hostile. The header has a size cap, every length and offset is
//! checked against the file before anything is read, and only `F32` tensors are accepted.
//! Tensors are copied out, so memory stays bounded by the file's size.

use std::collections::BTreeMap;
use std::path::Path;

use crate::tensor::numel;
use crate::{Error, Result, Tensor};

/// The largest JSON header read. Real headers take a few hundred bytes per tensor.
pub const MAX_HEADER: usize = 16 << 20;
/// The most axes a tensor may have.
pub const MAX_RANK: usize = 8;

/// The tensors of one file by name, and its free-form metadata.
#[derive(Clone, Debug, Default)]
pub struct SafeTensors {
    tensors: BTreeMap<String, Tensor>,
    metadata: BTreeMap<String, String>,
}

fn bad<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Format(msg.into()))
}

impl SafeTensors {
    /// Reads and parses a file.
    pub fn read(path: &Path) -> Result<SafeTensors> {
        let bytes =
            std::fs::read(path).map_err(|e| Error::Io(format!("cannot read {}: {e}", path.display())))?;
        SafeTensors::parse(&bytes)
    }

    /// Parses the bytes of a file.
    pub fn parse(bytes: &[u8]) -> Result<SafeTensors> {
        let Some((len_bytes, rest)) = bytes.split_first_chunk::<8>() else {
            return bad("shorter than the 8-byte header length");
        };
        let header_len = u64::from_le_bytes(*len_bytes);
        let header_len = match usize::try_from(header_len) {
            Ok(n) if n <= MAX_HEADER => n,
            _ => return bad(format!("header length {header_len} exceeds the cap of {MAX_HEADER} bytes")),
        };
        let Some((header, data)) = rest.split_at_checked(header_len) else {
            return bad(format!("header of {header_len} bytes, but only {} bytes follow", rest.len()));
        };
        let header: serde_json::Value =
            serde_json::from_slice(header).map_err(|e| Error::Format(format!("header is not JSON: {e}")))?;
        let Some(entries) = header.as_object() else {
            return bad("header is not a JSON object");
        };
        let mut out = SafeTensors::default();
        for (name, entry) in entries {
            if name == "__metadata__" {
                let Some(meta) = entry.as_object() else {
                    return bad("__metadata__ is not an object");
                };
                for (k, v) in meta {
                    let Some(v) = v.as_str() else {
                        return bad(format!("metadata {k:?} is not a string"));
                    };
                    out.metadata.insert(k.clone(), v.to_string());
                }
                continue;
            }
            let tensor = parse_entry(name, entry, data)?;
            out.tensors.insert(name.clone(), tensor);
        }
        Ok(out)
    }

    /// The tensor of this name, or an error naming it.
    pub fn get(&self, name: &str) -> Result<&Tensor> {
        self.tensors.get(name).ok_or_else(|| Error::Weights(format!("missing tensor {name:?}")))
    }

    /// The tensor of this name, which must have this shape.
    pub fn get_shaped(&self, name: &str, shape: &[usize]) -> Result<&Tensor> {
        let t = self.get(name)?;
        if t.shape() != shape {
            return Err(Error::Weights(format!("{name:?} has shape {:?}, expected {shape:?}", t.shape())));
        }
        Ok(t)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tensors.contains_key(name)
    }

    /// The names of all tensors, sorted.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.tensors.keys().map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.tensors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tensors.is_empty()
    }

    /// The string metadata of the file (`__metadata__`).
    pub fn metadata(&self) -> &BTreeMap<String, String> {
        &self.metadata
    }

    /// Adds or replaces a tensor (for building weights in code and in tests).
    pub fn insert(&mut self, name: impl Into<String>, tensor: Tensor) {
        self.tensors.insert(name.into(), tensor);
    }

    /// Takes a tensor out.
    pub fn remove(&mut self, name: &str) -> Option<Tensor> {
        self.tensors.remove(name)
    }
}

fn parse_entry(name: &str, entry: &serde_json::Value, data: &[u8]) -> Result<Tensor> {
    let dtype = entry.get("dtype").and_then(serde_json::Value::as_str);
    if dtype != Some("F32") {
        return bad(format!("{name:?} has dtype {dtype:?}; only F32 is read"));
    }
    let Some(dims) = entry.get("shape").and_then(serde_json::Value::as_array) else {
        return bad(format!("{name:?} has no shape"));
    };
    if dims.len() > MAX_RANK {
        return bad(format!("{name:?} has {} axes, more than {MAX_RANK}", dims.len()));
    }
    let mut shape = Vec::with_capacity(dims.len());
    for d in dims {
        match d.as_u64().and_then(|d| usize::try_from(d).ok()) {
            Some(d) => shape.push(d),
            None => return bad(format!("{name:?} has a bad axis length {d}")),
        }
    }
    let offsets = entry.get("data_offsets").and_then(serde_json::Value::as_array);
    let (begin, end) = match offsets.map(Vec::as_slice) {
        Some([b, e]) => match (b.as_u64(), e.as_u64()) {
            (Some(b), Some(e)) => (b, e),
            _ => return bad(format!("{name:?} has bad data offsets")),
        },
        _ => return bad(format!("{name:?} needs two data offsets")),
    };
    let range = usize::try_from(begin).ok().zip(usize::try_from(end).ok());
    let Some(bytes) = range.filter(|(b, e)| b <= e).and_then(|(b, e)| data.get(b..e)) else {
        return bad(format!(
            "{name:?} has data offsets {begin}..{end} outside the {} data bytes",
            data.len()
        ));
    };
    let Some(count) = numel(&shape) else {
        return bad(format!("{name:?} has a shape too large: {shape:?}"));
    };
    if Some(bytes.len()) != count.checked_mul(4) {
        return bad(format!("{name:?} has shape {shape:?} ({count} values) but {} bytes", bytes.len()));
    }
    let (words, _) = bytes.as_chunks::<4>();
    let values = words.iter().map(|w| f32::from_le_bytes(*w)).collect();
    Tensor::new(shape, values)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file from a header string and data bytes.
    fn file(header: &str, data: &[u8]) -> Vec<u8> {
        let mut out = (header.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        out
    }

    fn floats(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_le_bytes()).collect()
    }

    #[test]
    fn reads_tensors_and_metadata() {
        let h = r#"{"__metadata__":{"model":"x"},"a":{"dtype":"F32","shape":[2,2],"data_offsets":[0,16]},"b":{"dtype":"F32","shape":[],"data_offsets":[16,20]}}"#;
        let st = SafeTensors::parse(&file(h, &floats(&[1.0, 2.0, 3.0, 4.0, 5.0]))).unwrap();
        assert_eq!(st.get("a").unwrap().data(), &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(st.get("b").unwrap().shape(), &[] as &[usize]);
        assert_eq!(st.metadata().get("model").map(String::as_str), Some("x"));
        assert!(st.get("c").is_err());
        assert!(st.get_shaped("a", &[4]).is_err());
    }

    #[test]
    fn rejects_hostile_files() {
        let good = r#"{"a":{"dtype":"F32","shape":[2],"data_offsets":[0,8]}}"#;
        let data = floats(&[1.0, 2.0]);
        let full = file(good, &data);
        assert!(SafeTensors::parse(&full).is_ok());
        // Truncated anywhere: inside the length, the header or the data.
        for cut in 0..full.len() {
            assert!(SafeTensors::parse(&full[..cut]).is_err(), "cut at {cut}");
        }
        // A huge header length, with and without the bytes to back it.
        let mut huge = u64::MAX.to_le_bytes().to_vec();
        huge.extend_from_slice(b"{}");
        assert!(SafeTensors::parse(&huge).is_err());
        let mut capped = ((MAX_HEADER + 1) as u64).to_le_bytes().to_vec();
        capped.extend(std::iter::repeat_n(b' ', 64));
        assert!(SafeTensors::parse(&capped).is_err());
        let cases = [
            // Other dtypes.
            r#"{"a":{"dtype":"F16","shape":[4],"data_offsets":[0,8]}}"#,
            r#"{"a":{"dtype":"I64","shape":[1],"data_offsets":[0,8]}}"#,
            r#"{"a":{"shape":[2],"data_offsets":[0,8]}}"#,
            // Shape and byte count disagree.
            r#"{"a":{"dtype":"F32","shape":[3],"data_offsets":[0,8]}}"#,
            r#"{"a":{"dtype":"F32","shape":[1],"data_offsets":[0,8]}}"#,
            // Offsets backwards, past the end, overflowing, not numbers.
            r#"{"a":{"dtype":"F32","shape":[0],"data_offsets":[8,0]}}"#,
            r#"{"a":{"dtype":"F32","shape":[2],"data_offsets":[4,12]}}"#,
            r#"{"a":{"dtype":"F32","shape":[2],"data_offsets":[18446744073709551615,7]}}"#,
            r#"{"a":{"dtype":"F32","shape":[2],"data_offsets":["0","8"]}}"#,
            r#"{"a":{"dtype":"F32","shape":[2],"data_offsets":[0]}}"#,
            // Shapes whose product overflows, negative or fractional axes, too many axes.
            r#"{"a":{"dtype":"F32","shape":[4294967296,4294967296,4],"data_offsets":[0,8]}}"#,
            r#"{"a":{"dtype":"F32","shape":[-2],"data_offsets":[0,8]}}"#,
            r#"{"a":{"dtype":"F32","shape":[2.5],"data_offsets":[0,8]}}"#,
            r#"{"a":{"dtype":"F32","shape":[1,1,1,1,1,1,1,1,2],"data_offsets":[0,8]}}"#,
            // Not an object, not JSON, bad metadata.
            r#"[1,2]"#,
            r#"{"a":"#,
            r#"{"__metadata__":{"k":1}}"#,
            r#"{"__metadata__":[]}"#,
        ];
        for h in cases {
            assert!(SafeTensors::parse(&file(h, &data)).is_err(), "{h}");
        }
    }
}
