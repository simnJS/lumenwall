//! Buffer arenas: few buffer objects, because each one costs shared GPU memory the driver keeps
//! while frames run (tools/d3d11-probe, "Bisection": ≈ 0.1 MiB per 50 objects).
//!
//! * [`ConstantArena`]: one dynamic constant buffer for every pass block and the frame block,
//!   rewritten at most once per frame (`Map(WRITE_DISCARD)`) and bound by windows with
//!   `VSSetConstantBuffers1` / `PSSetConstantBuffers1`.
//! * [`StaticBuffer`]: immutable vertex or index data of every pass of a scene, built at load
//!   time from a [`crate::LinearArena`] and bound with offsets.
//! * [`DynamicBuffer`]: data rewritten from the CPU every frame (particle streams, CPU-skinned
//!   puppets), one per stream, allocated at its maximum size.

use windows::Win32::Graphics::Direct3D11::{
    D3D11_BIND_CONSTANT_BUFFER, D3D11_BIND_INDEX_BUFFER, D3D11_BIND_VERTEX_BUFFER,
    D3D11_BUFFER_DESC, D3D11_CPU_ACCESS_WRITE, D3D11_MAP_WRITE_DISCARD, D3D11_MAPPED_SUBRESOURCE,
    D3D11_SUBRESOURCE_DATA, D3D11_USAGE_DYNAMIC, D3D11_USAGE_IMMUTABLE, ID3D11Buffer,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R16_UINT, DXGI_FORMAT_R32_UINT};

use super::device::Device;
use crate::error::{Error, Result, out};
use crate::layout::{ConstantLayout, ConstantRegion};

fn create_buffer(device: &Device, desc: &D3D11_BUFFER_DESC, init: Option<&[u8]>) -> Result<ID3D11Buffer> {
    let data = init.map(|d| D3D11_SUBRESOURCE_DATA {
        pSysMem: d.as_ptr().cast(),
        SysMemPitch: 0,
        SysMemSlicePitch: 0,
    });
    let mut b = None;
    // SAFETY: valid descriptor; initial data (if any) holds at least `ByteWidth` bytes and
    // outlives the call; live out-parameter.
    unsafe {
        device
            .raw()
            .CreateBuffer(desc, data.as_ref().map(std::ptr::from_ref), Some(&mut b))
    }
    .map_err(|e| device.error("CreateBuffer", &e))?;
    out(b, "CreateBuffer")
}

/// Writes `data` into a dynamic buffer with `WRITE_DISCARD`.
fn map_discard(device: &Device, buffer: &ID3D11Buffer, data: &[u8]) -> Result<()> {
    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
    // SAFETY: a dynamic, CPU-writable buffer of at least `data.len()` bytes (callers check);
    // the mapping is written then released before anything else touches the buffer.
    unsafe {
        device
            .context()
            .Map(buffer, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m))
            .map_err(|e| device.error("Map", &e))?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), m.pData.cast::<u8>(), data.len());
        device.context().Unmap(buffer, 0);
    }
    Ok(())
}

/// The frame's constants: one dynamic buffer, a CPU shadow, windows bound with offsets.
pub struct ConstantArena {
    buffer: ID3D11Buffer,
    shadow: Vec<u8>,
    dirty: bool,
}

impl ConstantArena {
    /// An arena with room for every window of `layout` (zeroed).
    pub fn new(device: &Device, layout: &ConstantLayout) -> Result<Self> {
        let len = layout.len();
        let buffer = create_buffer(
            device,
            &D3D11_BUFFER_DESC {
                ByteWidth: len,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: 0,
                StructureByteStride: 0,
            },
            None,
        )?;
        Ok(Self {
            buffer,
            shadow: vec![0; len as usize],
            dirty: true,
        })
    }

    /// Writes `data` at `offset` bytes into `region`; the arena is uploaded at the next
    /// [`ConstantArena::upload`] only if a byte changed.
    pub fn write(&mut self, region: ConstantRegion, offset: u32, data: &[u8]) -> Result<()> {
        let start = region.offset as usize + offset as usize;
        let end = start + data.len();
        if end > region.range().end || end > self.shadow.len() {
            return Err(Error::InvalidData(format!(
                "constant write {start}..{end} outside window {:?}",
                region.range()
            )));
        }
        let dst = &mut self.shadow[start..end];
        if dst != data {
            dst.copy_from_slice(data);
            self.dirty = true;
        }
        Ok(())
    }

    /// The CPU bytes of a window.
    #[must_use]
    pub fn bytes(&self, region: ConstantRegion) -> &[u8] {
        &self.shadow[region.range()]
    }

    /// Copies the shadow to the GPU when it changed (one `Map` per frame at most). `Ok(true)`
    /// when it did.
    pub fn upload(&mut self, device: &Device) -> Result<bool> {
        if !self.dirty {
            return Ok(false);
        }
        map_discard(device, &self.buffer, &self.shadow)?;
        self.dirty = false;
        Ok(true)
    }

