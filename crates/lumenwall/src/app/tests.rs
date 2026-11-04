//! Lifecycle tests with a scripted engine: exit codes, output, call order, hot reload.

use std::sync::Mutex;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use super::*;
use crate::plan::{ExecutionPlan, PlanChange};
use crate::services::Control;
use crate::services::{ServiceError, StopReason, Waker};
use crate::settings::WallpaperSource;

struct Resolver;

impl WallpaperResolver for Resolver {
    fn resolve(&self, source: &WallpaperSource) -> Result<PathBuf, ServiceError> {
        match source {
            WallpaperSource::WorkshopId(id) => Ok(PathBuf::from("/workshop").join(id)),
            WallpaperSource::Path(p) => Ok(p.clone()),
        }
    }
}

struct NoPlaylists;

impl PlaylistCatalog for NoPlaylists {
    fn first_wallpaper(&self, name: &str) -> Result<PathBuf, ServiceError> {
        Err(format!("playlist '{name}' not found").into())
    }
}

struct NoWake;

impl Waker for NoWake {
    fn wake(&self) {}
}

#[derive(Default)]
struct Record {
    calls: Vec<&'static str>,
    plan: Option<ExecutionPlan>,
    reloads: Vec<(Arc<ExecutionPlan>, Vec<PlanChange>)>,
}

type RunScript =
    Box<dyn FnOnce(Receiver<Control>, &Mutex<Record>) -> Result<StopReason, ServiceError>>;

struct ScriptedEngine {
    record: Arc<Mutex<Record>>,
    fail_at: Option<&'static str>,
    run: Option<RunScript>,
}

impl ScriptedEngine {
    fn step(&self, name: &'static str) -> Result<(), ServiceError> {
        self.record.lock().unwrap().calls.push(name);
        if self.fail_at == Some(name) {
            Err(format!("{name} failed").into())
        } else {
            Ok(())
        }
    }
}

impl Engine for ScriptedEngine {
    fn waker(&self) -> Arc<dyn Waker> {
        Arc::new(NoWake)
    }

    fn load(&mut self, plan: &ExecutionPlan) -> Result<(), ServiceError> {
        self.record.lock().unwrap().plan = Some(plan.clone());
        self.step("load")
    }

    fn write_properties(&mut self, out: &mut dyn Write) -> Result<(), ServiceError> {
        self.step("write_properties")?;
        out.write_all(b"bloom - bool\n\tValue: 1\n")?;
        Ok(())
    }

    fn start(&mut self) -> Result<(), ServiceError> {
        self.step("start")
    }

    fn write_structure(&mut self, out: &mut dyn Write) -> Result<(), ServiceError> {
        self.step("write_structure")?;
        out.write_all(b"scene structure\n")?;
        Ok(())
    }

    fn run(&mut self, control: Receiver<Control>) -> Result<StopReason, ServiceError> {
        self.step("run")?;
        match self.run.take() {
            Some(script) => script(control, &self.record),
            None => Ok(StopReason::Signal(15)),
        }
    }
}

struct Outcome {
    exit: Exit,
    out: String,
    err: String,
    record: Arc<Mutex<Record>>,
}

impl Outcome {
    fn calls(&self) -> Vec<&'static str> {
        self.record.lock().unwrap().calls.clone()
    }
}

fn launch(args: &[&str], fail_at: Option<&'static str>, script: Option<RunScript>) -> Outcome {
    launch_with(args, fail_at, script, Arc::new(NoPlaylists), None)
}

fn launch_with(
    args: &[&str],
    fail_at: Option<&'static str>,
    script: Option<RunScript>,
    playlists: Arc<dyn PlaylistCatalog>,
    ipc: Option<lumenwall_ipc::Endpoint>,
) -> Outcome {
    let record = Arc::new(Mutex::new(Record::default()));
    let services = Services {
        resolver: Arc::new(Resolver),
        playlists,
        engine: Box::new(ScriptedEngine {
            record: record.clone(),
            fail_at,
            run: script,
        }),
        ipc,
        power: None,
    };
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let args = std::iter::once("lumenwall")
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect();
    // `attached: false` keeps test logs off the terminal.
    let exit = run(
        args,
        services,
        Console {
            out: &mut out,
            err: &mut err,
            attached: false,
        },
    );
    Outcome {
        exit,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
        record,
    }
}

