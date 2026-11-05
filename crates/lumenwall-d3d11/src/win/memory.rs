//! GPU memory of this process on the device's adapter (`IDXGIAdapter3::QueryVideoMemoryInfo`):
//! "local" is the adapter's own memory (dedicated VRAM on a discrete GPU), "non-local" the system
//! memory the GPU maps (what Task Manager shows as shared GPU memory on a discrete GPU).

use windows::Win32::Graphics::Dxgi::{
    DXGI_MEMORY_SEGMENT_GROUP, DXGI_MEMORY_SEGMENT_GROUP_LOCAL, DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL,
    DXGI_QUERY_VIDEO_MEMORY_INFO,
};

use super::device::Device;
use crate::error::Result;

/// One segment group, bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SegmentMemory {
    /// What this process uses now.
    pub usage: u64,
    /// What the OS lets it use before it starts paging it.
    pub budget: u64,
    pub reservation: u64,
    pub available_for_reservation: u64,
}

/// Process GPU memory, both segment groups.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuMemory {
    pub local: SegmentMemory,
    pub non_local: SegmentMemory,
}

impl Device {
    /// This process's GPU memory on the adapter (cheap: no GPU work, no allocation).
    pub fn memory(&self) -> Result<GpuMemory> {
        let adapter = self.adapter3()?;
        let read = |group: DXGI_MEMORY_SEGMENT_GROUP| -> Result<SegmentMemory> {
            let mut info = DXGI_QUERY_VIDEO_MEMORY_INFO::default();
            // SAFETY: node 0 of a single-node adapter; live out-parameter.
            unsafe { adapter.QueryVideoMemoryInfo(0, group, &mut info) }
                .map_err(|e| self.error("QueryVideoMemoryInfo", &e))?;
            Ok(SegmentMemory {
                usage: info.CurrentUsage,
                budget: info.Budget,
                reservation: info.CurrentReservation,
                available_for_reservation: info.AvailableForReservation,
            })
        };
        Ok(GpuMemory {
            local: read(DXGI_MEMORY_SEGMENT_GROUP_LOCAL)?,
            non_local: read(DXGI_MEMORY_SEGMENT_GROUP_NON_LOCAL)?,
        })
    }
}
