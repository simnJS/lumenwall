//! Command-line tests: every option, the §2.3 targeting rules, the §2.2 errors.

use super::*;
use crate::settings::RenderDebug;

fn parse_all(args: &[&str]) -> Result<CliOutcome, CliError> {
    parse(std::iter::once("lumenwall").chain(args.iter().copied()))
}

fn ok(args: &[&str]) -> CliArgs {
    match parse_all(args) {
        Ok(CliOutcome::Run(cli)) => *cli,
        other => panic!("{args:?}: expected a run, got {other:?}"),
    }
}

fn err(args: &[&str]) -> CliError {
    match parse_all(args) {
        Err(e) => e,
        other => panic!("{args:?}: expected an error, got {other:?}"),
    }
}

fn id(s: &str) -> WallpaperRef {
    WallpaperRef::Source(WallpaperSource::WorkshopId(s.into()))
}

fn path(s: &str) -> WallpaperRef {
    WallpaperRef::Source(WallpaperSource::Path(s.into()))
}

fn first(s: &str) -> WallpaperRef {
    WallpaperRef::PlaylistFirst(s.into())
}

fn invalid_for(e: &CliError) -> &str {
    match e {
        CliError::InvalidValue { option, .. } => option,
        other => panic!("expected InvalidValue, got {other:?}"),
    }
}

fn screen(cli: &CliArgs, name: &str) -> TargetSettings {
    cli.layout
        .screens
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no screen {name}"))
        .settings
        .clone()
}

// --- Schema ---------------------------------------------------------------------------

#[test]
fn schema_is_consistent() {
    schema::CliSchema::command().debug_assert();
    let table = build_option_table();
    // Every option of spec 07 §2.2, its aliases, and whether it takes a value.
    let expected: &[(&str, Option<char>, bool)] = &[
        ("help", Some('h'), false),
        ("window", Some('w'), true),
        ("screen-root", Some('r'), true),
        ("screen-span", None, true),
        ("bg", Some('b'), true),
        ("playlist", None, true),
        ("scaling", None, true),
        ("clamp", None, true),
        ("clamping", None, true),
        ("layer", None, true),
        ("fps", Some('f'), true),
        ("hwdec", None, true),
        ("no-fullscreen-pause", None, false),
        ("fullscreen-pause-only-active", None, false),
        ("fullscreen-pause-ignore-appid", None, true),
        ("volume", Some('v'), true),
        ("silent", Some('s'), false),
        ("noautomute", None, false),
        ("no-audio-processing", None, false),
        ("screenshot", None, true),
        ("screenshot-delay", None, true),
        ("assets-dir", None, true),
        ("disable-particles", None, false),
        ("disable-mouse", None, false),
        ("disable-parallax", None, false),
        ("list-properties", Some('l'), false),
        ("set-property", None, true),
        ("property", None, true),
        ("dump-structure", Some('z'), false),
        ("render-debug", None, true),
        ("config", None, true),
        ("no-config", None, false),
        ("version", None, false),
    ];
    for (long, short, takes_value) in expected {
        let spec = table
            .long
            .get(*long)
            .unwrap_or_else(|| panic!("missing --{long}"));
        assert_eq!(spec.takes_value, *takes_value, "--{long}");
        if let Some(short) = short {
            assert_eq!(table.short[short].kind, spec.kind, "-{short}");
        }
    }
    assert_eq!(
        table.long.len(),
        expected.len(),
        "unexpected extra long options"
    );
    assert_eq!(table.short.len(), 9, "unexpected extra short options");
    assert!(
        !table.short.contains_key(&'V'),
        "no -V: --version is long-only"
    );
    assert_eq!(table.long["clamping"].kind, OptKind::Clamp);
    assert_eq!(table.long["property"].kind, OptKind::SetProperty);
}

#[test]
fn help_lists_every_option_and_group() {
    let Ok(CliOutcome::Help(text)) = parse_all(&["--help"]) else {
        panic!("no help")
    };
    for long in build_option_table().long.keys() {
        assert!(
            text.contains(&format!("--{long}")),
            "--{long} missing from help"
        );
    }
    for heading in [
        "Wallpaper:",
        "Performance:",
        "Sound:",
        "Screenshot:",
        "Content:",
        "Wallpaper configuration:",
        "Debug:",
        "Examples:",
    ] {
        assert!(text.contains(heading), "{heading} missing from help");
    }
    assert!(text.contains("stretch") && text.contains("nvdec-copy") && text.contains("overlay"));
    let Ok(CliOutcome::Help(short)) = parse_all(&["-h"]) else {
        panic!("no help")
    };
    assert!(short.contains("--screen-root"));
    assert_eq!(long_help(), text);
}

