//! Windows plumbing shared by both backends: the off-screen window, frame pacing and the
//! measurements (process counters, `GPU Process Memory` and `GPU Engine` performance counters).

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use windows::Win32::Foundation::{FILETIME, HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Performance::{
    PDH_FMT_COUNTERVALUE_ITEM_W, PDH_FMT_DOUBLE, PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY,
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW,
};
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX};
use windows::Win32::System::Threading::{
    CreateWaitableTimerExW, GetCurrentProcess, GetProcessTimes, IDLE_PRIORITY_CLASS,
    SetPriorityClass, SetWaitableTimer, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, PM_REMOVE, PeekMessageW,
    RegisterClassExW, SW_SHOWNOACTIVATE, ShowWindow, TranslateMessage, WNDCLASSEXW,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{PCWSTR, w};

pub fn set_idle_priority() {
    // SAFETY: the pseudo-handle of the current process is always valid.
    let _ = unsafe { SetPriorityClass(GetCurrentProcess(), IDLE_PRIORITY_CLASS) };
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // SAFETY: forwards the window's own message.
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

/// A `width`×`height` window below the monitors: nothing visible on the desktop, never
/// activated, absent from the task bar.
pub struct Window {
    pub hwnd: HWND,
}

impl Window {
    pub fn new(width: u32, height: u32) -> Result<Self> {
        // SAFETY: plain Win32 calls with valid arguments; the class name is a static string.
        unsafe {
            let hinstance = GetModuleHandleW(None)?;
            let class = w!("lumenwall_d3d11_probe");
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinstance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&wc);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                class,
                w!("d3d11-probe"),
                WS_POPUP,
                0,
                20_000,
                width as i32,
                height as i32,
                None,
                None,
                Some(hinstance.into()),
                None,
            )?;
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            Ok(Self { hwnd })
        }
    }

    pub fn pump(&self) {
        let mut msg = Default::default();
        // SAFETY: `msg` is a valid out parameter.
        unsafe {
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the window belongs to this thread.
        let _ = unsafe { DestroyWindow(self.hwnd) };
    }
}

/// Frame pacing on a high-resolution waitable timer (no spinning: the CPU is a metric).
pub struct Pacer {
    timer: HANDLE,
    interval: Duration,
    next: Instant,
}

impl Pacer {
    pub fn new(fps: u32) -> Result<Self> {
        // CREATE_WAITABLE_TIMER_HIGH_RESOLUTION = 0x2, TIMER_ALL_ACCESS = 0x1F0003.
        // SAFETY: no pointer arguments.
        let timer = unsafe { CreateWaitableTimerExW(None, None, 0x2, 0x1F_0003)? };
        let interval = Duration::from_secs_f64(1.0 / f64::from(fps));
        Ok(Self {
            timer,
            interval,
            next: Instant::now() + interval,
        })
    }

    /// Sleeps until the next frame time; returns how late the caller was (zero if on time).
    pub fn wait(&mut self) -> Duration {
        let now = Instant::now();
        let late = now.saturating_duration_since(self.next);
        if now < self.next {
            let due = -((self.next - now).as_nanos() as i64 / 100);
            // SAFETY: the timer handle is valid; `due` outlives the call.
            unsafe {
                if SetWaitableTimer(self.timer, &due, 0, None, None, false).is_ok() {
                    WaitForSingleObject(self.timer, 1000);
                }
            }
            self.next += self.interval;
        } else {
            // Behind: do not burst to catch up.
            self.next = now + self.interval;
        }
        late
    }
}

fn filetime_100ns(f: FILETIME) -> u64 {
    (u64::from(f.dwHighDateTime) << 32) | u64::from(f.dwLowDateTime)
}

/// CPU time (user + kernel) of this process.
pub fn cpu_time() -> Duration {
    let (mut c, mut e, mut k, mut u) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: valid out parameters, current process pseudo-handle.
    let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
    if ok.is_err() {
        return Duration::ZERO;
    }
    Duration::from_nanos((filetime_100ns(k) + filetime_100ns(u)) * 100)
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessMemory {
    pub working_set: u64,
    pub private: u64,
}

pub fn process_memory() -> ProcessMemory {
    let mut c = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    // SAFETY: `c` is a valid PROCESS_MEMORY_COUNTERS_EX; the base struct is its prefix.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&raw mut c).cast(),
            std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        )
    };
    if !ok.as_bool() {
        return ProcessMemory::default();
    }
    ProcessMemory {
        working_set: c.WorkingSetSize as u64,
        private: c.PrivateUsage as u64,
    }
}

