//! Scene sounds (playback) and the audio spectrum (capture) feeding `g_AudioSpectrum*`
//! (spec 07 §11–§13).

use std::sync::Arc;

use lumenwall_assets::Vfs;
use lumenwall_audio::playback::{Player, PlayerConfig, SoundFile};
use lumenwall_audio::sound::SoundObject;
use lumenwall_audio::{AudioCapture, AutomuteDetector, AutomuteSettings, SpectrumAnalyzer};
use serde_json::Value as Json;

use crate::assets::VfsBytes;
use crate::plan::AudioPlan;

/// Sound playback and spectrum analysis of the running wallpapers.
pub struct Audio {
    player: Player,
    analyzer: SpectrumAnalyzer,
    capture: Option<Box<dyn AudioCapture>>,
    automute: AutomuteDetector,
    sounds: usize,
}

static ZERO16: [f32; 16] = [0.0; 16];
static ZERO32: [f32; 32] = [0.0; 32];
static ZERO64: [f32; 64] = [0.0; 64];

impl Audio {
    /// `spectrum`: some wallpaper declares `supportsaudioprocessing` and processing is enabled.
    pub fn new(plan: &AudioPlan, fps: u32, spectrum: bool) -> Self {
        let player = Player::new(PlayerConfig {
            volume: plan.volume.min(128),
            silent: plan.silent,
        });
        // Silent until `resume` (an output shows the wallpaper).
        player.set_hidden(true);
        let handle = player.handle();
        let automute = AutomuteDetector::new(
            AutomuteSettings {
                enabled: plan.automute && !plan.silent,
            },
            Some(Arc::new(move |playing| {
                handle.set_other_app_playing(playing)
            })),
        );
        let (analyzer, capture) = if spectrum && plan.processing {
            let analyzer = SpectrumAnalyzer::new(fps as f32);
            let capture = analyzer
                .feed()
                .map(lumenwall_audio::capture::default_capture);
            if let Some(c) = &capture {
                tracing::info!("audio spectrum: capture through {}", c.backend_name());
            }
            (analyzer, capture)
        } else {
            (SpectrumAnalyzer::disabled(), None)
        };
        Self {
            player,
            analyzer,
            capture,
            automute,
            sounds: 0,
        }
    }

    /// Adds the sound objects of a scene (`objects` = raw `scene.json` objects).
    pub fn add_scene_sounds<'a>(
        &mut self,
        vfs: &Arc<Vfs>,
        objects: impl Iterator<Item = &'a Json>,
    ) {
        if !self.player.is_enabled() {
            return;
        }
        for o in objects {
            let Some(sound) = SoundObject::from_json(o) else {
                continue;
            };
            let files: Vec<SoundFile> = sound
                .files
                .iter()
                .filter_map(|name| match VfsBytes::open(vfs, name) {
                    Some(bytes) => Some(SoundFile {
                        name: name.clone(),
                        data: Arc::new(bytes),
                    }),
                    None => {
                        tracing::warn!("sound '{name}' not found");
                        None
                    }
                })
                .collect();
            if files.is_empty() {
                continue;
            }
            self.sounds += files.len();
            self.player.add_object(&sound, files);
        }
        if self.sounds > 0 {
            tracing::info!("{} scene sound file(s) playing", self.sounds);
        }
    }

    /// Plays the sound track of a video wallpaper in a loop (spec 07 §14.1): `data` is the
    /// whole file (memory-mapped), `name` gives the container hint. `None` with `--silent`.
    pub fn add_video_sound(
        &mut self,
        name: &str,
        data: lumenwall_audio::playback::SoundData,
        volume: u8,
    ) -> Option<lumenwall_audio::playback::ObjectId> {
        if !self.player.is_enabled() {
            return None;
        }
        // parity: the reference's player has a cubic volume curve, gain ≈ (v/128)³ (QO-15);
        // the mixer applies v/128 once, the object carries the remaining (v/128)².
        let v = f32::from(volume.min(128)) / 128.0;
        let object = SoundObject {
            files: vec![name.to_owned()],
            mode: lumenwall_audio::sound::PlaybackMode::Loop,
            volume: v * v,
            min_time: 0.0,
            max_time: 0.0,
            start_silent: false,
        };
        self.sounds += 1;
        self.player.add_object(
            &object,
            vec![SoundFile {
                name: name.to_owned(),
                data,
            }],
        )
    }

    /// Pauses or resumes one sound object (video wallpaper paused with its outputs).
    pub fn set_object_playing(&self, id: lumenwall_audio::playback::ObjectId, playing: bool) {
        self.player.set_object_playing(id, playing);
    }

    /// Changes one object's gain (video sound track after a volume change).
    pub fn set_object_volume(&self, id: lumenwall_audio::playback::ObjectId, volume: f32) {
        self.player.set_object_volume(id, volume);
    }

    /// Global volume, 0..=128.
    pub fn set_volume(&self, volume: u8) {
        self.player.set_volume(i32::from(volume.min(128)));
    }

    /// Paused by the user: the output stream closes until resumed.
    pub fn set_paused(&self, paused: bool) {
        self.player.set_paused(paused);
    }

    /// Starts the capture and the scene sounds (first frame, resume).
    pub fn resume(&mut self) {
        if let Some(c) = &mut self.capture {
            c.start();
        }
        self.player.set_hidden(false);
    }

    /// Stops the capture and suspends the scene sounds while every output is paused (no
    /// wake-ups, spec 07 §13.1; sounds: see the lumenwall-audio README, Deviations).
    pub fn pause(&mut self) {
        if let Some(c) = &mut self.capture {
            c.stop();
        }
        self.player.set_hidden(true);
        self.analyzer.reset();
    }

    /// Once per rendered frame: smoothing step and automute decision.
    pub fn update(&mut self, dt: f32, fullscreen: bool) {
        self.automute.update(fullscreen);
        if self.analyzer.is_enabled() {
            self.analyzer.update(dt);
        }
    }

    /// Whether the spectrum changes over time (the scene must keep rendering).
    pub fn spectrum_enabled(&self) -> bool {
        self.analyzer.is_enabled()
    }

    /// Spectrum arrays (left = right, spec 07 §13.6).
    pub fn bands(&self) -> (&[f32; 16], &[f32; 32], &[f32; 64]) {
        if self.analyzer.is_enabled() {
            (
                self.analyzer.left16(),
                self.analyzer.left32(),
                self.analyzer.left64(),
            )
        } else {
            (&ZERO16, &ZERO32, &ZERO64)
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        if let Some(c) = &mut self.capture {
            c.stop();
        }
        self.player.clear();
    }
}
