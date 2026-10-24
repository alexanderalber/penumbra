//! A scratch directory that removes itself, for tests that need real files.

use std::path::{Path, PathBuf};

/// `<temp>/penumbra-<name>-<pid>`, created empty and removed again on drop, so a test leaves
/// nothing behind even when it fails or panics. Declare it before anything that keeps files in
/// it open (a session, an engine, a writer): locals drop in reverse order, so the directory
/// then goes last.
#[derive(Debug)]
pub struct ScratchDir(PathBuf);

impl ScratchDir {
    /// Creates the directory, clearing what an earlier run with the same pid left there.
    pub fn new(name: &str) -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!("penumbra-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }

    /// The path, without creating anything: for a test that wants to create it itself.
    pub fn unborn(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("penumbra-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        Self(dir)
    }

    /// The directory.
    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

impl std::ops::Deref for ScratchDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::ScratchDir;

    #[test]
    fn removes_itself_with_its_contents() {
        let dir = ScratchDir::new("scratch-self").unwrap();
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/b/c.txt"), "x").unwrap();
        let path = dir.to_path_buf();
        assert!(path.is_dir());
        drop(dir);
        assert!(!path.exists());
    }
}