#[test]
fn exit_codes() {
    assert_eq!(ExitCode::from(Exit::Success), ExitCode::SUCCESS);
    assert_eq!(Exit::Failure as u8, 1);
}

#[test]
fn help_and_version() {
    let o = launch(&["--help"], None, None);
    assert_eq!(o.exit, Exit::Success);
    assert!(o.out.contains("Usage: lumenwall"), "{}", o.out);
    assert!(!o.out.contains("Running with"));
    assert!(o.calls().is_empty());
    let o = launch(&["--version", "--fps", "0"], None, None);
    assert_eq!(o.exit, Exit::Success);
    assert!(o.out.starts_with("lumenwall "));
}

#[test]
fn argument_errors_are_reported_once_with_a_hint() {
    let o = launch(&["--fps", "0"], None, None);
    assert_eq!(o.exit, Exit::Failure);
    assert_eq!(
        o.err,
        "error: invalid value '0' for '--fps': the frame rate must be a positive integer\n\
         Run 'lumenwall --help' for usage.\n"
    );
    assert!(o.out.is_empty());
    assert!(o.calls().is_empty());
}

#[test]
fn normal_run() {
    let o = launch(
        &[
            "--no-config",
            "-r",
            "HDMI-1",
            "--bg",
            "123",
            "--unknown-flag",
        ],
        None,
        None,
    );
    assert_eq!(o.exit, Exit::Success);
    assert_eq!(
        o.out,
        "Running with: lumenwall --no-config -r HDMI-1 --bg 123 --unknown-flag\n"
    );
    assert_eq!(o.calls(), ["load", "start", "run"]);
    let record = o.record.lock().unwrap();
    let plan = record.plan.as_ref().unwrap();
    assert_eq!(plan.targets[0].outputs, ["HDMI-1"]);
    assert_eq!(
        plan.targets[0].wallpaper.path,
        PathBuf::from("/workshop/123")
    );
}

#[test]
fn list_properties_exits_before_starting() {
    let o = launch(&["--no-config", "-l", "X"], None, None);
    assert_eq!(o.exit, Exit::Success);
    assert_eq!(o.calls(), ["load", "write_properties"]);
    assert!(o.out.ends_with("bloom - bool\n\tValue: 1\n"), "{}", o.out);
}

#[test]
fn dump_structure_after_start() {
    let o = launch(&["--no-config", "-z", "X"], None, None);
    assert_eq!(o.exit, Exit::Success);
    assert_eq!(o.calls(), ["load", "start", "write_structure", "run"]);
    assert!(o.out.ends_with("scene structure\n"));
}

#[test]
fn engine_failures_are_fatal() {
    for (step, calls) in [
        ("load", &["load"][..]),
        ("start", &["load", "start"]),
        ("run", &["load", "start", "run"]),
    ] {
        let o = launch(&["--no-config", "X"], Some(step), None);
        assert_eq!(o.exit, Exit::Failure, "{step}");
        assert_eq!(o.calls(), calls);
    }
    let o = launch(&["--no-config", "-l", "X"], Some("write_properties"), None);
    assert_eq!(o.exit, Exit::Failure);
}

#[test]
fn plan_errors_are_fatal() {
    let o = launch(&["--no-config"], None, None);
    assert_eq!(o.exit, Exit::Failure);
    assert!(o.out.starts_with("Running with:"));
    assert!(o.calls().is_empty());
    let o = launch(&["--no-config", "--playlist", "nope"], None, None);
    assert_eq!(o.exit, Exit::Failure);
    let o = launch(&["--no-config", "X", "--screenshot", "a.gif"], None, None);
    assert_eq!(o.exit, Exit::Failure);
}

#[test]
fn configuration_errors_are_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing.toml");
    let o = launch(&["--config", missing.to_str().unwrap(), "X"], None, None);
    assert_eq!(o.exit, Exit::Failure);
    assert!(o.calls().is_empty());

    let invalid = dir.path().join("bad.toml");
    std::fs::write(&invalid, "fps = 0").unwrap();
    let o = launch(&["--config", invalid.to_str().unwrap(), "X"], None, None);
    assert_eq!(o.exit, Exit::Failure);
}

