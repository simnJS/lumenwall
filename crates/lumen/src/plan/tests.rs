//! Execution plan tests: layout per mode, inheritance, configuration merge, diff.

use std::path::Path;
use std::sync::Mutex;

use super::*;
use crate::cli::{CliOutcome, parse};
use crate::services::ServiceError;

/// Workshop IDs live under `/workshop`; paths containing "missing" do not exist.
#[derive(Default)]
struct FakeResolver {
    calls: Mutex<Vec<WallpaperSource>>,
}

impl WallpaperResolver for FakeResolver {
    fn resolve(&self, source: &WallpaperSource) -> Result<PathBuf, ServiceError> {
        self.calls.lock().unwrap().push(source.clone());
        match source {
            WallpaperSource::WorkshopId(id) if id.contains("missing") => Err(format!(
                "workshop folder not found for application 431960 and item {id}"
            )
            .into()),
            WallpaperSource::WorkshopId(id) => Ok(PathBuf::from("/workshop").join(id)),
            WallpaperSource::Path(p) if p.to_string_lossy().contains("missing") => {
                Err(format!("{} does not exist", p.display()).into())
            }
            WallpaperSource::Path(p) => Ok(p.clone()),
        }
    }
}

/// Playlist `NAME` starts with `/playlists/NAME/first`; `unknown` does not exist.
struct FakePlaylists;

impl PlaylistCatalog for FakePlaylists {
    fn first_wallpaper(&self, name: &str) -> Result<PathBuf, ServiceError> {
        if name == "unknown" {
            return Err("playlist 'unknown' not found; available playlists: day".into());
        }
        Ok(PathBuf::from("/playlists").join(name).join("first"))
    }
}

fn cli(args: &[&str]) -> CliArgs {
    match parse(std::iter::once("lumen").chain(args.iter().copied())) {
        Ok(CliOutcome::Run(cli)) => *cli,
        other => panic!("{args:?}: {other:?}"),
    }
}

fn build_with(args: &[&str], config: Option<&str>) -> Result<ExecutionPlan, PlanError> {
    let config = config.map(|text| Config::parse(text).expect("valid config"));
    let resolver = FakeResolver::default();
    build(
        &cli(args),
        config.as_ref(),
        PlanServices {
            resolver: &resolver,
            playlists: &FakePlaylists,
        },
    )
}

fn plan(args: &[&str]) -> ExecutionPlan {
    build_with(args, None).unwrap()
}

fn plan_cfg(args: &[&str], config: &str) -> ExecutionPlan {
    build_with(args, Some(config)).unwrap()
}

fn screen<'a>(plan: &'a ExecutionPlan, name: &str) -> &'a TargetPlan {
    plan.target(&TargetKey::Screen(name.into()))
        .unwrap_or_else(|| panic!("no target {name}"))
}

fn keys(plan: &ExecutionPlan) -> Vec<String> {
    plan.targets.iter().map(|t| t.key.to_string()).collect()
}

fn shows(target: &TargetPlan) -> &Path {
    &target.wallpaper.path
}

// --- Modes and spec 07 §2.3 examples ---------------------------------------------------

#[test]
fn preview_in_a_window() {
    let p = plan(&["2317494988"]);
    assert_eq!(p.mode, OutputMode::Window);
    assert_eq!(keys(&p), ["default"]);
    let t = &p.targets[0];
    assert_eq!(t.key, TargetKey::Window);
    assert!(t.outputs.is_empty());
    assert_eq!(shows(t), Path::new("/workshop/2317494988"));
    assert_eq!(
        (t.scaling, t.clamp, t.playlist.as_deref()),
        (Scaling::Default, Clamp::Clamp, None)
    );
    assert_eq!(
        t.wallpaper.reference,
        WallpaperRef::Source(WallpaperSource::WorkshopId("2317494988".into()))
    );
}

