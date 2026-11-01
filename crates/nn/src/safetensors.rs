//! A reader for `.safetensors` files: an 8-byte little-endian header length, a JSON header
//! naming each tensor's dtype, shape and byte range, then the raw data.
//!
//! The file is treated as hostile. The header has a size cap, every length and offset is
//! checked against the file before anything is read, and tensors are copied out, so memory
//! stays bounded by a small multiple of the file's size.
//!
//! Dtypes read:
//!
//! - `F32` as it is, and `F16`, widened to f32 exactly.
//! - `U8` and `I8` only as the codes of a quantized weight, which is turned back into f32 on
//!   load. A weight `W` of shape `[O, ...]` (`K` values per row of the first axis) is stored as
//!   `W.q`, its integer codes, and `W.scale` (F16 or F32, `[O, K / group]`), and described by
//!   the metadata key `quant.W`, a JSON object `{"bits", "group", "layout", "shape"}`. Value `k`
//!   of row `o` is `code * scale[o][k / group]`. Two layouts exist:
//!   - `"packed6"`, `bits` 6: `W.q` is U8 `[O, K * 3 / 4]`. Four codes share three bytes, a
//!     24-bit little-endian word with the first code in its low 6 bits. Each code is stored
//!     plus 32, so 1..=63 stand for -31..=31.
//!   - `"int8"`, `bits` 8: `W.q` is I8 `[O, K]`, codes -127..=127.
//!
//! Integer tensors no quantized weight uses, codes outside their levels, scales that are not
//! finite and every disagreement between the metadata and the tensors are errors.

use std::collections::BTreeMap;
use std::path::Path;

use crate::tensor::numel;
use crate::{Error, Result, Tensor};

/// The largest JSON header read. Real headers take a few hundred bytes per tensor.
pub const MAX_HEADER: usize = 16 << 20;
/// The most axes a tensor may have.
pub const MAX_RANK: usize = 8;
/// The prefix of the metadata keys that describe quantized weights.
pub const QUANT_PREFIX: &str = "quant.";

/// The tensors of one file by name, and its free-form metadata.
#[derive(Clone, Debug, Default)]
pub struct SafeTensors {
    tensors: BTreeMap<String, Tensor>,
    metadata: BTreeMap<String, String>,
}

