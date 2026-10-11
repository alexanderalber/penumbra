//! OSC 1.0 packets: a bounded parser and an encoder.
//!
//! The parser reads from the received datagram only: every length is checked against the bytes
//! that are left before anything is taken, so a hostile packet can make it allocate at most a
//! small multiple of its own size. Counts are capped by [`Limits`] (messages per packet, args
//! per message, bundle depth, address length); a packet over a limit is refused as a whole.
//! Nothing here panics or indexes unchecked.
//!
//! Supported type tags: `i f s S b h t d c r m T F N I` and arrays (`[` `]`, flattened). A message
//! without a type tag string (OSC before 1.0) has no args.

use std::time::{SystemTime, UNIX_EPOCH};

/// The largest payload of one UDP datagram over IPv4.
pub const MAX_PACKET: usize = 65_507;

/// The timetag meaning "now".
pub const IMMEDIATE: u64 = 1;

/// Seconds from 1900 (NTP's epoch) to 1970.
const NTP_UNIX_OFFSET: u64 = 2_208_988_800;

/// One argument of a message.
#[derive(Clone, Debug, PartialEq)]
pub enum Arg {
    Int(i32),
    Float(f32),
    Str(String),
    Blob(Vec<u8>),
    Long(i64),
    Double(f64),
    /// A timetag (NTP format).
    Time(u64),
    /// An ASCII character sent as 32 bits.
    Char(u32),
    /// 32-bit RGBA.
    Color(u32),
    /// Port id, status byte, data 1, data 2.
    Midi([u8; 4]),
    True,
    False,
    Nil,
    /// "Infinitum" in OSC 1.0, "impulse" (a bang) in 1.1.
    Impulse,
}

impl Arg {
    /// The arg as a number: ints, floats, booleans (1 and 0), an impulse (1). `None` for text,
    /// blobs and the rest. Non-finite floats come out as they are; callers decide.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Arg::Int(v) => Some(f64::from(*v)),
            Arg::Float(v) => Some(f64::from(*v)),
            // `as` is the conversion wanted; i64 beyond 2^53 loses precision, not range.
            Arg::Long(v) => Some(*v as f64),
            Arg::Double(v) => Some(*v),
            Arg::True | Arg::Impulse => Some(1.0),
            Arg::False => Some(0.0),
            _ => None,
        }
    }

    /// The arg as text: strings and symbols.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Arg::Str(s) => Some(s),
            _ => None,
        }
    }

    fn tag(&self) -> u8 {
        match self {
            Arg::Int(_) => b'i',
            Arg::Float(_) => b'f',
            Arg::Str(_) => b's',
            Arg::Blob(_) => b'b',
            Arg::Long(_) => b'h',
            Arg::Double(_) => b'd',
            Arg::Time(_) => b't',
            Arg::Char(_) => b'c',
            Arg::Color(_) => b'r',
            Arg::Midi(_) => b'm',
            Arg::True => b'T',
            Arg::False => b'F',
            Arg::Nil => b'N',
            Arg::Impulse => b'I',
        }
    }
}

impl std::fmt::Display for Arg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Arg::Int(v) => write!(f, "{v}"),
            Arg::Float(v) => write!(f, "{v:.3}"),
            Arg::Str(s) => write!(f, "{s:?}"),
            Arg::Blob(b) => write!(f, "<{} bytes>", b.len()),
            Arg::Long(v) => write!(f, "{v}"),
            Arg::Double(v) => write!(f, "{v:.3}"),
            Arg::Time(t) => write!(f, "time {t:#x}"),
            Arg::Char(c) => write!(f, "{:?}", char::from_u32(*c).unwrap_or('?')),
            Arg::Color(c) => write!(f, "#{c:08x}"),
            Arg::Midi(m) => write!(f, "midi {m:?}"),
            Arg::True => f.write_str("true"),
            Arg::False => f.write_str("false"),
            Arg::Nil => f.write_str("nil"),
            Arg::Impulse => f.write_str("impulse"),
        }
    }
}