#[test]
fn defaults_of_spec_07_2_4() {
    let p = plan(&["1"]);
    assert_eq!(p.layer, Layer::Bottom);
    assert_eq!(p.fps, 30);
    assert_eq!(p.hwdec, HwDecode::Auto);
    assert_eq!(
        p.audio,
        AudioPlan {
            silent: false,
            volume: 15,
            automute: true,
            processing: true
        }
    );
    assert_eq!(
        p.fullscreen_pause,
        FullscreenPausePlan {
            enabled: true,
            only_active: false,
            ignore_app_ids: vec![]
        }
    );
    assert_eq!(
        p.input,
        InputPlan {
            mouse: true,
            parallax: true
        }
    );
    assert!(p.particles);
    assert_eq!(p.assets_dir, None);
    assert!(p.properties.is_empty());
    assert_eq!(p.screenshot, None);
    assert!(!p.list_properties && !p.dump_structure);
    assert!(!p.render_debug.is_enabled());
    assert_eq!(p.frame_interval(), Duration::from_secs(1) / 30);
}

#[test]
fn explicit_window() {
    let p = plan(&["-w", "10x20x800x600", "--scaling", "fill", "X"]);
    assert_eq!(
        p.mode,
        OutputMode::ExplicitWindow(WindowGeometry {
            x: 10,
            y: 20,
            width: 800,
            height: 600
        })
    );
    assert_eq!(p.targets[0].scaling, Scaling::Fill);
    assert_eq!(shows(&p.targets[0]), Path::new("/workshop/X"));
}

#[test]
fn at_least_one_wallpaper() {
    assert!(matches!(build_with(&[], None), Err(PlanError::NoWallpaper)));
    assert!(matches!(
        build_with(&["-r", "A"], None),
        Err(PlanError::NoWallpaper)
    ));
    assert!(matches!(
        build_with(&["-w", "0x0x1x1"], None),
        Err(PlanError::NoWallpaper)
    ));
    assert_eq!(
        PlanError::NoWallpaper.to_string(),
        "at least one wallpaper must be specified"
    );
}

#[test]
fn example_two_outputs_share_the_default() {
    let p = plan(&["-r", "HDMI-1", "-r", "HDMI-2", "2317494988"]);
    assert_eq!(p.mode, OutputMode::Desktop);
    assert_eq!(keys(&p), ["HDMI-1", "HDMI-2"]);
    assert!(
        p.targets
            .iter()
            .all(|t| shows(t) == Path::new("/workshop/2317494988"))
    );
    assert_eq!(screen(&p, "HDMI-2").outputs, ["HDMI-2"]);
}

#[test]
fn example_one_wallpaper_per_output() {
    let p = plan(&["-r", "HDMI-1", "--bg", "A", "-r", "HDMI-2", "--bg", "B"]);
    assert_eq!(shows(screen(&p, "HDMI-1")), Path::new("/workshop/A"));
    assert_eq!(shows(screen(&p, "HDMI-2")), Path::new("/workshop/B"));
}

#[test]
fn example_span() {
    let p = plan(&[
        "--screen-span",
        "HDMI-1,HDMI-2",
        "--bg",
        "2317494988",
        "--scaling",
        "fill",
    ]);
    assert_eq!(keys(&p), ["span:HDMI-1,HDMI-2"]);
    let t = &p.targets[0];
    assert_eq!(t.key, TargetKey::Span("span:HDMI-1,HDMI-2".into()));
    assert_eq!(t.outputs, ["HDMI-1", "HDMI-2"]);
    assert_eq!(t.scaling, Scaling::Fill);
    assert_eq!(p.requested_outputs(), ["HDMI-1", "HDMI-2"]);
}

#[test]
fn example_scaling_trap() {
    let p = plan(&[
        "--scaling",
        "stretch",
        "-r",
        "eDP-1",
        "--bg",
        "X",
        "--scaling",
        "fill",
        "-r",
        "HDMI-1",
        "--bg",
        "Y",
    ]);
    assert_eq!(screen(&p, "eDP-1").scaling, Scaling::Fill);
    assert_eq!(screen(&p, "HDMI-1").scaling, Scaling::Stretch);
}

#[test]
fn outputs_are_sorted_alphabetically_then_spans() {
    let p = plan(&[
        "-r",
        "eDP-1",
        "--screen-span",
        "Z-2,Z-1",
        "-r",
        "DP-2",
        "--screen-span",
        "A-1,A-2",
        "-r",
        "HDMI-1",
        "W",
    ]);
    assert_eq!(
        keys(&p),
        ["DP-2", "HDMI-1", "eDP-1", "span:A-1,A-2", "span:Z-2,Z-1"]
    );
    assert_eq!(
        p.requested_outputs(),
        ["DP-2", "HDMI-1", "eDP-1", "A-1", "A-2", "Z-2", "Z-1"]
    );
}