fn bad<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Format(msg.into()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dtype {
    F32,
    F16,
    U8,
    I8,
}

impl Dtype {
    fn parse(s: &str) -> Option<Dtype> {
        match s {
            "F32" => Some(Dtype::F32),
            "F16" => Some(Dtype::F16),
            "U8" => Some(Dtype::U8),
            "I8" => Some(Dtype::I8),
            _ => None,
        }
    }

    fn size(self) -> usize {
        match self {
            Dtype::F32 => 4,
            Dtype::F16 => 2,
            Dtype::U8 | Dtype::I8 => 1,
        }
    }
}

/// The integer codes of a quantized weight, as found in the file.
struct Codes<'a> {
    dtype: Dtype,
    shape: Vec<usize>,
    bytes: &'a [u8],
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
        let mut codes = BTreeMap::new();
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
            let (dtype, shape, bytes) = parse_entry(name, entry, data)?;
            match dtype {
                Dtype::F32 => {
                    let (words, _) = bytes.as_chunks::<4>();
                    let values = words.iter().map(|w| f32::from_le_bytes(*w)).collect();
                    out.tensors.insert(name.clone(), Tensor::new(shape, values)?);
                }
                Dtype::F16 => {
                    let (words, _) = bytes.as_chunks::<2>();
                    let values = words.iter().map(|w| f16_to_f32(u16::from_le_bytes(*w))).collect();
                    out.tensors.insert(name.clone(), Tensor::new(shape, values)?);
                }
                Dtype::U8 | Dtype::I8 => {
                    codes.insert(name.clone(), Codes { dtype, shape, bytes });
                }
            }
        }
        let quantized: Vec<(String, String)> = out
            .metadata
            .iter()
            .filter_map(|(k, v)| Some((k.strip_prefix(QUANT_PREFIX)?.to_string(), v.clone())))
            .collect();
        for (name, spec) in quantized {
            let tensor = dequantize(&name, &spec, &mut codes, &mut out.tensors)?;
            out.tensors.insert(name, tensor);
        }
        if let Some(name) = codes.keys().next() {
            return bad(format!("{name:?} holds integers, but no quantized weight uses them"));
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

/// An IEEE half-precision number widened to f32. Exact: every f16 value is an f32 value.
pub fn f16_to_f32(h: u16) -> f32 {
    let negative = h & 0x8000 != 0;
    let exp = u32::from((h >> 10) & 0x1f);
    let man = u32::from(h & 0x3ff);
    let magnitude = match exp {
        // Zero and subnormals: man * 2^-24. `as`: man is at most 1023, exact in f32.
        0 => man as f32 / 16_777_216.0,
        // Infinity and NaN, the NaN's payload kept.
        0x1f => f32::from_bits(0x7f80_0000 | (man << 13)),
        // Rebias the exponent from 15 to 127.
        _ => f32::from_bits(((exp + 112) << 23) | (man << 13)),
    };
    if negative { -magnitude } else { magnitude }
}

/// Checks one header entry; its dtype, shape and bytes (exactly the shape's size).
fn parse_entry<'a>(
    name: &str,
    entry: &serde_json::Value,
    data: &'a [u8],
) -> Result<(Dtype, Vec<usize>, &'a [u8])> {
    let raw = entry.get("dtype").and_then(serde_json::Value::as_str);
    let Some(dtype) = raw.and_then(Dtype::parse) else {
        return bad(format!("{name:?} has dtype {raw:?}; F32, F16, U8 and I8 are read"));
    };
    let Some(dims) = entry.get("shape").and_then(serde_json::Value::as_array) else {
        return bad(format!("{name:?} has no shape"));
    };
    let shape = parse_shape(name, dims)?;
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
    if Some(bytes.len()) != count.checked_mul(dtype.size()) {
        return bad(format!("{name:?} has shape {shape:?} ({count} values) but {} bytes", bytes.len()));
    }
    Ok((dtype, shape, bytes))
}

fn parse_shape(name: &str, dims: &[serde_json::Value]) -> Result<Vec<usize>> {
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
    Ok(shape)
}

