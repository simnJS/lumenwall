//! Shader feasibility for Direct3D 11: naga module -> HLSL (shader model 5.0) -> D3DCompile, and
//! the bytecode cache on disk. CPU only: nothing here creates a GPU device.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use naga::back::hlsl;
use naga::valid::{Capabilities, ValidationFlags, Validator};
use naga::{AddressSpace, Module, TypeInner};

use lumenwall_shader::{FRAGMENT_ENTRY, VERTEX_ENTRY};

pub struct Hlsl {
    pub source: String,
    pub vs_entry: String,
    pub ps_entry: String,
}

/// The `register` of a resource under the binding contract of lumenwall-render:
/// `@group(0) @binding(0|1)` are constant buffers `b0|b1`; `@group(1) @binding(2N)` is texture
/// `tN` and `@binding(2N+1)` its sampler `sN`.
fn register_of(group: u32, binding: u32) -> u32 {
    if group == 0 { binding } else { binding / 2 }
}

pub fn to_hlsl(module: &Module) -> Result<Hlsl> {
    let info = Validator::new(ValidationFlags::all(), Capabilities::all())
        .validate(module)
        .map_err(|e| anyhow!("validation: {e:?}"))?;
    let mut binding_map = BTreeMap::new();
    for (_, g) in module.global_variables.iter() {
        if let Some(b) = g.binding {
            binding_map.insert(
                b,
                hlsl::BindTarget {
                    space: 0,
                    register: register_of(b.group, b.binding),
                    binding_array_size: None,
                    dynamic_storage_buffer_offsets_index: None,
                    restrict_indexing: false,
                },
            );
        }
    }
    let options = hlsl::Options {
        shader_model: hlsl::ShaderModel::V5_0,
        binding_map,
        fake_missing_bindings: true,
        ..hlsl::Options::default()
    };
    let pipeline = hlsl::PipelineOptions::default();
    let mut source = String::new();
    let mut writer = hlsl::Writer::new(&mut source, &options, &pipeline);
    let reflection = writer
        .write(module, &info, None)
        .map_err(|e| anyhow!("hlsl-out: {e}"))?;
    let name_of = |wanted: &str| -> Result<String> {
        module
            .entry_points
            .iter()
            .zip(&reflection.entry_point_names)
            .find(|(ep, _)| ep.name == wanted)
            .ok_or_else(|| anyhow!("no entry point {wanted}"))?
            .1
            .clone()
            .map_err(|e| anyhow!("entry point {wanted}: {e}"))
    };
    Ok(Hlsl {
        vs_entry: name_of(VERTEX_ENTRY)?,
        ps_entry: name_of(FRAGMENT_ENTRY)?,
        source: plain_samplers(&source),
    })
}

/// A resource the module declares.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceKind {
    /// Uniform buffer of this size in bytes.
    Uniform(u32),
    Texture,
    Sampler,
}

/// Resources by `(group, binding)`.
pub fn resources(module: &Module) -> BTreeMap<(u32, u32), ResourceKind> {
    let mut out = BTreeMap::new();
    for (_, g) in module.global_variables.iter() {
        let Some(b) = g.binding else { continue };
        let kind = match (g.space, &module.types[g.ty].inner) {
            (AddressSpace::Uniform, inner) => {
                let size = inner.size(module.to_ctx());
                ResourceKind::Uniform(size)
            }
            (_, TypeInner::Image { .. }) => ResourceKind::Texture,
            (_, TypeInner::Sampler { .. }) => ResourceKind::Sampler,
            _ => continue,
        };
        out.insert((b.group, b.binding), kind);
    }
    out
}

pub struct Bytecode {
    pub vs: Vec<u8>,
    pub ps: Vec<u8>,
}

