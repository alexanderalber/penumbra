//! Windows only: embeds the app icon and version info into `penumbra.exe`, so the file, a
//! shortcut and a pinned taskbar entry show the icon before the app runs. On other targets this
//! does nothing; a missing resource compiler is a warning, the build still links.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/app-icon/penumbra.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/app-icon/penumbra.ico")
        .set("ProductName", "penumbra")
        .set("FileDescription", "penumbra")
        .set("LegalCopyright", "MIT OR Apache-2.0")
        .set("OriginalFilename", "penumbra.exe")
        .set("InternalName", "penumbra");
    if let Err(e) = res.compile() {
        println!("cargo:warning=penumbra.exe built without icon and version info: {e}");
    }
}
