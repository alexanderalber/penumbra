//! sACN (ANSI E1.31-2018): the data packet. Sent to the universe's multicast group unless a
//! host is given.

use std::net::Ipv4Addr;

use crate::SLOTS;

/// The UDP port of sACN.
pub const PORT: u16 = 5568;
/// Universes are 1 to 63999.
pub const MAX_UNIVERSE: u16 = 63999;
/// Bytes before the slot data (the start code is the last of them).
pub const HEADER: usize = 126;
/// The priority receivers assume when nobody says otherwise.
pub const DEFAULT_PRIORITY: u8 = 100;

/// The multicast group of a universe: 239.255.hi.lo.
pub fn multicast(universe: u16) -> Ipv4Addr {
    let [hi, lo] = universe.to_be_bytes();
    Ipv4Addr::new(239, 255, hi, lo)
}

/// A data packet for `universe` with start code 0. `cid` names this source for good (receivers
/// merge by it), `source` is shown in receivers' lists (cut to 63 bytes). `data` is cut to 512
/// slots.
pub fn data_packet(
    cid: &[u8; 16],
    source: &str,
    priority: u8,
    universe: u16,
    sequence: u8,
    data: &[u8],
) -> Vec<u8> {
    let data = data.get(..data.len().min(SLOTS)).unwrap_or_default();
    let total = HEADER + data.len();
    // Flags 0x7 and a 12-bit length counted from the field itself; `total` is at most 638.
    let pdu = |from: usize| (0x7000 | u16::try_from((total - from) & 0x0fff).unwrap_or(0)).to_be_bytes();
    let mut p = Vec::with_capacity(total);
    // Root layer.
    p.extend_from_slice(&0x0010u16.to_be_bytes());
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(b"ASC-E1.17\0\0\0");
    p.extend_from_slice(&pdu(16));
    p.extend_from_slice(&4u32.to_be_bytes());
    p.extend_from_slice(cid);
    // Framing layer.
    p.extend_from_slice(&pdu(38));
    p.extend_from_slice(&2u32.to_be_bytes());
    let mut cut = source.len().min(63);
    while !source.is_char_boundary(cut) {
        cut -= 1;
    }
    let mut name = [0u8; 64];
    for (d, s) in name.iter_mut().zip(source.as_bytes().iter().take(cut)) {
        *d = *s;
    }
    p.extend_from_slice(&name);
    p.push(priority.min(200));
    // Synchronization address: none.
    p.extend_from_slice(&0u16.to_be_bytes());
    p.push(sequence);
    // Options: not a preview, not terminated, not forcing synchronization.
    p.push(0);
    p.extend_from_slice(&universe.to_be_bytes());
    // DMP layer.
    p.extend_from_slice(&pdu(115));
    p.push(0x02);
    p.push(0xa1);
    p.extend_from_slice(&0u16.to_be_bytes());
    p.extend_from_slice(&1u16.to_be_bytes());
    p.extend_from_slice(&u16::try_from(data.len() + 1).unwrap_or(1).to_be_bytes());
    p.push(0);
    p.extend_from_slice(data);
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_layout() {
        let cid = [0xab; 16];
        let p = data_packet(&cid, "penumbra", 100, 0x0102, 9, &[1, 2, 3]);
        assert_eq!(p.len(), HEADER + 3);
        assert_eq!(&p[0..4], &[0x00, 0x10, 0, 0]);
        assert_eq!(&p[4..16], b"ASC-E1.17\0\0\0");
        let len = |at: usize| u16::from_be_bytes([p[at], p[at + 1]]);
        assert_eq!(len(16), 0x7000 | (p.len() - 16) as u16);
        assert_eq!(&p[18..22], &[0, 0, 0, 4]);
        assert_eq!(&p[22..38], &cid);
        assert_eq!(len(38), 0x7000 | (p.len() - 38) as u16);
        assert_eq!(&p[40..44], &[0, 0, 0, 2]);
        assert_eq!(&p[44..52], b"penumbra");
        assert_eq!(p[52], 0);
        assert_eq!(p[108], 100);
        assert_eq!(p[111], 9);
        assert_eq!(&p[113..115], &[1, 2]);
        assert_eq!(len(115), 0x7000 | (p.len() - 115) as u16);
        assert_eq!(&p[117..119], &[0x02, 0xa1]);
        assert_eq!(&p[119..125], &[0, 0, 0, 1, 0, 4]);
        assert_eq!(&p[125..], &[0, 1, 2, 3]);
    }

    #[test]
    fn long_names_and_data_are_cut() {
        let p = data_packet(&[0; 16], &"\u{fc}".repeat(40), 255, 1, 0, &[0; 700]);
        assert_eq!(p.len(), HEADER + SLOTS);
        // 31 two-byte characters fit into 63 bytes; the 64th stays the terminating zero.
        assert_eq!(p[44 + 62], 0);
        assert_eq!(p[44 + 63], 0);
        assert_eq!(p[108], 200);
    }

    #[test]
    fn multicast_groups() {
        assert_eq!(multicast(1), Ipv4Addr::new(239, 255, 0, 1));
        assert_eq!(multicast(0x1234), Ipv4Addr::new(239, 255, 0x12, 0x34));
    }
}
