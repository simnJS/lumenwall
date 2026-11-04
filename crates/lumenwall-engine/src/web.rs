//! Web wallpapers (`type: web`, spec 07 §15): one `lumenwall-web` host process per target
//! (offscreen Chromium), its frames copied into a render target Lumenwall owns and composed on
//! each output like a scene.
//!
//! Frames: the browser paints into a small pool of GPU textures and lends one per frame. On
//! Linux it arrives as a DMA-BUF, imported into the render device once per buffer of that pool
//! (`texture_from_dmabuf_fd`, cached by inode) and copied by one draw into the target; the frame
//! is released as soon as the copy completed on the GPU. On Windows it arrives as the NT handle
//! of a D3D11 texture, imported the same way on a Vulkan device created with
//! `VULKAN_EXTERNAL_MEMORY_WIN32` (`texture_from_d3d11_shared_handle`, cached by kernel object).
//! When the import is impossible (Linux driver without `VK_EXT_external_memory_dma_buf` +
//! `VK_EXT_image_drm_format_modifier`; Windows device without that feature or on DX12; import
//! error), the host is started or restarted with software painting into shared memory (a
//! `memfd` or a paging-file section), uploaded with `write_texture` (`LUMENWALL_WEB_FRAMES=memory`
//! forces it, for measurements).
//!
//! Pacing: the browser's own scheduler at the wallpaper fps (`FramePacing::Internal`). It only
//! paints when the page changed, so a static page produces no frame and no wake-up; a new frame
//! wakes the engine through its waker and the outputs showing the page are redrawn.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use lumenwall_render::compose::{ComposeSettings, Compositor};
use lumenwall_render::targets::{RenderTarget, RenderTargetPool, RtId};
use lumenwall_render::texture::SamplerKey;
use lumenwall_render::{GpuContext, HeadlessOutput, UvRange};
use lumenwall_web::audio::{AudioPacer, audio_frame};
use lumenwall_web::ipc::FramePacing;
use lumenwall_web::mouse::{MouseInput, MouseTracker, Viewport, web_position};
use lumenwall_web::{
    AudioOutput, FrameTransport, HostCommand, LaunchParams, PixelFormat, PropertySet, SharedFrame,
    SharedTexture, WebEvent, WebHost,
};

use crate::headless::{HeadlessOptions, HeadlessReport};
use crate::plan::{AudioPlan, EnginePlan};
use crate::wallpaper::Wallpaper;

/// Imports kept per page: the browser's texture pool is 2–4 buffers; a resize allocates a
/// new pool, and the oldest entries go.
const MAX_IMPORTS: usize = 6;

/// Restarts after an unexpected exit of the host before giving up.
const MAX_RESTARTS: u32 = 3;

/// Longest wait for the GPU copy of one frame (the browser waits meanwhile).
const COPY_TIMEOUT: Duration = Duration::from_millis(100);

type Waker = Arc<dyn Fn() + Send + Sync>;

/// Why a web wallpaper cannot run in this build, if it cannot.
pub(crate) fn unsupported_reason() -> Option<String> {
    if !cfg!(any(target_os = "linux", windows)) {
        return Some("web wallpapers run on Linux and Windows only".into());
    }
    if lumenwall_web::process::find_host_executable().is_none() {
        return Some(format!(
            "{} not found next to lumenwall: build it with `cargo build --release -p lumenwall-web --features cef` (or set {})",
            lumenwall_web::process::host_executable_name(),
            lumenwall_web::process::HOST_ENV,
        ));
    }
    None
}

/// How the last frame reached the render target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FramePath {
    /// Imported GPU texture, one draw.
    ZeroCopy,
    /// Shared memory uploaded with `write_texture`, then one draw.
    Memory,
}

#[derive(Default)]
struct WebStats {
    frames: u64,
    memory_frames: u64,
    revoked: u64,
    imports: u64,
    import: Duration,
    import_max: Duration,
    copy: Duration,
    copy_max: Duration,
    started: Option<Instant>,
    first_frame: Option<Duration>,
}

/// A running web wallpaper.
pub(crate) struct WebSlot {
    host: Option<WebHost>,
    exe: PathBuf,
    /// Current page settings (also used to restart the host).
    params: LaunchParams,
    /// `project.json` values of the page properties.
    defaults: PropertySet,
    waker: Waker,
    copier: FrameCopier,
    targets: RenderTargetPool,
    rt: RtId,
    /// Incremented when the render target is reallocated (outputs rebind their composer).
    generation: u64,
    /// Incremented for every frame copied; an output presents when its last presented serial
    /// differs.
    pub serial: u64,
    /// Loop iteration of the last audio step (outputs sharing the page step it once).
    pub updated_at: u64,
    pub last_time: f64,
    audio_listener: bool,
    pacer: AudioPacer,
    mouse: MouseTracker,
    mouse_out: Vec<MouseInput>,
    restarts: u32,
    title: String,
    stats: WebStats,
}

