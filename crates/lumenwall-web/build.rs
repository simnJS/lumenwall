//! With the `cef` feature on Linux, the host executable looks for `libcef.so` next to itself.

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    let cef = std::env::var_os("CARGO_FEATURE_CEF").is_some();
    let linux = std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "linux");
    if cef && linux {
        println!("cargo::rustc-link-arg-bins=-Wl,-rpath,$ORIGIN");
    }
}
