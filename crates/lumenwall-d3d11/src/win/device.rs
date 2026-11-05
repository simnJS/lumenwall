//! Adapter choice and device creation: feature level 11_0 or 11_1, a Direct3D 11.1 immediate
//! context (constant buffer offsets), device-loss detection and `Trim`.

use std::ffi::c_void;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1,
};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_DEBUG, D3D11_CREATE_DEVICE_FLAG,
    D3D11_CREATE_DEVICE_SINGLETHREADED, D3D11_FEATURE_D3D11_OPTIONS,
    D3D11_FEATURE_DATA_D3D11_OPTIONS, D3D11_QUERY_DESC, D3D11_QUERY_EVENT, D3D11_SDK_VERSION,
    D3D11CreateDevice, ID3D11Asynchronous, ID3D11Device, ID3D11Device1, ID3D11DeviceContext,
    ID3D11DeviceContext1, ID3D11Query,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT;
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE, DXGI_GPU_PREFERENCE,
    DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE, DXGI_GPU_PREFERENCE_MINIMUM_POWER, IDXGIAdapter,
    IDXGIAdapter1, IDXGIAdapter3, IDXGIDevice1, IDXGIDevice3, IDXGIFactory2, IDXGIFactory4,
    IDXGIFactory6,
};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::core::{BOOL, Interface};

use crate::error::{Context, Error, LostReason, Result, hresult, out};

/// Locally unique identifier of an adapter (`DXGI_ADAPTER_DESC1::AdapterLuid`): how the video
/// decoder, the browser's shared textures and the device are matched to the same GPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Luid {
    pub low: u32,
    pub high: i32,
}

impl From<LUID> for Luid {
    fn from(l: LUID) -> Self {
        Self {
            low: l.LowPart,
            high: l.HighPart,
        }
    }
}

impl From<Luid> for LUID {
    fn from(l: Luid) -> Self {
        Self {
            LowPart: l.low,
            HighPart: l.high,
        }
    }
}

/// Which adapter the device is created on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AdapterChoice {
    /// DXGI's first hardware adapter (the one driving the primary display), the software
    /// adapter when there is no other (Windows Sandbox, a broken driver).
    #[default]
    Default,
    /// Windows' "high performance" GPU (per-app graphics settings honoured).
    HighPerformance,
    /// Windows' "power saving" GPU.
    MinimumPower,
    /// The adapter with this LUID (the one used last time, the decoder's, the browser's).
    Luid(Luid),
    /// The adapter that drives this monitor (the first output of the wallpaper), what the
    /// wgpu path does by filtering on the first surface.
    Monitor(isize),
    /// WARP, the software rasteriser: tests, and the last resort when no driver works.
    Warp,
}

/// Device creation options.
#[derive(Clone, Copy, Debug)]
pub struct DeviceOptions {
    pub adapter: AdapterChoice,
    /// The Direct3D debug layer (`D3D11_CREATE_DEVICE_DEBUG`); ignored with a warning when the
    /// SDK layers are not installed.
    pub debug: bool,
    /// `D3D11_CREATE_DEVICE_SINGLETHREADED`: no internal locking. Off when another thread uses
    /// the device (FFmpeg's D3D11VA decoder: `ID3D10Multithread` protection instead).
    pub single_threaded: bool,
    /// Frames the CPU may queue ahead of the GPU (`IDXGIDevice1::SetMaximumFrameLatency`), 1 by
    /// default: the wallpaper paces itself, nothing is gained by queueing.
    pub max_frame_latency: u32,
}

impl Default for DeviceOptions {
    fn default() -> Self {
        Self {
            adapter: AdapterChoice::Default,
            debug: false,
            single_threaded: false,
            max_frame_latency: 1,
        }
    }
}

/// What the adapter is.
#[derive(Clone, Debug)]
pub struct AdapterInfo {
    pub name: String,
    pub luid: Luid,
    pub vendor_id: u32,
    pub device_id: u32,
    pub dedicated_video_memory: u64,
    pub shared_system_memory: u64,
    /// WARP or the basic render driver.
    pub software: bool,
}

/// What the device can do beyond feature level 11_0.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    /// `0xb000` (11_0), `0xb100` (11_1)…
    pub feature_level: u32,
    /// `*SetConstantBuffers1` honours offsets (required: always true on a created [`Device`]).
    pub constant_buffer_offsetting: bool,
    /// `Map(WRITE_NO_OVERWRITE)` on dynamic constant buffers.
    pub map_no_overwrite_constant_buffers: bool,
}