impl WebSlot {
    /// Starts the host for `wp` with a page of `size` pixels.
    pub fn open(
        ctx: &GpuContext,
        wp: &Wallpaper,
        plan: &EnginePlan,
        size: (u32, u32),
        waker: Waker,
    ) -> Result<Self, String> {
        if let Some(reason) = unsupported_reason() {
            return Err(format!("'{}': {reason}", wp.project.title));
        }
        let exe = lumenwall_web::process::find_host_executable().ok_or("no web host")?;
        let defaults = page_properties(wp);
        let mut properties = defaults.clone();
        apply_overrides(&mut properties, &plan.properties);
        let copier = FrameCopier::new(ctx);
        let forced_memory = std::env::var("LUMENWALL_WEB_FRAMES").is_ok_and(|v| v == "memory");
        let transport = if copier.zero_copy && !forced_memory {
            FrameTransport::Gpu
        } else {
            FrameTransport::Memory
        };
        let size = (size.0.max(1), size.1.max(1));
        let params = LaunchParams {
            wallpaper_id: wp.project.workshop_id.clone(),
            root: wp.path.clone(),
            file: wp.project.file.clone(),
            width: size.0,
            height: size.1,
            fps: plan.fps.max(1),
            pacing: FramePacing::Internal,
            transport,
            properties,
            audio: audio_output(&plan.audio),
            paused: false,
            profile_dir: None,
            devtools_port: std::env::var("LUMENWALL_WEB_DEVTOOLS")
                .ok()
                .and_then(|p| p.parse().ok()),
        };
        let mut targets = RenderTargetPool::new();
        let rt = targets.create_anonymous(ctx, "lumenwall.web", size, SamplerKey::CLAMP_LINEAR);
        let mut slot = Self {
            host: None,
            exe,
            params,
            defaults,
            waker,
            copier,
            targets,
            rt,
            generation: 0,
            serial: 0,
            updated_at: u64::MAX,
            last_time: 0.0,
            audio_listener: false,
            pacer: AudioPacer::new(),
            mouse: MouseTracker::new(),
            mouse_out: Vec::with_capacity(4),
            restarts: 0,
            title: wp.project.title.clone(),
            stats: WebStats::default(),
        };
        slot.spawn()?;
        Ok(slot)
    }

    fn spawn(&mut self) -> Result<(), String> {
        let host = WebHost::spawn(&self.exe, &[], self.params.clone(), self.waker.clone())
            .map_err(|e| format!("web host {}: {e}", self.exe.display()))?;
        tracing::info!(
            "web page '{}' {}x{} at {} fps: host pid {}, {} frames",
            self.title,
            self.params.width,
            self.params.height,
            self.params.fps,
            host.pid().unwrap_or(0),
            match self.params.transport {
                FrameTransport::Gpu => "shared GPU (zero-copy)",
                FrameTransport::Memory => "shared-memory",
            }
        );
        self.host = Some(host);
        self.audio_listener = false;
        self.mouse = MouseTracker::new();
        self.stats.started.get_or_insert_with(Instant::now);
        Ok(())
    }

    fn send(&self, command: HostCommand) {
        if let Some(host) = &self.host {
            host.send(command);
        }
    }

    /// Handles what the host sent; true when a new frame was copied.
    pub fn pump(&mut self, ctx: &GpuContext) -> bool {
        let mut fresh = false;
        while let Some(event) = self.host.as_ref().and_then(WebHost::try_event) {
            match event {
                WebEvent::Frame(frame) => fresh |= self.copy_frame(ctx, frame),
                WebEvent::Started { pid } => tracing::debug!("web host {pid} ready"),
                WebEvent::Loaded { url, status } => {
                    tracing::info!("web page loaded: {url} ({status})")
                }
                WebEvent::LoadFailed { url, error } => {
                    tracing::warn!("web page {url} failed to load: {error}")
                }
                WebEvent::AudioListener(on) => {
                    tracing::debug!("web page audio listener: {on}");
                    self.audio_listener = on;
                    self.pacer.reset();
                }
                // No media source yet (MPRIS / Windows media session): nothing to forward.
                WebEvent::MediaListeners(_) => {}
                WebEvent::Error(e) => tracing::warn!("web host: {e}"),
                WebEvent::Exited => {
                    self.host = None;
                    if self.restarts < MAX_RESTARTS {
                        self.restarts += 1;
                        tracing::warn!(
                            "web host of '{}' exited; restarting ({}/{MAX_RESTARTS})",
                            self.title,
                            self.restarts
                        );
                        if let Err(e) = self.spawn() {
                            tracing::error!("{e}");
                        }
                    } else {
                        tracing::error!("web host of '{}' keeps exiting; stopped", self.title);
                    }
                }
            }
        }
        fresh
    }

