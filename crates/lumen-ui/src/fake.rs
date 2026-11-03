//! `--fake-daemon`: an in-process stand-in for the daemon, served over the real IPC
//! transport on a private endpoint. For developing the UI before the engine speaks
//! IPC, for screenshots, and as an end-to-end check of `lumen-ipc`.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use lumen_ipc::*;

#[derive(Default)]
pub struct FakeDaemon {
    state: Mutex<State>,
    settings: Mutex<Settings>,
    props: Mutex<std::collections::HashMap<String, PropertyMap>>,
    publisher: OnceLock<Publisher>,
    sampling: AtomicBool,
}

impl FakeDaemon {
    fn publish_state(&self) {
        if let Some(p) = self.publisher.get() {
            p.publish(&Event::State(self.state.lock().unwrap().clone()));
        }
    }

    fn outputs_list() -> Vec<Output> {
        vec![
            Output {
                id: "DP-1".into(),
                name: "27\" 1440p".into(),
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
                scale: 1.0,
                refresh_mhz: 165_000,
                primary: true,
            },
            Output {
                id: "HDMI-A-1".into(),
                name: "24\" 1080p".into(),
                x: 2560,
                y: 180,
                width: 1920,
                height: 1080,
                scale: 1.0,
                refresh_mhz: 60_000,
                primary: false,
            },
        ]
    }

    /// Fake 1 Hz stats, only while a client subscribes (like the real daemon should).
    fn start_sampling(self: &Arc<Self>) {
        if self.sampling.swap(true, Ordering::SeqCst) {
            return;
        }
        let me = Arc::clone(self);
        std::thread::spawn(move || {
            let mut tick = 0u32;
            while let Some(p) = me.publisher.get() {
                if !p.has_subscribers(Topic::Stats) {
                    break;
                }
                p.publish(&Event::Stats(me.stats_now(tick)));
                tick += 1;
                std::thread::sleep(Duration::from_secs(1));
            }
            me.sampling.store(false, Ordering::SeqCst);
        });
    }

    fn stats_now(&self, tick: u32) -> Stats {
        let state = self.state.lock().unwrap();
        let fps_cap = self.settings.lock().unwrap().fps_limit as f32;
        let playing = state
            .outputs
            .iter()
            .filter(|o| o.wallpaper.is_some())
            .count();
        let running = !state.paused && playing > 0;
        let wobble = ((tick % 7) as f32 - 3.0) * 0.03;
        let fps = if running { fps_cap - 0.1 + wobble } else { 0.0 };
        Stats {
            fps: fps * playing as f32,
            frame_time_ms: if running { 1.4 + wobble } else { 0.0 },
            cpu_percent: if running { 0.9 + wobble * 2.0 } else { 0.0 },
            gpu_percent: Some(if running { 3.2 + wobble * 5.0 } else { 0.0 }),
            rss_bytes: 92 << 20,
            vram_bytes: Some(180 << 20),
            outputs: state
                .outputs
                .iter()
                .map(|o| OutputStats {
                    output: o.output.clone(),
                    fps: if running && o.wallpaper.is_some() {
                        fps
                    } else {
                        0.0
                    },
                    frame_time_ms: 1.4,
                    rendering: running && o.wallpaper.is_some(),
                })
                .collect(),
        }
    }
}

impl Handler for FakeDaemon {
    fn outputs(&self) -> Result<Vec<Output>, RemoteError> {
        Ok(Self::outputs_list())
    }

    fn state(&self) -> Result<State, RemoteError> {
        Ok(self.state.lock().unwrap().clone())
    }

    fn apply(
        &self,
        output: Option<&str>,
        wallpaper: &Path,
        scaling: Scaling,
    ) -> Result<(), RemoteError> {
        if !wallpaper.join("project.json").is_file() {
            return Err(RemoteError::new(
                ErrorCode::NotFound,
                format!("{} is not a wallpaper", wallpaper.display()),
            ));
        }
        let ids: Vec<String> = match output {
            Some(id) if !Self::outputs_list().iter().any(|o| o.id == id) => {
                return Err(RemoteError::new(ErrorCode::UnknownOutput, id));
            }
            Some(id) => vec![id.to_owned()],
            None => Self::outputs_list().into_iter().map(|o| o.id).collect(),
        };
        {
            let mut s = self.state.lock().unwrap();
            for id in ids {
                s.outputs.retain(|o| o.output != id);
                s.outputs.push(OutputState {
                    output: id.clone(),
                    wallpaper: Some(wallpaper.to_owned()),
                    scaling,
                    status: WallpaperStatus::Running,
                    message: None,
                });
                self.props.lock().unwrap().remove(&id);
            }
            s.outputs.sort_by(|a, b| a.output.cmp(&b.output));
        }
        self.publish_state();
        Ok(())
    }

    fn clear(&self, output: Option<&str>) -> Result<(), RemoteError> {
        self.state
            .lock()
            .unwrap()
            .outputs
            .retain(|o| output.is_some_and(|id| id != o.output));
        self.publish_state();
        Ok(())
    }

