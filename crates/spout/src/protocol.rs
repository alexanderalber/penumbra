//! What Spout keeps in shared memory, as bytes: the list of sender names and each sender's
//! texture description. Pure data, no system calls, so it is tested on every platform.
//!
//! The layout is Spout 2's (`SpoutSenderNames.h` of the Spout2 SDK):
//!
//! - `SpoutSenderNames`: a map of `max` slots of [`NAME_LEN`] bytes, one null-terminated name
//!   each, sorted by bytes (a C++ `std::set<std::string>`), the first empty slot ends the list.
//!   `max` is the registry's `MaxSenders` (64 unless set) for whoever created the map first.
//! - `ActiveSenderName`: one [`NAME_LEN`] slot, the sender a receiver without a name takes.
//! - `<sender name>`: [`INFO_LEN`] bytes, [`Info`].
//!
//! Everything read from these maps comes from other programs and is treated as hostile:
//! names without a terminator end at the slot, sizes and formats are checked before use.

use std::collections::BTreeSet;

/// Bytes of one name slot, terminator included.
pub const NAME_LEN: usize = 256;
/// Bytes of a sender's texture description.
pub const INFO_LEN: usize = 280;
/// Senders the name list holds unless the registry says otherwise.
pub const DEFAULT_MAX_SENDERS: usize = 64;

/// A sender's texture description, as stored in the map named after the sender.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Info {
    /// The DX11 shared texture handle (a legacy shared handle, which fits 32 bits).
    pub handle: u32,
    pub width: u32,
    pub height: u32,
    /// The texture's `DXGI_FORMAT`; 0 or a DX9 format (21, 22) for a DX9 sender.
    pub format: u32,
    /// Unused (0).
    pub usage: u32,
    /// The sending program's executable path, null-terminated (Spout stores bytes).
    pub description: [u8; 256],
    /// Top bit: the sender shares through the CPU; next bit: GL/DX compatible hardware.
    pub partner: u32,
}

impl Default for Info {
    fn default() -> Self {
        Self { handle: 0, width: 0, height: 0, format: 0, usage: 0, description: [0; 256], partner: 0 }
    }
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    bytes.get(at..at + 4)?.try_into().ok().map(u32::from_le_bytes)
}

impl Info {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let mut description = [0u8; 256];
        description.copy_from_slice(bytes.get(20..276)?);
        Some(Self {
            handle: u32_at(bytes, 0)?,
            width: u32_at(bytes, 4)?,
            height: u32_at(bytes, 8)?,
            format: u32_at(bytes, 12)?,
            usage: u32_at(bytes, 16)?,
            description,
            partner: u32_at(bytes, 276)?,
        })
    }

    pub fn to_bytes(&self) -> [u8; INFO_LEN] {
        let mut out = [0u8; INFO_LEN];
        let words =
            [(0, self.handle), (4, self.width), (8, self.height), (12, self.format), (16, self.usage)];
        for (at, v) in words {
            if let Some(slot) = out.get_mut(at..at + 4) {
                slot.copy_from_slice(&v.to_le_bytes());
            }
        }
        if let Some(slot) = out.get_mut(20..276) {
            slot.copy_from_slice(&self.description);
        }
        if let Some(slot) = out.get_mut(276..280) {
            slot.copy_from_slice(&self.partner.to_le_bytes());
        }
        out
    }

    /// The sending program, from the description: its file name without the folder.
    pub fn program(&self) -> String {
        let path = text(&self.description);
        path.rsplit(['\\', '/']).next().unwrap_or_default().to_string()
    }

    /// `description` set to `path`, cut to fit with its terminator.
    pub fn with_description(mut self, path: &str) -> Self {
        self.description = [0; 256];
        let bytes = path.as_bytes();
        let n = bytes.len().min(255);
        if let (Some(dst), Some(src)) = (self.description.get_mut(..n), bytes.get(..n)) {
            dst.copy_from_slice(src);
        }
        self
    }
}

/// The text before the first null (all of it without one), lossily as UTF-8.
pub fn text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(bytes.get(..end).unwrap_or_default()).into_owned()
}

/// The names in a `SpoutSenderNames` buffer of `max` slots (fewer if the buffer is shorter).
pub fn read_names(buffer: &[u8], max: usize) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for slot in buffer.as_chunks::<NAME_LEN>().0.iter().take(max) {
        let name = text(slot);
        if name.is_empty() {
            break;
        }
        names.insert(name);
    }
    names
}