    fn copy_frame(&mut self, ctx: &GpuContext, frame: SharedFrame) -> bool {
        if frame.is_revoked() {
            // The browser stopped waiting: its texture may already be repainted.
            self.stats.revoked += 1;
            return false;
        }
        let [vx, vy, vw, vh] = frame.visible;
        let (vx, vy) = (vx.max(0) as u32, vy.max(0) as u32);
        let size = (
            (vw.max(1) as u32)
                .min(frame.width.saturating_sub(vx))
                .max(1),
            (vh.max(1) as u32)
                .min(frame.height.saturating_sub(vy))
                .max(1),
        );
        if self.targets.get(self.rt).size() != size {
            self.targets.resize(ctx, self.rt, size);
            self.generation += 1;
        }
        let t0 = Instant::now();
        let imports = self.copier.imports_done;
        let result = self
            .copier
            .copy(ctx, &frame, [vx, vy], self.targets.get(self.rt));
        frame.release();
        match result {
            Ok(path) => {
                let spent = t0.elapsed();
                let s = &mut self.stats;
                if self.copier.imports_done != imports {
                    // A first import of a pool buffer is not a steady-state copy.
                    s.imports += 1;
                    s.import += spent;
                    s.import_max = s.import_max.max(spent);
                } else {
                    s.copy += spent;
                    s.copy_max = s.copy_max.max(spent);
                    s.frames += 1;
                    s.memory_frames += u64::from(path == FramePath::Memory);
                }
                if s.first_frame.is_none() {
                    s.first_frame = s.started.map(|t| t.elapsed());
                }
                self.serial += 1;
                true
            }
            Err(e) if self.params.transport == FrameTransport::Gpu => {
                tracing::warn!(
                    "web frame import failed ({e}); restarting '{}' with shared-memory frames",
                    self.title
                );
                self.copier.zero_copy = false;
                self.params.transport = FrameTransport::Memory;
                self.host = None;
                if let Err(e) = self.spawn() {
                    tracing::error!("{e}");
                }
                false
            }
            Err(e) => {
                tracing::warn!("web frame dropped: {e}");
                false
            }
        }
    }

    /// The page image, at the page size.
    pub fn output(&self) -> &RenderTarget {
        self.targets.get(self.rt)
    }

    pub fn size(&self) -> (u32, u32) {
        self.output().size()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_paused(&self) -> bool {
        self.params.paused
    }

    /// Pause (page told, then frozen and silenced by the host) or resume.
    pub fn set_paused(&mut self, paused: bool) {
        if self.params.paused == paused {
            return;
        }
        self.params.paused = paused;
        self.pacer.reset();
        self.send(HostCommand::SetPaused { paused });
    }

    /// `--volume`, `--silent`.
    pub fn set_audio(&mut self, plan: &AudioPlan) {
        let audio = audio_output(plan);
        if audio != self.params.audio {
            self.params.audio = audio;
            self.send(HostCommand::SetAudio(audio));
        }
    }

    pub fn set_fps(&mut self, fps: u32) {
        let fps = fps.max(1);
        if fps != self.params.fps {
            self.params.fps = fps;
            self.send(HostCommand::SetFps { fps });
        }
    }

    /// New page size (the output showing it was resized).
    pub fn resize(&mut self, size: (u32, u32)) {
        let size = (size.0.max(1), size.1.max(1));
        if size != (self.params.width, self.params.height) {
            (self.params.width, self.params.height) = size;
            self.send(HostCommand::Resize {
                width: size.0,
                height: size.1,
            });
        }
    }

    /// The complete set of `--set-property` overrides: the page receives the properties whose
    /// value changed (`applyUserProperties`). Returns their names.
    pub fn set_properties(&mut self, overrides: &BTreeMap<String, String>) -> Vec<String> {
        let mut wanted = self.defaults.clone();
        apply_overrides(&mut wanted, overrides);
        let changes = self.params.properties.merge(wanted.iter().cloned());
        let names = changes.iter().map(|p| p.key.clone()).collect();
        if !changes.is_empty() {
            self.send(HostCommand::Properties { changes });
        }
        names
    }

    /// Whether the engine must keep stepping this page without new frames: the page listens to
    /// audio and a spectrum exists.
    pub fn wants_audio_steps(&self, spectrum: bool) -> bool {
        spectrum && self.audio_listener && !self.params.paused && self.host.is_some()
    }

    /// Sends the spectrum to the page's audio listener (≈ 30 times per second).
    pub fn audio_step(&mut self, now: Instant, spectrum64: &[f32; 64]) {
        if !self.audio_listener || self.params.paused {
            return;
        }
        // Spec 07 §13.6: one analysed channel, left = right.
        let frame = audio_frame(spectrum64, spectrum64);
        if self.pacer.should_send(now, &frame) {
            self.send(HostCommand::Audio {
                values: frame.to_vec(),
            });
        }
    }

    /// Pointer of an output showing the page: GL pixels of the output (origin bottom-left),
    /// buttons, and the output's composition window `uv` (spec 07 §10.4).
    pub fn pointer(
        &mut self,
        gl: (f64, f64),
        left: bool,
        right: bool,
        output: (u32, u32),
        uv: UvRange,
    ) {
        let page = self.size();
        let identity = uv.u0 == 0.0 && uv.u1 == 1.0 && uv.v0 == 0.0 && uv.v1 == 1.0;
        let pos = if identity && page == output {
            web_position(
                Viewport {
                    x: 0,
                    y: 0,
                    width: page.0 as i32,
                    height: page.1 as i32,
                },
                gl.0,
                gl.1,
            )
        } else {
            // Scaled page: through the composition window, like the scene pointer (§10.3).
            let mx = (gl.0 / f64::from(output.0.max(1))).clamp(0.0, 1.0);
            let ny = (gl.1 / f64::from(output.1.max(1))).clamp(0.0, 1.0);
            let u = f64::from(uv.u0) + mx * f64::from(uv.u1 - uv.u0);
            let v = f64::from(uv.v0) + (1.0 - ny) * f64::from(uv.v1 - uv.v0);
            (
                ((u * f64::from(page.0)).floor() as i32).clamp(0, page.0 as i32),
                ((v * f64::from(page.1)).floor() as i32).clamp(0, page.1 as i32),
            )
        };
        self.mouse.update(pos, left, right, &mut self.mouse_out);
        for input in std::mem::take(&mut self.mouse_out).drain(..) {
            self.send(HostCommand::Mouse(input));
        }
    }

    /// Logs the frame path and counters (end of run).
    pub fn log_stats(&self) {
        let s = &self.stats;
        tracing::info!(
            "web '{}': {} frames copied ({} through shared memory), {} pool import(s) ({:.2} ms each, max {:.2} ms), {} revoked, copy {:.3} ms/frame (max {:.2} ms), first frame after {}",
            self.title,
            s.frames,
            s.memory_frames,
            s.imports,
            s.import.as_secs_f64() * 1000.0 / s.imports.max(1) as f64,
            s.import_max.as_secs_f64() * 1000.0,
            s.revoked,
            s.copy.as_secs_f64() * 1000.0 / s.frames.max(1) as f64,
            s.copy_max.as_secs_f64() * 1000.0,
            s.first_frame.map_or("-".into(), |d| format!(
                "{:.0} ms",
                d.as_secs_f64() * 1000.0
            )),
        );
    }
}

/// Page audio: `--volume` (linear, v/128, like scene sounds) and `--silent`; pause is applied by
/// the host. Automute is not applied to the page: its stream comes from the host process,
/// which the detector would count as another application and mute the page in a loop.
fn audio_output(plan: &AudioPlan) -> AudioOutput {
    AudioOutput {
        volume: f32::from(plan.volume.min(128)) / 128.0,
        muted: plan.silent,
    }
}

/// `general.properties` of `project.json` as page properties.
fn page_properties(wp: &Wallpaper) -> PropertySet {
    let Ok(bytes) = wp.vfs.read("project.json") else {
        return PropertySet::new();
    };
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&bytes);
    let Ok(json) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return PropertySet::new();
    };
    PropertySet::from_project_properties(&json["general"]["properties"])
}

