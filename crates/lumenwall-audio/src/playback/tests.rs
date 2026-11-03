//! Playback pipeline tests without an audio device: decode → resample → channel map → ring.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::decode::{DeviceFormat, VoiceFeeder, probe_format};
use super::mix::VoiceOut;
use super::{Control, Engine, ObjectId, Player, PlayerConfig, SoundFile};
use crate::sound::{PlaybackMode, SoundObject};

/// 16-bit PCM WAV with a sine (or silence when `freq` is 0).
fn wav(rate: u32, channels: u16, frames: usize, freq: f64) -> SoundFile {
    let data_len = frames * usize::from(channels) * 2;
    let mut b = Vec::with_capacity(44 + data_len);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&channels.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * u32::from(channels) * 2).to_le_bytes());
    b.extend_from_slice(&(channels * 2).to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&(data_len as u32).to_le_bytes());
    for n in 0..frames {
        let s = (0.5
            * (2.0 * std::f64::consts::PI * freq * n as f64 / f64::from(rate)).sin()
            * 32767.0) as i16;
        for _ in 0..channels {
            b.extend_from_slice(&s.to_le_bytes());
        }
    }
    SoundFile {
        name: "test.wav".into(),
        data: Arc::new(b),
    }
}

const STEREO_48K: DeviceFormat = DeviceFormat {
    rate: 48_000,
    channels: 2,
};

fn feeder(
    files: Vec<SoundFile>,
    mode: PlaybackMode,
    times: (f32, f32),
    dev: DeviceFormat,
) -> (VoiceFeeder, VoiceOut) {
    VoiceFeeder::new(files, mode, 1.0, times, true, dev, 42)
}

/// Plays the voice "offline": calls `on_block` with every block the callback would read, until
/// the voice ends or `max_samples` were read.
fn drain(
    f: &mut VoiceFeeder,
    out: &mut VoiceOut,
    max_samples: usize,
    mut on_block: impl FnMut(&[f32]),
) -> usize {
    let mut total = 0;
    while total < max_samples {
        f.fill();
        let n = out.consumer.slots().min(max_samples - total);
        if n == 0 {
            assert!(
                out.shared.finished.load(Ordering::Acquire),
                "ring empty while the voice is not finished"
            );
            break;
        }
        let chunk = out.consumer.read_chunk(n).unwrap();
        let (a, b) = chunk.as_slices();
        on_block(a);
        on_block(b);
        chunk.commit_all();
        total += n;
    }
    total
}

#[test]
fn single_play_resamples_whole_file_and_finishes() {
    let file = wav(44_100, 1, 11_025, 440.0);
    let (mut f, mut out) = feeder(vec![file], PlaybackMode::Single, (0.0, 0.0), STEREO_48K);
    let mut samples = Vec::new();
    let n = drain(&mut f, &mut out, usize::MAX, |b| {
        samples.extend_from_slice(b)
    });
    assert!(out.shared.finished.load(Ordering::Acquire));
    // 0.25 s at 48 kHz stereo, within the filter's tail.
    let frames = n / 2;
    assert!((frames as i64 - 12_000).abs() <= 40, "{frames} frames");
    // Mono is duplicated to both channels, level preserved.
    assert!(samples.as_chunks::<2>().0.iter().all(|fr| fr[0] == fr[1]));
    let peak = samples.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!((peak - 0.5).abs() < 0.01, "peak {peak}");
}

#[test]
fn loop_restarts_without_end() {
    let file = wav(48_000, 2, 4_800, 1000.0);
    let (mut f, mut out) = feeder(vec![file], PlaybackMode::Loop, (0.0, 0.0), STEREO_48K);
    let n = drain(&mut f, &mut out, 48_000 * 2, |_| {});
    assert_eq!(n, 48_000 * 2, "ten loops of 0.1 s fill one second");
    assert!(!out.shared.finished.load(Ordering::Acquire));
}