/// Builds the f32 weight `name` from its codes and scales, which it takes out of `codes` and
/// `tensors`. `spec` is the JSON of its `quant.` metadata key.
fn dequantize(
    name: &str,
    spec: &str,
    codes: &mut BTreeMap<String, Codes<'_>>,
    tensors: &mut BTreeMap<String, Tensor>,
) -> Result<Tensor> {
    let spec: serde_json::Value = serde_json::from_str(spec)
        .map_err(|e| Error::Format(format!("quantization of {name:?} is not JSON: {e}")))?;
    let number = |key: &str| spec.get(key).and_then(serde_json::Value::as_u64);
    let bits = number("bits");
    let layout = spec.get("layout").and_then(serde_json::Value::as_str);
    match (bits, layout) {
        (Some(6), Some("packed6")) | (Some(8), Some("int8")) => {}
        _ => {
            return bad(format!(
                "{name:?} is quantized as {bits:?} bits in layout {layout:?}; 6 bits packed6 and 8 bits int8 are read"
            ));
        }
    }
    let Some(dims) = spec.get("shape").and_then(serde_json::Value::as_array) else {
        return bad(format!("quantization of {name:?} has no shape"));
    };
    let shape = parse_shape(name, dims)?;
    let Some((&rows, rest)) = shape.split_first() else {
        return bad(format!("quantized {name:?} has no axes"));
    };
    let (Some(k), Some(count)) = (numel(rest), numel(&shape)) else {
        return bad(format!("quantized {name:?} has a shape too large: {shape:?}"));
    };
    let group = number("group").and_then(|g| usize::try_from(g).ok()).unwrap_or(0);
    if group == 0 || k % group != 0 {
        return bad(format!("{name:?} has {k} values per row, which groups of {group} do not split"));
    }
    if tensors.contains_key(name) {
        return bad(format!("{name:?} is stored both quantized and as floats"));
    }

    let code_name = format!("{name}.q");
    let Some(c) = codes.remove(&code_name) else {
        return bad(format!("quantized {name:?} has no integer tensor {code_name:?}"));
    };
    let packed = bits == Some(6);
    let (want_dtype, row_bytes) = if packed {
        if k % 4 != 0 {
            return bad(format!("{name:?} has {k} values per row, not a multiple of 4 for packed6"));
        }
        (Dtype::U8, k / 4 * 3)
    } else {
        (Dtype::I8, k)
    };
    if c.dtype != want_dtype || c.shape != [rows, row_bytes] {
        return bad(format!(
            "{code_name:?} is {:?} {:?}, expected {want_dtype:?} [{rows}, {row_bytes}]",
            c.dtype, c.shape
        ));
    }
    let scale_name = format!("{name}.scale");
    let Some(scales) = tensors.remove(&scale_name) else {
        return bad(format!("quantized {name:?} has no tensor {scale_name:?}"));
    };
    let per_row = k / group;
    if scales.shape() != [rows, per_row] {
        return bad(format!("{scale_name:?} has shape {:?}, expected [{rows}, {per_row}]", scales.shape()));
    }
    if scales.data().iter().any(|s| !s.is_finite()) {
        return bad(format!("{scale_name:?} holds a value that is not finite"));
    }

    let mut values = Vec::with_capacity(count);
    let mut out_of_range = false;
    if packed {
        let (triples, _) = c.bytes.as_chunks::<3>();
        for t in triples {
            let word = u32::from(t[0]) | (u32::from(t[1]) << 8) | (u32::from(t[2]) << 16);
            for i in 0..4 {
                // `as`: six bits, 0..=63.
                let code = ((word >> (6 * i)) & 63) as i32;
                out_of_range |= code == 0;
                values.push((code - 32) as f32);
            }
        }
    } else {
        for &b in c.bytes {
            let code = i8::from_le_bytes([b]);
            out_of_range |= code == i8::MIN;
            values.push(f32::from(code));
        }
    }
    if out_of_range {
        return bad(format!("{code_name:?} holds a code outside its levels"));
    }
    // Row o, group j: values o * k + j * group .. + group, scale o * per_row + j.
    for (vs, s) in values.chunks_exact_mut(group).zip(scales.data()) {
        for v in vs {
            *v *= *s;
        }
    }
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

    fn halves(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|x| half::f16::from_f32(*x).to_bits().to_le_bytes()).collect()
    }

    /// int6 codes (-31..=31) packed as the `packed6` layout stores them.
    fn pack6(codes: &[i32]) -> Vec<u8> {
        codes
            .chunks(4)
            .flat_map(|c| {
                let w = c.iter().enumerate().fold(0u32, |w, (i, q)| w | (((q + 32) as u32) << (6 * i)));
                [w as u8, (w >> 8) as u8, (w >> 16) as u8]
            })
            .collect()
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
    fn f16_widens_exactly() {
        for bits in 0..=u16::MAX {
            let want = half::f16::from_bits(bits).to_f32();
            let got = f16_to_f32(bits);
            if want.is_nan() {
                assert!(got.is_nan(), "{bits:#06x}");
            } else {
                assert_eq!(got.to_bits(), want.to_bits(), "{bits:#06x}");
            }
        }
        let h = r#"{"a":{"dtype":"F16","shape":[3],"data_offsets":[0,6]}}"#;
        let st = SafeTensors::parse(&file(h, &halves(&[1.5, -2.0, 0.0]))).unwrap();
        assert_eq!(st.get("a").unwrap().data(), &[1.5, -2.0, 0.0]);
    }

    /// A file with one int6 weight `w` [2, 8] in groups of 4 and one int8 weight `v` [2, 1, 2]
    /// plus a plain tensor; `edit` may change the header before it is written.
    fn quantized(edit: impl Fn(String) -> String) -> Vec<u8> {
        let w_codes: Vec<i32> = vec![-31, -1, 0, 1, 31, 2, -2, 5, 7, 6, 5, 4, -3, -2, -1, 0];
        let w_q = pack6(&w_codes); // 12 bytes
        let w_s = halves(&[0.5, 2.0, 1.0, 0.25]); // 8 bytes
        let v_q: Vec<u8> = [-127i8, 127, 3, -3].iter().map(|c| c.to_le_bytes()[0]).collect(); // 4 bytes
        let v_s = halves(&[0.5, 4.0]); // 4 bytes
        let p = floats(&[9.0]); // 4 bytes
        let mut data = Vec::new();
        for part in [&w_q, &w_s, &v_q, &v_s, &p] {
            data.extend_from_slice(part);
        }
        let h = concat!(
            r#"{"__metadata__":{"#,
            r#""quant.w":"{\"bits\":6,\"group\":4,\"layout\":\"packed6\",\"shape\":[2,8]}","#,
            r#""quant.v":"{\"bits\":8,\"group\":2,\"layout\":\"int8\",\"shape\":[2,1,2]}"},"#,
            r#""w.q":{"dtype":"U8","shape":[2,6],"data_offsets":[0,12]},"#,
            r#""w.scale":{"dtype":"F16","shape":[2,2],"data_offsets":[12,20]},"#,
            r#""v.q":{"dtype":"I8","shape":[2,2],"data_offsets":[20,24]},"#,
            r#""v.scale":{"dtype":"F16","shape":[2,1],"data_offsets":[24,28]},"#,
            r#""p":{"dtype":"F32","shape":[1],"data_offsets":[28,32]}}"#
        );
        file(&edit(h.to_string()), &data)
    }

    #[test]
    fn reads_quantized_weights() {
        let st = SafeTensors::parse(&quantized(|h| h)).unwrap();
        let w = st.get("w").unwrap();
        assert_eq!(w.shape(), &[2, 8]);
        let want =
            [-15.5, -0.5, 0.0, 0.5, 62.0, 4.0, -4.0, 10.0, 7.0, 6.0, 5.0, 4.0, -0.75, -0.5, -0.25, 0.0];
        assert_eq!(w.data(), &want);
        let v = st.get("v").unwrap();
        assert_eq!(v.shape(), &[2, 1, 2]);
        assert_eq!(v.data(), &[-63.5, 63.5, 12.0, -12.0]);
        assert_eq!(st.get("p").unwrap().data(), &[9.0]);
        // The parts are gone; only the weights remain.
        assert_eq!(st.names().collect::<Vec<_>>(), ["p", "v", "w"]);
    }

    #[test]
    fn rejects_hostile_quantized_weights() {
        let edits: &[(&str, &str)] = &[
            // Other bit widths, and layouts that do not fit their width.
            (r#"\"bits\":6"#, r#"\"bits\":4"#),
            (r#"\"bits\":6"#, r#"\"bits\":8"#),
            (r#"\"bits\":8"#, r#"\"bits\":16"#),
            (r#"\"bits\":8"#, r#"\"bits\":-8"#),
            (r#"\"layout\":\"packed6\""#, r#"\"layout\":\"packed4\""#),
            (r#"\"layout\":\"int8\""#, r#"\"layout\":\"packed6\""#),
            // Groups that do not divide the row, zero, missing.
            (r#"\"group\":4"#, r#"\"group\":3"#),
            (r#"\"group\":4"#, r#"\"group\":0"#),
            (r#"\"group\":4,"#, ""),
            (r#"\"group\":4"#, r#"\"group\":8"#),
            // Shapes that disagree with the codes, overflow, are empty or missing.
            (r#"\"shape\":[2,8]"#, r#"\"shape\":[2,4]"#),
            (r#"\"shape\":[2,8]"#, r#"\"shape\":[4,4]"#),
            (r#"\"shape\":[2,8]"#, r#"\"shape\":[]"#),
            (r#"\"shape\":[2,8]"#, r#"\"shape\":[4294967296,4294967296,8]"#),
            (r#",\"shape\":[2,8]"#, ""),
            (r#"\"shape\":[2,1,2]"#, r#"\"shape\":[1,4]"#),
            // Spec not JSON.
            (r#"{\"bits\":6"#, r#"{bits:6"#),
            // Codes of the wrong dtype or shape, or missing.
            (r#""w.q":{"dtype":"U8""#, r#""w.q":{"dtype":"I8""#),
            (r#""v.q":{"dtype":"I8""#, r#""v.q":{"dtype":"U8""#),
            (r#""w.q":{"dtype":"U8","shape":[2,6]"#, r#""w.q":{"dtype":"U8","shape":[3,4]"#),
            (r#""w.q""#, r#""w.codes""#),
            // Scales of the wrong shape, missing, integer.
            (r#""w.scale":{"dtype":"F16","shape":[2,2]"#, r#""w.scale":{"dtype":"F16","shape":[4]"#),
            (r#""v.scale""#, r#""v.scales""#),
            (r#""v.scale":{"dtype":"F16""#, r#""v.scale":{"dtype":"I8""#),
            // An integer tensor nothing uses; a weight stored twice.
            (r#""quant.v":"{\"bits\":8,\"group\":2,\"layout\":\"int8\",\"shape\":[2,1,2]}""#, r#""x":"y""#),
            (r#""p":"#, r#""w":"#),
            // A dtype still not read.
            (r#""p":{"dtype":"F32","shape":[1]"#, r#""p":{"dtype":"BF16","shape":[2]"#),
        ];
        for (from, to) in edits {
            let bytes = quantized(|h| {
                assert!(h.contains(from), "{from} not in the header");
                h.replacen(from, to, 1)
            });
            assert!(SafeTensors::parse(&bytes).is_err(), "{from} -> {to}");
        }
        // Truncated anywhere.
        let full = quantized(|h| h);
        for cut in 0..full.len() {
            assert!(SafeTensors::parse(&full[..cut]).is_err(), "cut at {cut}");
        }
    }

    #[test]
    fn rejects_codes_outside_their_levels_and_bad_scales() {
        let full = quantized(|h| h);
        let data = full.len() - 32;
        // Code 0 in packed6 stands for -32: the first code of w is in the low six bits.
        let mut b = full.clone();
        b[data] &= 0xc0;
        assert!(SafeTensors::parse(&b).is_err());
        // -128 in int8.
        let mut b = full.clone();
        b[data + 20] = 0x80;
        assert!(SafeTensors::parse(&b).is_err());
        // A scale of infinity or NaN.
        for bits in [0x7c00u16, 0x7e00] {
            let mut b = full.clone();
            b[data + 12..data + 14].copy_from_slice(&bits.to_le_bytes());
            assert!(SafeTensors::parse(&b).is_err(), "{bits:#06x}");
        }
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
            // Other dtypes, and a size that does not fit the dtype.
            r#"{"a":{"dtype":"F16","shape":[3],"data_offsets":[0,8]}}"#,
            r#"{"a":{"dtype":"I64","shape":[1],"data_offsets":[0,8]}}"#,
            r#"{"a":{"dtype":"BF16","shape":[4],"data_offsets":[0,8]}}"#,
            r#"{"a":{"shape":[2],"data_offsets":[0,8]}}"#,
            // Integers without a quantized weight using them.
            r#"{"a":{"dtype":"U8","shape":[8],"data_offsets":[0,8]}}"#,
            r#"{"a":{"dtype":"I8","shape":[8],"data_offsets":[0,8]}}"#,
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
            // A quantized weight whose parts are missing.
            r#"{"__metadata__":{"quant.b":"{\"bits\":8,\"group\":1,\"layout\":\"int8\",\"shape\":[1]}"}}"#,
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