fn apply_overrides(properties: &mut PropertySet, overrides: &BTreeMap<String, String>) {
    for (key, text) in overrides {
        if properties.get(key).is_none() {
            continue; // Meant for another wallpaper.
        }
        if let Err(e) = properties.set_from_text(key, text) {
            tracing::warn!("{e}");
        }
    }
}

/// A browser texture imported into the render device.
struct Imported {
    key: ImportKey,
    buffer: BufferId,
    _texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    used: u64,
}

/// Which buffer of the browser's pool (or which shared memory buffer of the host) a frame
/// uses: (device, inode) of the DMA-BUF or `memfd`; on Windows a handle kept open on the
/// kernel object, compared with `same_object` (every frame brings a new duplicate).
#[cfg(unix)]
type BufferId = (u64, u64);
#[cfg(windows)]
type BufferId = std::os::windows::io::OwnedHandle;

/// Shared memory a software-painted frame arrives in: a `memfd` or a section handle.
#[cfg(unix)]
type MemoryHandle = std::os::fd::OwnedFd;
#[cfg(windows)]
type MemoryHandle = std::os::windows::io::OwnedHandle;

/// Layout of one buffer of the browser's pool (stride, offset and modifier: DMA-BUF only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ImportKey {
    size: (u32, u32),
    stride: u32,
    offset: u64,
    modifier: u64,
    format: PixelFormat,
}

/// Read-only mapping of the host's frame buffer, kept while the host reuses that buffer.
struct Mapping {
    buffer: BufferId,
    len: usize,
    memory: lumenwall_web::ipc::frame::MappedMemory,
}

impl Mapping {
    fn new(handle: &MemoryHandle, len: usize) -> Result<Self, String> {
        let memory = lumenwall_web::ipc::frame::MappedMemory::map(handle, len)
            .map_err(|e| format!("frame memory: {e}"))?;
        #[cfg(unix)]
        let buffer = lumenwall_web::ipc::frame::buffer_id(handle).map_err(|e| e.to_string())?;
        #[cfg(windows)]
        let buffer = handle.try_clone().map_err(|e| e.to_string())?;
        Ok(Self {
            buffer,
            len,
            memory,
        })
    }

    /// Whether this mapping shows the buffer behind `handle`.
    fn maps(&self, handle: &MemoryHandle, len: usize) -> Result<bool, String> {
        if self.len != len {
            return Ok(false);
        }
        #[cfg(unix)]
        return Ok(
            lumenwall_web::ipc::frame::buffer_id(handle).map_err(|e| e.to_string())? == self.buffer,
        );
        #[cfg(windows)]
        return Ok(lumenwall_web::ipc::frame::same_object(&self.buffer, handle));
    }
}