/// One wildcard performance counter, summed over the instances of this process
/// (`pid_<pid>_...`) whose name contains `filter`.
struct Counter {
    handle: PDH_HCOUNTER,
}

pub struct GpuCounters {
    query: PDH_HQUERY,
    shared: Counter,
    dedicated: Counter,
    engine: Counter,
    pid: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct GpuSample {
    /// Bytes.
    pub shared: u64,
    pub dedicated: u64,
    /// `GPU Engine(...engtype_3D)\Utilization Percentage`, summed over this process's engines.
    pub engine_3d: f64,
}

impl GpuCounters {
    pub fn new() -> Result<Self> {
        let mut query = PDH_HQUERY::default();
        // SAFETY: valid out parameters; counter paths are static wide strings.
        unsafe {
            check(PdhOpenQueryW(PCWSTR::null(), 0, &mut query), "PdhOpenQuery")?;
            let add = |path: PCWSTR| -> Result<Counter> {
                let mut handle = PDH_HCOUNTER::default();
                check(
                    PdhAddEnglishCounterW(query, path, 0, &mut handle),
                    "PdhAddEnglishCounter",
                )?;
                Ok(Counter { handle })
            };
            Ok(Self {
                query,
                shared: add(w!("\\GPU Process Memory(*)\\Shared Usage"))?,
                dedicated: add(w!("\\GPU Process Memory(*)\\Dedicated Usage"))?,
                engine: add(w!("\\GPU Engine(*)\\Utilization Percentage"))?,
                pid: std::process::id(),
            })
        }
    }

    /// Collects the counters; the engine utilization is the mean since the previous `collect`.
    pub fn collect(&self) -> Result<GpuSample> {
        // SAFETY: the query handle is valid.
        check(
            unsafe { PdhCollectQueryData(self.query) },
            "PdhCollectQueryData",
        )?;
        let prefix = format!("pid_{}_", self.pid);
        Ok(GpuSample {
            shared: array_sum(&self.shared, &prefix, "", PDH_FMT_LARGE)? as u64,
            dedicated: array_sum(&self.dedicated, &prefix, "", PDH_FMT_LARGE)? as u64,
            engine_3d: array_sum(&self.engine, &prefix, "engtype_3D", PDH_FMT_DOUBLE)?,
        })
    }
}

impl Drop for GpuCounters {
    fn drop(&mut self) {
        // SAFETY: the query handle is valid.
        let _ = unsafe { PdhCloseQuery(self.query) };
    }
}

fn check(status: u32, what: &str) -> Result<()> {
    if status == 0 {
        Ok(())
    } else {
        Err(anyhow!("{what}: PDH status {status:#x}"))
    }
}

fn array_sum(
    c: &Counter,
    prefix: &str,
    contains: &str,
    format: windows::Win32::System::Performance::PDH_FMT,
) -> Result<f64> {
    let (mut size, mut count) = (0u32, 0u32);
    // First call: PDH_MORE_DATA (0x800007D2) and the needed size.
    // SAFETY: valid out parameters.
    let status =
        unsafe { PdhGetFormattedCounterArrayW(c.handle, format, &mut size, &mut count, None) };
    if status != 0x8000_07D2 {
        // No instance yet (the counters appear with the first GPU use).
        // 0xC0000BBA: a rate counter needs two samples; 0x800007D5: no data yet.
        return if status == 0 || status == 0xC000_0BBA || status == 0x8000_07D5 {
            Ok(0.0)
        } else {
            Err(anyhow!("PdhGetFormattedCounterArray: {status:#x}"))
        };
    }
    let mut buf = vec![0u64; (size as usize).div_ceil(8)];
    let items = buf.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    // SAFETY: `buf` holds `size` bytes, 8-aligned, as the call asked.
    let status = unsafe {
        PdhGetFormattedCounterArrayW(c.handle, format, &mut size, &mut count, Some(items))
    };
    if status == 0xC000_0BBA {
        // A rate counter needs two samples.
        return Ok(0.0);
    }
    check(status, "PdhGetFormattedCounterArray")?;
    let mut sum = 0.0;
    for i in 0..count as usize {
        // SAFETY: `count` items were written at `items`; names point into `buf`.
        let item = unsafe { &*items.add(i) };
        let name = unsafe { item.szName.to_string() }.unwrap_or_default();
        if !name.starts_with(prefix) || !name.contains(contains) {
            continue;
        }
        // SAFETY: the union member matches the requested format.
        sum += unsafe {
            if format == PDH_FMT_DOUBLE {
                item.FmtValue.Anonymous.doubleValue
            } else {
                item.FmtValue.Anonymous.largeValue as f64
            }
        };
    }
    Ok(sum)
}
