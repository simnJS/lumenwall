// Release builds on Windows are GUI-subsystem programs: no console window opens at login.
// Logs go to a rotated file; a parent terminal is reattached when there is one.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, mpsc};

use lumen::app::{self, Console, Services};
use lumen::plan::{ExecutionPlan, OutputMode};
use lumen::playlist::{PathStyle, RealFs, WeConfigCatalog};
use lumen::services::{
    Control, Engine, EngineSnapshot, FrameCounters, Monitor, MonitorChange, MonitorListener,
    OutputSnapshot, ServiceError, StopReason, TargetSnapshot, TargetStatus, Waker,
    WallpaperResolver,
};
use lumen::settings::{Clamp, HwDecode, Layer, Scaling, WallpaperSource};
use lumen_engine::{EngineControl, EnginePlan, Stopped};

fn main() -> ExitCode {
    let attached = lumen::console::prepare();
    let resolver = lumen_engine::WallpaperResolver::new();
    let install = resolver.clone();
    let services = Services {
        resolver: Arc::new(Resolver(resolver.clone())),
        playlists: Arc::new(WeConfigCatalog::new(
            move || {
                let dir = install.install_dir()?;
                std::fs::read_to_string(dir.join("config.json"))
                    .map_err(|e| format!("{}: {e}", dir.join("config.json").display()).into())
            },
            RealFs,
            PathStyle::native(),
        )),
        engine: Box::new(LumenEngine(lumen_engine::Engine::new(resolver))),
        ipc: Some(lumen_ipc::Endpoint::for_current_user()),
    };
    let (mut out, mut err) = (std::io::stdout(), std::io::stderr());
    let console = Console {
        out: &mut out,
        err: &mut err,
        attached,
    };
    app::run(std::env::args_os().collect(), services, console).into()
}

// Adapters between the application's service traits and lumen-engine (which cannot depend on
// this crate: the binary depends on it).

struct Resolver(lumen_engine::WallpaperResolver);

impl WallpaperResolver for Resolver {
    fn resolve(&self, source: &WallpaperSource) -> Result<PathBuf, ServiceError> {
        match source {
            WallpaperSource::Path(path) if path.exists() => Ok(path.clone()),
            WallpaperSource::Path(path) => {
                Err(format!("wallpaper folder '{}' not found", path.display()).into())
            }
            WallpaperSource::WorkshopId(id) => Ok(self.0.workshop_dir(id)?),
        }
    }
}

struct EngineWaker(lumen_engine::EngineWaker);

impl Waker for EngineWaker {
    fn wake(&self) {
        self.0.wake();
    }
}

struct LumenEngine(lumen_engine::Engine);

