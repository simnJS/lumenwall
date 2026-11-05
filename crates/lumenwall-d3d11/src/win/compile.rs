//! HLSL → bytecode (`D3DCompile`, `d3dcompiler_47.dll`, part of Windows) through the bytecode
//! cache, shader objects and input layouts.

use std::time::Instant;

use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::ID3DBlob;
use windows::Win32::Graphics::Direct3D11::{
    D3D11_INPUT_ELEMENT_DESC, D3D11_INPUT_PER_VERTEX_DATA, ID3D11InputLayout, ID3D11PixelShader,
    ID3D11VertexShader,
};
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::PCSTR;

use super::device::Device;
use crate::cache::{Bytecode, BytecodeCache, CacheKey, CompileOptions};
use crate::error::{Error, Result, out};
use crate::hlsl::{HlslProgram, InputKind, Resources, VertexInput};

fn blob_bytes(b: &ID3DBlob) -> &[u8] {
    // SAFETY: a blob owns `GetBufferSize` readable bytes at `GetBufferPointer` for its lifetime,
    // which the returned slice borrows.
    unsafe { std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize()) }
}

/// Compiles one entry point (`vs_5_0`, `ps_5_0`).
pub fn compile_hlsl(source: &str, entry: &str, profile: &str, flags: u32) -> Result<Vec<u8>> {
    let c = |s: &str| {
        std::ffi::CString::new(s).map_err(|_| Error::Shader(format!("NUL in {s:?}")))
    };
    let (entry_c, profile_c) = (c(entry)?, c(profile)?);
    let mut code = None;
    let mut errors = None;
    // SAFETY: the source slice and the C strings outlive the call; no defines, no include
    // handler (naga output is self-contained); out-parameters are live locals.
    let r = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry_c.as_ptr().cast()),
            PCSTR(profile_c.as_ptr().cast()),
            flags,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = r {
        let msg = errors
            .as_ref()
            .map(|b| String::from_utf8_lossy(blob_bytes(b)).into_owned())
            .unwrap_or_default();
        let first: Vec<&str> = msg.lines().take(4).collect();
        return Err(Error::Shader(format!(
            "D3DCompile {entry} ({profile}): {e}: {}",
            first.join(" | ")
        )));
    }
    let code = code.ok_or_else(|| Error::Shader("D3DCompile returned no bytecode".into()))?;
    Ok(blob_bytes(&code).to_vec())
}

/// Where a program's bytecode came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheOutcome {
    /// Read from the cache.
    Hit,
    /// Compiled, then stored.
    Compiled,
    /// Compiled without a cache.
    Uncached,
}

/// Vertex and pixel shader objects of one program.
pub struct ShaderProgram {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    /// Kept for input layouts (a few KiB).
    vs_bytecode: Vec<u8>,
    inputs: Vec<VertexInput>,
    resources: Resources,
}

