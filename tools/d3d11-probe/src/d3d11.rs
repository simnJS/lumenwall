//! The workload on Direct3D 11: one immediate-context device, a flip-model DXGI swap chain,
//! immutable textures, render targets, a dynamic constant buffer per draw (`Map` with
//! `WRITE_DISCARD` every frame).

use std::ffi::c_void;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use lumenwall_formats::tex::GpuFormat;
use lumenwall_shader::ScalarKind;
use windows::Win32::Foundation::{HMODULE, TRUE};
use windows::Win32::Graphics::Direct3D::{
    D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP,
};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::{BOOL, Interface, PCSTR};

use crate::common::{self, Attr, cb_size, fill_frame, fill_pass, quad, slot_dims};
use crate::plan::{OUTPUT, Src, TargetFormat, Workload};
use crate::shaders::{BytecodeCache, ResourceKind, compile, resources};
use crate::sys::Window;
use crate::{Backend, LoadTimes};

fn dxgi_format(f: GpuFormat) -> DXGI_FORMAT {
    match f {
        GpuFormat::R8Unorm => DXGI_FORMAT_R8_UNORM,
        GpuFormat::Rg8Unorm => DXGI_FORMAT_R8G8_UNORM,
        GpuFormat::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
        GpuFormat::Bc1RgbaUnorm => DXGI_FORMAT_BC1_UNORM,
        GpuFormat::Bc2RgbaUnorm => DXGI_FORMAT_BC2_UNORM,
        GpuFormat::Bc3RgbaUnorm => DXGI_FORMAT_BC3_UNORM,
        GpuFormat::Bc7RgbaUnorm => DXGI_FORMAT_BC7_UNORM,
        GpuFormat::R16Float => DXGI_FORMAT_R16_FLOAT,
        GpuFormat::Rg16Float => DXGI_FORMAT_R16G16_FLOAT,
        GpuFormat::Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
        GpuFormat::Rgb10a2Unorm => DXGI_FORMAT_R10G10B10A2_UNORM,
    }
}

fn target_format(f: TargetFormat) -> DXGI_FORMAT {
    match f {
        TargetFormat::Rgba8 => DXGI_FORMAT_R8G8B8A8_UNORM,
        TargetFormat::R8 => DXGI_FORMAT_R8_UNORM,
        TargetFormat::Rg8 => DXGI_FORMAT_R8G8_UNORM,
    }
}

struct Program {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    layout: ID3D11InputLayout,
    attrs: Vec<Attr>,
    stride: u32,
    /// Highest texture slot the program declares, plus one.
    slots: usize,
    time_offset: Option<usize>,
    cb_bytes: u32,
}

struct DrawState {
    program: usize,
    target: usize,
    vb: ID3D11Buffer,
    cb: ID3D11Buffer,
    data: Vec<u8>,
    srvs: Vec<Option<ID3D11ShaderResourceView>>,
}

pub struct D3d11 {
    device: ID3D11Device,
    ctx: ID3D11DeviceContext,
    swap: IDXGISwapChain1,
    back_rtv: ID3D11RenderTargetView,
    // After `load`:
    textures: Vec<Vec<ID3D11ShaderResourceView>>,
    blank: Option<ID3D11ShaderResourceView>,
    target_rtv: Vec<ID3D11RenderTargetView>,
    target_srv: Vec<ID3D11ShaderResourceView>,
    programs: Vec<Program>,
    draws: Vec<DrawState>,
    present: Option<DrawState>,
    frame_cb: Option<ID3D11Buffer>,
    frame_data: Vec<u8>,
    sampler: Option<ID3D11SamplerState>,
}

