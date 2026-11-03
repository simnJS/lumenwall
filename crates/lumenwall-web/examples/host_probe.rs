//! Starts the web host for one wallpaper folder and prints what it sends (frames are released
//! at once, nothing is drawn). Checks a CEF build without the rest of Lumenwall.
//!
//! ```text
//! cargo run -p lumenwall-web --example host_probe -- <host executable> <wallpaper folder> [seconds]
//! ```

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lumenwall_web::ipc::FramePacing;
use lumenwall_web::{AudioOutput, FrameTransport, LaunchParams, PropertySet, WebEvent, WebHost};

fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(host), Some(folder)) = (args.next(), args.next()) else {
        eprintln!("usage: host_probe <host executable> <wallpaper folder> [seconds]");
        std::process::exit(2);
    };
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let folder = PathBuf::from(folder);
    let project: serde_json::Value = std::fs::read(folder.join("project.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let file = project["file"].as_str().unwrap_or("index.html").to_owned();
    let properties = PropertySet::from_project_properties(&project["general"]["properties"]);
    let params = LaunchParams {
        wallpaper_id: "-1".into(),
        root: folder,
        file,
        width: 1280,
        height: 720,
        fps: 30,
        pacing: FramePacing::Internal,
        transport: match std::env::var("LUMENWALL_WEB_FRAMES").as_deref() {
            Ok("memory") => FrameTransport::Memory,
            _ => FrameTransport::Gpu,
        },
        properties,
        audio: AudioOutput {
            volume: 0.0,
            muted: true,
        },
        paused: false,
        profile_dir: None,
        devtools_port: std::env::var("LUMENWALL_WEB_DEVTOOLS")
            .ok()
            .and_then(|p| p.parse().ok()),
    };
    let (wake_tx, wake_rx) = std::sync::mpsc::channel::<()>();
    let waker = Arc::new(move || {
        let _ = wake_tx.send(());
    });
    let host = WebHost::spawn(&PathBuf::from(host), &[], params, waker).expect("spawn");
    let start = Instant::now();
    let end = start + Duration::from_secs(seconds);
    // `LUMENWALL_PROBE_PAUSE=1`: paused during the middle third of the run.
    let pause = std::env::var_os("LUMENWALL_PROBE_PAUSE").is_some();
    let (pause_at, resume_at) = (
        start + Duration::from_secs(seconds) / 3,
        start + Duration::from_secs(seconds) * 2 / 3,
    );
    let mut paused = false;
    let mut frames = 0u64;
    while Instant::now() < end {
        let next = if pause && !paused && Instant::now() < pause_at {
            pause_at
        } else if pause && paused {
            resume_at
        } else {
            end
        };
        let _ = wake_rx.recv_timeout(next.saturating_duration_since(Instant::now()));
        if pause {
            let now = Instant::now();
            let want = now >= pause_at && now < resume_at;
            if want != paused {
                paused = want;
                println!(
                    "{} after {frames} frame(s)",
                    if paused { "pause" } else { "resume" }
                );
                host.send(lumenwall_web::HostCommand::SetPaused { paused });
            }
        }
        while let Some(event) = host.try_event() {
            match event {
                WebEvent::Frame(frame) => {
                    frames += 1;
                    if frames <= 3 {
                        println!("{frame:?} visible {:?}", frame.visible);
                    }
                    frame.release();
                }
                WebEvent::Exited => {
                    println!("host exited");
                    return;
                }
                other => println!("{other:?}"),
            }
        }
    }
    println!("{frames} frame(s) in {seconds} s");
}
