//! GPU instance, adapter, device and queue: one device for every output.
//!
//! Backends: Vulkan on Linux; Vulkan then DX12 on Windows. The device asks for
//! `MemoryHints::MemoryUsage`, BC texture compression when available, and the
//! on-disk pipeline cache on Vulkan.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use crate::error::{RenderError, Result};
use crate::texture::{AddressMode, SamplerKey};

/// Backends compiled in and tried, in order of preference.
#[must_use]
pub fn default_backends() -> wgpu::Backends {
    if cfg!(windows) {
        wgpu::Backends::VULKAN | wgpu::Backends::DX12
    } else {
        wgpu::Backends::VULKAN
    }
}

/// Context creation options.
#[derive(Clone, Debug)]
pub struct ContextOptions {
    /// Backends to consider; `None` = [`default_backends`].
    pub backends: Option<wgpu::Backends>,
    /// Adapter preference. `None` keeps the driver order (first discrete GPU
    /// in practice, which is usually the one driving the displays).
    pub power_preference: wgpu::PowerPreference,
    /// Prefer a CPU adapter (lavapipe / WARP): deterministic tests.
    pub prefer_software: bool,
    /// Directory of the wgpu pipeline cache (Vulkan only). `None` disables it.
    pub pipeline_cache_dir: Option<PathBuf>,
    /// Enable timestamp queries for the debug GPU frame timer.
    pub profiling: bool,
    /// Create the device with Vulkan Video decode queues when the adapter allows it
    /// (zero-copy video wallpapers, see `video_device`). Only set when a video is loaded.
    pub video_decode: bool,
}

impl Default for ContextOptions {
    fn default() -> Self {
        Self {
            backends: None,
            power_preference: wgpu::PowerPreference::None,
            prefer_software: false,
            pipeline_cache_dir: None,
            profiling: false,
            video_decode: false,
        }
    }
}

/// Optional device capabilities Lumenwall uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuCaps {
    /// BC1/BC2/BC3 uploads are possible.
    pub bc: bool,
    /// `ClampToBorder` samplers are possible (render targets with flag 8).
    pub clamp_to_border: bool,
    /// Depth clipping can be disabled per pipeline.
    pub unclipped_depth: bool,
    /// Pipeline cache available (Vulkan).
    pub pipeline_cache: bool,
    /// Timestamps can be written anywhere in a command encoder.
    pub timestamps: bool,
}

/// Bytes of GPU memory allocated by lumenwall-render (textures, targets, buffers).
#[derive(Clone, Debug, Default)]
pub struct MemoryCounter(Arc<AtomicU64>);

