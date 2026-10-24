//! Puts the icon into the Windows executable (as money-manager-ng does).
//!
//! Windows takes a program's icon from the resources linked into its .exe,
//! and GPUI asks for resource 1 when it makes a window. Nothing happens for
//! any other target. Regenerate `windows/aiddiction.ico` when the SVG changes:
//!
//! ```sh
//! magick -background none -density 384 \
//!   data/icons/hicolor/scalable/apps/app.akergez.Aiddiction.svg \
//!   -define icon:auto-resize=256,64,48,32,24,16 \
//!   crates/aiddiction/windows/aiddiction.ico
//! ```

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let resources = std::path::Path::new("windows");
    let rc = resources.join("aiddiction.rc");
    println!("cargo:rerun-if-changed={}", rc.display());
    println!("cargo:rerun-if-changed={}", resources.join("aiddiction.ico").display());
    embed_resource::compile(rc, embed_resource::ParamsIncludeDirs([resources]))
        .manifest_optional()
        .unwrap();
}