fn engine_plan(plan: &ExecutionPlan) -> EnginePlan {
    use lumen_engine::{ClampMode, ScalingMode};
    let mode = match plan.mode {
        // Lumen: a wallpaper given without -w/-r covers every output as a desktop wallpaper
        // (the reference opens a 640x480 window, kept behind LUMEN_WINDOWED=1 for debugging).
        OutputMode::Window if std::env::var_os("LUMEN_WINDOWED").is_some_and(|v| v != "0") => {
            lumen_engine::OutputMode::Window
        }
        OutputMode::Window => lumen_engine::OutputMode::Desktop,
        OutputMode::ExplicitWindow(g) => lumen_engine::OutputMode::ExplicitWindow {
            x: g.x,
            y: g.y,
            width: g.width.max(1) as u32,
            height: g.height.max(1) as u32,
        },
        OutputMode::Desktop => lumen_engine::OutputMode::Desktop,
    };
    let targets = plan
        .targets
        .iter()
        .map(|t| lumen_engine::TargetPlan {
            key: t.key.to_string(),
            outputs: t.outputs.clone(),
            wallpaper: t.wallpaper.path.clone(),
            scaling: match t.scaling {
                Scaling::Stretch => ScalingMode::Stretch,
                Scaling::Fit => ScalingMode::Fit,
                Scaling::Fill => ScalingMode::Fill,
                Scaling::Default => ScalingMode::Default,
            },
            clamp: match t.clamp {
                Clamp::Clamp => ClampMode::Clamp,
                Clamp::Border => ClampMode::Border,
                Clamp::Repeat => ClampMode::Repeat,
            },
        })
        .collect();
    EnginePlan {
        mode,
        targets,
        layer: match plan.layer {
            Layer::Background => lumen_engine::plan::Layer::Background,
            Layer::Bottom => lumen_engine::plan::Layer::Bottom,
            Layer::Top => lumen_engine::plan::Layer::Top,
            Layer::Overlay => lumen_engine::plan::Layer::Overlay,
        },
        fps: plan.fps.max(1),
        audio: lumen_engine::AudioPlan {
            silent: plan.audio.silent,
            volume: plan.audio.volume,
            automute: plan.audio.automute,
            processing: plan.audio.processing,
        },
        hwdec: match plan.hwdec {
            HwDecode::Auto => lumen_engine::plan::HwDecode::Auto,
            HwDecode::NvdecCopy => lumen_engine::plan::HwDecode::NvdecCopy,
            HwDecode::No => lumen_engine::plan::HwDecode::No,
        },
        pause: lumen_engine::PausePlan {
            enabled: plan.fullscreen_pause.enabled,
            only_active: plan.fullscreen_pause.only_active,
            ignore_app_ids: plan.fullscreen_pause.ignore_app_ids.clone(),
        },
        mouse: plan.input.mouse,
        parallax: plan.input.parallax,
        particles: plan.particles,
        assets_dir: plan.assets_dir.clone(),
        properties: plan.properties.clone(),
        screenshot: plan
            .screenshot
            .as_ref()
            .map(|s| (s.path.clone(), s.delay_frames)),
        list_properties: plan.list_properties,
        dump_structure: plan.dump_structure,
        debug: lumen_engine::DebugPlan {
            base_only: plan.render_debug.base_only,
            no_solid_final: plan.render_debug.no_solid_final,
            pass_log: plan.render_debug.pass_log,
            only_object: plan.render_debug.only_object,
            skip_objects: plan.render_debug.skip_objects.clone(),
            skip_effects: plan.render_debug.skip_effects.clone(),
        },
    }
}

/// The engine's status, in the daemon's terms.
struct EngineMonitor(lumen_engine::EngineMonitor);

fn ipc_scaling(s: lumen_engine::ScalingMode) -> lumen_ipc::Scaling {
    match s {
        lumen_engine::ScalingMode::Fit => lumen_ipc::Scaling::Fit,
        lumen_engine::ScalingMode::Stretch => lumen_ipc::Scaling::Stretch,
        // `default` (reference fitting) has no protocol name: reported as fill.
        lumen_engine::ScalingMode::Fill | lumen_engine::ScalingMode::Default => {
            lumen_ipc::Scaling::Fill
        }
    }
}

impl Monitor for EngineMonitor {
    fn snapshot(&self) -> EngineSnapshot {
        use lumen_engine::monitor::{PauseCause, PropertyValue as P, TargetState};
        self.0.with_report(|r| EngineSnapshot {
            active: r.active,
            outputs: r
                .outputs
                .iter()
                .map(|o| OutputSnapshot {
                    info: lumen_ipc::Output {
                        id: o.name.clone(),
                        name: o.description.clone().unwrap_or_default(),
                        x: o.position.0,
                        y: o.position.1,
                        width: o.size.0,
                        height: o.size.1,
                        scale: o.scale as f32,
                        refresh_mhz: o.refresh_mhz.unwrap_or(0),
                        primary: false,
                    },
                    shown: o.shown,
                    target: o.target,
                    paused: o.paused.map(|p| match p {
                        PauseCause::Locked => lumen_ipc::PauseReason::Locked,
                        PauseCause::Fullscreen => lumen_ipc::PauseReason::Fullscreen,
                        PauseCause::Hidden => lumen_ipc::PauseReason::Hidden,
                    }),
                })
                .collect(),
            targets: r
                .targets
                .iter()
                .map(|t| TargetSnapshot {
                    key: t.key.clone(),
                    wallpaper: t.wallpaper.clone(),
                    title: t.title.clone(),
                    scaling: ipc_scaling(t.scaling),
                    status: match &t.state {
                        TargetState::Loading => TargetStatus::Loading,
                        TargetState::Running => TargetStatus::Running,
                        TargetState::Error(e) => TargetStatus::Error(e.clone()),
                    },
                    properties: t
                        .properties
                        .iter()
                        .map(|(k, v)| {
                            let v = match v {
                                P::Bool(b) => lumen_ipc::PropertyValue::Bool(*b),
                                P::Number(n) => lumen_ipc::PropertyValue::Number(*n),
                                P::Color(c) => lumen_ipc::PropertyValue::Color(*c),
                                P::Text(s) => lumen_ipc::PropertyValue::Text(s.clone()),
                            };
                            (k.clone(), v)
                        })
                        .collect(),
                })
                .collect(),
        })
    }