#[test]
fn help_and_version_win_over_errors() {
    assert!(matches!(
        parse_all(&["--fps", "0", "--help"]),
        Ok(CliOutcome::Help(_))
    ));
    assert!(matches!(
        parse_all(&["-w", "bad", "-h"]),
        Ok(CliOutcome::Help(_))
    ));
    assert!(matches!(parse_all(&["-sh"]), Ok(CliOutcome::Help(_))));
    assert!(matches!(
        parse_all(&["--fps"]),
        Err(CliError::MissingValue(_))
    ));
    let Ok(CliOutcome::Version(v)) = parse_all(&["--version"]) else {
        panic!()
    };
    assert_eq!(v, format!("lumenwall {}\n", env!("CARGO_PKG_VERSION")));
    // As a value, "--help" is not the help option.
    assert_eq!(
        ok(&["--bg", "--help"]).layout.default_wallpaper,
        Some(id("--help"))
    );
}

// --- Defaults and raw arguments ----------------------------------------------------------

#[test]
fn no_arguments() {
    let cli = ok(&[]);
    assert_eq!(cli.raw, [OsString::from("lumenwall")]);
    assert_eq!(cli.layout, CliLayout::default());
    assert_eq!(cli.settings, CliSettings::default());
    assert_eq!(cli.config, ConfigChoice::Default);
    assert!(cli.mentioned.is_empty() && cli.ignored.is_empty());
}

#[test]
fn keeps_raw_arguments() {
    let cli = ok(&["-r", "HDMI-1", "--weird", "123"]);
    let raw: Vec<_> = cli.raw.iter().map(|a| a.to_str().unwrap()).collect();
    assert_eq!(raw, ["lumenwall", "-r", "HDMI-1", "--weird", "123"]);
}

// --- Positional and wallpaper sources (§3.1) -------------------------------------------

#[test]
fn positional_workshop_id_or_path() {
    assert_eq!(
        ok(&["2317494988"]).layout.default_wallpaper,
        Some(id("2317494988"))
    );
    assert_eq!(
        ok(&["mon_fond"]).layout.default_wallpaper,
        Some(id("mon_fond"))
    );
    assert_eq!(
        ok(&["./mon_fond"]).layout.default_wallpaper,
        Some(path("./mon_fond"))
    );
    assert_eq!(
        ok(&["/abs/w"]).layout.default_wallpaper,
        Some(path("/abs/w"))
    );
    let cli = ok(&["2317494988"]);
    assert_eq!(cli.layout.mode, CliMode::Unset);
    assert_eq!(cli.mentioned, [id("2317494988")]);
}

#[test]
fn empty_positional_is_ignored() {
    let cli = ok(&[""]);
    assert_eq!(cli.layout.default_wallpaper, None);
    assert!(cli.mentioned.is_empty());
}

#[test]
fn multiple_positionals_are_an_error() {
    let e = err(&["1", "2"]);
    assert_eq!(
        e,
        CliError::MultiplePositionals {
            first: "1".into(),
            second: "2".into()
        }
    );
    assert!(matches!(
        err(&["", "2"]),
        CliError::MultiplePositionals { .. }
    ));
}

#[test]
fn last_of_positional_and_bg_is_the_default() {
    assert_eq!(
        ok(&["A", "--bg", "B"]).layout.default_wallpaper,
        Some(id("B"))
    );
    assert_eq!(
        ok(&["--bg", "B", "A"]).layout.default_wallpaper,
        Some(id("A"))
    );
    // All of them are still resolved, in order.
    assert_eq!(ok(&["--bg", "B", "A"]).mentioned, [id("B"), id("A")]);
}

// --- Window -----------------------------------------------------------------------------

#[test]
fn window_geometry() {
    let geometry = WindowGeometry {
        x: 10,
        y: -20,
        width: 800,
        height: 600,
    };
    for args in [
        &["-w", "10x-20x800x600"][..],
        &["--window", "10x-20x800x600"],
        &["--window=10x-20x800x600"],
        &["-w10x-20x800x600"],
    ] {
        assert_eq!(ok(args).layout.mode, CliMode::Window(geometry), "{args:?}");
    }
    assert_eq!(
        ok(&["-w", "1ax2x3x4z", "X"]).layout.mode,
        CliMode::Window(WindowGeometry {
            x: 1,
            y: 2,
            width: 3,
            height: 4
        })
    );
}

