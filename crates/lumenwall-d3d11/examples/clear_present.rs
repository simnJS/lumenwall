//! Clear, present and read back on an off-screen window, with the process's GPU memory read after
//! each step: `GPU Process Memory` performance counters (what Task Manager shows as dedicated and
//! shared GPU memory) and `QueryVideoMemoryInfo` (local / non-local).
//!
//! ```text
//! cargo run --release -p lumenwall-d3d11 --example clear_present -- [--warp] [--seconds 10] [--size 2560x1440]
//! ```
//!
//! On the hardware adapter it takes `%LOCALAPPDATA%\lumenwall-dev\gpu.lock` (and refuses to start
//! when another GPU user holds it), runs at Idle priority, paced at 30 fps, at most 60 s, in a
//! window below the monitors. The README's "Measurements" come from this program.

#[cfg(not(windows))]
fn main() {
    eprintln!("Windows only");
}

#[cfg(windows)]
fn main() {
    if let Err(e) = win::run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

#[cfg(windows)]
mod win {
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use lumenwall_d3d11::windows::Win32::System::Performance::{
        PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY,
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
        PdhOpenQueryW,
    };
    use lumenwall_d3d11::windows::Win32::System::Threading::{
        GetCurrentProcess, IDLE_PRIORITY_CLASS, SetPriorityClass,
    };
    use lumenwall_d3d11::windows::core::{PCWSTR, w};
    use lumenwall_d3d11::*;

    const MIB: f64 = 1024.0 * 1024.0;

    /// `%LOCALAPPDATA%\lumenwall-dev\gpu.lock`, created exclusively, removed on drop.
    struct GpuLock(PathBuf);

    impl GpuLock {
        fn take() -> std::result::Result<Self, String> {
            let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA").ok_or("no LOCALAPPDATA")?)
                .join("lumenwall-dev");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let path = dir.join("gpu.lock");
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|e| format!("{} is held by another GPU user ({e})", path.display()))?;
            use std::io::Write;
            let _ = writeln!(f, "Luna lumenwall-d3d11 clear_present pid {}", std::process::id());
            Ok(Self(path))
        }
    }

    impl Drop for GpuLock {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// `GPU Process Memory(pid_<pid>_*)\{Shared,Dedicated} Usage`, summed over this process.
    struct Counters {
        query: PDH_HQUERY,
        shared: PDH_HCOUNTER,
        dedicated: PDH_HCOUNTER,
        prefix: String,
    }

    impl Counters {
        fn new() -> std::result::Result<Self, String> {
            let mut query = PDH_HQUERY::default();
            let (mut shared, mut dedicated) = (PDH_HCOUNTER::default(), PDH_HCOUNTER::default());
            // SAFETY: live out-parameters; static wide counter paths.
            unsafe {
                if PdhOpenQueryW(PCWSTR::null(), 0, &mut query) != 0
                    || PdhAddEnglishCounterW(query, w!("\\GPU Process Memory(*)\\Shared Usage"), 0, &mut shared) != 0
                    || PdhAddEnglishCounterW(query, w!("\\GPU Process Memory(*)\\Dedicated Usage"), 0, &mut dedicated)
                        != 0
                {
                    return Err("PDH counters unavailable".into());
                }
            }
            Ok(Self {
                query,
                shared,
                dedicated,
                prefix: format!("pid_{}_", std::process::id()),
            })
        }

        /// `(shared, dedicated)` bytes.
        fn read(&self) -> (u64, u64) {
            // SAFETY: valid query handle.
            unsafe { PdhCollectQueryData(self.query) };
            (self.sum(self.shared), self.sum(self.dedicated))
        }

        fn sum(&self, counter: PDH_HCOUNTER) -> u64 {
            let (mut size, mut count) = (0u32, 0u32);
            // SAFETY: first call asks for the size (PDH_MORE_DATA).
            let status =
                unsafe { PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &mut size, &mut count, None) };
            if status != 0x8000_07D2 {
                return 0; // no instance of this process yet
            }
            let mut buf = vec![0u64; (size as usize).div_ceil(8)];
            let items = buf.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
            // SAFETY: `buf` holds `size` 8-aligned bytes as requested.
            if unsafe { PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &mut size, &mut count, Some(items)) }
                != 0
            {
                return 0;
            }
            let mut total = 0u64;
            for i in 0..count as usize {
                // SAFETY: `count` items were written; their names point into `buf`.
                let item = unsafe { &*items.add(i) };
                // SAFETY: as above.
                let name = unsafe { item.szName.to_string() }.unwrap_or_default();
                if name.starts_with(&self.prefix) {
                    // SAFETY: the union member of PDH_FMT_LARGE.
                    total += unsafe { item.FmtValue.Anonymous.largeValue } as u64;
                }
            }
            total
        }
    }

    impl Drop for Counters {
        fn drop(&mut self) {
            // SAFETY: valid query handle.
            unsafe { PdhCloseQuery(self.query) };
        }
    }

    fn stage(name: &str, counters: &Counters, device: Option<&Device>) {
        let (shared, dedicated) = counters.read();
        let qvmi = device.and_then(|d| d.memory().ok());
        println!(
            "| {name} | {:.2} | {:.2} | {} | {} |",
            shared as f64 / MIB,
            dedicated as f64 / MIB,
            qvmi.map_or("–".into(), |m| format!("{:.2}", m.non_local.usage as f64 / MIB)),
            qvmi.map_or("–".into(), |m| format!("{:.2}", m.local.usage as f64 / MIB)),
        );
    }

    fn colour(frame: u32) -> [u8; 4] {
        // Exact UNORM values (multiples of 51) that change every frame.
        let v = [0u8, 51, 102, 153, 204, 255];
        [v[(frame % 6) as usize], v[((frame / 6) % 6) as usize], 102, 255]
    }

    fn clear(device: &Device, swap: &SwapChain, rgba: [u8; 4]) {
        let ctx = device.context();
        // SAFETY: the swap chain's view on its own device.
        unsafe {
            ctx.OMSetRenderTargets(Some(&[Some(swap.rtv().clone())]), None);
            ctx.ClearRenderTargetView(swap.rtv(), &rgba.map(|c| f32::from(c) / 255.0));
        }
    }

    fn check(device: &Device, swap: &SwapChain, rgba: [u8; 4]) -> std::result::Result<(), String> {
        let (w, h, px) = read_texture_rgba8(device, swap.back_buffer()).map_err(|e| e.to_string())?;
        let bad = px.as_chunks::<4>().0.iter().filter(|p| **p != rgba).count();
        println!("readback {w}×{h}: {} pixels, {bad} differ from {rgba:?}", w * h);
        if bad == 0 { Ok(()) } else { Err(format!("{bad} wrong pixels")) }
    }

    pub fn run() -> std::result::Result<(), String> {
        let mut warp = false;
        let mut seconds = 10u64;
        let mut size = (2560u32, 1440u32);
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "--warp" => warp = true,
                "--seconds" => seconds = args.next().and_then(|s| s.parse().ok()).ok_or("--seconds N")?,
                "--size" => {
                    let s = args.next().ok_or("--size WxH")?;
                    let (w, h) = s.split_once('x').ok_or("--size WxH")?;
                    size = (w.parse().map_err(|_| "--size")?, h.parse().map_err(|_| "--size")?);
                }
                other => return Err(format!("unknown argument {other}")),
            }
        }
        let seconds = seconds.clamp(1, 60);
        // SAFETY: the current process's pseudo-handle.
        let _ = unsafe { SetPriorityClass(GetCurrentProcess(), IDLE_PRIORITY_CLASS) };
        let _lock = if warp { None } else { Some(GpuLock::take()?) };
        let counters = Counters::new()?;

        println!("| Step | Shared (counters) MiB | Dedicated (counters) MiB | Non-local (QVMI) MiB | Local (QVMI) MiB |");
        println!("|---|---|---|---|---|");
        stage("start", &counters, None);
        let t = Instant::now();
        let device = Device::new(&DeviceOptions {
            adapter: if warp { AdapterChoice::Warp } else { AdapterChoice::Default },
            ..DeviceOptions::default()
        })
        .map_err(|e| e.to_string())?;
        let device_ms = t.elapsed().as_secs_f64() * 1000.0;
        stage("device", &counters, Some(&device));
        let window = OffscreenWindow::new(size.0, size.1).map_err(|e| e.to_string())?;
        let t = Instant::now();
        let swap = SwapChain::for_hwnd(&device, window.hwnd(), size).map_err(|e| e.to_string())?;
        let swap_ms = t.elapsed().as_secs_f64() * 1000.0;
        stage("swap chain", &counters, Some(&device));

        // Frame 1: clear, read back, present.
        clear(&device, &swap, colour(0));
        check(&device, &swap, colour(0))?;
        let mut statuses = [0u32; 2];
        let mut count = |s: PresentStatus| statuses[usize::from(s == PresentStatus::Occluded)] += 1;
        count(swap.present(&device).map_err(|e| e.to_string())?);
        stage("frame 1 (with readback)", &counters, Some(&device));

        let interval = Duration::from_secs_f64(1.0 / 30.0);
        let mut next = Instant::now() + interval;
        let frames = (seconds * 30) as u32;
        let mut timer = GpuTimer::new(&device).map_err(|e| e.to_string())?;
        let mut gpu_total = Duration::ZERO;
        let mut gpu_samples = 0u32;
        for frame in 1..frames {
            window.pump();
            timer.begin(&device).map_err(|e| e.to_string())?;
            clear(&device, &swap, colour(frame));
            timer.end(&device);
            count(swap.present(&device).map_err(|e| e.to_string())?);
            if let Some(t) = timer.poll(&device).map_err(|e| e.to_string())? {
                gpu_total += t;
                gpu_samples += 1;
            }
            match frame {
                29 => stage("30 frames", &counters, Some(&device)),
                30 => {
                    device.trim().map_err(|e| e.to_string())?;
                    stage("Trim", &counters, Some(&device));
                }
                60 => stage("30 frames after Trim", &counters, Some(&device)),
                _ => {}
            }
            let now = Instant::now();
            if now < next {
                std::thread::sleep(next - now);
                next += interval;
            } else {
                next = now + interval;
            }
        }
        stage(&format!("end ({frames} frames)"), &counters, Some(&device));
        clear(&device, &swap, colour(frames));
        check(&device, &swap, colour(frames))?;
        println!(
            "adapter: {} ({:#x}); device {device_ms:.1} ms, swap chain {swap_ms:.1} ms; presents: {} shown, {} occluded; GPU time per frame (clear): {}",
            device.info().name,
            device.caps().feature_level,
            statuses[0],
            statuses[1],
            if gpu_samples > 0 {
                format!("{:.3} ms over {gpu_samples} frames", gpu_total.as_secs_f64() * 1000.0 / f64::from(gpu_samples))
            } else {
                "–".into()
            }
        );
        Ok(())
    }
}
