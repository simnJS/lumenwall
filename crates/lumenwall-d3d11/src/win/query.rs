//! GPU time of a frame from timestamp queries, read back asynchronously over three frames: the
//! CPU never waits for the GPU (a slot still in flight skips the measurement of that frame).

use std::time::Duration;

use windows::Win32::Graphics::Direct3D11::{
    D3D11_ASYNC_GETDATA_DONOTFLUSH, D3D11_QUERY, D3D11_QUERY_DATA_TIMESTAMP_DISJOINT,
    D3D11_QUERY_DESC, D3D11_QUERY_TIMESTAMP, D3D11_QUERY_TIMESTAMP_DISJOINT, ID3D11Query,
};

use super::device::{Device, get_data};
use crate::error::{Result, out};

const SLOTS: usize = 3;

struct Slot {
    disjoint: ID3D11Query,
    begin: ID3D11Query,
    end: ID3D11Query,
    /// Frame number of the measurement in flight.
    pending: Option<u64>,
}

/// Frame timer (`begin` / `end` around the frame's commands, `poll` after the submission).
pub struct GpuTimer {
    slots: [Slot; SLOTS],
    current: usize,
    measuring: bool,
    frame: u64,
    last: Option<(u64, Duration)>,
}

fn query(device: &Device, kind: D3D11_QUERY) -> Result<ID3D11Query> {
    let mut q = None;
    // SAFETY: valid descriptor, live out-parameter.
    unsafe {
        device.raw().CreateQuery(
            &D3D11_QUERY_DESC {
                Query: kind,
                MiscFlags: 0,
            },
            Some(&mut q),
        )
    }
    .map_err(|e| device.error("CreateQuery", &e))?;
    out(q, "CreateQuery")
}

impl GpuTimer {
    pub fn new(device: &Device) -> Result<Self> {
        let slot = || -> Result<Slot> {
            Ok(Slot {
                disjoint: query(device, D3D11_QUERY_TIMESTAMP_DISJOINT)?,
                begin: query(device, D3D11_QUERY_TIMESTAMP)?,
                end: query(device, D3D11_QUERY_TIMESTAMP)?,
                pending: None,
            })
        };
        Ok(Self {
            slots: [slot()?, slot()?, slot()?],
            current: 0,
            measuring: false,
            frame: 0,
            last: None,
        })
    }

    /// Starts measuring this frame, unless the slot's previous measurement is still in flight.
    pub fn begin(&mut self, device: &Device) -> Result<()> {
        self.frame += 1;
        if self.slots[self.current].pending.is_some() {
            self.poll(device)?;
        }
        let slot = &self.slots[self.current];
        self.measuring = slot.pending.is_none();
        if self.measuring {
            // SAFETY: queries of this device, begun and ended in order.
            unsafe {
                device.context().Begin(&slot.disjoint);
                device.context().End(&slot.begin);
            }
        }
        Ok(())
    }

    /// Ends the measurement begun by [`GpuTimer::begin`].
    pub fn end(&mut self, device: &Device) {
        if !self.measuring {
            return;
        }
        let slot = &mut self.slots[self.current];
        // SAFETY: as in `begin`.
        unsafe {
            device.context().End(&slot.end);
            device.context().End(&slot.disjoint);
        }
        slot.pending = Some(self.frame);
        self.measuring = false;
        self.current = (self.current + 1) % SLOTS;
    }

    /// Reads every finished measurement without waiting or flushing; returns the most recent
    /// GPU time known (`None` before the first one, or when the clock was disjoint).
    pub fn poll(&mut self, device: &Device) -> Result<Option<Duration>> {
        let ctx = device.context();
        for slot in &mut self.slots {
            let Some(frame) = slot.pending else { continue };
            let mut dj = D3D11_QUERY_DATA_TIMESTAMP_DISJOINT::default();
            let flags = D3D11_ASYNC_GETDATA_DONOTFLUSH.0 as u32;
            if !get_data(ctx, &slot.disjoint, &mut dj, flags)
                .map_err(|e| device.error("GetData", &e))?
            {
                continue;
            }
            let (mut t0, mut t1) = (0u64, 0u64);
            let ready = get_data(ctx, &slot.begin, &mut t0, flags)
                .map_err(|e| device.error("GetData", &e))?
                && get_data(ctx, &slot.end, &mut t1, flags)
                    .map_err(|e| device.error("GetData", &e))?;
            if !ready {
                continue;
            }
            slot.pending = None;
            if dj.Disjoint.as_bool() || dj.Frequency == 0 || t1 < t0 {
                continue;
            }
            let ns = u128::from(t1 - t0) * 1_000_000_000 / u128::from(dj.Frequency);
            let d = Duration::from_nanos(ns as u64);
            if self.last.is_none_or(|(f, _)| frame > f) {
                self.last = Some((frame, d));
            }
        }
        Ok(self.last.map(|(_, d)| d))
    }

    /// The most recent GPU time read.
    #[must_use]
    pub fn last(&self) -> Option<Duration> {
        self.last.map(|(_, d)| d)
    }
}