// --- Resolution -------------------------------------------------------------------------

#[test]
fn every_named_wallpaper_is_resolved_once() {
    let resolver = FakeResolver::default();
    let args = cli(&[
        "X", "-r", "A", "--bg", "./w", "-r", "B", "--bg", "./w", "-r", "C",
    ]);
    let p = build(
        &args,
        None,
        PlanServices {
            resolver: &resolver,
            playlists: &FakePlaylists,
        },
    )
    .unwrap();
    // X was overridden as the default but is still looked up (a typo stays fatal).
    let calls = resolver.calls.lock().unwrap().clone();
    assert_eq!(
        calls,
        [
            WallpaperSource::WorkshopId("X".into()),
            WallpaperSource::Path("./w".into())
        ]
    );
    assert_eq!(shows(screen(&p, "C")), Path::new("./w"));
}

#[test]
fn resolution_errors_are_fatal() {
    let e = build_with(&["missing-id"], None).unwrap_err();
    assert_eq!(
        e.to_string(),
        "workshop folder not found for application 431960 and item missing-id"
    );
    assert!(matches!(e, PlanError::Resolve { .. }));
    // Even when a later --bg replaces it.
    assert!(build_with(&["./missing", "-r", "A", "--bg", "X"], None).is_err());
    assert!(build_with(&["--playlist", "unknown", "--bg", "X"], None).is_err());
}

#[test]
fn playlists() {
    let p = plan(&["--playlist", "day"]);
    let t = &p.targets[0];
    assert_eq!(t.playlist.as_deref(), Some("day"));
    assert_eq!(shows(t), Path::new("/playlists/day/first"));

    // The window playlist wins over the default wallpaper (§4.3).
    let p = plan(&["X", "--playlist", "day"]);
    assert_eq!(shows(&p.targets[0]), Path::new("/playlists/day/first"));

    let p = plan(&[
        "-r",
        "A",
        "--playlist",
        "day",
        "-r",
        "B",
        "--screen-span",
        "C,D",
        "--playlist",
        "night",
    ]);
    assert_eq!(screen(&p, "A").playlist.as_deref(), Some("day"));
    assert_eq!(shows(screen(&p, "B")), Path::new("/playlists/day/first"));
    assert_eq!(screen(&p, "B").playlist, None);
    // Deviation (QO-25): span groups keep their playlist.
    let span = p.target(&TargetKey::Span("span:C,D".into())).unwrap();
    assert_eq!(span.playlist.as_deref(), Some("night"));
    assert_eq!(shows(span), Path::new("/playlists/night/first"));
}

// --- Settings ---------------------------------------------------------------------------

#[test]
fn command_line_settings() {
    let p = plan(&[
        "X",
        "--fps",
        "60",
        "--layer",
        "overlay",
        "--hwdec",
        "nvdec-copy",
        "--volume",
        "200",
        "--noautomute",
        "--no-audio-processing",
        "--no-fullscreen-pause",
        "--fullscreen-pause-only-active",
        "--fullscreen-pause-ignore-appid",
        "mpv",
        "--disable-mouse",
        "--disable-parallax",
        "--disable-particles",
        "--assets-dir",
        "/we/assets",
        "--set-property",
        "a",
        "--set-property",
        "b=2",
        "--set-property",
        "a=3",
        "--screenshot",
        "/tmp/x.jpeg",
        "--screenshot-delay",
        "2",
        "-l",
        "-z",
        "--render-debug",
        "pass-log",
    ]);
    assert_eq!(p.fps, 60);
    assert_eq!(p.layer, Layer::Overlay);
    assert_eq!(p.hwdec, HwDecode::NvdecCopy);
    assert_eq!(
        p.audio,
        AudioPlan {
            silent: false,
            volume: 128,
            automute: false,
            processing: false
        }
    );
    assert_eq!(
        p.fullscreen_pause,
        FullscreenPausePlan {
            enabled: false,
            only_active: true,
            ignore_app_ids: vec!["mpv".into()]
        }
    );
    assert_eq!(
        p.input,
        InputPlan {
            mouse: false,
            parallax: false
        }
    );
    assert!(!p.particles);
    assert_eq!(p.assets_dir.as_deref(), Some(Path::new("/we/assets")));
    assert_eq!(
        p.properties,
        BTreeMap::from([("a".into(), "3".into()), ("b".into(), "2".into())])
    );
    assert_eq!(
        p.screenshot,
        Some(ScreenshotPlan {
            path: "/tmp/x.jpeg".into(),
            format: ScreenshotFormat::Jpeg,
            delay_frames: 2
        })
    );
    assert!(p.list_properties && p.dump_structure && p.render_debug.pass_log);
    assert!(plan(&["X", "-s"]).audio.silent);
}

