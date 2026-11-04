//! Windows: FFmpeg's DLLs are loaded at the first video (`ffmpeg::sys`). When they cannot be
//! found, opening a video fails with a message naming them; nothing else is affected (this
//! test executable has no import entry on them, or it would not start).
#![cfg(all(windows, feature = "ffmpeg"))]

use lumenwall_video::VideoError;

#[test]
fn missing_dlls_are_named() {
    // Only the Windows folders on PATH: the FFmpeg folder of a development or CI setup is
    // out of reach, and the DLLs are not next to this test executable.
    let windows = std::path::PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
    let path = std::env::join_paths([windows.join("System32"), windows]).expect("PATH");
    // SAFETY: the only test of this executable: no other thread reads the environment.
    unsafe { std::env::set_var("PATH", path) };
    match lumenwall_video::load_decoder() {
        Err(VideoError::Libraries(message)) => {
            for dll in ["avutil-", "swscale-", "avcodec-", "avformat-", "PATH"] {
                assert!(message.contains(dll), "{message}");
            }
            eprintln!("{}", VideoError::Libraries(message));
        }
        other => panic!("expected a missing-DLL error, got {other:?}"),
    }
    // Later calls give the same outcome without searching again.
    assert!(matches!(
        lumenwall_video::load_decoder(),
        Err(VideoError::Libraries(_))
    ));
}