#[test]
fn random_mode_pauses_between_files() {
    let a = wav(48_000, 1, 2_400, 500.0);
    let b = wav(48_000, 1, 2_400, 700.0);
    let (mut f, mut out) = feeder(vec![a, b], PlaybackMode::Random, (0.1, 0.1), STEREO_48K);
    let mut samples = Vec::new();
    drain(&mut f, &mut out, 48_000 * 2, |blk| {
        samples.extend_from_slice(blk)
    });
    // Longest run of silent frames ≈ the 0.1 s pause (4800 frames).
    let mut run = 0;
    let mut longest = 0;
    for fr in samples.as_chunks::<2>().0 {
        if fr[0] == 0.0 && fr[1] == 0.0 {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    assert!(
        (4_790..=4_900).contains(&longest),
        "longest silence {longest} frames"
    );
    assert!(!out.shared.finished.load(Ordering::Acquire));
}

#[test]
fn undecodable_file_ends_voice() {
    let junk = SoundFile {
        name: "x.mp3".into(),
        data: Arc::new(vec![7u8; 300]),
    };
    assert!(probe_format(&junk).is_err());
    let (mut f, mut out) = feeder(vec![junk], PlaybackMode::Loop, (0.0, 0.0), STEREO_48K);
    assert_eq!(drain(&mut f, &mut out, 1000, |_| {}), 0);
    assert!(out.shared.finished.load(Ordering::Acquire));
}

#[test]
fn surround_and_mono_devices() {
    let file = wav(48_000, 2, 480, 300.0);
    let (mut f, mut out) = feeder(
        vec![file.clone()],
        PlaybackMode::Single,
        (0.0, 0.0),
        DeviceFormat {
            rate: 48_000,
            channels: 6,
        },
    );
    let mut s = Vec::new();
    drain(&mut f, &mut out, usize::MAX, |b| s.extend_from_slice(b));
    assert_eq!(s.len(), 480 * 6);
    assert!(
        s.as_chunks::<6>()
            .0
            .iter()
            .all(|fr| fr[2..].iter().all(|&x| x == 0.0))
    );
    let (mut f, mut out) = feeder(
        vec![file],
        PlaybackMode::Single,
        (0.0, 0.0),
        DeviceFormat {
            rate: 48_000,
            channels: 1,
        },
    );
    assert_eq!(drain(&mut f, &mut out, usize::MAX, |_| {}), 480);
}

#[test]
fn silent_player_loads_nothing() {
    let mut p = Player::new(PlayerConfig {
        volume: 15,
        silent: true,
    });
    assert!(!p.is_enabled());
    let obj = SoundObject::from_json(&serde_json::json!({"sound": ["a.wav"]})).unwrap();
    assert!(p.add_object(&obj, vec![wav(48_000, 1, 10, 0.0)]).is_none());
}

/// "Hidden" (every output paused) suspends playback on its own, independently of the user
/// pause, and is visible through handles.
#[test]
fn hidden_suspends_independently_of_user_pause() {
    let ctl = Control::new(64);
    assert!(!ctl.suspended());
    ctl.hidden.store(true, Ordering::Release);
    assert!(ctl.suspended(), "hidden alone suspends");
    ctl.paused.store(true, Ordering::Release);
    ctl.hidden.store(false, Ordering::Release);
    assert!(ctl.suspended(), "user pause alone suspends");
    ctl.paused.store(false, Ordering::Release);
    assert!(!ctl.suspended());

    let p = Player::new(PlayerConfig {
        volume: 64,
        silent: true,
    });
    p.set_hidden(true);
    assert!(p.handle().ctl.suspended());
    p.handle().set_hidden(false);
    assert!(!p.handle().ctl.suspended());
}

/// While hidden, audible sounds never open the output device (no stream, no device thread, no
/// wake-up) and are not consumed: their rings stay full for the resume. Runs without a device:
/// the control thread's refresh is driven directly.
#[test]
fn hidden_player_keeps_the_device_closed_and_sounds_unconsumed() {
    let ctl = Arc::new(Control::new(64));
    ctl.hidden.store(true, Ordering::Release);
    let (tx, _rx) = std::sync::mpsc::sync_channel(4);
    let mut engine = Engine::new(ctl.clone(), tx);
    let obj =
        SoundObject::from_json(&serde_json::json!({"sound": ["s.wav"], "playbackmode": "loop"}))
            .unwrap();
    engine.add(ObjectId(0), &obj, vec![wav(48_000, 2, 48_000, 440.0)]);
    assert!(engine.audible());
    let queued = |e: &Engine| {
        e.mix
            .lock()
            .unwrap()
            .voices
            .iter()
            .map(|v| v.consumer.slots())
            .sum::<usize>()
    };
    let before = queued(&engine);
    assert!(before > 0, "the voice is pre-decoded");
    for _ in 0..3 {
        engine.refresh();
        assert!(engine.stream.is_none(), "no output stream while hidden");
        assert!(
            engine.host.is_none(),
            "the audio host is never touched while hidden"
        );
    }
    assert_eq!(queued(&engine), before, "suspended, not advanced");
}

/// Decodes the reference wallpaper's sound with the playback stack (no audio output).
/// Needs the unpacked copy described in docs/TESTING.md.
#[test]
#[ignore = "needs testdata/we/3018340632 (docs/TESTING.md)"]
fn reference_wallpaper_mp3_decodes_fully() {
    let root =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/we/3018340632");
    let scene: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root.join("scene.json")).expect("scene.json (docs/TESTING.md)"),
    )
    .unwrap();
    let object = scene["objects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["name"] == "literally_me.mp3")
        .expect("sound object literally_me.mp3");
    let sound = SoundObject::from_json(object).expect("sound object");
    assert_eq!(sound.mode, PlaybackMode::Random);
    assert_eq!(
        (
            sound.volume,
            sound.min_time,
            sound.max_time,
            sound.start_silent
        ),
        (1.0, 1.0, 5.0, false)
    );
    assert_eq!(sound.files.len(), 5);
    assert_eq!(sound.files[0], "sounds/literally_me.mp3");

    let data = std::fs::read(root.join(&sound.files[0])).expect("sounds/literally_me.mp3");
    let file = SoundFile {
        name: sound.files[0].clone(),
        data: Arc::new(data),
    };
    assert_eq!(probe_format(&file).unwrap(), (48_000, 2));

    // Whole file through decode → (pass-through) resampler → ring, as the device would read it.
    let (mut f, mut out) = feeder(
        vec![file.clone()],
        PlaybackMode::Single,
        (0.0, 0.0),
        STEREO_48K,
    );
    let mut peak = 0.0f32;
    let mut finite = true;
    let n = drain(&mut f, &mut out, usize::MAX, |b| {
        for &s in b {
            finite &= s.is_finite();
            peak = peak.max(s.abs());
        }
    });
    let seconds = (n / 2) as f64 / 48_000.0;
    // ffprobe: 130.296 s, 48 kHz, stereo (gapless trimming may differ by a few ms).
    assert!((seconds - 130.296).abs() < 0.05, "decoded {seconds} s");
    assert!(finite && peak > 0.1 && peak <= 1.5, "peak {peak}");

    // Same file to a 44.1 kHz mono device: duration preserved by the resampler.
    let dev = DeviceFormat {
        rate: 44_100,
        channels: 1,
    };
    let (mut f, mut out) = feeder(vec![file], PlaybackMode::Single, (0.0, 0.0), dev);
    let n = drain(&mut f, &mut out, usize::MAX, |_| {});
    let seconds = n as f64 / 44_100.0;
    assert!((seconds - 130.296).abs() < 0.05, "resampled {seconds} s");
}

/// Opens the real default output device and plays digital silence: the control thread opens,
/// closes and reopens the stream on pause/automute without hanging, and shuts down promptly.
#[test]
#[ignore = "opens the default audio output device"]
fn live_device_lifecycle_with_silence() {
    use std::time::{Duration, Instant};
    let mut p = Player::new(PlayerConfig::default());
    assert!(p.is_enabled());
    let obj =
        SoundObject::from_json(&serde_json::json!({"sound": ["s.wav"], "playbackmode": "loop"}))
            .unwrap();
    let id = p
        .add_object(&obj, vec![wav(44_100, 1, 4_410, 0.0)])
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    p.set_paused(true);
    std::thread::sleep(Duration::from_millis(100));
    p.set_paused(false);
    p.set_hidden(true);
    std::thread::sleep(Duration::from_millis(100));
    p.set_hidden(false);
    p.set_other_app_playing(true);
    p.set_other_app_playing(false);
    p.set_object_volume(id, 0.5);
    p.set_object_playing(id, false);
    p.set_object_playing(id, true);
    std::thread::sleep(Duration::from_millis(200));
    p.clear();
    let t = Instant::now();
    drop(p);
    assert!(
        t.elapsed() < Duration::from_secs(1),
        "shutdown took {:?}",
        t.elapsed()
    );
}