#[test]
fn screenshot_validation() {
    let e = build_with(&["X", "--screenshot", "/tmp/x.PNG"], None).unwrap_err();
    assert!(matches!(e, PlanError::Screenshot { .. }), "{e}");
    assert!(e.to_string().contains("unknown screenshot extension"));
    assert!(build_with(&["X", "--screenshot", "/tmp/noext"], None).is_err());
    let p = plan(&["X", "--screenshot", "out.bmp"]);
    assert_eq!(p.screenshot.unwrap().delay_frames, 5);
}

// --- Configuration file merge -------------------------------------------------------------

#[test]
fn config_scalars_apply_when_the_command_line_is_silent() {
    let config = r#"
        fps = 15
        volume = 300
        automute = false
        audio-processing = false
        fullscreen-pause = false
        fullscreen-pause-only-active = true
        fullscreen-pause-ignore-appids = ["steam", ""]
        layer = "top"
        hwdec = "no"
        mouse = false
        parallax = false
        particles = false
        assets-dir = "/cfg/assets"
        screenshot = "/cfg/shot.png"
        screenshot-delay = 1
    "#;
    let p = plan_cfg(&["X"], config);
    assert_eq!(p.fps, 15);
    assert_eq!(
        p.audio,
        AudioPlan {
            silent: false,
            volume: 128,
            automute: false,
            processing: false
        }
    );
    assert_eq!(
        p.fullscreen_pause,
        FullscreenPausePlan {
            enabled: false,
            only_active: true,
            ignore_app_ids: vec!["steam".into()]
        }
    );
    assert_eq!((p.layer, p.hwdec), (Layer::Top, HwDecode::No));
    assert_eq!(
        p.input,
        InputPlan {
            mouse: false,
            parallax: false
        }
    );
    assert!(!p.particles);
    assert_eq!(p.assets_dir.as_deref(), Some(Path::new("/cfg/assets")));
    assert_eq!(p.screenshot.as_ref().map(|s| s.delay_frames), Some(1));

    let p = plan_cfg(
        &[
            "X",
            "-f",
            "90",
            "--layer",
            "background",
            "--hwdec",
            "auto",
            "--assets-dir",
            "/cli",
            "--screenshot",
            "/cli.bmp",
            "--fullscreen-pause-ignore-appid",
            "mpv",
        ],
        config,
    );
    assert_eq!(p.fps, 90);
    assert_eq!((p.layer, p.hwdec), (Layer::Background, HwDecode::Auto));
    assert_eq!(p.assets_dir.as_deref(), Some(Path::new("/cli")));
    let shot = p.screenshot.unwrap();
    assert_eq!(
        (shot.path.as_path(), shot.format, shot.delay_frames),
        (Path::new("/cli.bmp"), ScreenshotFormat::Bmp, 1)
    );
    assert_eq!(p.fullscreen_pause.ignore_app_ids, ["mpv"]);
}

#[test]
fn config_sound() {
    // --volume turns a configured silence off; --silent keeps the configured volume.
    let config = "silent = true\nvolume = 40";
    assert_eq!(
        plan_cfg(&["X"], config).audio,
        AudioPlan {
            silent: true,
            volume: 40,
            automute: true,
            processing: true
        }
    );
    let p = plan_cfg(&["X", "-v", "7"], config);
    assert_eq!((p.audio.silent, p.audio.volume), (false, 7));
    let p = plan_cfg(&["X", "-s"], "volume = 40");
    assert_eq!((p.audio.silent, p.audio.volume), (true, 40));
}

