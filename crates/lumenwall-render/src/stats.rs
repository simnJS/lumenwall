//! Debug counters: GPU frame time (timestamp queries), CPU encode time, GPU
//! memory. Opt-in (`ContextOptions::profiling`); nothing here runs otherwise.
//!
//! The GPU timer never stalls: results are read back asynchronously through
//! a ring of three buffers, a few frames late.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use crate::context::GpuContext;

const RING: usize = 3;
const FREE: u8 = 0;
const ENCODED: u8 = 1;
const MAPPING: u8 = 2;
const MAPPED: u8 = 3;

/// GPU time between [`GpuTimer::begin`] and [`GpuTimer::end`].
pub struct GpuTimer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: [wgpu::Buffer; RING],
    state: [Arc<AtomicU8>; RING],
    current: Option<usize>,
    next: usize,
    period_ns: f64,
    last: Option<Duration>,
}

impl GpuTimer {
    /// `None` when the device has no encoder-level timestamps.
    #[must_use]
    pub fn new(ctx: &GpuContext) -> Option<Self> {
        if !ctx.caps().timestamps {
            return None;
        }
        let device = ctx.device();
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("lumenwall.timer"),
            ty: wgpu::QueryType::Timestamp,
            count: 2 * RING as u32,
        });
        let size = RING as u64 * wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT;
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("lumenwall.timer.resolve"),
            size,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        // Not in a block of system memory of its own (`readback_usage`: −4 MiB of shared GPU
        // memory on NVIDIA/Windows).
        let usage = crate::context::readback_usage(ctx.caps());
        let readback = std::array::from_fn(|_| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("lumenwall.timer.readback"),
                size: 2 * wgpu::QUERY_SIZE as u64,
                usage,
                mapped_at_creation: false,
            })
        });
        ctx.note_resources(2 + RING as u64);
        Some(Self {
            queries,
            resolve,
            readback,
            state: std::array::from_fn(|_| Arc::new(AtomicU8::new(FREE))),
            current: None,
            next: 0,
            period_ns: f64::from(ctx.queue().get_timestamp_period()),
            last: None,
        })
    }

    /// Start timing (first command of the frame). Skipped if the ring is full.
    pub fn begin(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let i = self.next;
        if self.state[i].load(Ordering::Acquire) != FREE {
            self.current = None;
            return;
        }
        encoder.write_timestamp(&self.queries, 2 * i as u32);
        self.current = Some(i);
    }

    /// Stop timing (last command of the frame).
    pub fn end(&mut self, encoder: &mut wgpu::CommandEncoder) {
        let Some(i) = self.current else {
            return;
        };
        let q = 2 * i as u32;
        encoder.write_timestamp(&self.queries, q + 1);
        let off = i as u64 * wgpu::QUERY_RESOLVE_BUFFER_ALIGNMENT;
        encoder.resolve_query_set(&self.queries, q..q + 2, &self.resolve, off);
        encoder.copy_buffer_to_buffer(
            &self.resolve,
            off,
            &self.readback[i],
            0,
            2 * wgpu::QUERY_SIZE as u64,
        );
        self.state[i].store(ENCODED, Ordering::Release);
    }

    /// Call right after `queue.submit` of the frame.
    pub fn after_submit(&mut self) {
        let Some(i) = self.current.take() else {
            return;
        };
        let state = self.state[i].clone();
        state.store(MAPPING, Ordering::Release);
        self.readback[i].map_async(wgpu::MapMode::Read, .., move |r| {
            state.store(if r.is_ok() { MAPPED } else { FREE }, Ordering::Release);
        });
        self.next = (i + 1) % RING;
    }

    /// Collect finished measurements (non-blocking). Returns the latest one.
    pub fn poll(&mut self, ctx: &GpuContext) -> Option<Duration> {
        let _ = ctx.device().poll(wgpu::PollType::Poll);
        for i in 0..RING {
            if self.state[i].load(Ordering::Acquire) != MAPPED {
                continue;
            }
            if let Ok(view) = self.readback[i].slice(..).get_mapped_range() {
                let t: &[u64] = bytemuck::cast_slice(&view[..16]);
                if t[1] > t[0] {
                    let ns = (t[1] - t[0]) as f64 * self.period_ns;
                    self.last = Some(Duration::from_nanos(ns as u64));
                }
            }
            self.readback[i].unmap();
            self.state[i].store(FREE, Ordering::Release);
        }
        self.last
    }

    #[must_use]
    pub fn last(&self) -> Option<Duration> {
        self.last
    }
}

/// Snapshot of the debug counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// CPU time spent recording the frame.
    pub cpu_encode: Duration,
    /// GPU time of the frame, a few frames late; `None` without timestamps.
    pub gpu: Option<Duration>,
    /// GPU bytes allocated by lumenwall-render (textures, targets, buffers).
    pub tracked_bytes: u64,
    /// Number of GPU objects created by lumenwall-render since start-up.
    pub resources_created: u64,
}

impl FrameStats {
    #[must_use]
    pub fn collect(ctx: &GpuContext, cpu_encode: Duration, timer: Option<&GpuTimer>) -> Self {
        Self {
            cpu_encode,
            gpu: timer.and_then(GpuTimer::last),
            tracked_bytes: ctx.memory().bytes(),
            resources_created: ctx.resources_created(),
        }
    }
}

/// Total bytes held by the GPU allocator, when the backend reports it.
/// Builds a full report: call it from a debug overlay, not every frame.
#[must_use]
pub fn allocator_bytes(ctx: &GpuContext) -> Option<u64> {
    ctx.device()
        .generate_allocator_report()
        .map(|r| r.total_allocated_bytes)
}
