//! Renders wallpapers off screen and writes PNG files.
//!
//! ```text
//! cargo run -p lumenwall-engine --example render_png -- [options] <id-or-path>...
//!   --size WxH         output size (default 1920x1080)
//!   --frames N         frames simulated (default 1)
//!   --fps F            simulated frame rate (default 30)
//!   --start T          scene time of the first frame in seconds (default 1)
//!   --audio L          constant audio spectrum level in every band (default 0)
//!   --save a,b,c       frames written (default: the last one)
//!   --out DIR          output folder (default target/agent-integration/shots)
//!   --set name=value   property override (repeatable)
//!   --assets DIR       Wallpaper Engine assets folder (default: Steam install)
//!   --software         CPU adapter
//!   --scaling MODE     stretch | fit | fill | default (default: default)
//!   --hwdec MODE       video decoding: auto | nvdec-copy | no (default: auto)
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use lumenwall_engine::{HeadlessOptions, WallpaperResolver, render_headless};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let mut opts = HeadlessOptions {
        out_dir: PathBuf::from("target/agent-integration/shots"),
        ..HeadlessOptions::default()
    };
    let mut assets: Option<PathBuf> = None;
    let mut wallpapers = Vec::new();
    let mut props = BTreeMap::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().unwrap_or_default();
        match a.as_str() {
            "--size" => {
                let v = val();
                let (w, h) = v.split_once('x').unwrap_or(("1920", "1080"));
                opts.width = w.parse().unwrap_or(1920);
                opts.height = h.parse().unwrap_or(1080);
            }
            "--frames" => opts.frames = val().parse().unwrap_or(1),
            "--fps" => opts.fps = val().parse().unwrap_or(30.0),
            "--start" => opts.start_time = val().parse().unwrap_or(1.0),
            "--audio" => opts.audio_level = val().parse().unwrap_or(0.0),
            "--save" => {
                opts.save = val()
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
            }
            "--out" => opts.out_dir = PathBuf::from(val()),
            "--set" => {
                let v = val();
                let (k, v) = v.split_once('=').unwrap_or((&v, "1"));
                props.insert(k.to_owned(), v.to_owned());
            }
            "--assets" => assets = Some(PathBuf::from(val())),
            "--software" => opts.software = true,
            "--no-particles" => opts.particles = false,
            "--scaling" => {
                opts.scaling = match val().as_str() {
                    "stretch" => lumenwall_engine::ScalingMode::Stretch,
                    "fit" => lumenwall_engine::ScalingMode::Fit,
                    "fill" => lumenwall_engine::ScalingMode::Fill,
                    _ => lumenwall_engine::ScalingMode::Default,
                }
            }
            "--hwdec" => {
                opts.hwdec = match val().as_str() {
                    "no" => lumenwall_engine::plan::HwDecode::No,
                    "nvdec-copy" => lumenwall_engine::plan::HwDecode::NvdecCopy,
                    _ => lumenwall_engine::plan::HwDecode::Auto,
                }
            }
            other => wallpapers.push(other.to_owned()),
        }
    }
    opts.properties = props;
    let resolver = WallpaperResolver::new();
    let assets = match resolver.assets_dir(assets.as_deref()) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("assets folder: {e}");
            std::process::exit(1);
        }
    };
    let mut failures = 0;
    for w in &wallpapers {
        let path = match resolver.resolve(w) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("{w}: {e}");
                failures += 1;
                continue;
            }
        };
        let mut o = opts.clone();
        o.prefix = std::path::Path::new(w)
            .file_name()
            .map_or_else(|| "wallpaper".into(), |n| n.to_string_lossy().into_owned());
        match render_headless(&path, &assets, &o) {
            Ok(r) => println!(
                "{w}: OK {}x{} on {} | load {:.0} ms | {} image(s), {} pass(es), {} particle system(s) ({} live), {} texture(s) {:.1} MiB, {} stand-in shader(s), {} skipped object(s), {} puppet(s) ({} GPU-skinned, {} attached object(s)), {} 3D object(s) ({} mesh(es)) | CPU {:.2} ms/frame, GPU {} | GPU memory {:.1} MiB | {}",
                r.scene_size.0,
                r.scene_size.1,
                r.adapter,
                r.load_time.as_secs_f64() * 1000.0,
                r.stats.images,
                r.stats.passes,
                r.stats.particle_systems,
                r.live_particles,
                r.stats.textures,
                r.stats.texture_bytes as f64 / 1048576.0,
                r.stats.stand_in_shaders,
                r.stats.skipped_objects,
                r.stats.puppets,
                r.stats.gpu_skinned_puppets,
                r.stats.attached_objects,
                r.stats.models,
                r.stats.meshes,
                r.cpu_frame.as_secs_f64() * 1000.0,
                r.gpu_frame.map_or("n/a".into(), |g| format!(
                    "{:.2} ms/frame",
                    g.as_secs_f64() * 1000.0
                )),
                r.gpu_memory as f64 / 1048576.0,
                r.pngs
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            Err(e) => {
                println!("{w}: FAILED: {e}");
                failures += 1;
            }
        }
    }
    if failures > 0 {
        std::process::exit(1);
    }
}
