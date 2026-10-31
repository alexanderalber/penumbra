//! Static files from one folder: GET and HEAD only, `index.html` for a folder, no listing, no
//! way out of the folder, nothing hidden, at most [`MAX_FILE`] bytes.

use std::path::{Path, PathBuf};

/// Largest file served.
pub const MAX_FILE: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// Not there (or a folder without `index.html`, or no folder at all).
    NotFound,
    /// A path that names something outside the folder, hidden or reserved.
    Forbidden(&'static str),
    /// Over [`MAX_FILE`].
    TooBig(u64),
}

/// Windows device names, refused whatever the extension (`con.html` opens the console).
const DEVICES: &[&str] = &["con", "prn", "aux", "nul", "conin$", "conout$"];

fn segment_ok(seg: &str) -> Result<(), &'static str> {
    if seg == "." || seg == ".." {
        return Err("`..` and `.` are not allowed in a path");
    }
    if seg.starts_with('.') {
        return Err("hidden files are not served");
    }
    if seg.chars().any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err("a path with characters a file name cannot have");
    }
    let stem = seg.split('.').next().unwrap_or(seg).trim_end().to_ascii_lowercase();
    let numbered = (stem.starts_with("com") || stem.starts_with("lpt"))
        && stem.len() == 4
        && stem.chars().last().is_some_and(|c| c.is_ascii_digit());
    if DEVICES.contains(&stem.as_str()) || numbered {
        return Err("a reserved device name");
    }
    Ok(())
}

/// The file `url_path` (decoded, starting with `/`) names in `folder`.
pub fn resolve(folder: &Path, url_path: &str) -> Result<PathBuf, Refused> {
    let mut rel = PathBuf::new();
    for seg in url_path.split('/').filter(|s| !s.is_empty()) {
        segment_ok(seg).map_err(Refused::Forbidden)?;
        rel.push(seg);
    }
    let root = std::fs::canonicalize(folder).map_err(|_| Refused::NotFound)?;
    let mut path = std::fs::canonicalize(root.join(&rel)).map_err(|_| Refused::NotFound)?;
    // A link inside the folder that points out of it.
    if !path.starts_with(&root) {
        return Err(Refused::Forbidden("the path leads out of the folder"));
    }
    if path.is_dir() {
        path = path.join("index.html");
    }
    let meta = std::fs::metadata(&path).map_err(|_| Refused::NotFound)?;
    if !meta.is_file() {
        return Err(Refused::NotFound);
    }
    if meta.len() > MAX_FILE {
        return Err(Refused::TooBig(meta.len()));
    }
    Ok(path)
}

/// The content type for a file by its extension.
pub fn content_type(path: &Path) -> &'static str {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "txt" | "md" => "text/plain; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "woff2" => "font/woff2",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp4" => "video/mp4",
        "webmanifest" => "application/manifest+json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use penumbra_core::scratch::ScratchDir;

    #[test]
    fn files_inside_the_folder_only() {
        let dir = ScratchDir::new("http_files").unwrap();
        let www = dir.path().join("www");
        std::fs::create_dir_all(www.join("sub")).unwrap();
        std::fs::create_dir_all(www.join("empty")).unwrap();
        std::fs::write(www.join("index.html"), "<p>hi").unwrap();
        std::fs::write(www.join("sub").join("app.js"), "1").unwrap();
        std::fs::write(www.join(".secret"), "x").unwrap();
        std::fs::write(dir.path().join("project.ron"), "x").unwrap();
        assert_eq!(resolve(&www, "/").unwrap(), std::fs::canonicalize(www.join("index.html")).unwrap());
        assert!(resolve(&www, "/sub/app.js").is_ok());
        assert_eq!(resolve(&www, "/sub"), Err(Refused::NotFound), "no listing");
        assert_eq!(resolve(&www, "/empty/"), Err(Refused::NotFound));
        assert_eq!(resolve(&www, "/nothing.html"), Err(Refused::NotFound));
        for bad in [
            "/../project.ron",
            "/sub/../../project.ron",
            "/..\\project.ron",
            "/.secret",
            "/C:/Windows/win.ini",
            "/con",
            "/NUL.html",
            "/com1.txt",
            "/a\u{0}b",
        ] {
            assert!(matches!(resolve(&www, bad), Err(Refused::Forbidden(_))), "{bad}");
        }
        assert_eq!(resolve(&dir.path().join("missing"), "/"), Err(Refused::NotFound));
        assert_eq!(content_type(Path::new("a/b.JS")), "text/javascript; charset=utf-8");
        assert_eq!(content_type(Path::new("x")), "application/octet-stream");
    }
}