#[test]
fn config_properties_merge_by_key() {
    let p = plan_cfg(
        &["X", "--set-property", "b=cli"],
        "[properties]\na = true\nb = \"file\"\nc = 2.5",
    );
    assert_eq!(
        p.properties,
        BTreeMap::from([
            ("a".into(), "1".into()),
            ("b".into(), "cli".into()),
            ("c".into(), "2.5".into())
        ])
    );
}

#[test]
fn config_default_content() {
    assert_eq!(
        shows(&plan_cfg(&[], "wallpaper = \"cfg\"").targets[0]),
        Path::new("/workshop/cfg")
    );
    assert_eq!(
        shows(&plan_cfg(&[], "wallpaper = \"./dir\"").targets[0]),
        Path::new("./dir")
    );
    let p = plan_cfg(&[], "playlist = \"day\"");
    assert_eq!(p.targets[0].playlist.as_deref(), Some("day"));
    // The command line replaces both the file's wallpaper and playlist.
    let p = plan_cfg(&["cli"], "wallpaper = \"cfg\"\nplaylist = \"day\"");
    assert_eq!(shows(&p.targets[0]), Path::new("/workshop/cli"));
    assert_eq!(p.targets[0].playlist, None);
    // A command-line layout without wallpaper uses the file's default.
    let p = plan_cfg(&["-r", "A"], "wallpaper = \"cfg\"");
    assert_eq!(shows(screen(&p, "A")), Path::new("/workshop/cfg"));
}

const LAYOUT: &str = r#"
    wallpaper = "default"
    scaling = "fit"
    clamp = "border"

    [[screen]]
    name = "HDMI-1"
    wallpaper = "own"
    scaling = "stretch"

    [[screen]]
    name = "DP-1"

    [[screen]]
    name = "eDP-1"
    playlist = "day"

    [[span]]
    screens = ["L", "R"]
    wallpaper = "./wide"
    clamp = "repeat"
"#;

#[test]
fn config_layout() {
    let p = plan_cfg(&[], LAYOUT);
    assert_eq!(p.mode, OutputMode::Desktop);
    assert_eq!(keys(&p), ["DP-1", "HDMI-1", "eDP-1", "span:L,R"]);
    let hdmi = screen(&p, "HDMI-1");
    assert_eq!(
        (shows(hdmi), hdmi.scaling, hdmi.clamp),
        (Path::new("/workshop/own"), Scaling::Stretch, Clamp::Border)
    );
    let dp = screen(&p, "DP-1");
    assert_eq!(
        (shows(dp), dp.scaling, dp.clamp),
        (Path::new("/workshop/default"), Scaling::Fit, Clamp::Border)
    );
    let edp = screen(&p, "eDP-1");
    assert_eq!(
        (shows(edp), edp.playlist.as_deref()),
        (Path::new("/playlists/day/first"), Some("day"))
    );
    let span = &p.targets[3];
    assert_eq!(
        (shows(span), span.outputs.clone(), span.clamp),
        (
            Path::new("./wide"),
            vec!["L".to_owned(), "R".to_owned()],
            Clamp::Repeat
        )
    );
}

#[test]
fn config_layout_with_command_line_globals() {
    // A command-line default wallpaper and global scaling apply to the file's layout; the
    // file's per-output values stay.
    let p = plan_cfg(&["--scaling", "fill", "cli"], LAYOUT);
    assert_eq!(screen(&p, "DP-1").scaling, Scaling::Fill);
    assert_eq!(shows(screen(&p, "DP-1")), Path::new("/workshop/cli"));
    assert_eq!(screen(&p, "HDMI-1").scaling, Scaling::Stretch);
    assert_eq!(shows(screen(&p, "HDMI-1")), Path::new("/workshop/own"));
}

