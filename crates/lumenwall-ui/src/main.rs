//! Desktop application to browse, apply and configure wallpapers.
//!
//! ```text
//! lumenwall-ui                 open the window
//! lumenwall-ui --tray          tray icon only (no window, no GPU)
//! lumenwall-ui --background    start the daemon if needed, then the tray (used by autostart)
//! ```
//! Developer options: `--fake-daemon`, `--demo-library`, `--page library|screens|settings`,
//! `--select <title>`, `--lang en|fr`, `--screenshot <file.png>`, `--size <W>x<H>`.

// Release builds on Windows are GUI programs: no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod catalog;
mod chrome;
mod config;
mod daemon;
mod demo;
mod fake;
mod i18n;
mod icons;
mod picker;
mod platform;
mod previews;
mod theme;
mod thumbs;
mod tray;
mod updater;
mod view;
mod workshop;

use std::cell::RefCell;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use crate::app::{App, Launch, Page};
use crate::config::{LangPref, UiConfig};
use crate::i18n::Lang;

#[derive(Default)]
struct Args {
    tray: bool,
    background: bool,
    fake_daemon: bool,
    demo_library: bool,
    bench_animate: bool,
    fake_steam: bool,
    ws_select: Option<String>,
    no_key: bool,
    screenshot_delay: Option<f32>,
    page: Option<Page>,
    select: Option<String>,
    search: Option<String>,
    lang: Option<LangPref>,
    screenshot: Option<PathBuf>,
    size: Option<[f32; 2]>,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or(format!("{name} needs a value"));
        match arg.as_str() {
            "--tray" => a.tray = true,
            "--background" => a.background = true,
            "--fake-daemon" => a.fake_daemon = true,
            "--demo-library" => a.demo_library = true,
            "--bench-animate" => a.bench_animate = true,
            "--fake-steam" => a.fake_steam = true,
            "--ws-select" => a.ws_select = Some(value("--ws-select")?),
            "--no-key" => a.no_key = true,
            "--screenshot-delay" => {
                a.screenshot_delay = Some(
                    value("--screenshot-delay")?
                        .parse()
                        .map_err(|_| "bad delay")?,
                )
            }
            "--page" => {
                a.page = Some(match value("--page")?.as_str() {
                    "library" => Page::Library,
                    "screens" => Page::Screens,
                    "settings" => Page::Settings,
                    "workshop" => Page::Workshop,
                    other => return Err(format!("unknown page {other}")),
                })
            }
            "--select" => a.select = Some(value("--select")?),
            "--search" => a.search = Some(value("--search")?),
            "--lang" => {
                a.lang = Some(match value("--lang")?.as_str() {
                    "fr" => LangPref::Fr,
                    "en" => LangPref::En,
                    _ => LangPref::System,
                })
            }
            "--screenshot" => a.screenshot = Some(PathBuf::from(value("--screenshot")?)),
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v.split_once('x').ok_or("--size wants WxH")?;
                a.size = Some([
                    w.parse().map_err(|_| "bad width")?,
                    h.parse().map_err(|_| "bad height")?,
                ]);
            }
            "--version" | "-V" => {
                println!("lumenwall-ui {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "--help" | "-h" => {
                println!("{}", include_str!("help.txt"));
                std::process::exit(0);
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    Ok(a)
}

fn main() -> ExitCode {
    // `LUMENWALL_UI_RENDERER=software` forces the CPU renderer (tiny-skia); by default
    // iced tries wgpu and falls back to tiny-skia when no GPU adapter works.
    if let Ok(r) = std::env::var("LUMENWALL_UI_RENDERER")
        && std::env::var_os("ICED_BACKEND").is_none()
    {
        let backend = if r == "software" || r == "tiny-skia" {
            "tiny-skia"
        } else {
            "wgpu"
        };
        // SAFETY: first statement of main, before any thread exists.
        unsafe { std::env::set_var("ICED_BACKEND", backend) };
    }
    let filter =
        tracing_subscriber::EnvFilter::try_from_env("LUMENWALL_LOG").unwrap_or_else(|_| {
            tracing_subscriber::EnvFilter::new("warn,lumenwall_ui=info,lumenwall_ipc=info")
        });
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("lumenwall-ui: {e} (see --help)");
            return ExitCode::from(2);
        }
    };
    let lang = Lang::resolve(args.lang.unwrap_or(UiConfig::load().language));
    let endpoint = lumenwall_ipc::Endpoint::for_current_user();