    fn properties(&self, output: &str) -> Result<PropertyMap, RemoteError> {
        Ok(self
            .props
            .lock()
            .unwrap()
            .get(output)
            .cloned()
            .unwrap_or_default())
    }

    fn set_properties(
        &self,
        output: &str,
        values: PropertyMap,
        _persist: bool,
    ) -> Result<(), RemoteError> {
        let mut all = self.props.lock().unwrap();
        let entry = all.entry(output.to_owned()).or_default();
        entry.extend(values);
        if let Some(p) = self.publisher.get() {
            p.publish(&Event::Properties {
                output: output.to_owned(),
                values: entry.clone(),
            });
        }
        Ok(())
    }

    fn reset_properties(&self, output: &str) -> Result<(), RemoteError> {
        self.props.lock().unwrap().remove(output);
        if let Some(p) = self.publisher.get() {
            p.publish(&Event::Properties {
                output: output.to_owned(),
                values: PropertyMap::new(),
            });
        }
        Ok(())
    }

    fn set_paused(&self, paused: bool) -> Result<(), RemoteError> {
        {
            let mut s = self.state.lock().unwrap();
            s.paused = paused;
            s.pause_reason = paused.then_some(PauseReason::User);
        }
        self.publish_state();
        Ok(())
    }

    fn set_muted(&self, muted: bool) -> Result<(), RemoteError> {
        self.state.lock().unwrap().muted = muted;
        self.publish_state();
        Ok(())
    }

    fn set_volume(&self, volume: f32) -> Result<(), RemoteError> {
        self.state.lock().unwrap().volume = volume;
        self.publish_state();
        Ok(())
    }

    fn settings(&self) -> Result<Settings, RemoteError> {
        Ok(self.settings.lock().unwrap().clone())
    }

    fn set_settings(&self, settings: Settings) -> Result<(), RemoteError> {
        *self.settings.lock().unwrap() = settings.clone();
        if let Some(p) = self.publisher.get() {
            p.publish(&Event::Settings(settings));
        }
        Ok(())
    }

    fn stats(&self) -> Result<Stats, RemoteError> {
        Ok(self.stats_now(0))
    }

    fn subscribers_changed(&self, _topic: Topic, _count: usize) {}
}

/// Serves a fake daemon on a private endpoint; keep the returned server alive.
/// `playing` pre-applies wallpapers to the fake screens, in order.
pub fn start(playing: &[std::path::PathBuf]) -> Result<(Server, Endpoint), Error> {
    let endpoint = Endpoint::Name(format!("lumen-ui-fake-{}", std::process::id()));
    let daemon = Arc::new(FakeDaemon {
        state: Mutex::new(State {
            volume: 0.5,
            ..State::default()
        }),
        ..FakeDaemon::default()
    });
    let handler: Arc<dyn Handler> = Arc::new(SamplingHandler(Arc::clone(&daemon)));
    let server = Server::bind(
        &endpoint,
        handler,
        "lumen (fake)",
        env!("CARGO_PKG_VERSION"),
    )?;
    let _ = daemon.publisher.set(server.publisher());
    for (dir, out) in playing.iter().zip(FakeDaemon::outputs_list()) {
        let _ = daemon.apply(Some(&out.id), dir, Scaling::Fill);
    }
    Ok((server, endpoint))
}

/// Starts the stats thread when the first stats subscriber appears.
struct SamplingHandler(Arc<FakeDaemon>);

macro_rules! forward {
    ($($name:ident($($arg:ident: $ty:ty),*) -> $ret:ty;)*) => {
        $(fn $name(&self, $($arg: $ty),*) -> $ret { self.0.$name($($arg),*) })*
    };
}

impl Handler for SamplingHandler {
    forward! {
        outputs() -> Result<Vec<Output>, RemoteError>;
        state() -> Result<State, RemoteError>;
        apply(output: Option<&str>, wallpaper: &Path, scaling: Scaling) -> Result<(), RemoteError>;
        clear(output: Option<&str>) -> Result<(), RemoteError>;
        properties(output: &str) -> Result<PropertyMap, RemoteError>;
        set_properties(output: &str, values: PropertyMap, persist: bool) -> Result<(), RemoteError>;
        reset_properties(output: &str) -> Result<(), RemoteError>;
        set_paused(paused: bool) -> Result<(), RemoteError>;
        set_muted(muted: bool) -> Result<(), RemoteError>;
        set_volume(volume: f32) -> Result<(), RemoteError>;
        settings() -> Result<Settings, RemoteError>;
        set_settings(settings: Settings) -> Result<(), RemoteError>;
        stats() -> Result<Stats, RemoteError>;
    }

    fn subscribers_changed(&self, topic: Topic, count: usize) {
        if topic == Topic::Stats && count > 0 {
            self.0.start_sampling();
        }
    }
}
