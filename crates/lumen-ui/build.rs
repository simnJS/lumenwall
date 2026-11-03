//! Windows: embeds the app icon (`assets/brand/icon/lumenwall.ico`, resource 1,
//! used by Explorer, the taskbar and `platform::set_window_icon`) and a version
//! resource, whose description is the name Task Manager and the notification-area
//! settings show. Compiled by rc.exe (MSVC) or MinGW's windres.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let ico = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("set by Cargo"))
        .join("../../assets/brand/icon/lumenwall.ico");
    println!("cargo:rerun-if-changed={}", ico.display());
    // The build script runs on the host: test the *target* OS, not `cfg!(windows)`.
    if std::env::var_os("CARGO_CFG_WINDOWS").is_none() {
        return;
    }
    assert!(ico.is_file(), "missing {}", ico.display());
    let version = std::env::var("CARGO_PKG_VERSION").expect("set by Cargo");
    let numbers = ["MAJOR", "MINOR", "PATCH"]
        .map(|p| std::env::var(format!("CARGO_PKG_VERSION_{p}")).unwrap_or_else(|_| "0".into()))
        .join(",");
    let rc = format!(
        r#"1 ICON "{ico}"

1 VERSIONINFO
FILEVERSION {numbers},0
PRODUCTVERSION {numbers},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904B0"
    BEGIN
      VALUE "FileDescription", "Lumenwall"
      VALUE "ProductName", "Lumenwall"
      VALUE "FileVersion", "{version}"
      VALUE "ProductVersion", "{version}"
      VALUE "InternalName", "lumen-ui"
      VALUE "OriginalFilename", "lumen-ui.exe"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        ico = ico.display().to_string().replace('\\', "\\\\"),
    );
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("set by Cargo")).join("lumen-ui.rc");
    std::fs::write(&out, rc).expect("write the resource script");
    match embed_resource::compile(&out, embed_resource::NONE) {
        embed_resource::CompilationResult::NotAttempted(why) if !cfg!(windows) => {
            // Type-checking for Windows from another OS without a resource
            // compiler: fine for `cargo check`, not for a shipped build.
            println!("cargo:warning=lumen-ui.exe built without its icon: {why}");
        }
        r => r
            .manifest_required()
            .unwrap_or_else(|e| panic!("embedding the Windows icon: {e}")),
    }
}
