use super::*;

#[test]
fn splits_annex_b_with_both_start_code_forms() {
    let s = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 9, 9, 0];
    let n = annex_b_nals(&s);
    assert_eq!(n, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 9, 9][..]]);
    assert_eq!(annex_b_to_length_prefixed(&s).unwrap(), [0, 0, 0, 3, 0x65, 9, 9]);
    assert_eq!(split_parameter_sets(&s).unwrap(), (vec![0x67, 1, 2], vec![0x68, 3]));
}

#[test]
fn hostile_streams_are_errors() {
    for s in [&[][..], &[0, 0, 1], &[1, 2, 3], &[0, 0, 1, 0x09, 0xf0], &[0, 0, 0, 1, 0x67, 1]] {
        assert!(annex_b_to_length_prefixed(s).is_err(), "{s:?}");
    }
    assert!(split_parameter_sets(&[0, 0, 1, 0x65, 1]).is_err());
    assert!(annex_b_nals(&[0, 0, 1, 0, 0, 1]).is_empty());
    assert!(avcc(&[0x67, 100], &[0x68]).is_err());
    assert!(avcc(&[0x67, 100, 0, 40], &[]).is_err());
}

#[test]
fn the_avcc_names_profile_and_level_and_holds_both_parameter_sets() {
    let (sps, pps) = ([0x67, 100, 0, 51, 0xac], [0x68, 0xee, 0x3c]);
    let a = avcc(&sps, &pps).unwrap();
    assert_eq!(&a[..6], &[1, 100, 0, 51, 0xff, 0xe1]);
    assert_eq!(&a[6..8], &[0, 5]);
    assert_eq!(&a[8..13], &sps);
    assert_eq!(&a[13..16], &[1, 0, 3]);
    assert_eq!(&a[16..19], &pps);
    // High profile: 4:2:0, 8 bits.
    assert_eq!(&a[19..], &[0xfd, 0xf8, 0xf8, 0]);
    let baseline = avcc(&[0x67, 66, 0xc0, 30], &pps).unwrap();
    assert_eq!(baseline.len(), 6 + 2 + 4 + 1 + 2 + 3);
}

#[test]
fn nv12_takes_strided_planes_and_interleaves_chroma_after_the_luma_rows() {
    // 4 x 2 with luma rows 8 apart and chroma rows 4 apart, as a recording pads them.
    let (w, h, pitch) = (4usize, 2usize, 6usize);
    let mut planes = vec![0u8; 16 + 4 + 4];
    planes[..4].copy_from_slice(&[0, 1, 2, 3]);
    planes[8..12].copy_from_slice(&[4, 5, 6, 7]);
    planes[16..18].copy_from_slice(&[100, 101]);
    planes[20..22].copy_from_slice(&[200, 201]);
    let src = Source::new(&planes, [0, 16, 20], (8, 4), w, h).unwrap();
    let mut buf = vec![0xaau8; pitch * h * 3 / 2];
    src.fill_nv12(&mut buf, pitch);
    assert_eq!(&buf[..4], &[0, 1, 2, 3]);
    assert_eq!(&buf[pitch..pitch + 4], &[4, 5, 6, 7]);
    assert_eq!(&buf[pitch * h..pitch * h + 4], &[100, 200, 101, 201]);
}

#[test]
fn short_planes_are_refused_and_a_narrow_buffer_is_never_overrun() {
    let planes = vec![7u8; 8 * 4 + 2 * 4 * 2];
    assert!(Source::new(&planes, [0, 32, 40], (8, 4), 8, 4).is_ok());
    assert!(Source::new(&planes, [0, 32, 41], (8, 4), 8, 4).is_err());
    assert!(Source::new(&planes, [0, 32, 40], (4, 4), 8, 4).is_err());
    assert!(Source::new(&planes, [usize::MAX, 32, 40], (8, 4), 8, 4).is_err());
    let src = Source::new(&planes, [0, 32, 40], (8, 4), 8, 4).unwrap();
    let mut buf = vec![0u8; 4 * 4 * 3 / 2];
    src.fill_nv12(&mut buf, 4);
    src.fill_nv12(&mut [], 0);
    src.fill_nv12(&mut buf[..3], 8);
}

/// The graphics card's encoder, when this machine has one; `None` (and a note) otherwise.
fn card(w: u32, h: u32) -> Option<Nvenc> {
    if let Err(e) = available() {
        eprintln!("no NVENC here, skipped: {e}");
        return None;
    }
    let cfg =
        Config { width: w, height: h, fps: (30, 1), keyint: 10, quality: Quality { preset: 5, cq: 20 } };
    Some(Nvenc::open(&cfg).unwrap())
}

#[test]
fn the_card_encodes_every_picture_in_order_with_keyframes_at_each_gop() {
    let (w, h) = (256u32, 144u32);
    let Some(mut enc) = card(w, h) else { return };
    let (sps, pps) = enc.parameter_sets();
    assert_eq!(sps.first().map(|b| b & 0x1f), Some(7));
    assert_eq!(pps.first().map(|b| b & 0x1f), Some(8));
    assert_eq!(sps.get(1), Some(&100), "High profile");
    filmcraft_isobmff::AvcConfig::parse(&enc.avcc().unwrap()).unwrap();
    let luma = (w * h) as usize;
    let mut planes = vec![128u8; luma * 3 / 2];
    let mut out = Vec::new();
    // Indices with a gap, as a recording that dropped frames hands them.
    let indices: Vec<i64> = (0..20).chain(25..45).collect();
    for &i in &indices {
        planes[..luma].fill(if i < 25 { 40 } else { 200 });
        let offsets = [0, luma, luma + luma / 4];
        out.extend(enc.encode(&planes, offsets, (w as usize, w as usize / 2), i).unwrap());
    }
    out.extend(enc.flush().unwrap());
    assert_eq!(out.iter().map(|p| p.pts).collect::<Vec<_>>(), indices);
    let keys: Vec<usize> = out.iter().enumerate().filter(|(_, p)| p.key).map(|(k, _)| k).collect();
    assert_eq!(keys, vec![0, 10, 20, 30]);
    for p in &out {
        // Length-prefixed slices only.
        let len = u32::from_be_bytes(p.data[..4].try_into().unwrap()) as usize;
        assert!(len + 4 <= p.data.len());
        assert!(matches!(p.data[4] & 0x1f, 1 | 5 | 6), "nal type {}", p.data[4] & 0x1f);
    }
    // Flushing again, or with nothing in flight, is harmless.
    assert!(enc.flush().unwrap().is_empty());
}

#[test]
fn the_card_refuses_short_planes_and_odd_sizes() {
    let Some(mut enc) = card(256, 144) else { return };
    assert!(enc.encode(&[0u8; 100], [0, 50, 75], (256, 128), 0).is_err());
    let cfg =
        Config { width: 255, height: 144, fps: (30, 1), keyint: 10, quality: Quality { preset: 4, cq: 23 } };
    assert!(Nvenc::open(&cfg).is_err());
    let cfg = Config { fps: (0, 1), width: 256, ..cfg };
    assert!(Nvenc::open(&cfg).is_err());
}

#[test]
fn every_preset_opens() {
    if available().is_err() {
        return;
    }
    for preset in [4, 5, 6] {
        let cfg = Config {
            width: 640,
            height: 360,
            fps: (60, 1),
            keyint: 120,
            quality: Quality { preset, cq: 20 },
        };
        if let Err(e) = Nvenc::open(&cfg) {
            panic!("P{preset}: {e}");
        }
    }
}