/// Compiles (or loads from the cache) a program and creates its shader objects. `key` defaults to
/// [`CacheKey::for_hlsl`]. A cache that fails to write only logs: the program still works.
pub fn compile_program(
    device: &Device,
    program: &HlslProgram,
    cache: Option<&BytecodeCache>,
    key: Option<CacheKey>,
    options: CompileOptions,
) -> Result<(ShaderProgram, CacheOutcome)> {
    let key = key.unwrap_or_else(|| {
        CacheKey::for_hlsl(&program.source, &program.vertex_entry, &program.fragment_entry, options)
    });
    let cached = cache.and_then(|c| c.load(&key));
    let (code, outcome) = match cached {
        Some(code) => (code, CacheOutcome::Hit),
        None => {
            let t = Instant::now();
            let code = Bytecode {
                vs: compile_hlsl(&program.source, &program.vertex_entry, CompileOptions::VERTEX_PROFILE, options.flags)?,
                ps: compile_hlsl(&program.source, &program.fragment_entry, CompileOptions::PIXEL_PROFILE, options.flags)?,
            };
            tracing::debug!("D3DCompile: {:.1} ms", t.elapsed().as_secs_f64() * 1000.0);
            match cache {
                Some(c) => {
                    if let Err(e) = c.store(&key, &code) {
                        tracing::warn!("shader cache write failed: {e}");
                    }
                    (code, CacheOutcome::Compiled)
                }
                None => (code, CacheOutcome::Uncached),
            }
        }
    };
    let (mut vs, mut ps) = (None, None);
    // SAFETY: bytecode produced by D3DCompile (or read back byte-exact from the cache, checksum
    // verified); live out-parameters. The runtime validates the bytecode itself.
    unsafe {
        device
            .raw()
            .CreateVertexShader(&code.vs, None, Some(&mut vs))
            .map_err(|e| device.error("CreateVertexShader", &e))?;
        device
            .raw()
            .CreatePixelShader(&code.ps, None, Some(&mut ps))
            .map_err(|e| device.error("CreatePixelShader", &e))?;
    }
    Ok((
        ShaderProgram {
            vs: out(vs, "CreateVertexShader")?,
            ps: out(ps, "CreatePixelShader")?,
            vs_bytecode: code.vs,
            inputs: program.vertex_inputs.clone(),
            resources: program.resources.clone(),
        },
        outcome,
    ))
}

impl ShaderProgram {
    #[must_use]
    pub fn vs(&self) -> &ID3D11VertexShader {
        &self.vs
    }

    #[must_use]
    pub fn ps(&self) -> &ID3D11PixelShader {
        &self.ps
    }

    #[must_use]
    pub fn inputs(&self) -> &[VertexInput] {
        &self.inputs
    }

    #[must_use]
    pub fn resources(&self) -> &Resources {
        &self.resources
    }

    /// Binds both shaders.
    pub fn bind(&self, device: &Device) {
        // SAFETY: shader objects of this device, no class instances.
        unsafe {
            device.context().VSSetShader(&self.vs, None);
            device.context().PSSetShader(&self.ps, None);
        }
    }

    /// `fed` plus one element per input it leaves out, reading [`DEFAULT_ATTRIBUTES`] at input
    /// slot `defaults_slot` (bound with stride 0): an unfed input reads `(0, 0, 0, 1)`, as on
    /// the wgpu path.
    #[must_use]
    pub fn complete_elements(&self, fed: &[VertexElement], defaults_slot: u32) -> Vec<VertexElement> {
        let mut out = fed.to_vec();
        for input in &self.inputs {
            if fed.iter().any(|e| e.location == input.location) {
                continue;
            }
            let (format, offset) = match input.kind {
                InputKind::Float => (VertexFormat::Float32x4, 0),
                InputKind::Sint => (VertexFormat::Sint32x4, 16),
                InputKind::Uint => (VertexFormat::Uint32x4, 16),
            };
            out.push(VertexElement {
                location: input.location,
                format,
                offset,
                slot: defaults_slot,
            });
        }
        out
    }
}

/// Content of the stride-0 vertex buffer that feeds unfed inputs: `(0.0, 0.0, 0.0, 1.0)` as
/// floats at offset 0, `(0, 0, 0, 1)` as integers at offset 16.
pub const DEFAULT_ATTRIBUTES: [u8; 32] = {
    let mut b = [0u8; 32];
    let one = 1.0f32.to_le_bytes();
    b[12] = one[0];
    b[13] = one[1];
    b[14] = one[2];
    b[15] = one[3];
    b[28] = 1;
    b
};

/// Format of a vertex attribute in a buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VertexFormat {
    Float32,
    Float32x2,
    Float32x3,
    Float32x4,
    Sint32,
    Sint32x2,
    Sint32x3,
    Sint32x4,
    Uint32,
    Uint32x2,
    Uint32x3,
    Uint32x4,
    Unorm8x4,
    Uint8x4,
    Unorm16x2,
    Unorm16x4,
    Uint16x4,
}

