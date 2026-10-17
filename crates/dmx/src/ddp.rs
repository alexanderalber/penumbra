//! DDP (Distributed Display Protocol), what pixel controllers such as WLED take: no universes,
//! a byte offset into the controller's buffer and up to 1440 bytes per packet; the last packet
//! of a frame carries the push flag, which shows it.

/// The UDP port of DDP.
pub const PORT: u16 = 4048;
/// Data bytes per packet (480 RGB pixels), as the protocol recommends.
pub const CHUNK: usize = 1440;
/// Bytes before the data.
pub const HEADER: usize = 10;

/// Version 1.
const VERSION: u8 = 0x40;
const PUSH: u8 = 0x01;
/// RGB, 8 bits per channel.
const RGB8: u8 = 0x0b;
/// The display's default output device.
const DEFAULT_ID: u8 = 1;

/// The packets of one frame: `data` from byte 0 of the controller's buffer, in chunks, the
/// last one pushed. `sequence` uses its low four bits (1 to 15; 0 means no sequence numbers).
/// Nothing for empty data.
pub fn packets(sequence: u8, data: &[u8]) -> Vec<Vec<u8>> {
    let count = data.len().div_ceil(CHUNK);
    data.chunks(CHUNK)
        .enumerate()
        .map(|(i, chunk)| {
            let mut p = Vec::with_capacity(HEADER + chunk.len());
            p.push(if i + 1 == count { VERSION | PUSH } else { VERSION });
            p.push(sequence & 0x0f);
            p.push(RGB8);
            p.push(DEFAULT_ID);
            p.extend_from_slice(&u32::try_from(i * CHUNK).unwrap_or(u32::MAX).to_be_bytes());
            p.extend_from_slice(&u16::try_from(chunk.len()).unwrap_or(0).to_be_bytes());
            p.extend_from_slice(chunk);
            p
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_frame_in_chunks() {
        let data: Vec<u8> = (0..3000u32).map(|i| (i % 251) as u8).collect();
        let ps = packets(0x13, &data);
        assert_eq!(ps.len(), 3);
        assert_eq!(&ps[0][..10], &[0x40, 0x03, 0x0b, 1, 0, 0, 0, 0, 0x05, 0xa0]);
        assert_eq!(&ps[1][4..10], &[0, 0, 0x05, 0xa0, 0x05, 0xa0]);
        assert_eq!(ps[2][0], 0x41);
        assert_eq!(&ps[2][4..10], &[0, 0, 0x0b, 0x40, 0, 120]);
        let joined: Vec<u8> = ps.iter().flat_map(|p| p[HEADER..].to_vec()).collect();
        assert_eq!(joined, data);
        assert!(packets(1, &[]).is_empty());
    }
}