/// An OSC message.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub address: String,
    pub args: Vec<Arg>,
}

impl Message {
    pub fn new(address: impl Into<String>, args: Vec<Arg>) -> Self {
        Self { address: address.into(), args }
    }
}

/// Caps on what one packet may hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Limits {
    /// Messages in one packet, counted through all bundles.
    pub max_messages: usize,
    /// Args of one message.
    pub max_args: usize,
    /// Bundles inside bundles, and arrays inside arrays.
    pub max_depth: usize,
    /// Bytes of an address.
    pub max_address: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_messages: 256, max_args: 256, max_depth: 8, max_address: 512 }
    }
}

/// Why a packet was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParseError(pub &'static str);

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// The messages of one packet, each with the timetag of the bundle it came in ([`IMMEDIATE`]
/// for a message on its own).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Packet {
    pub messages: Vec<(u64, Message)>,
    /// Bundles seen, nested ones included.
    pub bundles: usize,
}

type Result<T> = std::result::Result<T, ParseError>;

const BUNDLE: &[u8; 8] = b"#bundle\0";

/// Parses one datagram.
pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Packet> {
    if bytes.len() > MAX_PACKET {
        return Err(ParseError("packet larger than a UDP datagram"));
    }
    let mut packet = Packet::default();
    element(bytes, limits, 0, IMMEDIATE, &mut packet)?;
    Ok(packet)
}

fn element(bytes: &[u8], limits: &Limits, depth: usize, time: u64, out: &mut Packet) -> Result<()> {
    if bytes.starts_with(BUNDLE) {
        if depth >= limits.max_depth {
            return Err(ParseError("bundles nest too deep"));
        }
        out.bundles += 1;
        let mut r = Reader::new(bytes);
        r.take(BUNDLE.len())?;
        let time = r.u64()?;
        while !r.done() {
            let size = r.i32()?;
            let size = usize::try_from(size).map_err(|_| ParseError("negative bundle element size"))?;
            if size == 0 {
                return Err(ParseError("empty bundle element"));
            }
            let inner = r.take(size)?;
            element(inner, limits, depth + 1, time, out)?;
        }
        Ok(())
    } else {
        if out.messages.len() >= limits.max_messages {
            return Err(ParseError("too many messages in one packet"));
        }
        let m = message(bytes, limits)?;
        out.messages.push((time, m));
        Ok(())
    }
}

