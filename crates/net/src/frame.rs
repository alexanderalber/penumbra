//! Framing: where one message ends in a byte stream (a serial port, a TCP connection).
//!
//! A [`Framer`] cuts at a delimiter (`\n` by default), after a fixed number of bytes, or, with
//! neither, takes each read as it comes. A message longer than the cap is dropped and counted;
//! with a delimiter, the bytes up to the next delimiter are dropped with it, so the stream picks
//! up again at the next message. The buffer never holds more than the cap plus one read.

/// Longest delimiter.
pub const MAX_DELIMITER: usize = 8;

/// A delimiter as typed in a param: `\n`, `\r\n`, `\t`, `\0`, `\\` and `\xHH` are escapes, every
/// other character stands for its UTF-8 bytes. Empty is no delimiter.
pub fn delimiter(text: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut b = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('0') => out.push(0),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let hex: String = chars.by_ref().take(2).collect();
                let b = u8::from_str_radix(&hex, 16)
                    .ok()
                    .filter(|_| hex.len() == 2)
                    .ok_or_else(|| format!("`\\x{hex}` is not a byte: write two hex digits, \\x0d"))?;
                out.push(b);
            }
            Some(other) => {
                return Err(format!("`\\{other}` is not an escape: use \\n, \\r, \\t, \\0, \\\\ or \\xHH"));
            }
            None => return Err("a delimiter cannot end in a single \\".to_string()),
        }
    }
    if out.len() > MAX_DELIMITER {
        return Err(format!("a delimiter has at most {MAX_DELIMITER} bytes"));
    }
    Ok(out)
}

/// What one [`Framer::push`] gave.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Frames {
    pub messages: Vec<Vec<u8>>,
    /// Messages dropped for being longer than the cap.
    pub too_long: u64,
}

/// Cuts a byte stream into messages.
#[derive(Clone, Debug)]
pub struct Framer {
    delimiter: Vec<u8>,
    length: usize,
    max: usize,
    buf: Vec<u8>,
    /// Where the search for the delimiter goes on (everything before has none).
    scanned: usize,
    /// A message over the cap is being dropped up to its delimiter.
    skipping: bool,
}

impl Framer {
    /// At `delimiter` when it is not empty, else every `length` bytes when that is not 0, else
    /// each read. `max` caps a message (at least 1); a `length` above it is cut to it.
    pub fn new(delimiter: &[u8], length: usize, max: usize) -> Self {
        let max = max.max(1);
        Framer {
            delimiter: delimiter.iter().copied().take(MAX_DELIMITER).collect(),
            length: length.min(max),
            max,
            buf: Vec::new(),
            scanned: 0,
            skipping: false,
        }
    }

    /// Bytes held for a message not yet complete.
    pub fn pending(&self) -> usize {
        self.buf.len()
    }

    /// Takes in what was read; the messages it completes go into `out`.
    pub fn push(&mut self, bytes: &[u8], out: &mut Frames) {
        if !self.delimiter.is_empty() {
            self.push_delimited(bytes, out);
        } else if self.length > 0 {
            self.buf.extend_from_slice(bytes);
            let mut start = 0;
            while let Some(m) = self.buf.get(start..start + self.length) {
                out.messages.push(m.to_vec());
                start += self.length;
            }
            self.buf.drain(..start.min(self.buf.len()));
        } else if bytes.len() > self.max {
            out.too_long += 1;
        } else if !bytes.is_empty() {
            out.messages.push(bytes.to_vec());
        }
    }

    fn push_delimited(&mut self, bytes: &[u8], out: &mut Frames) {
        self.buf.extend_from_slice(bytes);
        let d = self.delimiter.len();
        loop {
            let found = self
                .buf
                .get(self.scanned..)
                .and_then(|rest| rest.windows(d).position(|w| w == self.delimiter.as_slice()))
                .map(|p| p + self.scanned);
            let Some(at) = found else { break };
            let message = self.buf.get(..at).unwrap_or_default();
            if self.skipping {
                self.skipping = false;
            } else if message.len() > self.max {
                out.too_long += 1;
            } else {
                out.messages.push(message.to_vec());
            }
            self.buf.drain(..(at + d).min(self.buf.len()));
            self.scanned = 0;
        }
        if self.buf.len() > self.max {
            // No delimiter within the cap: drop what came, keep only what could be the start of
            // a delimiter, and skip on to the next one.
            if !self.skipping {
                out.too_long += 1;
                self.skipping = true;
            }
            let keep = d.saturating_sub(1);
            let cut = self.buf.len().saturating_sub(keep);
            self.buf.drain(..cut);
        }
        self.scanned = self.buf.len().saturating_sub(d.saturating_sub(1));
    }