/// Writes `names` into a `SpoutSenderNames` buffer of `max` slots, in their order (bytes), and
/// ends the list with an empty slot where one is left. Names that do not fit a slot are left
/// out (a valid name always fits, see [`check_name`]).
pub fn write_names(names: &BTreeSet<String>, buffer: &mut [u8], max: usize) {
    let mut slots = buffer.as_chunks_mut::<NAME_LEN>().0.iter_mut().take(max);
    for name in names.iter().filter(|n| n.len() < NAME_LEN) {
        let Some(slot) = slots.next() else { return };
        slot.fill(0);
        if let Some(dst) = slot.get_mut(..name.len()) {
            dst.copy_from_slice(name.as_bytes());
        }
    }
    if let Some(slot) = slots.next() {
        slot.fill(0);
    }
}

/// A sender name others can open: 1 to 200 printable ASCII characters (the name also names the
/// sender's mutexes and semaphore, which add up to 18 bytes). Spout names are ANSI strings, so
/// anything beyond ASCII would mean something else in another program's code page.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("the sender name is empty".into());
    }
    if name.len() > 200 {
        return Err(format!("the sender name is {} characters long, at most 200 work", name.len()));
    }
    if let Some(c) = name.chars().find(|c| !(c.is_ascii_graphic() || *c == ' ')) {
        return Err(format!(
            "the sender name contains {c:?}: only ASCII letters, digits, spaces and punctuation work"
        ));
    }
    if name.contains('\\') {
        return Err("the sender name contains a backslash, which Windows reserves in object names".into());
    }
    Ok(())
}

/// `base`, or `base_1`, `base_2`, ... when taken: how Spout names a second sender of the same
/// name.
pub fn free_name(base: &str, taken: &BTreeSet<String>) -> String {
    if !taken.contains(base) {
        return base.to_string();
    }
    (1..).map(|i| format!("{base}_{i}")).find(|n| !taken.contains(n)).unwrap_or_else(|| base.to_string())
}

/// `DXGI_FORMAT` values penumbra sends and receives.
pub mod dxgi {
    pub const R32G32B32A32_FLOAT: u32 = 2;
    pub const R16G16B16A16_FLOAT: u32 = 10;
    pub const R10G10B10A2_UNORM: u32 = 24;
    pub const R8G8B8A8_UNORM: u32 = 28;
    pub const R8G8B8A8_UNORM_SRGB: u32 = 29;
    pub const B8G8R8A8_UNORM: u32 = 87;
    pub const B8G8R8X8_UNORM: u32 = 88;
    pub const B8G8R8A8_UNORM_SRGB: u32 = 91;
    /// What a DX9 sender stores (`D3DFMT_A8R8G8B8`, `D3DFMT_X8R8G8B8`); its texture is BGRA.
    pub const DX9_ARGB: u32 = 21;
    pub const DX9_XRGB: u32 = 22;
}

/// How a received texture of `DXGI_FORMAT` `format` is copied: the wgpu format to copy into (the
/// same bytes; an sRGB format's bytes go into its UNORM twin, the shader decodes them) and
/// whether its alpha means nothing. `None` for formats penumbra does not take.
pub fn receive_format(format: u32) -> Option<(wgpu::TextureFormat, bool)> {
    use wgpu::TextureFormat as F;
    Some(match format {
        dxgi::B8G8R8A8_UNORM | dxgi::B8G8R8A8_UNORM_SRGB | 0 | dxgi::DX9_ARGB => (F::Bgra8Unorm, false),
        dxgi::DX9_XRGB => (F::Bgra8Unorm, true),
        dxgi::R8G8B8A8_UNORM | dxgi::R8G8B8A8_UNORM_SRGB => (F::Rgba8Unorm, false),
        dxgi::R10G10B10A2_UNORM => (F::Rgb10a2Unorm, false),
        dxgi::R16G16B16A16_FLOAT => (F::Rgba16Float, false),
        dxgi::R32G32B32A32_FLOAT => (F::Rgba32Float, false),
        _ => return None,
    })
}