    if args.background || args.tray {
        if args.background {
            ensure_daemon(&endpoint);
        }
        return match tray::run(lang, endpoint) {
            Ok(()) => ExitCode::SUCCESS,
            // Without a tray host the daemon keeps running; the app works without the tray.
            Err(tray::TrayError::NoHost) if args.background => {
                tracing::info!("no system tray; the engine keeps running without an icon");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("lumenwall-ui: {e}");
                ExitCode::FAILURE
            }
        };
    }
    run_window(args)
}

/// Starts the daemon when nothing answers on its endpoint.
fn ensure_daemon(endpoint: &lumenwall_ipc::Endpoint) {
    if lumenwall_ipc::Client::connect(endpoint, "lumenwall-ui").is_ok() {
        return;
    }
    match platform::find_daemon() {
        Some(exe) => {
            if let Err(e) = platform::spawn_daemon(&exe) {
                tracing::error!("cannot start {}: {e}", exe.display());
            }
        }
        None => tracing::error!("the {} program was not found", platform::DAEMON_EXE),
    }
}

fn run_window(args: Args) -> ExitCode {
    let mut launch = Launch {
        page: args.page,
        select: args.select,
        search: args.search,
        screenshot: args.screenshot,
        lang: args.lang,
        bench_animate: args.bench_animate,
        ws_select: args.ws_select,
        screenshot_delay: args
            .screenshot_delay
            .map(std::time::Duration::from_secs_f32),
        ..Launch::default()
    };
    let mut playing = Vec::new();
    if args.demo_library {
        let root = config::cache_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("demo-library");
        match demo::ensure(&root) {
            Ok(ws) => {
                playing = vec![ws.join("demo-00"), ws.join("demo-07")];
                launch.discovery = Some(Arc::new(demo::DemoDiscovery { root }));
            }
            Err(e) => tracing::error!("demo library: {e}"),
        }
    }
    if args.fake_daemon {
        match fake::start(&playing) {
            Ok((server, endpoint)) => {
                launch.endpoint = Some(endpoint);
                launch.keep_alive = Some(Box::new(server));
            }
            Err(e) => tracing::error!("fake daemon: {e}"),
        }
    }

    if args.fake_steam {
        match workshop::mock::start() {
            Ok(mock) => {
                workshop::api::use_mock_server(mock.base.clone());
                launch.workshop_key =
                    Some((!args.no_key).then(|| workshop::mock::MOCK_KEY.to_owned()));
                launch.keep_alive = Some(Box::new((launch.keep_alive.take(), mock.hits)));
            }
            Err(e) => tracing::error!("fake Steam: {e}"),
        }
    } else if args.no_key {
        launch.workshop_key = Some(None);
    }
    let font = theme::init_font(&installed_families());
    let size = args
        .size
        .or(UiConfig::load().window)
        .unwrap_or([1360.0, 860.0]);
    let window = iced::window::Settings {
        size: iced::Size::new(size[0], size[1]),
        min_size: Some(iced::Size::new(980.0, 620.0)),
        icon: icons::window_icon(),
        exit_on_close_request: false,
        // Linux: the app draws its own title bar and frame; see-through for rounded
        // corners only where they come out clear (chrome.rs).
        decorations: !chrome::DRAWN,
        transparent: chrome::transparent(),
        // Wayland app id and X11 WM_CLASS: the desktop entry
        // `packaging/linux/lumenwall-ui.desktop` (name and icon in docks).
        #[cfg(target_os = "linux")]
        platform_specific: iced::window::settings::PlatformSpecific {
            application_id: "lumenwall-ui".into(),
            ..Default::default()
        },
        ..Default::default()
    };
    let launch = RefCell::new(Some(launch));
    let result = iced::application(
        move || App::new(launch.borrow_mut().take().unwrap_or_default()),
        App::update,
        App::view,
    )
    .title(App::title)
    .theme(App::theme)
    .style(App::style)
    .subscription(App::subscription)
    .default_font(font)
    .window(window)
    .antialiasing(true)
    .run();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lumenwall-ui: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Font families installed on the system with their upright, normal-width
/// weights, as iced's text engine sees them.
fn installed_families() -> Vec<(String, Vec<u16>)> {
    use std::collections::BTreeMap;
    let system = iced::advanced::graphics::text::font_system();
    let Ok(mut guard) = system.write() else {
        return Vec::new();
    };
    let mut map: BTreeMap<String, Vec<u16>> = BTreeMap::new();
    for face in guard.raw().db().faces() {
        if face.style != iced::advanced::graphics::text::cosmic_text::fontdb::Style::Normal
            || face.stretch != iced::advanced::graphics::text::cosmic_text::fontdb::Stretch::Normal
        {
            continue;
        }
        for (name, _) in &face.families {
            map.entry(name.clone()).or_default().push(face.weight.0);
        }
    }
    map.into_iter().collect()
}