fn pick_adapter(factory: &IDXGIFactory2) -> Result<(IDXGIAdapter1, String)> {
    let mut best: Option<(IDXGIAdapter1, u64, String)> = None;
    let mut i = 0;
    // SAFETY: plain DXGI enumeration.
    while let Ok(a) = unsafe { factory.EnumAdapters1(i) } {
        i += 1;
        let d = unsafe { a.GetDesc1()? };
        if d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
            continue;
        }
        let name = String::from_utf16_lossy(&d.Description)
            .trim_end_matches('\0')
            .to_owned();
        let mem = d.DedicatedVideoMemory as u64;
        if best.as_ref().is_none_or(|b| mem > b.1) {
            best = Some((a, mem, name));
        }
    }
    best.map(|(a, _, n)| (a, n))
        .ok_or_else(|| anyhow!("no hardware adapter"))
}

impl Backend for D3d11 {
    fn create(window: &Window) -> Result<(Self, f64, String)> {
        let t = Instant::now();
        // SAFETY: Direct3D 11 / DXGI calls with valid descriptors and out parameters.
        unsafe {
            let factory: IDXGIFactory2 = CreateDXGIFactory1()?;
            let (adapter, name) = pick_adapter(&factory)?;
            let mut device = None;
            let mut ctx = None;
            D3D11CreateDevice(
                &adapter.cast::<IDXGIAdapter>()?,
                D3D_DRIVER_TYPE_UNKNOWN,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut ctx),
            )?;
            let device = device.ok_or_else(|| anyhow!("no device"))?;
            let ctx = ctx.ok_or_else(|| anyhow!("no context"))?;
            let desc = DXGI_SWAP_CHAIN_DESC1 {
                Width: OUTPUT.0,
                Height: OUTPUT.1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                Stereo: BOOL(0),
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
                BufferCount: 2,
                Scaling: DXGI_SCALING_STRETCH,
                SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
                AlphaMode: DXGI_ALPHA_MODE_IGNORE,
                Flags: 0,
            };
            let swap = factory.CreateSwapChainForHwnd(&device, window.hwnd, &desc, None, None)?;
            let _ = factory.MakeWindowAssociation(window.hwnd, DXGI_MWA_NO_ALT_ENTER);
            let back: ID3D11Texture2D = swap.GetBuffer(0)?;
            let mut back_rtv = None;
            device.CreateRenderTargetView(&back, None, Some(&mut back_rtv))?;
            let back_rtv = back_rtv.ok_or_else(|| anyhow!("no back buffer view"))?;
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            Ok((
                Self {
                    device,
                    ctx,
                    swap,
                    back_rtv,
                    textures: Vec::new(),
                    blank: None,
                    target_rtv: Vec::new(),
                    target_srv: Vec::new(),
                    programs: Vec::new(),
                    draws: Vec::new(),
                    present: None,
                    frame_cb: None,
                    frame_data: Vec::new(),
                    sampler: None,
                },
                ms,
                format!("{name} (Direct3D 11, feature level 11.0)"),
            ))
        }
    }

    fn load(&mut self, w: &mut Workload, cache: &BytecodeCache) -> Result<LoadTimes> {
        let mut times = LoadTimes::default();
        // SAFETY: Direct3D 11 resource creation with valid descriptors; initial data slices
        // outlive the calls.
        unsafe {
            // Shaders: bytecode cache first, D3DCompile on a miss.
            let t = Instant::now();
            let mut codes = Vec::new();
            for p in &w.programs {
                let code = match cache.load(&p.key) {
                    Some(c) => c,
                    None => {
                        let tc = Instant::now();
                        let (c, _) = compile(&p.program.module)?;
                        times.compile_ms += tc.elapsed().as_secs_f64() * 1000.0;
                        let _ = cache.store(&p.key, &c);
                        c
                    }
                };
                codes.push(code);
            }
            times.shaders_ms = t.elapsed().as_secs_f64() * 1000.0 - times.compile_ms;

            // State objects, shader objects, input layouts.
            let t = Instant::now();
            for (i, p) in w.programs.iter().enumerate() {
                let code = &codes[i];
                let mut vs = None;
                self.device
                    .CreateVertexShader(&code.vs, None, Some(&mut vs))?;
                let mut ps = None;
                self.device
                    .CreatePixelShader(&code.ps, None, Some(&mut ps))?;
                let (attrs, stride) = common::attributes(p);
                let elements: Vec<D3D11_INPUT_ELEMENT_DESC> = attrs
                    .iter()
                    .map(|a| D3D11_INPUT_ELEMENT_DESC {
                        SemanticName: PCSTR(c"LOC".as_ptr().cast()),
                        SemanticIndex: a.location,
                        Format: match (a.kind, a.comps) {
                            (ScalarKind::Float, 1) => DXGI_FORMAT_R32_FLOAT,
                            (ScalarKind::Float, 2) => DXGI_FORMAT_R32G32_FLOAT,
                            (ScalarKind::Float, 3) => DXGI_FORMAT_R32G32B32_FLOAT,
                            (ScalarKind::Float, _) => DXGI_FORMAT_R32G32B32A32_FLOAT,
                            (_, 1) => DXGI_FORMAT_R32_SINT,
                            (_, 2) => DXGI_FORMAT_R32G32_SINT,
                            (_, 3) => DXGI_FORMAT_R32G32B32_SINT,
                            (_, _) => DXGI_FORMAT_R32G32B32A32_SINT,
                        },
                        InputSlot: 0,
                        AlignedByteOffset: a.offset,
                        InputSlotClass: D3D11_INPUT_PER_VERTEX_DATA,
                        InstanceDataStepRate: 0,
                    })
                    .collect();
                let mut layout = None;
                self.device
                    .CreateInputLayout(&elements, &code.vs, Some(&mut layout))?;
                let res = resources(&p.program.module);
                let slots = res
                    .iter()
                    .filter(|((g, _), k)| *g == 1 && **k == ResourceKind::Texture)
                    .map(|((_, b), _)| (*b / 2) as usize + 1)
                    .max()
                    .unwrap_or(0);
                let cb_bytes = match res.get(&(0, 0)) {
                    Some(ResourceKind::Uniform(s)) => cb_size(*s),
                    _ => 16,
                };
                let time_offset = p
                    .program
                    .reflection
                    .uniform_block
                    .as_ref()
                    .and_then(|b| b.members.iter().find(|m| m.name == "g_Time"))
                    .map(|m| m.offset as usize);
                self.programs.push(Program {
                    vs: vs.ok_or_else(|| anyhow!("vertex shader"))?,
                    ps: ps.ok_or_else(|| anyhow!("pixel shader"))?,
                    layout: layout.ok_or_else(|| anyhow!("input layout"))?,
                    attrs,
                    stride,
                    slots,
                    time_offset,
                    cb_bytes,
                });
            }
            let mut sampler = None;
            self.device.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_LINEAR,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MaxAnisotropy: 1,
                    ComparisonFunc: D3D11_COMPARISON_NEVER,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut sampler),
            )?;
            self.sampler = sampler;
            let mut blend = None;
            let mut bd = D3D11_BLEND_DESC::default();
            bd.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: TRUE,
                SrcBlend: D3D11_BLEND_SRC_ALPHA,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: 0xF,
            };
            self.device.CreateBlendState(&bd, Some(&mut blend))?;
            self.ctx.OMSetBlendState(blend.as_ref(), None, 0xFFFF_FFFF);
            let mut raster = None;
            self.device.CreateRasterizerState(
                &D3D11_RASTERIZER_DESC {
                    FillMode: D3D11_FILL_SOLID,
                    CullMode: D3D11_CULL_NONE,
                    DepthClipEnable: TRUE,
                    ..Default::default()
                },
                Some(&mut raster),
            )?;
            self.ctx.RSSetState(raster.as_ref());
            self.ctx
                .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLESTRIP);
            times.pipelines_ms = t.elapsed().as_secs_f64() * 1000.0;

            // Textures: the real files, immutable, one subresource per mip.
            let t = Instant::now();
            for tex in &mut w.textures {
                let images = std::mem::take(&mut tex.images);
                let mut views = Vec::new();
                for mips in &images {
                    let init: Vec<D3D11_SUBRESOURCE_DATA> = mips
                        .iter()
                        .map(|(mw, _, data)| D3D11_SUBRESOURCE_DATA {
                            pSysMem: data.as_ptr().cast::<c_void>(),
                            SysMemPitch: tex.format.bytes_per_row(*mw) as u32,
                            SysMemSlicePitch: 0,
                        })
                        .collect();
                    let (bw, bh) = mips.first().map_or((1, 1), |m| (m.0, m.1));
                    let desc = D3D11_TEXTURE2D_DESC {
                        Width: bw,
                        Height: bh,
                        MipLevels: mips.len() as u32,
                        ArraySize: 1,
                        Format: dxgi_format(tex.format),
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_IMMUTABLE,
                        BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                        CPUAccessFlags: 0,
                        MiscFlags: 0,
                    };
                    let mut t2d = None;
                    self.device
                        .CreateTexture2D(&desc, Some(init.as_ptr()), Some(&mut t2d))
                        .map_err(|e| anyhow!("texture {}: {e}", tex.name))?;
                    let t2d = t2d.ok_or_else(|| anyhow!("texture"))?;
                    let mut srv = None;
                    self.device
                        .CreateShaderResourceView(&t2d, None, Some(&mut srv))?;
                    views.push(srv.ok_or_else(|| anyhow!("srv"))?);
                }
                self.textures.push(views);
            }
            // A 1x1 transparent texture for slots a draw does not feed.
            let blank_data = [0u8; 4];
            let blank_init = D3D11_SUBRESOURCE_DATA {
                pSysMem: blank_data.as_ptr().cast(),
                SysMemPitch: 4,
                SysMemSlicePitch: 0,
            };
            let mut blank = None;
            self.device.CreateTexture2D(
                &D3D11_TEXTURE2D_DESC {
                    Width: 1,
                    Height: 1,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                    SampleDesc: DXGI_SAMPLE_DESC {
                        Count: 1,
                        Quality: 0,
                    },
                    Usage: D3D11_USAGE_IMMUTABLE,
                    BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                },
                Some(&blank_init),
                Some(&mut blank),
            )?;
            let mut blank_srv = None;
            self.device.CreateShaderResourceView(
                &blank.ok_or_else(|| anyhow!("blank"))?,
                None,
                Some(&mut blank_srv),
            )?;
            self.blank = blank_srv;
            self.wait_idle();
            times.upload_ms = t.elapsed().as_secs_f64() * 1000.0;

            // Render targets.
            let t = Instant::now();
            for rt in &w.targets {
                let mut t2d = None;
                self.device.CreateTexture2D(
                    &D3D11_TEXTURE2D_DESC {
                        Width: rt.width,
                        Height: rt.height,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: target_format(rt.format),
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        Usage: D3D11_USAGE_DEFAULT,
                        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0)
                            as u32,
                        CPUAccessFlags: 0,
                        MiscFlags: 0,
                    },
                    None,
                    Some(&mut t2d),
                )?;
                let t2d = t2d.ok_or_else(|| anyhow!("target"))?;
                let mut rtv = None;
                self.device
                    .CreateRenderTargetView(&t2d, None, Some(&mut rtv))?;
                let mut srv = None;
                self.device
                    .CreateShaderResourceView(&t2d, None, Some(&mut srv))?;
                self.target_rtv.push(rtv.ok_or_else(|| anyhow!("rtv"))?);
                self.target_srv.push(srv.ok_or_else(|| anyhow!("srv"))?);
            }
            times.targets_ms = t.elapsed().as_secs_f64() * 1000.0;

            // Per-draw constant buffers, vertex buffers, SRV tables; the frame block.
            let t = Instant::now();
            let mut draws = Vec::new();
            for d in &w.draws {
                draws.push(self.draw_state(w, d.program, d.target, &d.slots, d.coverage, d)?);
            }
            self.draws = draws;
            let pd = crate::plan::Draw {
                program: w.present_program,
                slots: vec![(0, Src::Target(0))],
                target: usize::MAX,
                label: "present".into(),
                coverage: (1.0, 1.0),
            };
            self.present = Some(self.draw_state(w, pd.program, 0, &pd.slots, pd.coverage, &pd)?);
            let frame_bytes = w
                .programs
                .iter()
                .find_map(|p| match resources(&p.program.module).get(&(0, 1)) {
                    Some(ResourceKind::Uniform(s)) => Some(cb_size(*s)),
                    _ => None,
                })
                .unwrap_or(16);
            self.frame_data = vec![0; frame_bytes as usize];
            self.frame_cb = Some(self.dynamic_cb(frame_bytes)?);
            times.pipelines_ms += t.elapsed().as_secs_f64() * 1000.0;
            self.wait_idle();
        }
        Ok(times)
    }

    fn frame(&mut self, w: &Workload, time: f32) -> Result<()> {
        // SAFETY: Direct3D 11 immediate-context calls on resources this struct owns.
        unsafe {
            let ctx = &self.ctx;
            let frame_cb = self
                .frame_cb
                .as_ref()
                .ok_or_else(|| anyhow!("not loaded"))?;
            fill_frame(&mut self.frame_data, time);
            Self::upload(ctx, frame_cb, &self.frame_data)?;
            ctx.PSSetSamplers(
                0,
                Some(&std::array::from_fn::<_, 8, _>(|_| self.sampler.clone())),
            );
            let vp = |wd: u32, ht: u32| D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: wd as f32,
                Height: ht as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            let mut cleared = vec![false; w.targets.len()];
            let mut bound = usize::MAX;
            for d in &mut self.draws {
                if d.target != bound {
                    bound = d.target;
                    // Unbind the sampled textures before the target changes (hazards).
                    ctx.PSSetShaderResources(0, Some(&[const { None }; 8]));
                    ctx.OMSetRenderTargets(Some(&[Some(self.target_rtv[bound].clone())]), None);
                    ctx.RSSetViewports(Some(&[vp(
                        w.targets[bound].width,
                        w.targets[bound].height,
                    )]));
                    if !cleared[bound] {
                        cleared[bound] = true;
                        let colour = if bound == 0 {
                            [0.0, 0.0, 0.0, 1.0]
                        } else {
                            [0.0; 4]
                        };
                        ctx.ClearRenderTargetView(&self.target_rtv[bound], &colour);
                    }
                }
                let p = &self.programs[d.program];
                if let Some(o) = p.time_offset
                    && let Some(dst) = d.data.get_mut(o..o + 4)
                {
                    dst.copy_from_slice(&time.to_le_bytes());
                }
                Self::upload(ctx, &d.cb, &d.data)?;
                Self::draw(ctx, p, d, frame_cb);
            }
            // The scene target to the swap chain.
            ctx.PSSetShaderResources(0, Some(&[const { None }; 8]));
            ctx.OMSetRenderTargets(Some(&[Some(self.back_rtv.clone())]), None);
            ctx.RSSetViewports(Some(&[vp(OUTPUT.0, OUTPUT.1)]));
            if let Some(d) = &mut self.present {
                let p = &self.programs[d.program];
                Self::draw(ctx, p, d, frame_cb);
            }
            self.swap.Present(0, DXGI_PRESENT(0)).ok()?;
        }
        Ok(())
    }
}