    fn set_listener(&self, listener: Option<MonitorListener>) {
        use lumen_engine::monitor::Change;
        self.0.set_listener(listener.map(|l| {
            Arc::new(move |c: Change| {
                l(match c {
                    Change::Outputs => MonitorChange::Outputs,
                    Change::State => MonitorChange::State,
                    Change::Properties => MonitorChange::Properties,
                })
            }) as lumen_engine::monitor::Listener
        }));
    }

    fn set_sampling(&self, on: bool) {
        self.0.set_sampling(on);
    }

    fn take_counters(&self) -> FrameCounters {
        let c = self.0.take_counters();
        FrameCounters {
            elapsed: c.elapsed,
            outputs: c
                .outputs
                .into_iter()
                .map(|o| (o.name, o.frames, o.cpu))
                .collect(),
            gpu: c.gpu,
            gpu_samples: c.gpu_samples,
            gpu_memory: c.gpu_memory,
        }
    }
}

impl Engine for LumenEngine {
    fn waker(&self) -> Arc<dyn Waker> {
        Arc::new(EngineWaker(self.0.waker()))
    }

    fn monitor(&self) -> Option<Arc<dyn Monitor>> {
        Some(Arc::new(EngineMonitor(self.0.monitor())))
    }

    fn load(&mut self, plan: &ExecutionPlan) -> Result<(), ServiceError> {
        Ok(self.0.load(&engine_plan(plan))?)
    }

    fn write_properties(&mut self, out: &mut dyn std::io::Write) -> Result<(), ServiceError> {
        Ok(self.0.write_properties(out)?)
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        Ok(self.0.start()?)
    }

    fn write_structure(&mut self, out: &mut dyn std::io::Write) -> Result<(), ServiceError> {
        Ok(self.0.write_structure(out)?)
    }

    fn run(&mut self, control: mpsc::Receiver<Control>) -> Result<StopReason, ServiceError> {
        let mut queue: VecDeque<EngineControl> = VecDeque::new();
        let mut next = || {
            while queue.is_empty() {
                match control.try_recv().ok()? {
                    Control::Stop(StopReason::Signal(n)) => {
                        queue.push_back(EngineControl::Stop(Some(n)));
                    }
                    Control::Stop(StopReason::Driver | StopReason::Client) => {
                        queue.push_back(EngineControl::Stop(None));
                    }
                    Control::Reload {
                        plan,
                        changes,
                        done,
                    } => {
                        tracing::debug!("applying {changes:?}");
                        queue.push_back(EngineControl::Reload {
                            plan: Box::new(engine_plan(&plan)),
                            done,
                        });
                    }
                    Control::Properties(values) => {
                        queue.push_back(EngineControl::SetProperties(values));
                    }
                    Control::Pause(paused) => queue.push_back(EngineControl::SetPaused(paused)),
                }
            }
            queue.pop_front()
        };
        Ok(match self.0.run(&mut next)? {
            Stopped::Signal(n) => StopReason::Signal(n),
            Stopped::Requested | Stopped::Driver => StopReason::Driver,
        })
    }
}
