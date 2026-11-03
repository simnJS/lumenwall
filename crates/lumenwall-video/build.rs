//! FFmpeg (LGPL) must stay a separate shared library that users can replace (deny.toml,
//! "LGPL rule"). `ffmpeg-sys-next` links it dynamically unless its `static`/`build` features
//! are on (banned by cargo-deny); this script also rejects the environment overrides that
//! would make pkg-config hand out static archives.

fn main() {
    for var in ["FFMPEG_STATIC", "PKG_CONFIG_ALL_STATIC"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    if std::env::var_os("CARGO_FEATURE_FFMPEG").is_none() {
        return;
    }
    let set = |v: &str| std::env::var_os(v).is_some_and(|x| !x.is_empty() && x != "0");
    if set("FFMPEG_STATIC") || set("PKG_CONFIG_ALL_STATIC") {
        panic!(
            "lumenwall-video: FFmpeg must be linked dynamically (LGPL, see deny.toml); \
             unset FFMPEG_STATIC / PKG_CONFIG_ALL_STATIC"
        );
    }
}
