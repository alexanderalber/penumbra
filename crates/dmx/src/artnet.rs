//! Art-Net 4: the ArtDmx packet (OpCode 0x5000), the only one an output needs. Receivers learn
//! about a sender from its ArtDmx packets; ArtPoll replies are optional for a controller that
//! is told where to send.

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

#[cfg(test)]
mod tests {
    use super::*;

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
