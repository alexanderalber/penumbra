//! WebSocket (RFC 6455), the server side only: the handshake's accept key (SHA-1 and base64,
//! a few lines each, so no crate is needed), a bounded frame parser and the frames the server
//! sends.
//!
//! The parser reads from a buffer that may hold less than a frame and says how many bytes it
//! used. A frame's length is checked before its payload is waited for, so a client cannot make
//! the server buffer more than the cap.

/// The GUID RFC 6455 appends to the client's key.
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Largest payload of a control frame (ping, pong, close).
const MAX_CONTROL: usize = 125;

pub const OP_CONTINUATION: u8 = 0x0;
pub const OP_TEXT: u8 = 0x1;
pub const OP_BINARY: u8 = 0x2;
pub const OP_CLOSE: u8 = 0x8;
pub const OP_PING: u8 = 0x9;
pub const OP_PONG: u8 = 0xA;

/// Close codes the server sends.
pub const CLOSE_NORMAL: u16 = 1000;
pub const CLOSE_GOING_AWAY: u16 = 1001;
pub const CLOSE_PROTOCOL: u16 = 1002;
pub const CLOSE_TOO_BIG: u16 = 1009;

/// SHA-1 of `data`. Only for the handshake, where RFC 6455 asks for it; not for anything that
/// needs a secure hash.
pub fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let bits = (data.len() as u64).wrapping_mul(8);
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.as_chunks::<64>().0 {
        let mut w: Vec<u32> = chunk.as_chunks::<4>().0.iter().map(|b| u32::from_be_bytes(*b)).collect();
        for i in 16..80 {
            let at = |k: usize| w.get(i - k).copied().unwrap_or(0);
            let x = (at(3) ^ at(8) ^ at(14) ^ at(16)).rotate_left(1);
            w.push(x);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        let [h0, h1, h2, h3, h4] = h;
        h = [
            h0.wrapping_add(a),
            h1.wrapping_add(b),
            h2.wrapping_add(c),
            h3.wrapping_add(d),
            h4.wrapping_add(e),
        ];
    }
    let mut out = [0u8; 20];
    for (o, b) in out.iter_mut().zip(h.iter().flat_map(|x| x.to_be_bytes())) {
        *o = b;
    }
    out
}

/// Standard base64 with padding.
pub fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let sym = |n: u32| char::from(ALPHABET.get((n & 63) as usize).copied().unwrap_or(b'A'));
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let byte = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (byte(0) << 16) | (byte(1) << 8) | byte(2);
        s.push(sym(n >> 18));
        s.push(sym(n >> 12));
        s.push(if chunk.len() > 1 { sym(n >> 6) } else { '=' });
        s.push(if chunk.len() > 2 { sym(n) } else { '=' });
    }
    s
}

/// The `Sec-WebSocket-Accept` for a client's `Sec-WebSocket-Key`.
pub fn accept_key(key: &str) -> String {
    base64(&sha1(format!("{}{GUID}", key.trim()).as_bytes()))
}

/// One frame as it arrived, unmasked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub fin: bool,
    pub opcode: u8,
    pub payload: Vec<u8>,
}

/// Why a frame was refused; the connection closes with [`FrameError::code`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Against the protocol: unmasked, reserved bits, an unknown opcode, a long or fragmented
    /// control frame.
    Protocol(&'static str),
    /// A payload over the cap.
    TooBig(u64),
}

impl FrameError {
    pub fn code(&self) -> u16 {
        match self {
            FrameError::Protocol(_) => CLOSE_PROTOCOL,
            FrameError::TooBig(_) => CLOSE_TOO_BIG,
        }
    }
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Protocol(why) => write!(f, "{why}"),
            FrameError::TooBig(n) => write!(f, "a frame of {n} bytes is over the cap"),
        }
    }
}

