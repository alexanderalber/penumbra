//! Art-Net 4: the ArtDmx packet (OpCode 0x5000), built for sending and read when receiving.
//! Receivers learn about a sender from its ArtDmx packets; ArtPoll replies are optional for a
//! controller that is told where to send, so neither side answers polls.

use crate::SLOTS;

/// The UDP port every Art-Net node listens on.
pub const PORT: u16 = 6454;
/// Highest port address (15 bits: net, sub-net, universe).
pub const MAX_UNIVERSE: u16 = 0x7fff;
/// Bytes before the data.
pub const HEADER: usize = 18;

/// An ArtDmx packet for `universe` (the 15-bit port address, as on the wire: the first is 0).
/// `data` is cut to 512 slots and padded with zeros to an even length of at least 2, as the
/// standard asks. `sequence` 1 to 255 lets a receiver reorder packets; 0 switches that off.
pub fn dmx_packet(universe: u16, sequence: u8, data: &[u8]) -> Vec<u8> {
    let data = data.get(..data.len().min(SLOTS)).unwrap_or_default();
    let len = (data.len().max(2) + 1) & !1;
    let universe = universe & MAX_UNIVERSE;
    let mut p = Vec::with_capacity(HEADER + len);
    p.extend_from_slice(b"Art-Net\0");
    p.extend_from_slice(&0x5000u16.to_le_bytes());
    p.extend_from_slice(&14u16.to_be_bytes());
    p.push(sequence);
    // Physical: the input port the data came from; informational.
    p.push(0);
    // SubUni (the low byte), then Net.
    p.extend_from_slice(&universe.to_le_bytes());
    p.extend_from_slice(&u16::try_from(len).unwrap_or(2).to_be_bytes());
    p.extend_from_slice(data);
    p.resize(HEADER + len, 0);
    p
}

/// What an ArtDmx packet carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dmx<'a> {
    /// The 15-bit port address.
    pub universe: u16,
    pub sequence: u8,
    /// At most 512 slots.
    pub data: &'a [u8],
}

/// Reads an ArtDmx packet; `None` for any other packet. A length past the packet's end is cut
/// to what is there.
pub fn parse_dmx(p: &[u8]) -> Option<Dmx<'_>> {
    if p.get(..8)? != b"Art-Net\0" || p.get(8..10)? != [0x00, 0x50] {
        return None;
    }
    let sequence = *p.get(12)?;
    let universe = u16::from_le_bytes([*p.get(14)?, *p.get(15)?]) & MAX_UNIVERSE;
    let len = usize::from(u16::from_be_bytes([*p.get(16)?, *p.get(17)?])).min(SLOTS);
    let rest = p.get(HEADER..)?;
    let data = rest.get(..len.min(rest.len()))?;
    Some(Dmx { universe, sequence, data })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_read_back() {
        let p = dmx_packet(0x0123, 7, &[10, 20, 30]);
        let d = parse_dmx(&p).unwrap();
        assert_eq!((d.universe, d.sequence, d.data), (0x0123, 7, &[10, 20, 30, 0][..]));
        // Cut short: what is there.
        assert_eq!(parse_dmx(&p[..HEADER + 1]).unwrap().data, &[10]);
        assert!(parse_dmx(&p[..10]).is_none());
        let mut poll = p.clone();
        poll[9] = 0x20;
        assert!(parse_dmx(&poll).is_none());
        assert!(parse_dmx(b"Art-Net").is_none());
    }

    #[test]
    fn packet_layout() {
        let p = dmx_packet(0x0123, 7, &[10, 20, 30]);
        assert_eq!(&p[..8], b"Art-Net\0");
        assert_eq!(&p[8..10], &[0x00, 0x50]);
        assert_eq!(&p[10..12], &[0, 14]);
        assert_eq!(p[12], 7);
        assert_eq!(&p[14..16], &[0x23, 0x01]);
        // Length rounded up to even, big endian.
        assert_eq!(&p[16..18], &[0, 4]);
        assert_eq!(&p[18..], &[10, 20, 30, 0]);
    }

    #[test]
    fn short_and_long_data() {
        assert_eq!(dmx_packet(0, 0, &[]).len(), HEADER + 2);
        let p = dmx_packet(0xffff, 0, &[1; 600]);
        assert_eq!(p.len(), HEADER + SLOTS);
        assert_eq!(&p[14..16], &[0xff, 0x7f]);
    }
}
