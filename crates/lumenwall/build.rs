//! Embeds the Windows application manifest (per-monitor v2 DPI awareness, UTF-8 code page,
//! long paths, `asInvoker`). Works for MSVC and GNU targets without external tools.
//! Also embeds the app icon (`assets/brand/icon/lumenwall.ico`) with the platform's resource
//! compiler (rc.exe, or MinGW's windres when cross-compiling); the linker merges both resources.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The build script runs on the host: test the *target* OS, not `cfg!(windows)`.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::manifest::DpiAwareness;
        use embed_manifest::{embed_manifest, new_manifest};
        let manifest = new_manifest("Lumenwall.WallpaperEngine")
            // V2 per-monitor awareness, falling back to V1 on Windows 8.1 / early Windows 10.
            .dpi_awareness(DpiAwareness::PerMonitorV2);
        embed_manifest(manifest).expect("unable to embed the Windows manifest");
        embed_icon();
    }
}

fn embed_icon() {
    let dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by Cargo"));
    let ico = dir.join("../../assets/brand/icon/lumenwall.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    let rc =
        std::path::PathBuf::from(std::env::var("OUT_DIR").expect("set by Cargo")).join("icon.rc");
    let path = ico.display().to_string().replace('\\', "\\\\");
    std::fs::write(&rc, format!("1 ICON \"{path}\"\n")).expect("write the resource script");
    match embed_resource::compile(&rc, embed_resource::NONE) {
        // Type-checking for Windows from another OS without a resource compiler.
        embed_resource::CompilationResult::NotAttempted(why) if !cfg!(windows) => {
            println!("cargo:warning=lumenwall.exe built without its icon: {why}");
        }
        r => r
            .manifest_required()
            .unwrap_or_else(|e| panic!("embedding the Windows icon: {e}")),
    }
}