impl D3d11 {
    unsafe fn dynamic_cb(&self, bytes: u32) -> Result<ID3D11Buffer> {
        let mut b = None;
        // SAFETY: valid descriptor.
        unsafe {
            self.device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: bytes,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    MiscFlags: 0,
                    StructureByteStride: 0,
                },
                None,
                Some(&mut b),
            )?;
        }
        b.ok_or_else(|| anyhow!("constant buffer"))
    }

    unsafe fn upload(ctx: &ID3D11DeviceContext, cb: &ID3D11Buffer, data: &[u8]) -> Result<()> {
        // SAFETY: the buffer is dynamic and at least `data.len()` bytes.
        unsafe {
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m))?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), m.pData.cast::<u8>(), data.len());
            ctx.Unmap(cb, 0);
        }
        Ok(())
    }

    unsafe fn draw(ctx: &ID3D11DeviceContext, p: &Program, d: &DrawState, frame_cb: &ID3D11Buffer) {
        // SAFETY: the objects belong to the device of `ctx`.
        unsafe {
            ctx.IASetInputLayout(&p.layout);
            ctx.IASetVertexBuffers(0, 1, Some(&Some(d.vb.clone())), Some(&p.stride), Some(&0));
            ctx.VSSetShader(&p.vs, None);
            ctx.PSSetShader(&p.ps, None);
            let cbs = [Some(d.cb.clone()), Some(frame_cb.clone())];
            ctx.VSSetConstantBuffers(0, Some(&cbs));
            ctx.PSSetConstantBuffers(0, Some(&cbs));
            ctx.PSSetShaderResources(0, Some(&d.srvs));
            ctx.Draw(4, 0);
        }
    }

    unsafe fn draw_state(
        &self,
        w: &Workload,
        program: usize,
        target: usize,
        slots: &[(u32, Src)],
        coverage: (f32, f32),
        draw: &crate::plan::Draw,
    ) -> Result<DrawState> {
        let p = &self.programs[program];
        let shader = &w.programs[program];
        let vdata = quad(&p.attrs, p.stride.max(4), coverage);
        let mut vb = None;
        let mut data = vec![0u8; p.cb_bytes as usize];
        fill_pass(&mut data, shader, 0.0, &|s| slot_dims(w, draw, s));
        let init = D3D11_SUBRESOURCE_DATA {
            pSysMem: vdata.as_ptr().cast(),
            SysMemPitch: 0,
            SysMemSlicePitch: 0,
        };
        // SAFETY: valid descriptors; `vdata` outlives the call.
        unsafe {
            self.device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: vdata.len() as u32,
                    Usage: D3D11_USAGE_IMMUTABLE,
                    BindFlags: D3D11_BIND_VERTEX_BUFFER.0 as u32,
                    CPUAccessFlags: 0,
                    MiscFlags: 0,
                    StructureByteStride: 0,
                },
                Some(&init),
                Some(&mut vb),
            )?;
        }
        let mut srvs: Vec<Option<ID3D11ShaderResourceView>> =
            vec![self.blank.clone(); p.slots.max(1)];
        for (slot, src) in slots {
            let Some(entry) = srvs.get_mut(*slot as usize) else {
                continue;
            };
            *entry = Some(match src {
                Src::Tex(i) => self.textures[*i]
                    .first()
                    .cloned()
                    .ok_or_else(|| anyhow!("texture without image"))?,
                Src::Target(i) => self.target_srv[*i].clone(),
            });
        }
        Ok(DrawState {
            program,
            target,
            vb: vb.ok_or_else(|| anyhow!("vertex buffer"))?,
            // SAFETY: valid descriptor.
            cb: unsafe { self.dynamic_cb(p.cb_bytes)? },
            data,
            srvs,
        })
    }

    /// Waits until the GPU has finished everything submitted (an event query).
    fn wait_idle(&self) {
        // SAFETY: valid query descriptor; `done` outlives the polls.
        unsafe {
            let mut q = None;
            if self
                .device
                .CreateQuery(
                    &D3D11_QUERY_DESC {
                        Query: D3D11_QUERY_EVENT,
                        MiscFlags: 0,
                    },
                    Some(&mut q),
                )
                .is_err()
            {
                return;
            }
            let Some(q) = q else { return };
            self.ctx.End(&q);
            self.ctx.Flush();
            let mut done = BOOL(0);
            let deadline = Instant::now() + Duration::from_secs(20);
            while !done.as_bool() && Instant::now() < deadline {
                let _ = self.ctx.GetData(
                    &q,
                    Some((&raw mut done).cast::<c_void>()),
                    std::mem::size_of::<BOOL>() as u32,
                    0,
                );
                if !done.as_bool() {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }
}