impl MemoryCounter {
    /// Register an allocation; the returned token releases it on drop.
    #[must_use]
    pub fn track(&self, bytes: u64) -> MemoryToken {
        self.0.fetch_add(bytes, Ordering::Relaxed);
        MemoryToken {
            counter: self.0.clone(),
            bytes,
        }
    }

    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// Released share of a [`MemoryCounter`].
#[derive(Debug)]
pub struct MemoryToken {
    counter: Arc<AtomicU64>,
    bytes: u64,
}

impl MemoryToken {
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for MemoryToken {
    fn drop(&mut self) {
        self.counter.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// Rank of an adapter: lower is better. Pure, for testing.
#[must_use]
pub fn adapter_rank(
    backend: wgpu::Backend,
    device_type: wgpu::DeviceType,
    preference: wgpu::PowerPreference,
    prefer_software: bool,
) -> (u8, u8) {
    use wgpu::DeviceType as D;
    let backend_rank = match backend {
        wgpu::Backend::Vulkan => 0,
        wgpu::Backend::Dx12 => 1,
        _ => 2,
    };
    let type_rank = if prefer_software {
        match device_type {
            D::Cpu => 0,
            _ => 1,
        }
    } else {
        match (preference, device_type) {
            (_, D::Cpu) => 4,
            (wgpu::PowerPreference::LowPower, D::IntegratedGpu) => 0,
            (wgpu::PowerPreference::LowPower, D::DiscreteGpu) => 1,
            (wgpu::PowerPreference::HighPerformance, D::DiscreteGpu) => 0,
            (wgpu::PowerPreference::HighPerformance, D::IntegratedGpu) => 1,
            (wgpu::PowerPreference::None, D::DiscreteGpu | D::IntegratedGpu) => 0,
            (_, D::VirtualGpu) => 2,
            _ => 3,
        }
    };
    (backend_rank, type_rank)
}

/// Instance wrapper used before the device exists, to create the surfaces the
/// adapter must be compatible with.
pub struct GpuInstance {
    instance: wgpu::Instance,
    backends: wgpu::Backends,
}

impl GpuInstance {
    #[must_use]
    pub fn new(backends: Option<wgpu::Backends>) -> Self {
        let backends = backends.unwrap_or_else(default_backends);
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        Self { instance, backends }
    }

    /// Create a presentation surface from raw-window-handle 0.6 handles.
    ///
    /// # Safety
    ///
    /// The handles must stay valid until the returned surface is dropped.
    pub unsafe fn create_surface(
        &self,
        display: RawDisplayHandle,
        window: RawWindowHandle,
    ) -> Result<wgpu::Surface<'static>> {
        let target = wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(display),
            raw_window_handle: window,
        };
        // SAFETY: forwarded to the caller.
        Ok(unsafe { self.instance.create_surface_unsafe(target) }?)
    }

    /// Pick the adapter and create the device. Every surface in `surfaces`
    /// must be presentable by the chosen adapter (one device for all screens).
    pub fn into_context(
        self,
        options: &ContextOptions,
        surfaces: &[&wgpu::Surface<'_>],
    ) -> Result<GpuContext> {
        let adapters = pollster::block_on(self.instance.enumerate_adapters(self.backends));
        let adapter = adapters
            .into_iter()
            .filter(|a| surfaces.iter().all(|s| a.is_surface_supported(s)))
            .min_by_key(|a| {
                let info = a.get_info();
                adapter_rank(
                    info.backend,
                    info.device_type,
                    options.power_preference,
                    options.prefer_software,
                )
            })
            .ok_or(RenderError::NoAdapter(self.backends))?;
        GpuContext::from_adapter(self.instance, adapter, options)
    }
}

/// The GPU context shared by every scene and output.
pub struct GpuContext {
    instance: wgpu::Instance,
    adapter: wgpu::Adapter,
    device: wgpu::Device,
    queue: wgpu::Queue,
    caps: GpuCaps,
    pipeline_cache: Option<wgpu::PipelineCache>,
    pipeline_cache_path: Option<PathBuf>,
    memory: MemoryCounter,
    resources: AtomicU64,
    /// Shared samplers by `(key, volume)`.
    samplers: Mutex<HashMap<(SamplerKey, bool), wgpu::Sampler>>,
    video: Option<crate::video_device::VideoQueues>,
}

impl GpuContext {
    /// Context without any surface (headless rendering, tests, tools).
    pub fn new_headless(options: &ContextOptions) -> Result<Self> {
        GpuInstance::new(options.backends).into_context(options, &[])
    }

    fn from_adapter(
        instance: wgpu::Instance,
        adapter: wgpu::Adapter,
        options: &ContextOptions,
    ) -> Result<Self> {
        let available = adapter.features();
        let mut wanted = wgpu::Features::TEXTURE_COMPRESSION_BC
            | wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER
            | wgpu::Features::DEPTH_CLIP_CONTROL;
        if options.pipeline_cache_dir.is_some() {
            wanted |= wgpu::Features::PIPELINE_CACHE;
        }
        if options.profiling {
            wanted |=
                wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        }
        if options.video_decode {
            // Plane views of decoded NV12 images (zero-copy video).
            wanted |= wgpu::Features::TEXTURE_FORMAT_NV12;
        }
        let features = available & wanted;
        let caps = GpuCaps {
            bc: features.contains(wgpu::Features::TEXTURE_COMPRESSION_BC),
            clamp_to_border: features.contains(wgpu::Features::ADDRESS_MODE_CLAMP_TO_BORDER),
            unclipped_depth: features.contains(wgpu::Features::DEPTH_CLIP_CONTROL),
            pipeline_cache: features.contains(wgpu::Features::PIPELINE_CACHE),
            timestamps: features.contains(
                wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
            ),
        };
        let info = adapter.get_info();
        let desc = wgpu::DeviceDescriptor {
            label: Some("lumenwall.device"),
            required_features: features,
            // Large spans (3 × 4K) need the adapter's real texture limits.
            required_limits: adapter.limits(),
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            memory_hints: wgpu::MemoryHints::MemoryUsage,
            trace: wgpu::Trace::Off,
        };
        let video_device = (options.video_decode
            && info.backend == wgpu::Backend::Vulkan
            && features.contains(wgpu::Features::TEXTURE_FORMAT_NV12))
        .then(|| crate::video_device::open(&adapter, &desc))
        .flatten();
        let (device, queue, video) = match video_device {
            Some((d, q, v)) => {
                tracing::info!(target: "lumenwall_render", "video decode queues on the render device: {v:?}");
                (d, q, Some(v))
            }
            None => {
                let (d, q) = pollster::block_on(adapter.request_device(&desc))?;
                (d, q, None)
            }
        };
        // Degrade gracefully (parity policy rule 2): log instead of aborting.
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| {
            tracing::error!(target: "lumenwall_render", "uncaptured GPU error: {e}");
        }));
        tracing::info!(
            target: "lumenwall_render",
            "GPU: {} ({:?}, {:?}), caps {:?}",
            info.name,
            info.backend,
            info.device_type,
            caps
        );

        let mut ctx = Self {
            instance,
            adapter,
            device,
            queue,
            caps,
            pipeline_cache: None,
            pipeline_cache_path: None,
            memory: MemoryCounter::default(),
            resources: AtomicU64::new(0),
            samplers: Mutex::new(HashMap::new()),
            video,
        };
        if caps.pipeline_cache
            && let Some(dir) = &options.pipeline_cache_dir
            && let Some(key) = wgpu::util::pipeline_cache_key(&info)
        {
            ctx.load_pipeline_cache(&dir.join(key));
        }
        Ok(ctx)
    }

