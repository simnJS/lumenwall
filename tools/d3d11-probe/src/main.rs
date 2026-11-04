//! Direct3D 11 renderer probe: the same scene workload on D3D11 and on Vulkan (wgpu), every metric
//! measured. See README.md.

mod common;
mod d3d11;
mod plan;
mod shaders;
mod sys;
mod vulkan;

use std::path::PathBuf;

use anyhow::{Result, anyhow};

const DEFAULT_STEAM: &str = r"C:\Program Files (x86)\Steam\steamapps";

struct Args {
    cmd: String,
    wallpapers: Vec<String>,
    workshop: PathBuf,
    assets: PathBuf,
    cache: PathBuf,
    backend: String,
    seconds: f64,
    warmup: f64,
    fps: u32,
    clear_cache: bool,
}

fn parse_args() -> Result<Args> {
    let mut it = std::env::args().skip(1);
    let cmd = it
        .next()
        .ok_or_else(|| anyhow!("usage: d3d11-probe <plan|shaders> [--wallpaper ID]..."))?;
    let mut a = Args {
        cmd,
        wallpapers: Vec::new(),
        workshop: std::env::var_os("LUMENWALL_WE_WORKSHOP")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_STEAM).join(r"workshop\content\431960")),
        assets: std::env::var_os("LUMENWALL_WE_ASSETS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(DEFAULT_STEAM).join(r"common\wallpaper_engine\assets")
            }),
        cache: std::env::temp_dir().join("d3d11-probe-cache"),
        backend: "d3d11".into(),
        seconds: 20.0,
        warmup: 5.0,
        fps: 30,
        clear_cache: false,
    };
    while let Some(arg) = it.next() {
        let mut value = |name: &str| it.next().ok_or_else(|| anyhow!("{name} needs a value"));
        match arg.as_str() {
            "--wallpaper" => a.wallpapers.push(value("--wallpaper")?),
            "--workshop" => a.workshop = value("--workshop")?.into(),
            "--assets" => a.assets = value("--assets")?.into(),
            "--cache" => a.cache = value("--cache")?.into(),
            "--backend" => a.backend = value("--backend")?,
            "--seconds" => a.seconds = value("--seconds")?.parse()?,
            "--warmup" => a.warmup = value("--warmup")?.parse()?,
            "--fps" => a.fps = value("--fps")?.parse()?,
            "--clear-cache" => a.clear_cache = true,
            other => return Err(anyhow!("unknown argument {other}")),
        }
    }
    if a.wallpapers.is_empty() {
        a.wallpapers = vec!["3463520581".into(), "3018340632".into()];
    }
    Ok(a)
}

fn main() -> Result<()> {
    let args = parse_args()?;
    match args.cmd.as_str() {
        "plan" => cmd_plan(&args),
        "run" => cmd_run(&args),
        "shaders" => cmd_shaders(&args),
        "hlsl" => {
            let w = load(&args, &args.wallpapers[0], None)?;
            let n: usize = std::env::var("PROGRAM")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            println!("// {}", w.program_names[n]);
            println!(
                "{}",
                shaders::to_hlsl(&w.programs[n].program.module)?.source
            );
            Ok(())
        }
        other => Err(anyhow!("unknown command {other}")),
    }
}

fn load(args: &Args, id: &str, shader_cache: Option<PathBuf>) -> Result<plan::Workload> {
    let dir = args.workshop.join(id);
    plan::build(
        &dir,
        &args.assets,
        &plan::PlanOptions { shader_cache },
        &mut |m| eprintln!("  {m}"),
    )
}

fn cmd_plan(args: &Args) -> Result<()> {
    for id in &args.wallpapers {
        let w = load(args, id, None)?;
        println!("== {id}");
        println!(
            "textures {} ({:.1} MiB), targets {} ({:.1} MiB), programs {}, draws {}, render passes {}",
            w.textures.len(),
            w.texture_bytes() as f64 / 1048576.0,
            w.targets.len(),
            w.target_bytes() as f64 / 1048576.0,
            w.programs.len(),
            w.draws.len(),
            w.render_passes()
        );
        println!("  shaders: {}", w.program_names.join(", "));
        for t in &w.targets {
            println!(
                "  target {:>5}x{:<5} {:?} {}",
                t.width, t.height, t.format, t.label
            );
        }
    }
    Ok(())
}