/// A Direct3D 11 device and its immediate context.
pub struct Device {
    device: ID3D11Device1,
    context: ID3D11DeviceContext1,
    adapter: IDXGIAdapter1,
    factory: IDXGIFactory2,
    info: AdapterInfo,
    caps: Caps,
    idle_query: ID3D11Query,
}

impl Device {
    /// Creates the device (feature level 11_1, else 11_0) on the chosen adapter.
    pub fn new(options: &DeviceOptions) -> Result<Self> {
        // SAFETY: plain factory creation, no arguments.
        let factory: IDXGIFactory2 = unsafe { CreateDXGIFactory1() }.ctx("CreateDXGIFactory1")?;
        let adapter = pick_adapter(&factory, options.adapter)?;
        let info = adapter_info(&adapter)?;
        let mut flags = D3D11_CREATE_DEVICE_BGRA_SUPPORT;
        if options.single_threaded {
            flags |= D3D11_CREATE_DEVICE_SINGLETHREADED;
        }
        let created = if options.debug {
            match create(&adapter, flags | D3D11_CREATE_DEVICE_DEBUG) {
                Err(Error::Call {
                    code: hresult::DXGI_ERROR_SDK_COMPONENT_MISSING,
                    ..
                }) => {
                    tracing::warn!(
                        "Direct3D debug layer not installed; creating the device without it"
                    );
                    create(&adapter, flags)
                }
                r => r,
            }
        } else {
            create(&adapter, flags)
        };
        let (device, context, level) = created?;
        let device: ID3D11Device1 = device
            .cast()
            .map_err(|_| Error::Unsupported("Direct3D 11.1 runtime (ID3D11Device1)".into()))?;
        let context: ID3D11DeviceContext1 = context.cast().map_err(|_| {
            Error::Unsupported("Direct3D 11.1 context (ID3D11DeviceContext1)".into())
        })?;
        let mut opts = D3D11_FEATURE_DATA_D3D11_OPTIONS::default();
        // SAFETY: `opts` is the structure this feature fills, with its exact size.
        unsafe {
            device.CheckFeatureSupport(
                D3D11_FEATURE_D3D11_OPTIONS,
                (&raw mut opts).cast::<c_void>(),
                size_of::<D3D11_FEATURE_DATA_D3D11_OPTIONS>() as u32,
            )
        }
        .ctx("CheckFeatureSupport")?;
        let caps = Caps {
            feature_level: level.0 as u32,
            constant_buffer_offsetting: opts.ConstantBufferOffsetting.as_bool(),
            map_no_overwrite_constant_buffers: opts.MapNoOverwriteOnDynamicConstantBuffer.as_bool(),
        };
        if !caps.constant_buffer_offsetting {
            return Err(Error::Unsupported(
                "constant buffer offsetting (driver without Direct3D 11.1 support)".into(),
            ));
        }
        if let Ok(dxgi) = device.cast::<IDXGIDevice1>() {
            // SAFETY: plain setter on a live device.
            let _ = unsafe { dxgi.SetMaximumFrameLatency(options.max_frame_latency.clamp(1, 16)) };
        }
        let mut idle_query = None;
        // SAFETY: valid descriptor and out-parameter.
        unsafe {
            device.CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                },
                Some(&mut idle_query),
            )
        }
        .ctx("CreateQuery")?;
        tracing::debug!(
            "Direct3D 11 device on {} (feature level {:#x})",
            info.name,
            caps.feature_level
        );
        Ok(Self {
            device,
            context,
            adapter,
            factory,
            info,
            caps,
            idle_query: out(idle_query, "CreateQuery")?,
        })
    }

    #[must_use]
    pub fn raw(&self) -> &ID3D11Device1 {
        &self.device
    }

    /// The immediate context. Single-threaded use only (the render thread).
    #[must_use]
    pub fn context(&self) -> &ID3D11DeviceContext1 {
        &self.context
    }

    #[must_use]
    pub fn adapter(&self) -> &IDXGIAdapter1 {
        &self.adapter
    }

    #[must_use]
    pub fn factory(&self) -> &IDXGIFactory2 {
        &self.factory
    }

    #[must_use]
    pub fn info(&self) -> &AdapterInfo {
        &self.info
    }

    #[must_use]
    pub fn caps(&self) -> Caps {
        self.caps
    }

    /// `Err(DeviceLost)` once the device is gone (`GetDeviceRemovedReason`).
    pub fn check_lost(&self) -> Result<()> {
        match self.removed_reason() {
            None => Ok(()),
            Some(reason) => Err(Error::DeviceLost {
                call: "GetDeviceRemovedReason",
                reason,
            }),
        }
    }

    fn removed_reason(&self) -> Option<LostReason> {
        // SAFETY: no arguments.
        let r = unsafe { self.device.GetDeviceRemovedReason() };
        r.err()
            .map(|e| LostReason::from_hresult(e.code().0).unwrap_or(LostReason::Other(e.code().0)))
    }

    /// The error of a failed call, with the device's own removal reason when it is gone.
    pub(crate) fn error(&self, call: &'static str, e: &windows::core::Error) -> Error {
        match Error::from_hresult(call, e.code().0) {
            Error::DeviceLost { reason, .. } => Error::DeviceLost {
                call,
                reason: self.removed_reason().unwrap_or(reason),
            },
            other => other,
        }
    }

    /// Submits the queued commands.
    pub fn flush(&self) {
        // SAFETY: no arguments.
        unsafe { self.context.Flush() };
    }

    /// Lets the driver release the memory it keeps for this process (staging of uploads,
    /// internal buffers): `ClearState`, `Flush`, `IDXGIDevice3::Trim`. Call once after every
    /// large load (tools/d3d11-probe: shared GPU memory 2.9–3.2 → 1.2–2.0 MiB, loading peak
    /// 13–16 → 0.8–0.9 MiB). Bindings are cleared: the next frame binds everything again.
    pub fn trim(&self) -> Result<()> {
        let dxgi: IDXGIDevice3 = self
            .device
            .cast()
            .map_err(|_| Error::Unsupported("IDXGIDevice3 (Windows 8.1)".into()))?;
        // SAFETY: no arguments; Microsoft's documentation asks for ClearState before Trim.
        unsafe {
            self.context.ClearState();
            self.context.Flush();
            dxgi.Trim();
        }
        self.check_lost()
    }

    /// Waits until the GPU has run every command submitted so far (load time, readbacks,
    /// shutdown; never per frame). `Ok(false)` on timeout.
    pub fn wait_idle(&self, timeout: Duration) -> Result<bool> {
        // SAFETY: the query belongs to this device.
        unsafe { self.context.End(&self.idle_query) };
        self.flush();
        let deadline = Instant::now() + timeout;
        loop {
            let mut done = BOOL(0);
            if get_data(&self.context, &self.idle_query, &mut done, 0)
                .map_err(|e| self.error("GetData", &e))?
                && done.as_bool()
            {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// MSAA quality levels for `format` at `samples` (0 = unsupported).
    #[must_use]
    pub fn msaa_quality_levels(&self, format: DXGI_FORMAT, samples: u32) -> u32 {
        // SAFETY: plain query.
        unsafe { self.device.CheckMultisampleQualityLevels(format, samples) }.unwrap_or(0)
    }

    /// `IDXGIAdapter3` for memory queries.
    pub(crate) fn adapter3(&self) -> Result<IDXGIAdapter3> {
        self.adapter
            .cast()
            .map_err(|_| Error::Unsupported("IDXGIAdapter3 (Windows 10)".into()))
    }
}

/// `GetData` with its real status: `Ok(true)` when the data was written (`S_OK`), `Ok(false)`
/// while the GPU has not reached the query (`S_FALSE`, which the `windows` wrapper folds into
/// success).
pub(crate) fn get_data<T>(
    context: &ID3D11DeviceContext1,
    query: &impl Interface,
    data: &mut T,
    flags: u32,
) -> windows::core::Result<bool> {
    let query: ID3D11Asynchronous = query.cast()?;
    let ctx: &ID3D11DeviceContext = context;
    // SAFETY: calls the vtable entry of a live context with a live query and a destination of
    // exactly `size_of::<T>()` writable bytes; the HRESULT is returned as is.
    let hr = unsafe {
        (Interface::vtable(ctx).GetData)(
            Interface::as_raw(ctx),
            Interface::as_raw(&query),
            (data as *mut T).cast::<c_void>(),
            size_of::<T>() as u32,
            flags,
        )
    };
    hr.ok()?;
    Ok(hr.0 == 0)
}

fn create(
    adapter: &IDXGIAdapter1,
    flags: D3D11_CREATE_DEVICE_FLAG,
) -> Result<(ID3D11Device, ID3D11DeviceContext, D3D_FEATURE_LEVEL)> {
    let adapter: IDXGIAdapter = adapter.cast().ctx("IDXGIAdapter")?;
    let attempt = |levels: &[D3D_FEATURE_LEVEL]| {
        let (mut device, mut context, mut level) = (None, None, D3D_FEATURE_LEVEL::default());
        // SAFETY: valid adapter, flags and feature level slice; out-parameters are live locals.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                flags,
                Some(levels),
                D3D11_SDK_VERSION,
                Some(&mut device),
                Some(&mut level),
                Some(&mut context),
            )
        }
        .map(|()| (device, context, level))
    };
    // A runtime that does not know 11_1 refuses the whole list with E_INVALIDARG.
    let r = attempt(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]).or_else(|e| {
        if e.code().0 == hresult::E_INVALIDARG {
            attempt(&[D3D_FEATURE_LEVEL_11_0])
        } else {
            Err(e)
        }
    });
    let (device, context, level) = r.ctx("D3D11CreateDevice")?;
    Ok((
        out(device, "D3D11CreateDevice")?,
        out(context, "D3D11CreateDevice")?,
        level,
    ))
}