    /// Binds `region` at constant buffer `slot` of the vertex and pixel shaders.
    pub fn bind(&self, device: &Device, slot: u32, region: ConstantRegion) {
        let buffers = [Some(self.buffer.clone())];
        let first = [region.first_constant()];
        let count = [region.num_constants()];
        // SAFETY: one buffer, one offset and one count, all of this device; offsets and
        // counts are multiples of 16 constants by construction of `ConstantRegion`.
        unsafe {
            let ctx = device.context();
            ctx.VSSetConstantBuffers1(slot, 1, Some(buffers.as_ptr()), Some(first.as_ptr()), Some(count.as_ptr()));
            ctx.PSSetConstantBuffers1(slot, 1, Some(buffers.as_ptr()), Some(first.as_ptr()), Some(count.as_ptr()));
        }
    }

    #[must_use]
    pub fn buffer(&self) -> &ID3D11Buffer {
        &self.buffer
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.shadow.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.shadow.is_empty()
    }
}

/// What a static buffer holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StaticBufferKind {
    Vertex,
    Index,
}

/// Immutable vertex or index data, uploaded once.
pub struct StaticBuffer {
    buffer: ID3D11Buffer,
    kind: StaticBufferKind,
    len: u32,
}

impl StaticBuffer {
    pub fn new(device: &Device, kind: StaticBufferKind, bytes: &[u8]) -> Result<Self> {
        // A zero-sized buffer is invalid: keep 16 bytes for empty arenas.
        let mut padded;
        let data = if bytes.len() < 16 {
            padded = bytes.to_vec();
            padded.resize(16, 0);
            &padded[..]
        } else {
            bytes
        };
        let len = u32::try_from(data.len()).map_err(|_| Error::InvalidData("buffer over 4 GiB".into()))?;
        let bind = match kind {
            StaticBufferKind::Vertex => D3D11_BIND_VERTEX_BUFFER,
            StaticBufferKind::Index => D3D11_BIND_INDEX_BUFFER,
        };
        let buffer = create_buffer(
            device,
            &D3D11_BUFFER_DESC {
                ByteWidth: len,
                Usage: D3D11_USAGE_IMMUTABLE,
                BindFlags: bind.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                StructureByteStride: 0,
            },
            Some(data),
        )?;
        Ok(Self { buffer, kind, len })
    }

    /// Binds this vertex buffer at input slot `slot`.
    pub fn bind_vertices(&self, device: &Device, slot: u32, stride: u32, offset: u32) {
        debug_assert_eq!(self.kind, StaticBufferKind::Vertex);
        bind_vertices(device, &self.buffer, slot, stride, offset);
    }

    /// Binds this index buffer (16- or 32-bit indices) from `offset` bytes.
    pub fn bind_indices(&self, device: &Device, wide: bool, offset: u32) {
        debug_assert_eq!(self.kind, StaticBufferKind::Index);
        let format = if wide { DXGI_FORMAT_R32_UINT } else { DXGI_FORMAT_R16_UINT };
        // SAFETY: an index buffer of this device.
        unsafe { device.context().IASetIndexBuffer(&self.buffer, format, offset) };
    }

    #[must_use]
    pub fn buffer(&self) -> &ID3D11Buffer {
        &self.buffer
    }

    #[must_use]
    pub fn len(&self) -> u32 {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

fn bind_vertices(device: &Device, buffer: &ID3D11Buffer, slot: u32, stride: u32, offset: u32) {
    let buffers = [Some(buffer.clone())];
    // SAFETY: one buffer, one stride, one offset, all live for the call.
    unsafe {
        device.context().IASetVertexBuffers(
            slot,
            1,
            Some(buffers.as_ptr()),
            Some(&raw const stride),
            Some(&raw const offset),
        );
    }
}

/// A vertex or index buffer rewritten from the CPU (at most once per frame).
pub struct DynamicBuffer {
    buffer: ID3D11Buffer,
    capacity: u32,
}

impl DynamicBuffer {
    /// A buffer of `capacity` bytes bound as vertex (`index == false`) or index data.
    pub fn new(device: &Device, capacity: u32, index: bool) -> Result<Self> {
        let capacity = capacity.max(16).next_multiple_of(16);
        let bind = if index {
            D3D11_BIND_INDEX_BUFFER
        } else {
            D3D11_BIND_VERTEX_BUFFER
        };
        let buffer = create_buffer(
            device,
            &D3D11_BUFFER_DESC {
                ByteWidth: capacity,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: bind.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                MiscFlags: 0,
                StructureByteStride: 0,
            },
            None,
        )?;
        Ok(Self { buffer, capacity })
    }

    /// Replaces the content (`WRITE_DISCARD`: the GPU keeps reading the previous version until
    /// it is done with it).
    pub fn write(&self, device: &Device, data: &[u8]) -> Result<()> {
        if data.len() > self.capacity as usize {
            return Err(Error::InvalidData(format!(
                "{} bytes into a {}-byte stream",
                data.len(),
                self.capacity
            )));
        }
        map_discard(device, &self.buffer, data)
    }

    pub fn bind_vertices(&self, device: &Device, slot: u32, stride: u32, offset: u32) {
        bind_vertices(device, &self.buffer, slot, stride, offset);
    }

    #[must_use]
    pub fn buffer(&self) -> &ID3D11Buffer {
        &self.buffer
    }

    #[must_use]
    pub fn capacity(&self) -> u32 {
        self.capacity
    }
}