    /// Forgets a message begun: the connection it came on ended.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.scanned = 0;
        self.skipping = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(f: &mut Framer, parts: &[&[u8]]) -> Frames {
        let mut out = Frames::default();
        for p in parts {
            f.push(p, &mut out);
        }
        out
    }

    #[test]
    fn delimiters_parse_with_escapes() {
        assert_eq!(delimiter("\\n").unwrap(), b"\n");
        assert_eq!(delimiter("\\r\\n").unwrap(), b"\r\n");
        assert_eq!(delimiter("\\x0d\\x0A").unwrap(), b"\r\n");
        assert_eq!(delimiter(";").unwrap(), b";");
        assert_eq!(delimiter("").unwrap(), b"");
        assert_eq!(delimiter("\\0\\t\\\\").unwrap(), b"\0\t\\");
        assert!(delimiter("\\q").is_err());
        assert!(delimiter("\\x1").is_err());
        assert!(delimiter("\\xzz").is_err());
        assert!(delimiter("\\").is_err());
        assert!(delimiter("123456789").is_err());
    }

    #[test]
    fn lines_split_across_reads() {
        let mut f = Framer::new(b"\n", 0, 100);
        let out = feed(&mut f, &[b"1,2", b",3\n4", b"\n\n5"]);
        assert_eq!(out.messages, vec![b"1,2,3".to_vec(), b"4".to_vec(), b"".to_vec()]);
        assert_eq!(f.pending(), 1);
    }

    #[test]
    fn a_long_delimiter_split_across_reads_is_found() {
        let mut f = Framer::new(b"\r\n", 0, 100);
        let out = feed(&mut f, &[b"ab\r", b"\ncd\r", b"x\r", b"\n"]);
        assert_eq!(out.messages, vec![b"ab".to_vec(), b"cd\rx".to_vec()]);
    }

    #[test]
    fn a_message_over_the_cap_is_dropped_up_to_its_delimiter() {
        let mut f = Framer::new(b"\n", 0, 8);
        let out = feed(&mut f, &[b"ok\n0123456789", b"abcdef", b"ghij\nnext\n"]);
        assert_eq!(out.messages, vec![b"ok".to_vec(), b"next".to_vec()]);
        assert_eq!(out.too_long, 1);
        // The buffer stays bounded while a stream sends no delimiter at all.
        let mut f = Framer::new(b"\n", 0, 64);
        let mut out = Frames::default();
        for _ in 0..10_000 {
            f.push(&[b'x'; 100], &mut out);
            assert!(f.pending() <= 64 + 100);
        }
        assert_eq!(out.too_long, 1);
        f.push(b"\nfine\n", &mut out);
        assert_eq!(out.messages, vec![b"fine".to_vec()]);
    }

    #[test]
    fn fixed_lengths_and_whole_reads() {
        let mut f = Framer::new(b"", 3, 100);
        let out = feed(&mut f, &[b"abcd", b"ef", b"g"]);
        assert_eq!(out.messages, vec![b"abc".to_vec(), b"def".to_vec()]);
        assert_eq!(f.pending(), 1);
        let mut f = Framer::new(b"", 0, 4);
        let out = feed(&mut f, &[b"ab", b"", b"abcde"]);
        assert_eq!(out.messages, vec![b"ab".to_vec()]);
        assert_eq!(out.too_long, 1);
    }

    #[test]
    fn random_streams_never_panic_and_stay_bounded() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for round in 0..200 {
            let d: &[u8] = match round % 4 {
                0 => b"\n",
                1 => b"\r\n",
                2 => b"",
                _ => b"\xff\x00",
            };
            let mut f = Framer::new(d, (round % 3) * 5, 1 + (round % 50));
            let mut out = Frames::default();
            for _ in 0..200 {
                let n = usize::try_from(next() % 70).unwrap();
                let bytes: Vec<u8> =
                    (0..n).map(|_| [b'\n', b'\r', 0, 0xff, b'a'][(next() % 5) as usize]).collect();
                f.push(&bytes, &mut out);
                assert!(f.pending() <= 1 + (round % 50) + 70);
            }
            assert!(out.messages.iter().all(|m| m.len() <= 1 + (round % 50)));
        }
    }
}