#[cfg(windows)]
fn cmd_shaders(args: &Args) -> Result<()> {
    use std::time::Instant;
    let cache = shaders::BytecodeCache::new(&args.cache.join("dxbc"))?;
    cache.clear()?;
    for id in &args.wallpapers {
        let w = load(args, id, None)?;
        let n = w.programs.len();
        println!(
            "== {id}: {} programs translated by lumenwall-shader ({} failed) in {:.0} ms, {} draws",
            n,
            w.shader_errors.len(),
            w.translate_ms,
            w.draws.len()
        );
        for (name, e) in &w.shader_errors {
            println!("  translate FAILED {name}: {e}");
        }
        let (mut ok, mut failed) = (0usize, 0usize);
        let (mut hlsl_ms, mut cold_ms) = (0.0, 0.0);
        let mut bytes = 0usize;
        let mut codes = Vec::new();
        for (i, p) in w.programs.iter().enumerate() {
            let t = Instant::now();
            match shaders::to_hlsl(&p.program.module) {
                Err(e) => {
                    failed += 1;
                    println!("  hlsl FAILED {}: {e}", w.program_names[i]);
                    continue;
                }
                Ok(h) => {
                    hlsl_ms += t.elapsed().as_secs_f64() * 1000.0;
                    let t = Instant::now();
                    match shaders::compile(&p.program.module) {
                        Ok((code, _)) => {
                            cold_ms += t.elapsed().as_secs_f64() * 1000.0;
                            bytes += code.vs.len() + code.ps.len();
                            cache.store(&p.key, &code)?;
                            codes.push(i);
                            ok += 1;
                        }
                        Err(e) => {
                            failed += 1;
                            println!("  D3DCompile FAILED {}: {e}", w.program_names[i]);
                            let _ = h;
                        }
                    }
                }
            }
        }
        let t = Instant::now();
        let mut loaded = 0usize;
        for i in &codes {
            if cache.load(&w.programs[*i].key).is_some() {
                loaded += 1;
            }
        }
        let warm_ms = t.elapsed().as_secs_f64() * 1000.0;
        println!(
            "  HLSL SM5.0: {ok}/{n} compile ({failed} fail); naga->HLSL {hlsl_ms:.0} ms, D3DCompile cold {cold_ms:.0} ms (total {:.0} ms), bytecode {:.1} KiB; cached load of {loaded} programs {warm_ms:.1} ms",
            hlsl_ms + cold_ms,
            bytes as f64 / 1024.0
        );
    }
    Ok(())
}

#[cfg(not(windows))]
fn cmd_shaders(_: &Args) -> Result<()> {
    Err(anyhow!("Windows only"))
}

/// Times of the resource creation, filled by [`Backend::load`].
#[derive(Default)]
pub struct LoadTimes {
    pub upload_ms: f64,
    pub targets_ms: f64,
    pub shaders_ms: f64,
    pub compile_ms: f64,
    pub pipelines_ms: f64,
}

/// A renderer the probe can run the workload on.
pub trait Backend: Sized {
    /// Device + swap chain on `window`: the backend, the time it took, the adapter's name.
    fn create(window: &sys::Window) -> Result<(Self, f64, String)>;
    /// Textures (freed from the workload once uploaded), targets, shaders, states.
    fn load(
        &mut self,
        w: &mut plan::Workload,
        cache: &shaders::BytecodeCache,
        mark: &mut dyn FnMut(&str),
    ) -> Result<LoadTimes>;
    /// One frame: constants, every render pass, the copy to the swap chain, `Present`.
    fn frame(&mut self, w: &plan::Workload, time: f32) -> Result<()>;
}

#[cfg(windows)]
fn cmd_run(args: &Args) -> Result<()> {
    let report = match args.backend.as_str() {
        "d3d11" => run::<d3d11::D3d11>(args)?,
        "vulkan" => run::<vulkan::Vulkan>(args)?,
        other => return Err(anyhow!("unknown backend {other} (d3d11 or vulkan)")),
    };
    print_report(&report);
    Ok(())
}

#[cfg(not(windows))]
fn cmd_run(_: &Args) -> Result<()> {
    Err(anyhow!("Windows only"))
}