/// A readable name for a `DXGI_FORMAT`.
pub fn format_name(format: u32) -> String {
    match format {
        0 | dxgi::DX9_ARGB | dxgi::DX9_XRGB => "BGRA 8 bit (DX9)".into(),
        dxgi::B8G8R8A8_UNORM => "BGRA 8 bit".into(),
        dxgi::B8G8R8A8_UNORM_SRGB => "BGRA 8 bit sRGB".into(),
        dxgi::B8G8R8X8_UNORM => "BGRX 8 bit".into(),
        dxgi::R8G8B8A8_UNORM => "RGBA 8 bit".into(),
        dxgi::R8G8B8A8_UNORM_SRGB => "RGBA 8 bit sRGB".into(),
        dxgi::R10G10B10A2_UNORM => "RGB 10 bit, alpha 2 bit".into(),
        dxgi::R16G16B16A16_FLOAT => "RGBA 16 bit float".into(),
        dxgi::R32G32B32A32_FLOAT => "RGBA 32 bit float".into(),
        other => format!("DXGI format {other}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_round_trips_in_spouts_layout() {
        let info = Info {
            handle: 0x4000_1234,
            width: 1920,
            height: 1080,
            format: 87,
            partner: 0x4000_0000,
            ..Info::default()
        }
        .with_description("C:\\Program Files\\penumbra\\penumbra.exe");
        let bytes = info.to_bytes();
        assert_eq!(bytes.len(), 280);
        assert_eq!(&bytes[0..4], &0x4000_1234u32.to_le_bytes());
        assert_eq!(&bytes[4..8], &1920u32.to_le_bytes());
        assert_eq!(&bytes[12..16], &87u32.to_le_bytes());
        assert_eq!(&bytes[20..23], b"C:\\");
        assert_eq!(&bytes[276..280], &0x4000_0000u32.to_le_bytes());
        assert_eq!(Info::from_bytes(&bytes), Some(info.clone()));
        assert_eq!(info.program(), "penumbra.exe");
        assert_eq!(Info::from_bytes(&bytes[..279]), None, "too short");
    }

    #[test]
    fn names_read_until_the_first_empty_slot() {
        let mut buf = vec![0u8; 4 * NAME_LEN];
        buf[..3].copy_from_slice(b"abc");
        buf[NAME_LEN..NAME_LEN + 2].copy_from_slice(b"TD");
        buf[3 * NAME_LEN..3 * NAME_LEN + 4].copy_from_slice(b"lost");
        let names = read_names(&buf, 4);
        assert_eq!(names.into_iter().collect::<Vec<_>>(), ["TD", "abc"]);
        // A slot without a terminator ends at the slot; a short buffer is no error.
        let full = vec![b'x'; NAME_LEN + 10];
        let names = read_names(&full, 64);
        assert_eq!(names.len(), 1);
        assert_eq!(names.first().map(String::len), Some(NAME_LEN));
        assert!(read_names(&[], 64).is_empty());
    }

    #[test]
    fn names_written_sorted_and_terminated() {
        let mut buf = vec![b'?'; 3 * NAME_LEN];
        let names: BTreeSet<String> = ["b".to_string(), "a".to_string()].into();
        write_names(&names, &mut buf, 3);
        assert_eq!(&buf[..2], b"a\0");
        assert_eq!(&buf[NAME_LEN..NAME_LEN + 2], b"b\0");
        assert_eq!(buf[2 * NAME_LEN], 0, "the list ends");
        assert_eq!(read_names(&buf, 3), names);
        // More names than slots: the first `max`, no terminator slot needed.
        let many: BTreeSet<String> = (0..5).map(|i| format!("s{i}")).collect();
        write_names(&many, &mut buf, 3);
        assert_eq!(read_names(&buf, 3).len(), 3);
    }

    #[test]
    fn a_taken_name_gets_a_number() {
        let taken: BTreeSet<String> = ["out".to_string(), "out_1".to_string()].into();
        assert_eq!(free_name("out", &taken), "out_2");
        assert_eq!(free_name("other", &taken), "other");
    }

    #[test]
    fn names_are_ascii() {
        assert!(check_name("penumbra out1").is_ok());
        assert!(check_name("").is_err());
        assert!(check_name("Bühne").is_err());
        assert!(check_name("a\\b").is_err());
        assert!(check_name(&"x".repeat(201)).is_err());
    }

    #[test]
    fn formats() {
        assert_eq!(receive_format(87), Some((wgpu::TextureFormat::Bgra8Unorm, false)));
        assert_eq!(receive_format(91), Some((wgpu::TextureFormat::Bgra8Unorm, false)));
        assert_eq!(receive_format(22), Some((wgpu::TextureFormat::Bgra8Unorm, true)));
        assert_eq!(receive_format(10), Some((wgpu::TextureFormat::Rgba16Float, false)));
        assert_eq!(receive_format(88), None, "no wgpu twin for BGRX");
        assert_eq!(format_name(10), "RGBA 16 bit float");
    }
}