#[test]
fn command_line_layout_replaces_the_config_layout() {
    let p = plan_cfg(&["-r", "X-1"], LAYOUT);
    assert_eq!(keys(&p), ["X-1"]);
    let t = screen(&p, "X-1");
    // Inherits the file's default wallpaper, scaling and clamp.
    assert_eq!(
        (shows(t), t.scaling, t.clamp),
        (Path::new("/workshop/default"), Scaling::Fit, Clamp::Border)
    );
    // A command-line global value copied by -r wins over the file's.
    let p = plan_cfg(&["--clamp", "repeat", "-r", "X-1"], LAYOUT);
    assert_eq!(screen(&p, "X-1").clamp, Clamp::Repeat);
    let p = plan_cfg(&["-w", "0x0x100x100"], LAYOUT);
    assert_eq!(
        p.mode,
        OutputMode::ExplicitWindow(WindowGeometry {
            x: 0,
            y: 0,
            width: 100,
            height: 100
        })
    );
    assert_eq!(p.targets[0].scaling, Scaling::Fit);
}

#[test]
fn config_window() {
    let p = plan_cfg(&["X"], "[window]\nx = 5\nwidth = 300\nheight = 200");
    assert_eq!(
        p.mode,
        OutputMode::ExplicitWindow(WindowGeometry {
            x: 5,
            y: 0,
            width: 300,
            height: 200
        })
    );
}

#[test]
fn config_layout_without_wallpaper() {
    assert!(matches!(
        build_with(&[], Some("[[screen]]\nname = \"A\"")),
        Err(PlanError::NoWallpaper)
    ));
}

// --- Diff ---------------------------------------------------------------------------------

#[test]
fn diff_of_identical_plans_is_empty() {
    let a = plan_cfg(&[], LAYOUT);
    assert!(diff(&a, &a.clone()).is_empty());
}

#[test]
fn diff_lists_global_changes() {
    let a = plan(&["X"]);
    let b = plan(&[
        "X",
        "-f",
        "60",
        "-s",
        "--disable-mouse",
        "--no-fullscreen-pause",
        "--disable-particles",
        "--layer",
        "top",
        "--hwdec",
        "no",
        "--assets-dir",
        "/a",
        "--set-property",
        "k",
        "--screenshot",
        "a.png",
    ]);
    assert_eq!(
        diff(&a, &b),
        [
            PlanChange::Layer,
            PlanChange::Fps,
            PlanChange::HwDecode,
            PlanChange::Audio,
            PlanChange::FullscreenPause,
            PlanChange::Input,
            PlanChange::Particles,
            PlanChange::AssetsDir,
            PlanChange::Properties,
            PlanChange::Screenshot,
        ]
    );
}

#[test]
fn diff_lists_target_changes() {
    let a = plan(&[
        "-r", "A", "--bg", "1", "-r", "B", "--bg", "2", "-r", "C", "--bg", "3",
    ]);
    let b = plan(&[
        "-r",
        "A",
        "--bg",
        "1",
        "--scaling",
        "fill",
        "-r",
        "B",
        "--bg",
        "9",
        "-r",
        "D",
        "--bg",
        "3",
    ]);
    assert_eq!(
        diff(&a, &b),
        [
            PlanChange::TargetRemoved(TargetKey::Screen("C".into())),
            PlanChange::TargetScaling(TargetKey::Screen("A".into())),
            PlanChange::TargetContent(TargetKey::Screen("B".into())),
            PlanChange::TargetAdded(TargetKey::Screen("D".into())),
        ]
    );
}

#[test]
fn diff_of_a_mode_change() {
    let a = plan(&["X"]);
    let b = plan(&["-r", "A", "--bg", "X", "-f", "10"]);
    assert_eq!(diff(&a, &b), [PlanChange::OutputMode, PlanChange::Fps]);
    let c = plan(&["-w", "0x0x10x10", "X"]);
    let d = plan(&["-w", "0x0x20x10", "X"]);
    assert_eq!(diff(&c, &d), [PlanChange::OutputMode]);
}

#[test]
fn display_summarises_the_plan() {
    let text = plan(&[
        "-r",
        "A",
        "--bg",
        "./w",
        "--scaling",
        "fill",
        "--playlist",
        "day",
        "-s",
    ])
    .to_string();
    assert!(text.contains("mode: desktop"), "{text}");
    assert!(
        text.contains("A: /playlists/day/first (fill, clamp, playlist day)"),
        "{text}"
    );
    assert!(text.contains("fps 30, volume 15 (silent)"), "{text}");
    assert!(
        plan(&["-w", "1x2x3x4", "X"])
            .to_string()
            .starts_with("mode: window 1x2x3x4")
    );
}