#[cfg(windows)]
fn run<B: Backend>(args: &Args) -> Result<common::Report> {
    use std::time::{Duration, Instant};

    sys::set_idle_priority();
    let id = &args.wallpapers[0];
    let cache = shaders::BytecodeCache::new(&args.cache.join("dxbc"))?;
    if args.clear_cache {
        cache.clear()?;
    }
    let mut w = load(args, id, None)?;
    let mut r = common::Report {
        backend: args.backend.clone(),
        wallpaper: id.clone(),
        draws: w.draws.len(),
        render_passes: w.render_passes(),
        ..Default::default()
    };
    let counters = sys::GpuCounters::new()?;
    let window = sys::Window::new(plan::OUTPUT.0, plan::OUTPUT.1)?;
    let (mut backend, create_ms, adapter) = B::create(&window)?;
    window.pump();
    r.create_ms = create_ms;
    r.adapter = adapter;
    std::thread::sleep(Duration::from_millis(500));
    let s = counters.collect()?;
    r.shared_device = s.shared;
    r.dedicated_device = s.dedicated;

    let stage = |name: &str| {
        if let Ok(s) = counters.collect() {
            println!(
                "STAGE {name} shared_mib={:.2} dedicated_mib={:.1}",
                common::mib(s.shared),
                common::mib(s.dedicated)
            );
        }
    };
    stage("device");
    let times = backend.load(&mut w, &cache, &mut |n| stage(n))?;
    r.upload_ms = times.upload_ms;
    r.targets_ms = times.targets_ms;
    r.shaders_ms = times.shaders_ms;
    r.compile_ms = times.compile_ms;
    r.pipelines_ms = times.pipelines_ms;

    let mut pacer = sys::Pacer::new(args.fps)?;
    let start = Instant::now();
    let (mut frames, mut late) = (0u64, 0u64);
    let mut window_start: Option<(Instant, Duration, u64, u64)> = None;
    loop {
        window.pump();
        backend.frame(&w, start.elapsed().as_secs_f32())?;
        frames += 1;
        if frames == 1 || frames == 30 {
            stage(&format!("frame{frames}"));
        }
        let elapsed = start.elapsed().as_secs_f64();
        if window_start.is_none() && elapsed >= args.warmup {
            counters.collect()?;
            window_start = Some((Instant::now(), sys::cpu_time(), frames, late));
        }
        if let Some((t0, cpu0, f0, l0)) = window_start
            && elapsed >= args.warmup + args.seconds
        {
            let cpu = sys::cpu_time().saturating_sub(cpu0);
            let wall = t0.elapsed();
            let s = counters.collect()?;
            let mem = sys::process_memory();
            r.frames = frames - f0;
            r.late_frames = late - l0;
            r.cpu_percent = cpu.as_secs_f64() / wall.as_secs_f64() * 100.0;
            r.gpu_3d_percent = s.engine_3d;
            r.shared = s.shared;
            r.dedicated = s.dedicated;
            r.private = mem.private;
            r.working_set = mem.working_set;
            break;
        }
        if pacer.wait() > Duration::from_millis(5) {
            late += 1;
        }
    }
    Ok(r)
}

fn print_report(r: &common::Report) {
    use common::mib;
    println!(
        "RESULT backend={} wallpaper={} adapter=\"{}\" draws={} render_passes={} frames={} late_frames={} \
         cpu_percent={:.2} gpu_3d_percent={:.2} shared_mib={:.2} dedicated_mib={:.1} private_mib={:.1} \
         working_set_mib={:.1} shared_device_mib={:.2} dedicated_device_mib={:.1} create_ms={:.1} \
         upload_ms={:.1} targets_ms={:.1} shaders_ms={:.1} compile_ms={:.1} pipelines_ms={:.1}",
        r.backend,
        r.wallpaper,
        r.adapter,
        r.draws,
        r.render_passes,
        r.frames,
        r.late_frames,
        r.cpu_percent,
        r.gpu_3d_percent,
        mib(r.shared),
        mib(r.dedicated),
        mib(r.private),
        mib(r.working_set),
        mib(r.shared_device),
        mib(r.dedicated_device),
        r.create_ms,
        r.upload_ms,
        r.targets_ms,
        r.shaders_ms,
        r.compile_ms,
        r.pipelines_ms
    );
}