/// Reads one client frame from the start of `buf`: `Ok(None)` while `buf` holds less than a
/// frame, else the frame and the bytes it took. Payloads over `max` are refused as soon as the
/// header is complete.
pub fn parse_frame(buf: &[u8], max: usize) -> Result<Option<(Frame, usize)>, FrameError> {
    let (Some(&b0), Some(&b1)) = (buf.first(), buf.get(1)) else { return Ok(None) };
    let fin = b0 & 0x80 != 0;
    if b0 & 0x70 != 0 {
        return Err(FrameError::Protocol("reserved bits set (no extension was agreed)"));
    }
    let opcode = b0 & 0x0F;
    if !matches!(opcode, OP_CONTINUATION | OP_TEXT | OP_BINARY | OP_CLOSE | OP_PING | OP_PONG) {
        return Err(FrameError::Protocol("unknown opcode"));
    }
    if b1 & 0x80 == 0 {
        return Err(FrameError::Protocol("a client frame must be masked"));
    }
    let (len, mut at): (u64, usize) = match b1 & 0x7F {
        126 => {
            let Some(b) = buf.get(2..4) else { return Ok(None) };
            (
                u64::from(u16::from_be_bytes([
                    b.first().copied().unwrap_or(0),
                    b.get(1).copied().unwrap_or(0),
                ])),
                4,
            )
        }
        127 => {
            let Some(b) = buf.get(2..10) else { return Ok(None) };
            let mut a = [0u8; 8];
            a.copy_from_slice(b);
            (u64::from_be_bytes(a), 10)
        }
        n => (u64::from(n), 2),
    };
    let control = opcode >= OP_CLOSE;
    if control && (!fin || len > MAX_CONTROL as u64) {
        return Err(FrameError::Protocol("a control frame is short and not fragmented"));
    }
    if len > max as u64 {
        return Err(FrameError::TooBig(len));
    }
    let Some(mask) = buf.get(at..at + 4) else { return Ok(None) };
    let mask: Vec<u8> = mask.to_vec();
    at += 4;
    // `len` is at most `max`, a usize.
    let len = usize::try_from(len).map_err(|_| FrameError::TooBig(len))?;
    let Some(data) = buf.get(at..at + len) else { return Ok(None) };
    let payload: Vec<u8> = data.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m).collect();
    Ok(Some((Frame { fin, opcode, payload }, at + len)))
}

/// A server frame (unmasked, not fragmented).
pub fn frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 10);
    out.push(0x80 | (opcode & 0x0F));
    match payload.len() {
        n if n < 126 => out.push(n as u8),
        n if n <= usize::from(u16::MAX) => {
            out.push(126);
            out.extend_from_slice(&(n as u16).to_be_bytes());
        }
        n => {
            out.push(127);
            out.extend_from_slice(&(n as u64).to_be_bytes());
        }
    }
    out.extend_from_slice(payload);
    out
}

/// A close frame with a code and a reason (cut to fit a control frame).
pub fn close_frame(code: u16, reason: &str) -> Vec<u8> {
    let mut payload = code.to_be_bytes().to_vec();
    let mut r = reason.to_string();
    while r.len() > MAX_CONTROL - 2 {
        r.pop();
    }
    payload.extend_from_slice(r.as_bytes());
    frame(OP_CLOSE, &payload)
}

/// Puts fragmented messages back together, up to a cap.
#[derive(Default)]
pub struct Assembler {
    /// The opcode of the message being assembled and what came so far.
    partial: Option<(u8, Vec<u8>)>,
}

/// A whole message.
#[derive(Debug, PartialEq, Eq)]
pub enum Whole {
    Text(String),
    Binary(usize),
}