#[cfg(windows)]
fn compile_one(source: &str, entry: &str, target: &str) -> Result<Vec<u8>> {
    use std::ffi::CString;
    use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
    use windows::core::PCSTR;

    let entry = CString::new(entry)?;
    let target = CString::new(target)?;
    let mut code = None;
    let mut errors = None;
    // SAFETY: the source slice and the C strings outlive the call; the out pointers are valid.
    let r = unsafe {
        D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            PCSTR::null(),
            None,
            None,
            PCSTR(entry.as_ptr().cast()),
            PCSTR(target.as_ptr().cast()),
            D3DCOMPILE_OPTIMIZATION_LEVEL3,
            0,
            &mut code,
            Some(&mut errors),
        )
    };
    if let Err(e) = r {
        let msg = errors
            .map(|b| {
                // SAFETY: a blob owns `GetBufferSize` readable bytes at `GetBufferPointer`.
                let s = unsafe {
                    std::slice::from_raw_parts(b.GetBufferPointer().cast::<u8>(), b.GetBufferSize())
                };
                String::from_utf8_lossy(s).into_owned()
            })
            .unwrap_or_default();
        return Err(anyhow!("D3DCompile {target:?}: {e}: {}", first_lines(&msg)));
    }
    let code = code.ok_or_else(|| anyhow!("D3DCompile returned no code"))?;
    // SAFETY: as above.
    let bytes = unsafe {
        std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize())
    };
    Ok(bytes.to_vec())
}

fn first_lines(s: &str) -> String {
    s.lines().take(3).collect::<Vec<_>>().join(" | ")
}

/// HLSL and D3DCompile of one program.
#[cfg(windows)]
pub fn compile(module: &Module) -> Result<(Bytecode, Hlsl)> {
    let h = to_hlsl(module)?;
    let vs = compile_one(&h.source, &h.vs_entry, "vs_5_0")?;
    let ps = compile_one(&h.source, &h.ps_entry, "ps_5_0")?;
    Ok((Bytecode { vs, ps }, h))
}

/// Compiled bytecode on disk: `<dir>/<blake3 of the program key>.dxbc` = vs length (u32 LE), vs, ps.
pub struct BytecodeCache {
    dir: PathBuf,
}

impl BytecodeCache {
    pub fn new(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    fn path(&self, key: &[u8; 32]) -> PathBuf {
        let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
        self.dir.join(format!("{hex}.dxbc"))
    }

    pub fn load(&self, key: &[u8; 32]) -> Option<Bytecode> {
        let data = std::fs::read(self.path(key)).ok()?;
        let n = u32::from_le_bytes(data.get(..4)?.try_into().ok()?) as usize;
        let vs = data.get(4..4 + n)?.to_vec();
        let ps = data.get(4 + n..)?.to_vec();
        Some(Bytecode { vs, ps })
    }

    pub fn store(&self, key: &[u8; 32], code: &Bytecode) -> std::io::Result<()> {
        let mut data = (code.vs.len() as u32).to_le_bytes().to_vec();
        data.extend_from_slice(&code.vs);
        data.extend_from_slice(&code.ps);
        std::fs::write(self.path(key), data)
    }

    pub fn clear(&self) -> std::io::Result<()> {
        for e in std::fs::read_dir(&self.dir)? {
            let p = e?.path();
            if p.extension().is_some_and(|x| x == "dxbc") {
                std::fs::remove_file(p)?;
            }
        }
        Ok(())
    }
}

/// naga 30's HLSL writer declares every sampler through a sampler heap
/// (`nagaSamplerHeap[nagaGroupNSamplerIndexArray[i]]`, shader model 5.1 spaces and a structured
/// buffer of indices): Direct3D 12's model, not 11's. Rewrites those declarations as plain
/// `SamplerState name : register(si);`, the form shader model 5.0 compiles.
fn plain_samplers(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    for line in source.lines() {
        if line.starts_with("SamplerState nagaSamplerHeap")
            || line.starts_with("SamplerComparisonState nagaComparisonSamplerHeap")
            || line.starts_with("StructuredBuffer<uint> nagaGroup")
        {
            continue;
        }
        if let Some(rest) = line.strip_prefix("static const SamplerState ")
            && let Some((name, tail)) = rest.split_once(" = nagaSamplerHeap[")
            && let Some((_, idx)) = tail.split_once("SamplerIndexArray[")
            && let Some((idx, _)) = idx.split_once(']')
        {
            out.push_str(&format!("SamplerState {name} : register(s{idx});\n"));
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}