fn message(bytes: &[u8], limits: &Limits) -> Result<Message> {
    let mut r = Reader::new(bytes);
    let address = r.string()?;
    if !address.starts_with('/') {
        return Err(ParseError("an address starts with /"));
    }
    if address.len() > limits.max_address {
        return Err(ParseError("address too long"));
    }
    let mut args = Vec::new();
    if r.done() {
        return Ok(Message { address: address.to_string(), args });
    }
    let tags = r.string()?;
    let Some(tags) = tags.strip_prefix(',') else {
        // OSC before 1.0: no type tags, args unknown.
        return Ok(Message { address: address.to_string(), args });
    };
    let mut depth = 0usize;
    for tag in tags.bytes() {
        let arg = match tag {
            b'i' => Arg::Int(r.i32()?),
            b'f' => Arg::Float(f32::from_bits(r.u32()?)),
            b's' | b'S' => Arg::Str(r.string()?.to_string()),
            b'b' => Arg::Blob(r.blob()?.to_vec()),
            b'h' => Arg::Long(i64::from_be_bytes(r.u64()?.to_be_bytes())),
            b't' => Arg::Time(r.u64()?),
            b'd' => Arg::Double(f64::from_bits(r.u64()?)),
            b'c' => Arg::Char(r.u32()?),
            b'r' => Arg::Color(r.u32()?),
            b'm' => {
                let b = r.u32()?.to_be_bytes();
                Arg::Midi(b)
            }
            b'T' => Arg::True,
            b'F' => Arg::False,
            b'N' => Arg::Nil,
            b'I' => Arg::Impulse,
            b'[' => {
                depth += 1;
                if depth > limits.max_depth {
                    return Err(ParseError("arrays nest too deep"));
                }
                continue;
            }
            b']' => {
                depth = depth.checked_sub(1).ok_or(ParseError("] without ["))?;
                continue;
            }
            _ => return Err(ParseError("unknown type tag")),
        };
        if args.len() >= limits.max_args {
            return Err(ParseError("too many args"));
        }
        args.push(arg);
    }
    Ok(Message { address: address.to_string(), args })
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(ParseError("length overflows"))?;
        let s = self.bytes.get(self.pos..end).ok_or(ParseError("packet ends early"))?;
        self.pos = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32> {
        let s = self.take(4)?;
        let a: [u8; 4] = s.try_into().map_err(|_| ParseError("packet ends early"))?;
        Ok(u32::from_be_bytes(a))
    }

    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_be_bytes(self.u32()?.to_be_bytes()))
    }

    fn u64(&mut self) -> Result<u64> {
        let s = self.take(8)?;
        let a: [u8; 8] = s.try_into().map_err(|_| ParseError("packet ends early"))?;
        Ok(u64::from_be_bytes(a))
    }

    /// Skips the padding after `len` bytes up to a multiple of four.
    fn pad(&mut self, len: usize) -> Result<()> {
        let pad = (4 - len % 4) % 4;
        self.take(pad).map(|_| ())
    }

    /// A string ends with a zero byte and is padded with zeros to a multiple of four.
    fn string(&mut self) -> Result<&'a str> {
        let rest = self.bytes.get(self.pos..).ok_or(ParseError("packet ends early"))?;
        let len = rest.iter().position(|b| *b == 0).ok_or(ParseError("string without end"))?;
        let s = self.take(len)?;
        // The terminator plus padding: len + 1 rounded up to four.
        let total = (len + 1).div_ceil(4) * 4;
        self.take(total - len)?;
        std::str::from_utf8(s).map_err(|_| ParseError("string is not UTF-8"))
    }

    fn blob(&mut self) -> Result<&'a [u8]> {
        let size = usize::try_from(self.i32()?).map_err(|_| ParseError("negative blob size"))?;
        let s = self.take(size)?;
        self.pad(size)?;
        Ok(s)
    }
}

// ---------------------------------------------------------------------------------------------
// Encoding

fn put_str(out: &mut Vec<u8>, s: &str) {
    // A zero byte would end the string early; what follows it is left out.
    let s = s.split('\0').next().unwrap_or("");
    out.extend_from_slice(s.as_bytes());
    let pad = 4 - s.len() % 4;
    out.extend(std::iter::repeat_n(0u8, pad));
}

