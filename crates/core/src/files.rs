//! Files a node writes without overwriting what is there.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};

/// Most numbered names tried beside an existing file.
pub const MAX_NUMBER: u32 = 9999;

/// Creates `wanted`, or the first free numbered name beside it (`take.mp4`, `take_2.mp4`,
/// `take_3.mp4`, ...). Never overwrites a file and never creates a folder.
pub fn create_numbered(wanted: &Path) -> Result<(File, PathBuf), String> {
    let stem = wanted.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = wanted.extension().map(|e| e.to_string_lossy().into_owned());
    for n in 1..=MAX_NUMBER {
        let path = if n == 1 {
            wanted.to_path_buf()
        } else {
            let name = match &ext {
                Some(e) => format!("{stem}_{n}.{e}"),
                None => format!("{stem}_{n}"),
            };
            wanted.with_file_name(name)
        };
        match OpenOptions::new().read(true).write(true).create_new(true).open(&path) {
            Ok(f) => return Ok((f, path)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("cannot create {}: {e}", path.display())),
        }
    }
    Err(format!("{} and {MAX_NUMBER} numbered names beside it exist already", wanted.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_beside_existing_files() {
        let dir = crate::scratch::ScratchDir::new("files").unwrap();
        let wanted = dir.join("take.csv");
        for want in ["take.csv", "take_2.csv", "take_3.csv"] {
            let (_f, path) = create_numbered(&wanted).unwrap();
            assert_eq!(path, dir.join(want));
        }
        let bare = dir.join("bare");
        assert_eq!(create_numbered(&bare).unwrap().1, bare);
        assert_eq!(create_numbered(&bare).unwrap().1, dir.join("bare_2"));
        assert!(create_numbered(&dir.join("no").join("x.csv")).is_err(), "no folder is created");
    }
}
