//! Opens the wallpaper surfaces and prints what the platform layer reports. No GPU rendering: on Wayland
//! the layer surfaces therefore stay unmapped, elsewhere the windows show whatever the system leaves in them.
//!
//! ```text
//! cargo run -p lumenwall-platform --example surface_info -- [options]
//!   --window[=XxYxWxH]   windowed debug mode (spec 07 §7)
//!   --output NAME        only this output (repeatable, like -r)
//!   --x11 | --wayland    force a Linux backend
//!   --fps N              frame cap (default 30)
//!   --clicks             report mouse buttons too
//!   --no-mouse           no pointer at all
//!   --idle               never request frames: the process must then sit at 0% CPU
//!   --seconds N          quit after N seconds
//! ```

use std::collections::HashMap;
use std::time::{Duration, Instant};

use lumenwall_platform::mouse::PointerTracker;
use lumenwall_platform::{
    BackendChoice, Config, Event, Mode, OutputId, OutputSelection, PointerKind, PointerMode, Wait,
    WindowGeometry,
};

fn parse_geometry(s: &str) -> Option<WindowGeometry> {
    let mut it = s.split('x').map(|v| v.trim().parse::<i64>().ok());
    let (x, y, w, h) = (it.next()??, it.next()??, it.next()??, it.next()??);
    Some(WindowGeometry {
        x: x as i32,
        y: y as i32,
        width: w.max(1) as u32,
        height: h.max(1) as u32,
    })
}

struct Options {
    config: Config,
    idle: bool,
    seconds: Option<u64>,
}

fn parse_args() -> Result<Options, String> {
    let mut config = Config::default();
    let mut names = Vec::new();
    let (mut idle, mut seconds) = (false, None);
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--window" => config.mode = Mode::Window(None),
            a if a.starts_with("--window=") => {
                let g =
                    parse_geometry(&a["--window=".len()..]).ok_or("expected --window=XxYxWxH")?;
                config.mode = Mode::Window(Some(g));
            }
            "--output" => names.push(args.next().ok_or("--output needs a name")?),
            "--x11" => config.backend = BackendChoice::X11,
            "--wayland" => config.backend = BackendChoice::Wayland,
            "--fps" => {
                config.max_fps = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("--fps needs a number")?
            }
            "--clicks" => config.pointer = PointerMode::MotionAndButtons,
            "--no-mouse" => config.pointer = PointerMode::Disabled,
            "--idle" => idle = true,
            "--seconds" => {
                seconds = Some(
                    args.next()
                        .and_then(|v| v.parse().ok())
                        .ok_or("--seconds needs a number")?,
                )
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    if !names.is_empty() {
        config.outputs = OutputSelection::Named(names);
    }
    Ok(Options {
        config,
        idle,
        seconds,
    })
}

#[derive(Default)]
struct Stats {
    frames: u64,
    since: Option<Instant>,
}

fn main() {
    // RUST_LOG=lumenwall_platform=debug (or trace) shows what the backends decide.
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let opts = match parse_args() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    let mut surfaces = match lumenwall_platform::open(&opts.config) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open the wallpaper surfaces: {e}");
            std::process::exit(1);
        }
    };
    println!("backend: {:?}", surfaces.backend());
    println!("display handle: {:?}", surfaces.raw_display_handle());

    if let Some(secs) = opts.seconds {
        let waker = surfaces.waker();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(secs));
            waker.wake();
        });
    }

    let mut tracker = PointerTracker::new(opts.config.pointer != PointerMode::Disabled);
    let mut stats: HashMap<OutputId, Stats> = HashMap::new();
    loop {
        if let Err(e) = surfaces.wait(Wait::Block) {
            eprintln!("platform error: {e}");
            break;
        }
        while let Some(event) = surfaces.next_event() {
            match event {
                Event::OutputAdded(id) => {
                    if let Some(o) = surfaces.output(id) {
                        println!("output added {id}: {o:#?}");
                    }
                    println!("  window handle: {:?}", surfaces.raw_window_handle(id));
                    stats.insert(id, Stats::default());
                }
                Event::OutputRemoved(id) => {
                    println!("output removed {id}");
                    stats.remove(&id);
                    tracker.remove_output(id);
                }
                Event::Frame(id) => {
                    let s = stats.entry(id).or_default();
                    s.frames += 1;
                    let since = *s.since.get_or_insert_with(Instant::now);
                    let elapsed = since.elapsed();
                    if elapsed >= Duration::from_secs(5) {
                        let size = surfaces.output(id).map_or((0, 0), |o| o.physical_size);
                        let p = tracker.state(id, size);
                        println!(
                            "output {id}: {:.1} frames/s, pointer (GL) = ({:.0}, {:.0}) left={} right={}",
                            s.frames as f64 / elapsed.as_secs_f64(),
                            p.x,
                            p.y,
                            p.left,
                            p.right
                        );
                        s.frames = 0;
                        s.since = Some(Instant::now());
                    }
                    if !opts.idle {
                        // A renderer would draw here, then call pre_present + present.
                        surfaces.request_frame(id);
                    }
                }
                Event::Pointer(p) => {
                    let height = surfaces
                        .output(p.output)
                        .map_or(0.0, |o| o.physical_height());
                    tracker.handle(&p, height);
                    if !matches!(p.kind, PointerKind::Motion) {
                        println!(
                            "pointer {}: {:?} at ({:.0}, {:.0})",
                            p.output, p.kind, p.x, p.y
                        );
                    }
                }
                Event::Wake => {
                    println!("time is up");
                    return;
                }
                Event::CloseRequested => {
                    println!("stop requested by the driver");
                    return;
                }
                other => println!("{other:?}"),
            }
        }
    }
}