#[test]
fn window_errors() {
    for bad in ["", "800x600", "1x2x3", "0x0x0x0", "0x0x10x-1"] {
        assert_eq!(invalid_for(&err(&["-w", bad])), "-w", "{bad:?}");
    }
    assert_eq!(
        err(&["-w", "0x0x1x1", "--window", "0x0x2x2"]),
        CliError::WindowTwice
    );
    assert_eq!(
        err(&["-r", "A", "-w", "0x0x1x1"]),
        CliError::WindowAndDesktop
    );
    assert_eq!(
        err(&["--screen-span", "A,B", "-w", "0x0x1x1"]),
        CliError::WindowAndDesktop
    );
    assert_eq!(
        err(&["-w", "0x0x1x1", "-r", "A"]),
        CliError::WindowAndDesktop
    );
    assert_eq!(
        err(&["-w", "0x0x1x1", "--screen-span", "A,B"]),
        CliError::WindowAndDesktop
    );
}

#[test]
fn window_mode_options_are_global() {
    let cli = ok(&[
        "-w",
        "0x0x640x480",
        "--scaling",
        "fit",
        "--clamp",
        "repeat",
        "--bg",
        "X",
        "--playlist",
        "P",
    ]);
    assert_eq!(cli.layout.global_scaling, Some(Scaling::Fit));
    assert_eq!(cli.layout.global_clamp, Some(Clamp::Repeat));
    assert_eq!(cli.layout.default_wallpaper, Some(id("X")));
    assert_eq!(cli.layout.window_playlist.as_deref(), Some("P"));
}

// --- Targeting (§2.3) -------------------------------------------------------------------

#[test]
fn example_single_output() {
    // lumenwall -r HDMI-1 --bg 2317494988 --scaling fill --clamp border
    let cli = ok(&[
        "-r",
        "HDMI-1",
        "--bg",
        "2317494988",
        "--scaling",
        "fill",
        "--clamp",
        "border",
    ]);
    assert_eq!(cli.layout.mode, CliMode::Desktop);
    let s = screen(&cli, "HDMI-1");
    assert_eq!(s.wallpaper, Some(id("2317494988")));
    assert_eq!(s.scaling, Some(Scaling::Fill));
    assert_eq!(s.clamp, Some(Clamp::Border));
    assert_eq!(cli.layout.global_scaling, None);
    assert_eq!(cli.layout.default_wallpaper, Some(id("2317494988")));
}

#[test]
fn example_same_wallpaper_on_two_outputs() {
    // lumenwall -r HDMI-1 -r HDMI-2 2317494988
    let cli = ok(&["-r", "HDMI-1", "-r", "HDMI-2", "2317494988"]);
    assert_eq!(screen(&cli, "HDMI-1").wallpaper, None);
    assert_eq!(screen(&cli, "HDMI-2").wallpaper, None);
    assert_eq!(cli.layout.default_wallpaper, Some(id("2317494988")));
}

#[test]
fn example_one_wallpaper_per_output() {
    // lumenwall -r HDMI-1 --bg A -r HDMI-2 --bg B
    let cli = ok(&["-r", "HDMI-1", "--bg", "A", "-r", "HDMI-2", "--bg", "B"]);
    assert_eq!(screen(&cli, "HDMI-1").wallpaper, Some(id("A")));
    assert_eq!(screen(&cli, "HDMI-2").wallpaper, Some(id("B")));
    assert_eq!(cli.layout.default_wallpaper, Some(id("B")));
    assert_eq!(
        cli.layout
            .screens
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["HDMI-1", "HDMI-2"]
    );
}

#[test]
fn example_span() {
    // lumenwall --screen-span HDMI-1,HDMI-2 --bg 2317494988 --scaling fill
    let cli = ok(&[
        "--screen-span",
        "HDMI-1,HDMI-2",
        "--bg",
        "2317494988",
        "--scaling",
        "fill",
    ]);
    assert_eq!(cli.layout.mode, CliMode::Desktop);
    assert!(cli.layout.screens.is_empty());
    let span = &cli.layout.spans[0];
    assert_eq!(span.key, "span:HDMI-1,HDMI-2");
    assert_eq!(span.outputs, ["HDMI-1", "HDMI-2"]);
    assert_eq!(span.settings.wallpaper, Some(id("2317494988")));
    assert_eq!(span.settings.scaling, Some(Scaling::Fill));
}