    fn load_pipeline_cache(&mut self, path: &Path) {
        let data = std::fs::read(path).ok();
        // SAFETY: the data was produced by `PipelineCache::get_data` of a
        // previous run (written atomically below); `fallback: true` makes wgpu
        // discard a cache that does not match this driver.
        let cache = unsafe {
            self.device
                .create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
                    label: Some("lumenwall.pipeline-cache"),
                    data: data.as_deref(),
                    fallback: true,
                })
        };
        self.pipeline_cache = Some(cache);
        self.pipeline_cache_path = Some(path.to_path_buf());
    }

    /// Write the pipeline cache to disk (call after loading a wallpaper).
    pub fn save_pipeline_cache(&self) -> Result<()> {
        let (Some(cache), Some(path)) = (&self.pipeline_cache, &self.pipeline_cache_path) else {
            return Ok(());
        };
        let Some(data) = cache.get_data() else {
            return Ok(());
        };
        let io = |source| RenderError::Io {
            path: path.clone(),
            source,
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &data).map_err(io)?;
        std::fs::rename(&tmp, path).map_err(io)?;
        Ok(())
    }

    #[must_use]
    pub fn instance(&self) -> &wgpu::Instance {
        &self.instance
    }

    #[must_use]
    pub fn adapter(&self) -> &wgpu::Adapter {
        &self.adapter
    }

    #[must_use]
    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }

    #[must_use]
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }

    #[must_use]
    pub fn caps(&self) -> GpuCaps {
        self.caps
    }

    /// Whether BC1/BC2/BC3 textures can be uploaded as-is. When `false`, the
    /// caller must decode them to RGBA8 before calling `upload_texture`.
    #[must_use]
    pub fn supports_bc(&self) -> bool {
        self.caps.bc
    }

    /// Vulkan Video queues of the device (zero-copy video decoding), when created with
    /// [`ContextOptions::video_decode`] on a capable adapter.
    #[must_use]
    pub fn video_queues(&self) -> Option<&crate::video_device::VideoQueues> {
        self.video.as_ref()
    }

    #[must_use]
    pub fn pipeline_cache(&self) -> Option<&wgpu::PipelineCache> {
        self.pipeline_cache.as_ref()
    }

    /// GPU bytes allocated by lumenwall-render.
    #[must_use]
    pub fn memory(&self) -> &MemoryCounter {
        &self.memory
    }

    /// Number of GPU objects created by lumenwall-render since start-up. Constant
    /// across frames in steady state (checked by tests).
    #[must_use]
    pub fn resources_created(&self) -> u64 {
        self.resources.load(Ordering::Relaxed)
    }

    pub(crate) fn note_resources(&self, n: u64) {
        self.resources.fetch_add(n, Ordering::Relaxed);
    }

    /// Shared sampler for a key (created once).
    #[must_use]
    pub fn sampler(&self, key: SamplerKey) -> wgpu::Sampler {
        self.sampler_for(key, false)
    }

    /// Shared sampler of a volume (3D) texture: like [`Self::sampler`] without anisotropy,
    /// which has no meaning for volumes (spec 01 A.1).
    #[must_use]
    pub fn volume_sampler(&self, key: SamplerKey) -> wgpu::Sampler {
        self.sampler_for(key, true)
    }

    fn sampler_for(&self, key: SamplerKey, volume: bool) -> wgpu::Sampler {
        let mut map = self
            .samplers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(s) = map.get(&(key, volume)) {
            return s.clone();
        }
        let address = match key.address {
            AddressMode::Clamp => wgpu::AddressMode::ClampToEdge,
            AddressMode::Repeat => wgpu::AddressMode::Repeat,
            AddressMode::Border if self.caps.clamp_to_border => wgpu::AddressMode::ClampToBorder,
            AddressMode::Border => {
                tracing::warn!(target: "lumenwall_render", "ClampToBorder unsupported, using ClampToEdge");
                wgpu::AddressMode::ClampToEdge
            }
        };
        let (filter, mip) = if key.nearest {
            (wgpu::FilterMode::Nearest, wgpu::MipmapFilterMode::Nearest)
        } else {
            (wgpu::FilterMode::Linear, wgpu::MipmapFilterMode::Linear)
        };
        let sampler = self.device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lumenwall.sampler"),
            address_mode_u: address,
            address_mode_v: address,
            address_mode_w: address,
            mag_filter: filter,
            min_filter: filter,
            mipmap_filter: mip,
            lod_min_clamp: 0.0,
            lod_max_clamp: if key.mipmaps { 32.0 } else { 0.0 },
            compare: None,
            // Anisotropy 8 only when every filter is linear (wgpu rule, spec 04 §24.7).
            anisotropy_clamp: if key.nearest || volume { 1 } else { 8 },
            border_color: (address == wgpu::AddressMode::ClampToBorder)
                .then_some(wgpu::SamplerBorderColor::TransparentBlack),
        });
        self.note_resources(1);
        map.insert((key, volume), sampler.clone());
        sampler
    }

    /// Block until the GPU is idle (load time, tests, shutdown only).
    pub fn wait_idle(&self) {
        let _ = self.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: None,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wgpu::{Backend, DeviceType as D, PowerPreference as P};

    #[test]
    fn adapter_ranking() {
        // Vulkan before DX12 whatever the device type.
        assert!(
            adapter_rank(Backend::Vulkan, D::IntegratedGpu, P::None, false)
                < adapter_rank(Backend::Dx12, D::DiscreteGpu, P::None, false)
        );
        // CPU adapters last unless software is requested.
        assert!(
            adapter_rank(Backend::Vulkan, D::DiscreteGpu, P::None, false)
                < adapter_rank(Backend::Vulkan, D::Cpu, P::None, false)
        );
        assert!(
            adapter_rank(Backend::Vulkan, D::Cpu, P::None, true)
                < adapter_rank(Backend::Vulkan, D::DiscreteGpu, P::None, true)
        );
        assert!(
            adapter_rank(Backend::Vulkan, D::IntegratedGpu, P::LowPower, false)
                < adapter_rank(Backend::Vulkan, D::DiscreteGpu, P::LowPower, false)
        );
        assert!(
            adapter_rank(Backend::Vulkan, D::DiscreteGpu, P::HighPerformance, false)
                < adapter_rank(Backend::Vulkan, D::IntegratedGpu, P::HighPerformance, false)
        );
    }

    #[test]
    fn memory_tokens_release_on_drop() {
        let m = MemoryCounter::default();
        let a = m.track(100);
        let b = m.track(50);
        assert_eq!(m.bytes(), 150);
        drop(a);
        assert_eq!(m.bytes(), 50);
        assert_eq!(b.bytes(), 50);
        drop(b);
        assert_eq!(m.bytes(), 0);
    }
}