#[test]
fn configuration_file_is_merged() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(
        &path,
        "fps = 12\nwallpaper = \"cfg\"\n[[screen]]\nname = \"DP-1\"",
    )
    .unwrap();
    let o = launch(
        &["--config", path.to_str().unwrap(), "--fps", "50"],
        None,
        None,
    );
    assert_eq!(o.exit, Exit::Success);
    let record = o.record.lock().unwrap();
    let plan = record.plan.as_ref().unwrap();
    assert_eq!(plan.fps, 50);
    assert_eq!(plan.targets[0].outputs, ["DP-1"]);
    assert_eq!(
        plan.targets[0].wallpaper.path,
        PathBuf::from("/workshop/cfg")
    );
}

/// Next reload within `timeout`. Stop requests are skipped: the signal test raises SIGTERM
/// in this process while other tests run.
fn next_reload(
    control: &Receiver<Control>,
    timeout: Duration,
) -> Option<(Arc<ExecutionPlan>, Vec<PlanChange>)> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        match control.recv_timeout(left).ok()? {
            Control::Reload { plan, changes, .. } => return Some((plan, changes)),
            Control::Stop(_) | Control::Properties(_) | Control::Pause(_) => {}
        }
    }
}

#[test]
fn configuration_hot_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.toml");
    std::fs::write(&path, "fps = 10").unwrap();
    let edited = path.clone();
    let script: RunScript = Box::new(move |control, record| {
        let wait = Duration::from_secs(10);
        // An invalid edit is rejected: the previous settings stay, nothing is sent.
        std::fs::write(&edited, "fps = 0").unwrap();
        assert_eq!(next_reload(&control, Duration::from_millis(800)), None);
        // A valid edit produces a reload with the differences.
        std::fs::write(&edited, "fps = 20\nvolume = 3").unwrap();
        record
            .lock()
            .unwrap()
            .reloads
            .push(next_reload(&control, wait).expect("no reload"));
        // Removing the file goes back to the defaults.
        std::fs::remove_file(&edited).unwrap();
        record
            .lock()
            .unwrap()
            .reloads
            .push(next_reload(&control, wait).expect("no reload"));
        Ok(StopReason::Driver)
    });
    let o = launch(
        &["--config", path.to_str().unwrap(), "X"],
        None,
        Some(script),
    );
    assert_eq!(o.exit, Exit::Success);
    let record = o.record.lock().unwrap();
    assert_eq!(record.plan.as_ref().unwrap().fps, 10);
    let [(first, first_changes), (second, second_changes)] = &record.reloads[..] else {
        panic!("expected two reloads")
    };
    assert_eq!(first_changes, &[PlanChange::Fps, PlanChange::Audio]);
    assert_eq!((first.fps, first.audio.volume), (20, 3));
    assert_eq!(second_changes, &[PlanChange::Fps, PlanChange::Audio]);
    assert_eq!((second.fps, second.audio.volume), (30, 64));
}

/// Playlist `fast` over the given folders, changing every 30 ms.
struct FastPlaylist(Vec<PathBuf>);

impl PlaylistCatalog for FastPlaylist {
    fn first_wallpaper(&self, name: &str) -> Result<PathBuf, ServiceError> {
        Ok(PlaylistCatalog::playlist(self, name)?.items[0].clone())
    }

    fn playlist(&self, name: &str) -> Result<crate::playlist::Playlist, ServiceError> {
        if name != "fast" {
            return Err(format!("playlist '{name}' not found; available playlists: fast").into());
        }
        Ok(crate::playlist::Playlist {
            name: name.to_owned(),
            items: self.0.clone(),
            settings: crate::playlist::PlaylistSettings {
                delay: Duration::from_millis(30),
                ..Default::default()
            },
        })
    }
}

fn wallpaper_folders(dir: &std::path::Path, names: &[&str]) -> Vec<PathBuf> {
    names
        .iter()
        .map(|n| {
            let path = dir.join(n);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(
                path.join("project.json"),
                r#"{"type":"scene","file":"scene.json"}"#,
            )
            .unwrap();
            path
        })
        .collect()
}