#[test]
fn scaling_trap_is_reproduced() {
    // QO-2: the second --scaling targets eDP-1; HDMI-1 copies the global value (stretch).
    let cli = ok(&[
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
    assert_eq!(screen(&cli, "eDP-1").scaling, Some(Scaling::Fill));
    assert_eq!(screen(&cli, "HDMI-1").scaling, Some(Scaling::Stretch));
    assert_eq!(cli.layout.global_scaling, Some(Scaling::Stretch));
    assert_eq!(screen(&cli, "eDP-1").wallpaper, Some(id("X")));
    assert_eq!(screen(&cli, "HDMI-1").wallpaper, Some(id("Y")));
}

#[test]
fn clamp_follows_the_same_rule_with_its_alias() {
    let cli = ok(&[
        "--clamping",
        "border",
        "-r",
        "A",
        "--clamp",
        "repeat",
        "-r",
        "B",
        "--screen-span",
        "C,D",
        "--clamping=clamp",
    ]);
    assert_eq!(screen(&cli, "A").clamp, Some(Clamp::Repeat));
    assert_eq!(screen(&cli, "B").clamp, Some(Clamp::Border));
    assert_eq!(cli.layout.spans[0].settings.clamp, Some(Clamp::Clamp));
    assert_eq!(cli.layout.global_clamp, Some(Clamp::Border));
}

#[test]
fn targets_without_global_values_inherit_later() {
    let cli = ok(&["-r", "A", "--screen-span", "B,C"]);
    assert_eq!(screen(&cli, "A").scaling, None);
    assert_eq!(screen(&cli, "A").clamp, None);
    assert_eq!(cli.layout.spans[0].settings.scaling, None);
}

#[test]
fn bg_without_target_only_sets_the_default() {
    let cli = ok(&["--bg", "X", "-r", "A"]);
    assert_eq!(screen(&cli, "A").wallpaper, None);
    assert_eq!(cli.layout.default_wallpaper, Some(id("X")));
}

#[test]
fn bg_targets_the_last_designated_span() {
    let cli = ok(&[
        "-r",
        "A",
        "--screen-span",
        "B,C",
        "--bg",
        "./w",
        "-r",
        "D",
        "-b",
        "Z",
    ]);
    assert_eq!(screen(&cli, "A").wallpaper, None);
    assert_eq!(cli.layout.spans[0].settings.wallpaper, Some(path("./w")));
    assert_eq!(screen(&cli, "D").wallpaper, Some(id("Z")));
    assert_eq!(cli.layout.default_wallpaper, Some(id("Z")));
}

#[test]
fn playlist_without_target() {
    let cli = ok(&["--playlist", "P"]);
    assert_eq!(cli.layout.window_playlist.as_deref(), Some("P"));
    assert_eq!(cli.layout.default_wallpaper, Some(first("P")));
    assert_eq!(cli.mentioned, [first("P")]);
    // An existing default is kept.
    let cli = ok(&["X", "--playlist", "P"]);
    assert_eq!(cli.layout.default_wallpaper, Some(id("X")));
    assert_eq!(cli.layout.window_playlist.as_deref(), Some("P"));
    // A later --bg replaces the default; the window playlist stays.
    let cli = ok(&["--playlist", "P", "--bg", "Y"]);
    assert_eq!(cli.layout.default_wallpaper, Some(id("Y")));
}

#[test]
fn playlist_with_target() {
    let cli = ok(&[
        "-r",
        "A",
        "--playlist",
        "P",
        "-r",
        "B",
        "--playlist",
        "Q",
        "--screen-span",
        "C,D",
        "--playlist",
        "R",
    ]);
    let a = screen(&cli, "A");
    assert_eq!(
        (a.playlist.as_deref(), a.wallpaper),
        (Some("P"), Some(first("P")))
    );
    let b = screen(&cli, "B");
    assert_eq!(
        (b.playlist.as_deref(), b.wallpaper),
        (Some("Q"), Some(first("Q")))
    );
    assert_eq!(cli.layout.spans[0].settings.playlist.as_deref(), Some("R"));
    // The default only takes the first playlist (it was empty then).
    assert_eq!(cli.layout.default_wallpaper, Some(first("P")));
    assert_eq!(cli.layout.window_playlist, None);
    assert_eq!(cli.mentioned, [first("P"), first("Q"), first("R")]);
}

#[test]
fn bg_after_playlist_overrides_the_target_wallpaper() {
    let cli = ok(&["-r", "A", "--playlist", "P", "--bg", "X"]);
    let a = screen(&cli, "A");
    assert_eq!(a.playlist.as_deref(), Some("P"));
    assert_eq!(a.wallpaper, Some(id("X")));
}

// --- Desktop errors --------------------------------------------------------------------

#[test]
fn screen_root_errors() {
    assert_eq!(
        err(&["-r", "A", "-r", "A"]),
        CliError::DuplicateOutput("A".into())
    );
    assert_eq!(
        err(&["--screen-span", "A,B", "-r", "B"]),
        CliError::OutputInSpan("B".into())
    );
    assert_eq!(invalid_for(&err(&["-r", ""])), "-r");
    assert_eq!(err(&["-r"]), CliError::MissingValue("-r".into()));
    assert_eq!(
        err(&["--screen-root"]),
        CliError::MissingValue("--screen-root".into())
    );
}

#[test]
fn screen_span_errors() {
    assert_eq!(
        err(&["--screen-span", "A"]),
        CliError::SpanTooSmall("A".into())
    );
    assert_eq!(
        err(&["--screen-span", ",A,,"]),
        CliError::SpanTooSmall(",A,,".into())
    );
    assert_eq!(
        err(&["--screen-span", ""]),
        CliError::SpanTooSmall(String::new())
    );
    assert_eq!(
        err(&["--screen-span", "A,B,A"]),
        CliError::DuplicateInSpan {
            group: "A,B,A".into(),
            output: "A".into()
        }
    );
    assert_eq!(
        err(&["-r", "A", "--screen-span", "B,A"]),
        CliError::DuplicateOutput("A".into())
    );
    assert_eq!(
        err(&["--screen-span", "A,B", "--screen-span", "C,B"]),
        CliError::OutputInSpan("B".into())
    );
}

#[test]
fn span_ignores_empty_items_but_keeps_the_raw_key() {
    let cli = ok(&["--screen-span", ",A,,B,"]);
    assert_eq!(cli.layout.spans[0].outputs, ["A", "B"]);
    assert_eq!(cli.layout.spans[0].key, "span:,A,,B,");
}

#[test]
fn several_spans_and_screens() {
    let cli = ok(&["--screen-span", "A,B", "-r", "C", "--screen-span", "D,E,F"]);
    assert_eq!(cli.layout.spans.len(), 2);
    assert_eq!(cli.layout.spans[1].outputs, ["D", "E", "F"]);
    assert_eq!(cli.layout.screens.len(), 1);
}

// --- Value options ----------------------------------------------------------------------

#[test]
fn enumerated_values() {
    for (value, expected) in [
        ("stretch", Scaling::Stretch),
        ("fit", Scaling::Fit),
        ("fill", Scaling::Fill),
        ("default", Scaling::Default),
    ] {
        assert_eq!(
            ok(&["--scaling", value]).layout.global_scaling,
            Some(expected)
        );
    }
    for (value, expected) in [
        ("clamp", Clamp::Clamp),
        ("border", Clamp::Border),
        ("repeat", Clamp::Repeat),
    ] {
        assert_eq!(ok(&["--clamp", value]).layout.global_clamp, Some(expected));
    }
    for (value, expected) in [
        ("background", Layer::Background),
        ("bottom", Layer::Bottom),
        ("top", Layer::Top),
        ("overlay", Layer::Overlay),
    ] {
        assert_eq!(ok(&["--layer", value]).settings.layer, Some(expected));
    }
    for (value, expected) in [
        ("auto", HwDecode::Auto),
        ("nvdec-copy", HwDecode::NvdecCopy),
        ("no", HwDecode::No),
    ] {
        assert_eq!(ok(&["--hwdec", value]).settings.hwdec, Some(expected));
    }
    for option in ["--scaling", "--clamp", "--clamping", "--layer", "--hwdec"] {
        assert_eq!(invalid_for(&err(&[option, "bogus"])), option);
        assert_eq!(invalid_for(&err(&[option, ""])), option);
    }
    // Case-sensitive.
    assert_eq!(invalid_for(&err(&["--scaling", "FILL"])), "--scaling");
    // Last one wins.
    assert_eq!(
        ok(&["--layer", "top", "--layer", "overlay"]).settings.layer,
        Some(Layer::Overlay)
    );
}

#[test]
fn fps() {
    for args in [
        &["-f", "60"][..],
        &["--fps", "60"],
        &["--fps=60"],
        &["-f60"],
        &["-f=60"],
        &["--fps", "+60"],
    ] {
        assert_eq!(ok(args).settings.fps, Some(60), "{args:?}");
    }
    assert_eq!(ok(&["-f", "30", "-f", "144"]).settings.fps, Some(144));
    for bad in ["0", "-5", "abc", "", "60fps", "1.5", "99999999999"] {
        assert_eq!(invalid_for(&err(&["--fps", bad])), "--fps", "{bad:?}");
    }
}

#[test]
fn volume_is_clamped() {
    assert_eq!(ok(&["-v", "50"]).settings.volume, Some(50));
    assert_eq!(ok(&["--volume", "200"]).settings.volume, Some(128));
    assert_eq!(ok(&["--volume", "-5"]).settings.volume, Some(0));
    assert_eq!(ok(&["-v", "-5"]).settings.volume, Some(0));
    assert_eq!(ok(&["-v-5"]).settings.volume, Some(0));
    assert_eq!(ok(&["--volume=0"]).settings.volume, Some(0));
    assert_eq!(
        ok(&["--volume", "99999999999999"]).settings.volume,
        Some(128)
    );
    assert_eq!(invalid_for(&err(&["--volume", "loud"])), "--volume");
}

#[test]
fn volume_and_silent_are_exclusive() {
    assert_eq!(err(&["-v", "10", "-s"]), CliError::VolumeAndSilent);
    assert_eq!(
        err(&["--silent", "--volume", "10"]),
        CliError::VolumeAndSilent
    );
    assert!(ok(&["-s"]).settings.silent);
}

#[test]
fn screenshot_options() {
    let cli = ok(&["--screenshot", "/tmp/shot.png", "--screenshot-delay", "3"]);
    assert_eq!(
        cli.settings.screenshot,
        Some(PathBuf::from("/tmp/shot.png"))
    );
    assert_eq!(cli.settings.screenshot_delay, Some(3));
    assert_eq!(
        ok(&["--screenshot-delay", "50"]).settings.screenshot_delay,
        Some(5)
    );
    assert_eq!(
        ok(&["--screenshot-delay", "0"]).settings.screenshot_delay,
        Some(0)
    );
    assert_eq!(
        invalid_for(&err(&["--screenshot-delay", "-1"])),
        "--screenshot-delay"
    );
    assert_eq!(
        invalid_for(&err(&["--screenshot-delay", "x"])),
        "--screenshot-delay"
    );
    assert_eq!(invalid_for(&err(&["--screenshot", ""])), "--screenshot");
    // The extension is checked when the plan is built (it may come from the config file).
    assert_eq!(
        ok(&["--screenshot", "shot.gif"]).settings.screenshot,
        Some(PathBuf::from("shot.gif"))
    );
}

#[test]
fn assets_dir() {
    assert_eq!(
        ok(&["--assets-dir", "/we/assets"]).settings.assets_dir,
        Some(PathBuf::from("/we/assets"))
    );
    assert_eq!(
        ok(&["--assets-dir=rel"]).settings.assets_dir,
        Some(PathBuf::from("rel"))
    );
    assert_eq!(invalid_for(&err(&["--assets-dir", ""])), "--assets-dir");
}

#[test]
fn flags() {
    let cli = ok(&[
        "--no-fullscreen-pause",
        "--fullscreen-pause-only-active",
        "--silent",
        "--noautomute",
        "--no-audio-processing",
        "--disable-particles",
        "--disable-mouse",
        "--disable-parallax",
        "--list-properties",
        "--dump-structure",
    ]);
    let s = &cli.settings;
    assert!(s.no_fullscreen_pause && s.fullscreen_pause_only_active && s.silent && s.no_automute);
    assert!(s.no_audio_processing && s.disable_particles && s.disable_mouse && s.disable_parallax);
    assert!(s.list_properties && s.dump_structure);
    let short = ok(&["-s", "-l", "-z"]).settings;
    assert!(short.silent && short.list_properties && short.dump_structure);
    let clustered = ok(&["-slz"]).settings;
    assert_eq!(clustered, short);
}

#[test]
fn flags_reject_values() {
    assert_eq!(
        err(&["--silent=1"]),
        CliError::UnexpectedValue("--silent".into())
    );
    assert_eq!(
        err(&["--disable-mouse=yes"]),
        CliError::UnexpectedValue("--disable-mouse".into())
    );
}

#[test]
fn fullscreen_ignore_patterns() {
    let cli = ok(&[
        "--fullscreen-pause-ignore-appid",
        "mpv",
        "--fullscreen-pause-ignore-appid",
        "",
        "--fullscreen-pause-ignore-appid=Firefox",
    ]);
    assert_eq!(
        cli.settings.fullscreen_pause_ignore_appids,
        ["mpv", "Firefox"]
    );
}

#[test]
fn set_property() {
    let cli = ok(&[
        "--set-property",
        "bloom",
        "--set-property",
        "color=1 0 0",
        "--property",
        "expr=a=b",
        "--set-property=empty=",
    ]);
    assert_eq!(
        cli.settings.properties,
        [
            ("bloom".to_owned(), "1".to_owned()),
            ("color".to_owned(), "1 0 0".to_owned()),
            ("expr".to_owned(), "a=b".to_owned()),
            ("empty".to_owned(), String::new()),
        ]
    );
    assert_eq!(
        invalid_for(&err(&["--set-property", "=5"])),
        "--set-property"
    );
    assert_eq!(invalid_for(&err(&["--property", ""])), "--property");
    // Keys are case-sensitive and kept in order; the plan keeps the last duplicate.
    let cli = ok(&[
        "--set-property",
        "A=1",
        "--set-property",
        "a=2",
        "--set-property",
        "A=3",
    ]);
    assert_eq!(cli.settings.properties.len(), 3);
}

#[test]
fn render_debug() {
    let cli = ok(&[
        "--render-debug",
        "base-only",
        "--render-debug=object=12",
        "--render-debug",
        "skip-effect=3",
        "--render-debug",
        "skip-object=4",
        "--render-debug",
        "skip-object=5",
        "--render-debug",
        "pass-log",
        "--render-debug",
        "no-solid-final",
    ]);
    assert_eq!(
        cli.settings.render_debug,
        RenderDebug {
            base_only: true,
            no_solid_final: true,
            pass_log: true,
            only_object: Some(12),
            skip_objects: vec![4, 5],
            skip_effects: vec![3],
        }
    );
    for bad in [
        "wireframe",
        "object=x",
        "skip-object=",
        "skip-effect=-1",
        "object=99999999999",
    ] {
        assert_eq!(
            invalid_for(&err(&["--render-debug", bad])),
            "--render-debug",
            "{bad}"
        );
    }
}

#[test]
fn config_choice() {
    assert_eq!(
        ok(&["--config", "/etc/lumenwall.toml"]).config,
        ConfigChoice::File("/etc/lumenwall.toml".into())
    );
    assert_eq!(ok(&["--no-config"]).config, ConfigChoice::Disabled);
    assert_eq!(
        ok(&["--no-config", "--config=a.toml"]).config,
        ConfigChoice::File("a.toml".into())
    );
    assert_eq!(
        ok(&["--config", "a.toml", "--no-config"]).config,
        ConfigChoice::Disabled
    );
    assert_eq!(invalid_for(&err(&["--config", ""])), "--config");
}

// --- Tokenizer ----------------------------------------------------------------------------

#[test]
fn unknown_arguments_are_ignored() {
    let cli = ok(&[
        "--type=renderer",
        "--unknown",
        "-q",
        "--enable-features=X,Y",
        "-r",
        "A",
        "-qsx",
        "W",
    ]);
    assert_eq!(
        cli.ignored,
        [
            "--type=renderer",
            "--unknown",
            "-q",
            "--enable-features=X,Y",
            "-qx"
        ]
    );
    assert!(cli.settings.silent);
    assert_eq!(screen(&cli, "A").wallpaper, None);
    assert_eq!(cli.layout.default_wallpaper, Some(id("W")));
}

#[test]
fn unknown_option_values_are_positionals() {
    // There is no way to know that an unknown option takes a value.
    assert_eq!(
        ok(&["--unknown", "123"]).layout.default_wallpaper,
        Some(id("123"))
    );
    assert!(matches!(
        err(&["--unknown", "123", "456"]),
        CliError::MultiplePositionals { .. }
    ));
}

#[test]
fn double_dash_ends_options() {
    let cli = ok(&["-s", "--", "--weird-name"]);
    assert!(cli.settings.silent);
    assert_eq!(cli.layout.default_wallpaper, Some(id("--weird-name")));
    assert_eq!(ok(&["--", "-"]).layout.default_wallpaper, Some(id("-")));
}

#[test]
fn single_dash_is_positional() {
    assert_eq!(ok(&["-"]).layout.default_wallpaper, Some(id("-")));
}

#[test]
fn values_may_start_with_a_dash() {
    assert_eq!(ok(&["-r", "-odd-"]).layout.screens[0].name, "-odd-");
    assert_eq!(ok(&["--bg", "-x"]).layout.default_wallpaper, Some(id("-x")));
}

#[test]
fn short_clusters() {
    let cli = ok(&["-sf60", "-rHDMI-1", "-bX"]);
    assert!(cli.settings.silent);
    assert_eq!(cli.settings.fps, Some(60));
    assert_eq!(screen(&cli, "HDMI-1").wallpaper, Some(id("X")));
    let cli = ok(&["-sr", "A"]);
    assert_eq!(cli.layout.screens[0].name, "A");
    assert_eq!(err(&["-sf"]), CliError::MissingValue("-f".into()));
    assert_eq!(err(&["--bg"]), CliError::MissingValue("--bg".into()));
}

#[test]
fn non_unicode_values() {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let raw = OsString::from_vec(b"/w/\xff".to_vec());
        for args in [
            vec![OsString::from("lumenwall"), "--bg".into(), raw.clone()],
            vec![
                "lumenwall".into(),
                OsString::from_vec(b"--bg=/w/\xff".to_vec()),
            ],
            vec![
                "lumenwall".into(),
                OsString::from_vec(b"-b/w/\xff".to_vec()),
            ],
            vec!["lumenwall".into(), raw.clone()],
        ] {
            let Ok(CliOutcome::Run(cli)) = parse(args.clone()) else {
                panic!("{args:?}")
            };
            let Some(WallpaperRef::Source(WallpaperSource::Path(p))) = cli.layout.default_wallpaper
            else {
                panic!("{args:?}")
            };
            assert_eq!(p.as_os_str().as_bytes(), b"/w/\xff");
        }
        let args = vec![
            OsString::from("lumenwall"),
            "-r".into(),
            OsString::from_vec(b"\xff".to_vec()),
        ];
        assert_eq!(parse(args), Err(CliError::NotUnicode("-r".into())));
        // A non-Unicode unknown option is ignored.
        let args = vec![
            OsString::from("lumenwall"),
            OsString::from_vec(b"--\xff".to_vec()),
        ];
        assert!(matches!(parse(args), Ok(CliOutcome::Run(_))));
    }
}

#[test]
fn error_messages() {
    assert_eq!(
        err(&["--fps", "0"]).to_string(),
        "invalid value '0' for '--fps': the frame rate must be a positive integer"
    );
    assert_eq!(
        err(&["--scaling", "zoom"]).to_string(),
        "invalid value 'zoom' for '--scaling': expected one of: stretch, fit, fill, default"
    );
    assert_eq!(
        err(&["-w", "1x2"]).to_string(),
        "invalid value '1x2' for '-w': expected format XxYxWxH"
    );
    assert_eq!(
        err(&["-v", "1", "-s"]).to_string(),
        "'--volume' and '--silent' are mutually exclusive"
    );
}

#[test]
fn full_combination() {
    let cli = ok(&[
        "--scaling",
        "fit",
        "--clamp",
        "border",
        "-f",
        "24",
        "--layer",
        "top",
        "--hwdec",
        "no",
        "-r",
        "eDP-1",
        "--bg",
        "1",
        "--scaling",
        "fill",
        "--screen-span",
        "DP-1,DP-2",
        "--playlist",
        "evening",
        "--clamp",
        "repeat",
        "-r",
        "HDMI-1",
        "--set-property",
        "speed=2",
        "--assets-dir",
        "/a",
        "-v",
        "40",
        "--screenshot",
        "s.jpg",
        "--fullscreen-pause-ignore-appid",
        "mpv",
        "--disable-mouse",
        "fallback",
    ]);
    assert_eq!(cli.layout.mode, CliMode::Desktop);
    assert_eq!(
        screen(&cli, "eDP-1"),
        TargetSettings {
            wallpaper: Some(id("1")),
            playlist: None,
            scaling: Some(Scaling::Fill),
            clamp: Some(Clamp::Border)
        }
    );
    assert_eq!(
        cli.layout.spans[0].settings,
        TargetSettings {
            wallpaper: Some(first("evening")),
            playlist: Some("evening".into()),
            scaling: Some(Scaling::Fit),
            clamp: Some(Clamp::Repeat)
        }
    );
    assert_eq!(
        screen(&cli, "HDMI-1"),
        TargetSettings {
            wallpaper: None,
            playlist: None,
            scaling: Some(Scaling::Fit),
            clamp: Some(Clamp::Border)
        }
    );
    assert_eq!(cli.layout.default_wallpaper, Some(id("fallback")));
    assert_eq!(cli.mentioned, [id("1"), first("evening"), id("fallback")]);
    assert_eq!(cli.settings.fps, Some(24));
    assert_eq!(cli.settings.layer, Some(Layer::Top));
    assert_eq!(cli.settings.hwdec, Some(HwDecode::No));
    assert_eq!(cli.settings.volume, Some(40));
    assert!(cli.settings.disable_mouse);
}
