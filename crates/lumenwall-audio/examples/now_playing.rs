//! Prints what the system's media session plays and the power source, as wallpapers and the
//! daemon receive them, then counts the notifications while nothing else happens.
//!
//! `cargo run -p lumenwall-audio --example now_playing -- [seconds] [both|media|power|none]`
//! (default 60 s, both sources; `none` is the baseline of an idle process). Read only: it
//! changes nothing on the system and plays nothing.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use lumenwall_audio::media::{MediaHub, MediaState};
use lumenwall_audio::power::PowerWatch;

fn describe(state: &MediaState) -> String {
    let Some(s) = &state.session else {
        return format!("available {}, no media session", state.available);
    };
    let art = s.artwork.as_ref().map_or("none".to_owned(), |a| {
        format!("{}x{} ({} B PNG)", a.width, a.height, a.png.len())
    });
    format!(
        "available {}, {:?}: '{}' by '{}', album '{}', type '{}', position {:.1}/{:.1} s, artwork {art}",
        state.available,
        s.state,
        s.title,
        s.artist,
        s.album_title,
        s.content_type,
        s.position_at(Instant::now()),
        s.duration
    )
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let seconds: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(60);
    let mode = std::env::args().nth(2).unwrap_or_else(|| "both".into());
    let (with_media, with_power) = match mode.as_str() {
        "media" => (true, false),
        "power" => (false, true),
        "none" => (false, false),
        _ => (true, true),
    };
    let power_events = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&power_events);
    let started = Instant::now();
    let power = with_power.then(|| {
        PowerWatch::start(Arc::new(move |on_battery| {
            counted.fetch_add(1, Ordering::Relaxed);
            println!(
                "[{:6.2} s] power: {}",
                started.elapsed().as_secs_f64(),
                if on_battery { "on battery" } else { "on mains" }
            );
        }))
    });

    let (tx, rx) = mpsc::channel::<()>();
    let wakes = Arc::new(AtomicU64::new(0));
    let w = Arc::clone(&wakes);
    let media_tx = tx.clone();
    let mut media = with_media.then(|| {
        MediaHub::system().subscribe(Arc::new(move || {
            w.fetch_add(1, Ordering::Relaxed);
            let _ = media_tx.send(());
        }))
    });
    let end = Instant::now() + Duration::from_secs(seconds);
    let mut changes = 0u64;
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(()) => {
                while let Some(state) = media.as_mut().and_then(|m| m.poll()) {
                    changes += 1;
                    println!(
                        "[{:6.2} s] media: {}",
                        started.elapsed().as_secs_f64(),
                        describe(&state)
                    );
                }
            }
            Err(_) => break,
        }
    }
    println!(
        "{seconds} s: {} media wake-up(s), {changes} state(s) read, {} power notification(s)",
        wakes.load(Ordering::Relaxed),
        power_events.load(Ordering::Relaxed)
    );
    // Stop the sources and let them log what they did.
    drop(media);
    drop(power);
    std::thread::sleep(Duration::from_millis(200));
}
