//! Embeds the Windows application manifest (per-monitor v2 DPI awareness, UTF-8 code page,
//! long paths, `asInvoker`). Works for MSVC and GNU targets without external tools.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    // The build script runs on the host: test the *target* OS, not `cfg!(windows)`.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_some() {
        use embed_manifest::manifest::DpiAwareness;
        use embed_manifest::{embed_manifest, new_manifest};
        let manifest = new_manifest("Lumen.WallpaperEngine")
            // V2 per-monitor awareness, falling back to V1 on Windows 8.1 / early Windows 10.
            .dpi_awareness(DpiAwareness::PerMonitorV2);
        embed_manifest(manifest).expect("unable to embed the Windows manifest");
    }
}