#[test]
fn a_command_line_playlist_changes_wallpaper() {
    let dir = tempfile::tempdir().unwrap();
    let items = wallpaper_folders(dir.path(), &["one", "two"]);
    let script: RunScript = Box::new(move |control, record| {
        // The daemon's playlist thread asks for the next item.
        let (plan, changes) =
            next_reload(&control, Duration::from_secs(10)).expect("the playlist did not change");
        record.lock().unwrap().reloads.push((plan, changes));
        Ok(StopReason::Driver)
    });
    let o = launch_with(
        &["--no-config", "-r", "DP-1", "--playlist", "fast"],
        None,
        Some(script),
        Arc::new(FastPlaylist(items.clone())),
        None,
    );
    assert_eq!(o.exit, Exit::Success);
    let record = o.record.lock().unwrap();
    let start = &record.plan.as_ref().unwrap().targets[0];
    assert_eq!(start.wallpaper.path, items[0]);
    assert_eq!(start.playlist.as_deref(), Some("fast"));
    let (next, changes) = &record.reloads[0];
    assert_eq!(next.targets[0].wallpaper.path, items[1]);
    assert_eq!(
        changes,
        &[PlanChange::TargetContent(crate::plan::TargetKey::Screen(
            "DP-1".into()
        ))]
    );
}

/// Records what a second `lumenwall` sends to the running daemon.
#[derive(Default)]
struct Running(Mutex<Vec<(Option<String>, String)>>);

impl lumenwall_ipc::Handler for Running {
    fn apply_playlist(
        &self,
        output: Option<&str>,
        name: &str,
        _scaling: lumenwall_ipc::Scaling,
    ) -> Result<(), lumenwall_ipc::RemoteError> {
        let entry = (output.map(str::to_owned), format!("playlist {name}"));
        self.0.lock().unwrap().push(entry);
        Ok(())
    }

    fn apply(
        &self,
        output: Option<&str>,
        wallpaper: &std::path::Path,
        _scaling: lumenwall_ipc::Scaling,
    ) -> Result<(), lumenwall_ipc::RemoteError> {
        let entry = (output.map(str::to_owned), wallpaper.display().to_string());
        self.0.lock().unwrap().push(entry);
        Ok(())
    }
}

#[test]
fn a_second_instance_sends_its_playlist_to_the_running_daemon() {
    let endpoint = if cfg!(unix) {
        let dir = std::env::temp_dir().join(format!("lumenwall-app-test-{}", std::process::id()));
        lumenwall_ipc::Endpoint::Path(dir.join("forward.sock"))
    } else {
        lumenwall_ipc::Endpoint::Name(format!("lumenwall-app-test-{}", std::process::id()))
    };
    let running = Arc::new(Running::default());
    let _server =
        lumenwall_ipc::Server::bind(&endpoint, running.clone(), "lumenwall", "0.0.0").unwrap();
    let o = launch_with(
        &[
            "--no-config",
            "-r",
            "DP-1",
            "--playlist",
            "day",
            "-r",
            "HDMI-A-1",
            "--bg",
            "123",
        ],
        None,
        None,
        Arc::new(NoPlaylists),
        Some(endpoint.clone()),
    );
    assert_eq!(o.exit, Exit::Success);
    assert!(o.out.ends_with("wallpaper sent to it\n"), "{}", o.out);
    // Nothing started here.
    assert!(o.calls().is_empty());
    let sent = running.0.lock().unwrap().clone();
    let workshop = PathBuf::from("/workshop").join("123").display().to_string();
    assert_eq!(
        sent,
        [
            (Some("DP-1".to_owned()), "playlist day".to_owned()),
            (Some("HDMI-A-1".to_owned()), workshop),
        ]
    );

    // Without a target, the playlist goes to every output.
    running.0.lock().unwrap().clear();
    let o = launch_with(
        &["--no-config", "--playlist", "night"],
        None,
        None,
        Arc::new(NoPlaylists),
        Some(endpoint),
    );
    assert_eq!(o.exit, Exit::Success);
    assert_eq!(
        running.0.lock().unwrap().clone(),
        [(None, "playlist night".to_owned())]
    );
}