/// Appends `msg` as an OSC message.
pub fn encode_message(msg: &Message, out: &mut Vec<u8>) {
    put_str(out, &msg.address);
    let mut tags = String::with_capacity(msg.args.len() + 1);
    tags.push(',');
    for a in &msg.args {
        tags.push(char::from(a.tag()));
    }
    put_str(out, &tags);
    for a in &msg.args {
        match a {
            Arg::Int(v) => out.extend_from_slice(&v.to_be_bytes()),
            Arg::Float(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
            Arg::Str(s) => put_str(out, s),
            Arg::Blob(b) => {
                let len = i32::try_from(b.len()).unwrap_or(i32::MAX);
                out.extend_from_slice(&len.to_be_bytes());
                let b = b.get(..usize::try_from(len).unwrap_or(0)).unwrap_or(&[]);
                out.extend_from_slice(b);
                out.extend(std::iter::repeat_n(0u8, (4 - b.len() % 4) % 4));
            }
            Arg::Long(v) => out.extend_from_slice(&v.to_be_bytes()),
            Arg::Double(v) => out.extend_from_slice(&v.to_bits().to_be_bytes()),
            Arg::Time(t) => out.extend_from_slice(&t.to_be_bytes()),
            Arg::Char(c) | Arg::Color(c) => out.extend_from_slice(&c.to_be_bytes()),
            Arg::Midi(m) => out.extend_from_slice(m),
            Arg::True | Arg::False | Arg::Nil | Arg::Impulse => {}
        }
    }
}

/// `msg` as a packet of its own.
pub fn message_packet(msg: &Message) -> Vec<u8> {
    let mut out = Vec::new();
    encode_message(msg, &mut out);
    out
}

/// Packets holding `messages`, each at most `max` bytes where possible (a single message larger
/// than that goes out alone). With `bundle`, each packet is one bundle with the immediate
/// timetag; without, one message per packet.
pub fn packets(messages: &[Message], max: usize, bundle: bool) -> Vec<Vec<u8>> {
    if !bundle {
        return messages.iter().map(message_packet).collect();
    }
    let mut out = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut encoded = Vec::new();
    for m in messages {
        encoded.clear();
        encode_message(m, &mut encoded);
        let need = 4 + encoded.len();
        if !current.is_empty() && current.len() + need > max {
            out.push(std::mem::take(&mut current));
        }
        if current.is_empty() {
            current.extend_from_slice(BUNDLE);
            current.extend_from_slice(&IMMEDIATE.to_be_bytes());
        }
        let len = i32::try_from(encoded.len()).unwrap_or(i32::MAX);
        current.extend_from_slice(&len.to_be_bytes());
        current.extend_from_slice(&encoded);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// The wall clock as an NTP timetag (seconds since 1900 in the high 32 bits).
pub fn ntp_now() -> u64 {
    let Ok(d) = SystemTime::now().duration_since(UNIX_EPOCH) else { return IMMEDIATE };
    let secs = d.as_secs().saturating_add(NTP_UNIX_OFFSET);
    let frac = (u64::from(d.subsec_nanos()) << 32) / 1_000_000_000;
    (secs << 32) | frac
}

/// Whether `time` lies more than `slack_ms` after `now` (both NTP timetags).
pub fn is_future(time: u64, now: u64, slack_ms: u64) -> bool {
    let slack = (slack_ms << 32) / 1000;
    time != IMMEDIATE && time > now.saturating_add(slack)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(m: &Message) -> Message {
        let p = parse(&message_packet(m), &Limits::default()).unwrap();
        assert_eq!(p.messages.len(), 1);
        p.messages[0].1.clone()
    }

    #[test]
    fn messages_round_trip() {
        let m = Message::new(
            "/scene/speed",
            vec![
                Arg::Float(0.5),
                Arg::Int(-3),
                Arg::Str("abc".into()),
                Arg::Str("abcd".into()),
                Arg::Blob(vec![1, 2, 3, 4, 5]),
                Arg::Long(-1 << 40),
                Arg::Double(1.25),
                Arg::Time(IMMEDIATE),
                Arg::Char(u32::from('x')),
                Arg::Color(0xff00_80ff),
                Arg::Midi([0, 0x90, 60, 127]),
                Arg::True,
                Arg::False,
                Arg::Nil,
                Arg::Impulse,
            ],
        );
        assert_eq!(roundtrip(&m), m);
        assert_eq!(roundtrip(&Message::new("/a", vec![])), Message::new("/a", vec![]));
    }

    #[test]
    fn known_bytes_from_the_spec() {
        // "/foo" with 1000, -1, "hello", 1.234, 5.678 from the OSC 1.0 spec's examples.
        let mut b = Vec::new();
        b.extend_from_slice(b"/foo\0\0\0\0,iisff\0\0");
        b.extend_from_slice(&1000i32.to_be_bytes());
        b.extend_from_slice(&(-1i32).to_be_bytes());
        b.extend_from_slice(b"hello\0\0\0");
        b.extend_from_slice(&1.234f32.to_bits().to_be_bytes());
        b.extend_from_slice(&5.678f32.to_bits().to_be_bytes());
        let p = parse(&b, &Limits::default()).unwrap();
        let m = &p.messages[0].1;
        assert_eq!(m.address, "/foo");
        assert_eq!(m.args[0], Arg::Int(1000));
        assert_eq!(m.args[2], Arg::Str("hello".into()));
        assert_eq!(m.args[4].as_f64().map(|v| (v * 1000.0).round()), Some(5678.0));
    }

    #[test]
    fn bundles_nest_and_carry_their_timetags() {
        let a = message_packet(&Message::new("/a", vec![Arg::Float(1.0)]));
        let b = message_packet(&Message::new("/b", vec![Arg::Int(2)]));
        let mut inner = BUNDLE.to_vec();
        inner.extend_from_slice(&7u64.to_be_bytes());
        inner.extend_from_slice(&(b.len() as i32).to_be_bytes());
        inner.extend_from_slice(&b);
        let mut outer = BUNDLE.to_vec();
        outer.extend_from_slice(&IMMEDIATE.to_be_bytes());
        outer.extend_from_slice(&(a.len() as i32).to_be_bytes());
        outer.extend_from_slice(&a);
        outer.extend_from_slice(&(inner.len() as i32).to_be_bytes());
        outer.extend_from_slice(&inner);
        let p = parse(&outer, &Limits::default()).unwrap();
        assert_eq!(p.bundles, 2);
        assert_eq!(p.messages[0], (IMMEDIATE, Message::new("/a", vec![Arg::Float(1.0)])));
        assert_eq!(p.messages[1], (7, Message::new("/b", vec![Arg::Int(2)])));
    }

    #[test]
    fn malformed_packets_are_refused() {
        let l = Limits::default();
        for bad in [
            &b""[..],
            b"/a",                                          // no terminator
            b"abc\0",                                       // no slash
            b"/a\0\0,f\0\0\0\0",                            // float cut short
            b"/a\0\0,q\0\0",                                // unknown tag
            b"/a\0\0,b\0\0\xff\xff\xff\xff",                // negative blob
            b"/a\0\0,b\0\0\x7f\xff\xff\xff",                // blob larger than the packet
            b"#bundle\0\0\0\0\0\0\0\0\x01\xff\xff\xff\xf0", // negative element
            b"#bundle\0\0\0\0\0\0\0\0\x01\x00\x00\x00\x00", // empty element
            b"/a\0\0,]\0\0",
            b"/\xff\0\0",
        ] {
            assert!(parse(bad, &l).is_err(), "{bad:?}");
        }
        // Too many args, too deep.
        let many = Message::new("/a", vec![Arg::Nil; 300]);
        assert!(parse(&message_packet(&many), &l).is_err());
        let mut deep = message_packet(&Message::new("/a", vec![]));
        for _ in 0..10 {
            let mut b = BUNDLE.to_vec();
            b.extend_from_slice(&IMMEDIATE.to_be_bytes());
            b.extend_from_slice(&(deep.len() as i32).to_be_bytes());
            b.extend_from_slice(&deep);
            deep = b;
        }
        assert_eq!(parse(&deep, &l), Err(ParseError("bundles nest too deep")));
        // Old style: no type tags.
        assert_eq!(parse(b"/a\0\0", &l).unwrap().messages[0].1.args, vec![]);
    }

    #[test]
    fn packets_split_bundles_by_size() {
        let ms: Vec<Message> =
            (0..100).map(|i| Message::new(format!("/ch{i}"), vec![Arg::Float(0.5)])).collect();
        let ps = packets(&ms, 512, true);
        assert!(ps.len() > 1);
        assert!(ps.iter().all(|p| p.len() <= 512));
        let back: Vec<Message> = ps
            .iter()
            .flat_map(|p| parse(p, &Limits::default()).unwrap().messages.into_iter().map(|(_, m)| m))
            .collect();
        assert_eq!(back, ms);
        assert_eq!(packets(&ms, 512, false).len(), 100);
    }

    #[test]
    fn timetags() {
        let now = ntp_now();
        assert!(now > (NTP_UNIX_OFFSET << 32));
        assert!(!is_future(IMMEDIATE, now, 0));
        assert!(!is_future(now, now, 10));
        assert!(is_future(now + (1 << 32), now, 10));
    }
}