fn adapter_info(adapter: &IDXGIAdapter1) -> Result<AdapterInfo> {
    // SAFETY: plain query.
    let d = unsafe { adapter.GetDesc1() }.ctx("GetDesc1")?;
    Ok(AdapterInfo {
        name: String::from_utf16_lossy(&d.Description)
            .trim_end_matches('\0')
            .to_owned(),
        luid: d.AdapterLuid.into(),
        vendor_id: d.VendorId,
        device_id: d.DeviceId,
        dedicated_video_memory: d.DedicatedVideoMemory as u64,
        shared_system_memory: d.SharedSystemMemory as u64,
        software: d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0,
    })
}

fn is_software(adapter: &IDXGIAdapter1) -> bool {
    adapter_info(adapter).is_ok_and(|i| i.software)
}

fn pick_adapter(factory: &IDXGIFactory2, choice: AdapterChoice) -> Result<IDXGIAdapter1> {
    let not_found = |what: &str| Error::Unsupported(format!("no adapter: {what}"));
    match choice {
        AdapterChoice::Default => {
            let mut software = None;
            for i in 0.. {
                // SAFETY: plain enumeration; DXGI_ERROR_NOT_FOUND ends it.
                let Ok(a) = (unsafe { factory.EnumAdapters1(i) }) else {
                    break;
                };
                if !is_software(&a) {
                    return Ok(a);
                }
                software.get_or_insert(a);
            }
            software.ok_or_else(|| not_found("no adapter at all"))
        }
        AdapterChoice::HighPerformance => {
            by_preference(factory, DXGI_GPU_PREFERENCE_HIGH_PERFORMANCE)
        }
        AdapterChoice::MinimumPower => by_preference(factory, DXGI_GPU_PREFERENCE_MINIMUM_POWER),
        AdapterChoice::Luid(luid) => {
            let f4: IDXGIFactory4 = factory.cast().ctx("IDXGIFactory4")?;
            // SAFETY: plain lookup.
            unsafe { f4.EnumAdapterByLuid(luid.into()) }
                .map_err(|_| not_found(&format!("LUID {:08x}:{:08x}", luid.high, luid.low)))
        }
        AdapterChoice::Monitor(monitor) => {
            let monitor = HMONITOR(monitor as *mut c_void);
            for i in 0.. {
                // SAFETY: plain enumeration.
                let Ok(a) = (unsafe { factory.EnumAdapters1(i) }) else {
                    break;
                };
                for j in 0.. {
                    // SAFETY: plain enumeration.
                    let Ok(output) = (unsafe { a.EnumOutputs(j) }) else {
                        break;
                    };
                    // SAFETY: plain query.
                    if unsafe { output.GetDesc() }.is_ok_and(|d| d.Monitor == monitor) {
                        return Ok(a);
                    }
                }
            }
            Err(not_found("no adapter drives this monitor"))
        }
        AdapterChoice::Warp => {
            let f4: IDXGIFactory4 = factory.cast().ctx("IDXGIFactory4")?;
            // SAFETY: plain lookup.
            unsafe { f4.EnumWarpAdapter() }.ctx("EnumWarpAdapter")
        }
    }
}

fn by_preference(factory: &IDXGIFactory2, pref: DXGI_GPU_PREFERENCE) -> Result<IDXGIAdapter1> {
    let f6: IDXGIFactory6 = factory
        .cast()
        .map_err(|_| Error::Unsupported("IDXGIFactory6 (Windows 10 1803)".into()))?;
    // SAFETY: plain enumeration.
    unsafe { f6.EnumAdapterByGpuPreference(0, pref) }.ctx("EnumAdapterByGpuPreference")
}