/// Upload texture of the shared-memory path.
struct Staging {
    texture: wgpu::Texture,
    bind: wgpu::BindGroup,
    size: (u32, u32),
    format: wgpu::TextureFormat,
    map: Option<Mapping>,
}

enum Source {
    Import(usize),
    Staging,
}

/// Copies browser frames into a render target with one draw (format conversion and crop to
/// the visible rectangle; no filtering).
struct FrameCopier {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
    origin: Option<[u32; 2]>,
    /// DMA-BUF imports possible on this device.
    zero_copy: bool,
    imports: Vec<Imported>,
    imports_done: u64,
    tick: u64,
    staging: Option<Staging>,
}

const COPY_WGSL: &str = r"
struct Params { origin: vec2<u32>, pad: vec2<u32> };
@group(0) @binding(0) var frame: texture_2d<f32>;
@group(0) @binding(1) var<uniform> params: Params;

@vertex
fn vs(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4<f32>(p * 2.0 - 1.0, 0.0, 1.0);
}

@fragment
fn fs(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(frame, vec2<u32>(pos.xy) + params.origin, 0);
}
";

impl FrameCopier {
    fn new(ctx: &GpuContext) -> Self {
        let device = ctx.device();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("lumenwall.web.copy"),
            source: wgpu::ShaderSource::Wgsl(COPY_WGSL.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("lumenwall.web.copy"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("lumenwall.web.copy"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("lumenwall.web.copy"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: lumenwall_render::targets::RT_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: ctx.pipeline_cache(),
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumenwall.web.copy"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Linux: DMA-BUF; Windows: D3D11 shared textures into Vulkan (the device must have been
        // created with `VULKAN_EXTERNAL_MEMORY_WIN32`; no DX12 import is written).
        let (import_feature, import_name) = if cfg!(windows) {
            (
                wgpu::Features::VULKAN_EXTERNAL_MEMORY_WIN32,
                "D3D11 shared texture",
            )
        } else {
            (wgpu::Features::VULKAN_EXTERNAL_MEMORY_DMA_BUF, "DMA-BUF")
        };
        let zero_copy = cfg!(any(target_os = "linux", windows))
            && ctx.adapter().get_info().backend == wgpu::Backend::Vulkan
            && ctx.device().features().contains(import_feature);
        if !zero_copy {
            tracing::info!(
                "web frames: no {import_name} import on this device, shared-memory copies instead"
            );
        }
        Self {
            pipeline,
            layout,
            params,
            origin: None,
            zero_copy,
            imports: Vec::new(),
            imports_done: 0,
            tick: 0,
            staging: None,
        }
    }

    fn bind_group(&self, ctx: &GpuContext, view: &wgpu::TextureView) -> wgpu::BindGroup {
        ctx.device().create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lumenwall.web.copy"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: self.params.as_entire_binding(),
                },
            ],
        })
    }