impl VertexFormat {
    #[must_use]
    pub fn dxgi(self) -> DXGI_FORMAT {
        match self {
            Self::Float32 => DXGI_FORMAT_R32_FLOAT,
            Self::Float32x2 => DXGI_FORMAT_R32G32_FLOAT,
            Self::Float32x3 => DXGI_FORMAT_R32G32B32_FLOAT,
            Self::Float32x4 => DXGI_FORMAT_R32G32B32A32_FLOAT,
            Self::Sint32 => DXGI_FORMAT_R32_SINT,
            Self::Sint32x2 => DXGI_FORMAT_R32G32_SINT,
            Self::Sint32x3 => DXGI_FORMAT_R32G32B32_SINT,
            Self::Sint32x4 => DXGI_FORMAT_R32G32B32A32_SINT,
            Self::Uint32 => DXGI_FORMAT_R32_UINT,
            Self::Uint32x2 => DXGI_FORMAT_R32G32_UINT,
            Self::Uint32x3 => DXGI_FORMAT_R32G32B32_UINT,
            Self::Uint32x4 => DXGI_FORMAT_R32G32B32A32_UINT,
            Self::Unorm8x4 => DXGI_FORMAT_R8G8B8A8_UNORM,
            Self::Uint8x4 => DXGI_FORMAT_R8G8B8A8_UINT,
            Self::Unorm16x2 => DXGI_FORMAT_R16G16_UNORM,
            Self::Unorm16x4 => DXGI_FORMAT_R16G16B16A16_UNORM,
            Self::Uint16x4 => DXGI_FORMAT_R16G16B16A16_UINT,
        }
    }
}

/// One element of an input layout: shader location, format, byte offset and input slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VertexElement {
    pub location: u32,
    pub format: VertexFormat,
    pub offset: u32,
    pub slot: u32,
}

/// The input layout of `program` for `elements` (every shader input must have one: see
/// [`ShaderProgram::complete_elements`]).
pub fn create_input_layout(
    device: &Device,
    program: &ShaderProgram,
    elements: &[VertexElement],
) -> Result<Option<ID3D11InputLayout>> {
    if let Some(missing) = program
        .inputs
        .iter()
        .find(|i| !elements.iter().any(|e| e.location == i.location))
    {
        return Err(Error::InvalidData(format!(
            "vertex input `{}` (location {}) has no element",
            missing.name, missing.location
        )));
    }
    if elements.is_empty() {
        // A program without vertex inputs (SV_VertexID quads) draws with no layout bound.
        return Ok(None);
    }
    let descs: Vec<D3D11_INPUT_ELEMENT_DESC> = elements
        .iter()
        .map(|e| D3D11_INPUT_ELEMENT_DESC {
            SemanticName: PCSTR(c"LOC".as_ptr().cast()),
            SemanticIndex: e.location,
            Format: e.format.dxgi(),
            InputSlot: e.slot,
            AlignedByteOffset: e.offset,
            InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
            InstanceDataStepRate: 0,
        })
        .collect();
    let mut layout = None;
    // SAFETY: element descriptors with a static semantic name; the vertex bytecode carries the
    // input signature; live out-parameter.
    unsafe {
        device
            .raw()
            .CreateInputLayout(&descs, &program.vs_bytecode, Some(&mut layout))
    }
    .map_err(|e| device.error("CreateInputLayout", &e))?;
    Ok(Some(out(layout, "CreateInputLayout")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_attributes_layout() {
        let f: Vec<f32> = DEFAULT_ATTRIBUTES[..16]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        assert_eq!(f, [0.0, 0.0, 0.0, 1.0]);
        let i: Vec<u32> = DEFAULT_ATTRIBUTES[16..]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_le_bytes(*c))
            .collect();
        assert_eq!(i, [0, 0, 0, 1]);
    }
}