impl Assembler {
    /// Takes a data frame (text, binary, continuation); a whole message once its last frame
    /// came.
    pub fn take(&mut self, f: Frame, max: usize) -> Result<Option<Whole>, FrameError> {
        let (opcode, data) = match (f.opcode, self.partial.take()) {
            (OP_CONTINUATION, Some((op, mut data))) => {
                if data.len() + f.payload.len() > max {
                    return Err(FrameError::TooBig((data.len() + f.payload.len()) as u64));
                }
                data.extend_from_slice(&f.payload);
                (op, data)
            }
            (OP_CONTINUATION, None) => return Err(FrameError::Protocol("a continuation without a start")),
            (_, Some(_)) => return Err(FrameError::Protocol("a new message inside a fragmented one")),
            (op, None) => (op, f.payload),
        };
        if !f.fin {
            self.partial = Some((opcode, data));
            return Ok(None);
        }
        Ok(Some(match opcode {
            OP_TEXT => Whole::Text(
                String::from_utf8(data)
                    .map_err(|_| FrameError::Protocol("a text frame that is not UTF-8"))?,
            ),
            _ => Whole::Binary(data.len()),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client frame, masked.
    pub fn client_frame(fin: bool, opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [0x12u8, 0x34, 0x56, 0x78];
        let mut out = vec![(if fin { 0x80 } else { 0 }) | opcode];
        match payload.len() {
            n if n < 126 => out.push(0x80 | n as u8),
            n if n <= 65535 => {
                out.push(0x80 | 126);
                out.extend_from_slice(&(n as u16).to_be_bytes());
            }
            n => {
                out.push(0x80 | 127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(&mask);
        out.extend(payload.iter().zip(mask.iter().cycle()).map(|(b, m)| b ^ m));
        out
    }

    #[test]
    fn sha1_and_base64_match_known_values() {
        let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        let long = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(hex(&sha1(long)), "84983e441c3bd26ebaae4aa1f95129e5e54670f1");
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // The example of RFC 6455, section 1.3.
        assert_eq!(accept_key("dGhlIHNhbXBsZSBub25jZQ=="), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn frames_parse_in_pieces_and_refuse_what_is_wrong() {
        let f = client_frame(true, OP_TEXT, b"{\"/a\": 1}");
        for cut in 0..f.len() {
            assert_eq!(parse_frame(&f[..cut], 1024), Ok(None), "{cut}");
        }
        let (frame, used) = parse_frame(&f, 1024).unwrap().unwrap();
        assert_eq!((frame.payload.as_slice(), used), (&b"{\"/a\": 1}"[..], f.len()));
        let big = client_frame(true, OP_TEXT, &vec![b'x'; 70_000]);
        assert_eq!(parse_frame(&big[..14], 65_536), Err(FrameError::TooBig(70_000)));
        assert!(parse_frame(&big, 100_000).unwrap().is_some());
        let mid = client_frame(true, OP_TEXT, &vec![b'x'; 300]);
        assert_eq!(parse_frame(&mid, 1024).unwrap().unwrap().0.payload.len(), 300);
        // Unmasked, reserved bits, unknown opcode, a long ping.
        assert!(matches!(parse_frame(&[0x81, 0x01, b'x'], 1024), Err(FrameError::Protocol(_))));
        assert!(matches!(parse_frame(&[0xC1, 0x80, 0, 0, 0, 0], 1024), Err(FrameError::Protocol(_))));
        assert!(matches!(parse_frame(&[0x83, 0x80, 0, 0, 0, 0], 1024), Err(FrameError::Protocol(_))));
        assert!(matches!(
            parse_frame(&client_frame(true, OP_PING, &[0; 126]), 1024),
            Err(FrameError::Protocol(_))
        ));
    }

    #[test]
    fn fragments_assemble_up_to_the_cap() {
        let mut a = Assembler::default();
        let part = |fin, op, p: &[u8]| Frame { fin, opcode: op, payload: p.to_vec() };
        assert_eq!(a.take(part(false, OP_TEXT, b"ab"), 8), Ok(None));
        assert_eq!(a.take(part(true, OP_CONTINUATION, b"cd"), 8), Ok(Some(Whole::Text("abcd".into()))));
        assert_eq!(a.take(part(false, OP_TEXT, b"abcde"), 8), Ok(None));
        assert!(matches!(a.take(part(true, OP_CONTINUATION, b"fghi"), 8), Err(FrameError::TooBig(9))));
        assert!(a.take(part(true, OP_CONTINUATION, b"x"), 8).is_err());
        assert!(a.take(part(true, OP_TEXT, &[0xFF, 0xFE]), 8).is_err());
        assert_eq!(a.take(part(true, OP_BINARY, b"xyz"), 8), Ok(Some(Whole::Binary(3))));
    }

    #[test]
    fn server_frames_carry_their_length() {
        assert_eq!(frame(OP_TEXT, b"hi"), vec![0x81, 2, b'h', b'i']);
        assert_eq!(&frame(OP_TEXT, &[0; 200])[..4], &[0x81, 126, 0, 200]);
        assert_eq!(&frame(OP_TEXT, &[0; 70_000])[..2], &[0x81, 127]);
        let c = close_frame(CLOSE_TOO_BIG, &"x".repeat(300));
        assert_eq!(c.len(), 2 + 125);
    }
}