    /// Copies the visible rectangle of `frame` (from `origin`) into `dst` and waits for the GPU.
    fn copy(
        &mut self,
        ctx: &GpuContext,
        frame: &SharedFrame,
        origin: [u32; 2],
        dst: &RenderTarget,
    ) -> Result<FramePath, String> {
        let format = match frame.format {
            PixelFormat::Bgra8 => wgpu::TextureFormat::Bgra8Unorm,
            PixelFormat::Rgba8 => wgpu::TextureFormat::Rgba8Unorm,
        };
        let (path, source) = match &frame.texture {
            #[cfg(unix)]
            SharedTexture::DmaBuf { planes, modifier } => (
                FramePath::ZeroCopy,
                Source::Import(self.import(ctx, frame, planes, *modifier, format)?),
            ),
            #[cfg(unix)]
            SharedTexture::Memory { fd, stride, size } => {
                self.upload(ctx, frame, fd, *stride, *size, format)?;
                (FramePath::Memory, Source::Staging)
            }
            #[cfg(windows)]
            SharedTexture::D3D11 { handle } => (
                FramePath::ZeroCopy,
                Source::Import(self.import(ctx, frame, handle, format)?),
            ),
            #[cfg(windows)]
            SharedTexture::Memory {
                handle,
                stride,
                size,
            } => {
                self.upload(ctx, frame, handle, *stride, *size, format)?;
                (FramePath::Memory, Source::Staging)
            }
        };
        if self.origin != Some(origin) {
            self.origin = Some(origin);
            let mut bytes = [0u8; 16];
            bytes[..4].copy_from_slice(&origin[0].to_le_bytes());
            bytes[4..8].copy_from_slice(&origin[1].to_le_bytes());
            ctx.queue().write_buffer(&self.params, 0, &bytes);
        }
        let bind = match source {
            Source::Import(i) => &self.imports[i].bind,
            Source::Staging => &self.staging.as_ref().ok_or("no staging texture")?.bind,
        };
        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumenwall.web.copy"),
            });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("lumenwall.web.copy"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: dst.view(),
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind, &[]);
            pass.draw(0..3, 0..1);
        }
        let index = ctx.queue().submit(std::iter::once(enc.finish()));
        // The browser waits for the release: wait for this copy only, then release at once.
        ctx.device()
            .poll(wgpu::PollType::Wait {
                submission_index: Some(index),
                timeout: Some(COPY_TIMEOUT),
            })
            .map_err(|e| format!("frame copy: {e}"))?;
        Ok(path)
    }

    /// Index of the cached import of the frame's buffer (`same` recognises it), if any.
    fn cached_import(
        &mut self,
        key: &ImportKey,
        same: impl Fn(&BufferId) -> bool,
    ) -> Option<usize> {
        self.tick += 1;
        let i = self
            .imports
            .iter()
            .position(|e| e.key == *key && same(&e.buffer))?;
        self.imports[i].used = self.tick;
        Some(i)
    }

    /// Caches a new import; the least recently used one goes when the cache is full.
    fn add_import(
        &mut self,
        ctx: &GpuContext,
        key: ImportKey,
        buffer: BufferId,
        texture: wgpu::Texture,
    ) -> usize {
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let bind = self.bind_group(ctx, &view);
        if self.imports.len() >= MAX_IMPORTS
            && let Some(oldest) = (0..self.imports.len()).min_by_key(|&i| self.imports[i].used)
        {
            self.imports.swap_remove(oldest);
        }
        self.imports.push(Imported {
            key,
            buffer,
            _texture: texture,
            bind,
            used: self.tick,
        });
        self.imports_done += 1;
        self.imports.len() - 1
    }

    /// Bind group of an imported DMA-BUF, imported on first sight of the buffer.
    #[cfg(unix)]
    fn import(
        &mut self,
        ctx: &GpuContext,
        frame: &SharedFrame,
        planes: &[lumenwall_web::ipc::frame::DmaBufPlane],
        modifier: u64,
        format: wgpu::TextureFormat,
    ) -> Result<usize, String> {
        if !self.zero_copy {
            return Err("DMA-BUF import unavailable on this device".into());
        }
        let [plane] = planes else {
            return Err(format!("{} planes (one supported)", planes.len()));
        };
        let key = ImportKey {
            size: (frame.width, frame.height),
            stride: plane.stride,
            offset: plane.offset,
            modifier,
            format: frame.format,
        };
        let buffer = plane.buffer_id().map_err(|e| e.to_string())?;
        if let Some(i) = self.cached_import(&key, |b| *b == buffer) {
            return Ok(i);
        }
        let fd = plane.fd.try_clone().map_err(|e| e.to_string())?;
        let texture = import_dmabuf(
            ctx,
            fd,
            (frame.width, frame.height),
            format,
            modifier,
            plane.stride,
            plane.offset,
        )?;
        let i = self.add_import(ctx, key, buffer, texture);
        tracing::debug!(
            "web frame buffer imported: {}x{}, modifier {modifier:#x}, stride {}",
            frame.width,
            frame.height,
            plane.stride
        );
        Ok(i)
    }

    /// Bind group of an imported D3D11 shared texture, imported on first sight of the texture.
    #[cfg(windows)]
    fn import(
        &mut self,
        ctx: &GpuContext,
        frame: &SharedFrame,
        handle: &std::os::windows::io::OwnedHandle,
        format: wgpu::TextureFormat,
    ) -> Result<usize, String> {
        use lumenwall_web::ipc::frame::same_object;
        if !self.zero_copy {
            return Err("D3D11 shared texture import unavailable on this device".into());
        }
        let key = ImportKey {
            size: (frame.width, frame.height),
            stride: 0,
            offset: 0,
            modifier: 0,
            format: frame.format,
        };
        if let Some(i) = self.cached_import(&key, |b| same_object(b, handle)) {
            return Ok(i);
        }
        let texture = import_d3d11(ctx, handle, (frame.width, frame.height), format)?;
        let buffer = handle.try_clone().map_err(|e| e.to_string())?;
        let i = self.add_import(ctx, key, buffer, texture);
        tracing::debug!(
            "web frame texture imported: {}x{}",
            frame.width,
            frame.height
        );
        Ok(i)
    }

    /// Uploads a shared-memory frame into the staging texture.
    fn upload(
        &mut self,
        ctx: &GpuContext,
        frame: &SharedFrame,
        handle: &MemoryHandle,
        stride: u32,
        size: u64,
        format: wgpu::TextureFormat,
    ) -> Result<(), String> {
        let extent = wgpu::Extent3d {
            width: frame.width.max(1),
            height: frame.height.max(1),
            depth_or_array_layers: 1,
        };
        let dims = (extent.width, extent.height);
        if self
            .staging
            .as_ref()
            .is_none_or(|s| s.size != dims || s.format != format)
        {
            let texture = ctx.device().create_texture(&wgpu::TextureDescriptor {
                label: Some("lumenwall.web.upload"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let bind = self.bind_group(ctx, &view);
            self.staging = Some(Staging {
                texture,
                bind,
                size: dims,
                format,
                map: None,
            });
        }
        let staging = self.staging.as_mut().ok_or("no staging texture")?;
        let len = usize::try_from(size).map_err(|e| e.to_string())?;
        let mapped = match &staging.map {
            Some(map) => map.maps(handle, len)?,
            None => false,
        };
        if !mapped {
            staging.map = None;
            staging.map = Some(Mapping::new(handle, len)?);
        }
        let Some(map) = &staging.map else {
            return Err("frame memory not mapped".into());
        };
        ctx.queue().write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &staging.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            map.memory.as_slice(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(stride),
                rows_per_image: Some(extent.height),
            },
            extent,
        );
        Ok(())
    }
}

/// Descriptors of an imported browser texture: wgpu-hal's, then wgpu's.
fn frame_descriptors(
    size: (u32, u32),
    format: wgpu::TextureFormat,
) -> (
    wgpu::hal::TextureDescriptor<'static>,
    wgpu::TextureDescriptor<'static>,
) {
    let extent = wgpu::Extent3d {
        width: size.0,
        height: size.1,
        depth_or_array_layers: 1,
    };
    (
        wgpu::hal::TextureDescriptor {
            label: Some("lumenwall.web.frame"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::wgt::TextureUses::RESOURCE,
            memory_flags: wgpu::hal::MemoryFlags::empty(),
            view_formats: Vec::new(),
        },
        wgpu::TextureDescriptor {
            label: Some("lumenwall.web.frame"),
            size: extent,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        },
    )
}

/// Wraps a DMA-BUF of the browser as a sampled texture of the render device.
#[cfg(unix)]
fn import_dmabuf(
    ctx: &GpuContext,
    fd: std::os::fd::OwnedFd,
    size: (u32, u32),
    format: wgpu::TextureFormat,
    modifier: u64,
    stride: u32,
    offset: u64,
) -> Result<wgpu::Texture, String> {
    let (hal_desc, desc) = frame_descriptors(size, format);
    // SAFETY: `fd` is a DMA-BUF of `size` pixels in `format` with this modifier, stride and
    // offset, as described by the browser; Vulkan takes ownership of it. The texture is only
    // sampled, after the browser finished painting it (it is handed over in its paint
    // callback) and before the frame is released. Its initial state is declared as already
    // readable: the memory holds the painted pixels, and a transition from `UNDEFINED` would
    // allow the driver to discard them.
    unsafe {
        let hal_device = ctx
            .device()
            .as_hal::<wgpu::hal::api::Vulkan>()
            .ok_or("not a Vulkan device")?;
        let hal_texture = hal_device
            .texture_from_dmabuf_fd(fd, &hal_desc, modifier, u64::from(stride), offset)
            .map_err(|e| format!("DMA-BUF import (modifier {modifier:#x}): {e}"))?;
        drop(hal_device);
        Ok(ctx
            .device()
            .create_texture_from_hal::<wgpu::hal::api::Vulkan>(
                hal_texture,
                &desc,
                wgpu::wgt::TextureUses::RESOURCE,
            ))
    }
}

/// Wraps a D3D11 shared texture of the browser as a sampled texture of the render device
/// (Vulkan `VK_KHR_external_memory_win32`, handle type `D3D11_TEXTURE`).
#[cfg(windows)]
fn import_d3d11(
    ctx: &GpuContext,
    handle: &std::os::windows::io::OwnedHandle,
    size: (u32, u32),
    format: wgpu::TextureFormat,
) -> Result<wgpu::Texture, String> {
    use std::os::windows::io::AsRawHandle;
    let (hal_desc, desc) = frame_descriptors(size, format);
    // SAFETY: `handle` is the NT handle of a D3D11 texture of `size` pixels in `format`,
    // shared without keyed mutex, as described by the browser; the imported memory holds its
    // own reference to the resource and the handle stays ours. The texture is only sampled,
    // after the browser finished painting it and before the frame is released; its initial
    // state is declared as already readable, as for DMA-BUF imports.
    unsafe {
        let hal_device = ctx
            .device()
            .as_hal::<wgpu::hal::api::Vulkan>()
            .ok_or("not a Vulkan device")?;
        let hal_texture = hal_device
            .texture_from_d3d11_shared_handle(win32_handle(handle.as_raw_handle()), &hal_desc)
            .map_err(|e| format!("D3D11 shared texture import: {e}"))?;
        drop(hal_device);
        Ok(ctx
            .device()
            .create_texture_from_hal::<wgpu::hal::api::Vulkan>(
                hal_texture,
                &desc,
                wgpu::wgt::TextureUses::RESOURCE,
            ))
    }
}

/// wgpu-hal takes the handle as the `windows` crate's `HANDLE`, a `#[repr(transparent)]`
/// wrapper of the raw pointer; it is inferred here so this crate needs no `windows` dependency.
///
/// # Safety
///
/// `H` must be a transparent wrapper of a raw handle (checked: same size).
#[cfg(windows)]
unsafe fn win32_handle<H>(raw: std::os::windows::raw::HANDLE) -> H {
    const { assert!(std::mem::size_of::<H>() == std::mem::size_of::<std::os::windows::raw::HANDLE>()) };
    // SAFETY: forwarded to the caller; sizes are equal.
    unsafe { std::mem::transmute_copy::<std::os::windows::raw::HANDLE, H>(&raw) }
}

/// [`crate::render_headless`] for web wallpapers. The page runs in real time: frame `i` is
/// taken `start_time + i / fps` seconds after the first painted frame.
pub(crate) fn render_headless_web(
    ctx: &GpuContext,
    wp: &Wallpaper,
    opts: &HeadlessOptions,
    t0: Instant,
) -> Result<HeadlessReport, String> {
    let fps = opts.fps.max(1.0);
    let plan = EnginePlan {
        fps: fps.round() as u32,
        properties: opts.properties.clone(),
        audio: AudioPlan {
            silent: true,
            ..AudioPlan::default()
        },
        ..EnginePlan::default()
    };
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let waker: Waker = Arc::new(move || {
        let _ = tx.send(());
    });
    let mut slot = WebSlot::open(ctx, wp, &plan, (opts.width, opts.height), waker)?;
    let wait_until = |slot: &mut WebSlot, until: Instant| {
        loop {
            slot.pump(ctx);
            let now = Instant::now();
            if now >= until {
                break;
            }
            let _ = rx.recv_timeout(until - now);
        }
    };
    let deadline = Instant::now() + Duration::from_secs(30);
    while slot.serial == 0 {
        if Instant::now() >= deadline || (slot.host.is_none() && slot.restarts >= MAX_RESTARTS) {
            return Err("no frame from the web page within 30 s".into());
        }
        let _ = rx.recv_timeout(Duration::from_millis(100));
        slot.pump(ctx);
    }
    let load_time = t0.elapsed();
    let start = Instant::now() + Duration::from_secs_f64(opts.start_time.max(0.0));
    if let Some(p) = opts.pointer {
        // Half-way through the warm-up, once the page scripts are running.
        wait_until(&mut slot, Instant::now() + (start - Instant::now()) / 2);
        let gl = (f64::from(p.x), f64::from(opts.height) - f64::from(p.y));
        slot.pointer(gl, false, false, (opts.width, opts.height), UvRange::FULL);
    }
    let out = HeadlessOutput::new(ctx, opts.width, opts.height);
    let mut compositor = Compositor::new(ctx);
    let mut composer = compositor.create_output(
        ctx,
        HeadlessOutput::FORMAT,
        slot.output(),
        ComposeSettings {
            scaling: opts.scaling,
            clamp: opts.clamp,
            flip_vertical: false,
        },
    );
    let mut generation = slot.generation();
    std::fs::create_dir_all(&opts.out_dir)
        .map_err(|e| format!("{}: {e}", opts.out_dir.display()))?;
    let mut report = HeadlessReport {
        adapter: ctx.adapter().get_info().name,
        load_time,
        ..HeadlessReport::default()
    };
    let last = opts.frames.saturating_sub(1);
    let mut cpu = Duration::ZERO;
    let mut changed = 0u64;
    let mut shown = slot.serial;
    // No capture off screen: `LUMENWALL_WEB_TEST_AUDIO=1` feeds a synthetic beat to pages that
    // listen to audio (visual check of `wallpaperRegisterAudioListener`).
    let test_audio = std::env::var_os("LUMENWALL_WEB_TEST_AUDIO").is_some_and(|v| v != "0");
    for i in 0..opts.frames {
        let due = start + Duration::from_secs_f64(f64::from(i) / f64::from(fps));
        if test_audio {
            let t = f64::from(i) / f64::from(fps);
            let beat = (0.5 + 0.5 * (t * std::f64::consts::TAU * 2.0).sin()) as f32;
            let mut bands = [0.0f32; 64];
            for (b, v) in bands.iter_mut().enumerate() {
                *v = beat * (1.0 - b as f32 / 64.0) * 0.9;
            }
            slot.audio_step(Instant::now(), &bands);
        }
        wait_until(&mut slot, due);
        let begin = Instant::now();
        changed += u64::from(slot.serial != shown);
        shown = slot.serial;
        if generation != slot.generation() {
            generation = slot.generation();
            let settings = composer.settings();
            compositor.rebind(ctx, &mut composer, slot.output(), settings);
        }
        let mut enc = ctx
            .device()
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("lumenwall.headless.web"),
            });
        composer.update(ctx, out.size(), slot.size());
        compositor.encode(
            &mut enc,
            &composer,
            out.view(),
            [0.0, 0.0, opts.width as f32, opts.height as f32],
            Some(wgpu::Color::BLACK),
        );
        ctx.queue().submit(std::iter::once(enc.finish()));
        cpu += begin.elapsed();
        let save = if opts.save.is_empty() {
            i == last
        } else {
            opts.save.contains(&i)
        };
        if save {
            let png = opts.out_dir.join(format!("{}-{i:04}.png", opts.prefix));
            out.write_png(ctx, &png).map_err(|e| e.to_string())?;
            report.pngs.push(png);
        }
    }
    slot.log_stats();
    tracing::info!(
        "web: {changed}/{} composed frame(s) showed a new page frame",
        opts.frames
    );
    report.scene_size = slot.size();
    report.frames = opts.frames;
    report.cpu_frame = cpu / opts.frames.max(1);
    report.gpu_memory = ctx.memory().bytes();
    Ok(report)
}
