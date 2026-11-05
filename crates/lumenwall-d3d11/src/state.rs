//! Keys of the fixed-function states (samplers, blending, rasteriser, depth). Plain data, tested
//! everywhere; `StateCache` (Windows) creates one Direct3D object per distinct key at load time.

/// Wrap mode of a sampler.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AddressMode {
    #[default]
    Repeat,
    Clamp,
    /// Clamp to a transparent black border (`D3D11_TEXTURE_ADDRESS_BORDER`, always available
    /// at feature level 11_0, where wgpu needs an optional feature).
    Border,
}

/// Sampler identity, the same choices as `lumenwall-render`'s sampler key plus anisotropy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SamplerKey {
    pub address: AddressMode,
    /// Nearest filtering, otherwise linear (trilinear with mipmaps).
    pub nearest: bool,
    /// Sample the mip chain; `false` clamps to level 0.
    pub mipmaps: bool,
    /// 1 = off; 2–16 with linear filtering. The wgpu path uses 8 for every linear 2D sampler.
    pub anisotropy: u8,
}

impl SamplerKey {
    /// The wgpu path's rule: anisotropy 8 when filtering is linear and the texture is 2D.
    #[must_use]
    pub fn new(address: AddressMode, nearest: bool, mipmaps: bool, volume: bool) -> Self {
        Self {
            address,
            nearest,
            mipmaps,
            anisotropy: if nearest || volume { 1 } else { 8 },
        }
    }

    /// Clamp-to-edge, linear, no mipmaps, no anisotropy (render targets, composition).
    pub const CLAMP_LINEAR: Self = Self {
        address: AddressMode::Clamp,
        nearest: false,
        mipmaps: false,
        anisotropy: 1,
    };
}

/// Blend factor (`D3D11_BLEND`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BlendFactor {
    Zero,
    One,
    SrcColor,
    InvSrcColor,
    SrcAlpha,
    InvSrcAlpha,
    DstColor,
    InvDstColor,
    DstAlpha,
    InvDstAlpha,
}

/// Blend operation (`D3D11_BLEND_OP`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BlendOp {
    #[default]
    Add,
    Subtract,
    RevSubtract,
    Min,
    Max,
}

/// `src · src_factor (op) dst · dst_factor`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlendComponent {
    pub src: BlendFactor,
    pub dst: BlendFactor,
    pub op: BlendOp,
}

/// Channels a draw writes (`D3D11_COLOR_WRITE_ENABLE` bits).
pub mod write_mask {
    pub const R: u8 = 1;
    pub const G: u8 = 2;
    pub const B: u8 = 4;
    pub const A: u8 = 8;
    pub const RGB: u8 = R | G | B;
    pub const ALL: u8 = RGB | A;
}

/// Blend state of the single colour target.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct BlendKey {
    /// `(colour, alpha)`; `None` disables blending.
    pub blend: Option<(BlendComponent, BlendComponent)>,
    /// [`write_mask`] bits.
    pub write_mask: u8,
}

impl BlendKey {
    /// Blending off, every channel written (`blending: normal`, which `lumenwall-render` also
    /// runs with the blend unit off: identical bits on UNORM targets).
    pub const OPAQUE: Self = Self {
        blend: None,
        write_mask: write_mask::ALL,
    };

    /// The same factors for colour and alpha, operation add (the spec 04 §14 table).
    #[must_use]
    pub fn uniform(src: BlendFactor, dst: BlendFactor) -> Self {
        let c = BlendComponent {
            src,
            dst,
            op: BlendOp::Add,
        };
        Self {
            blend: Some((c, c)),
            write_mask: write_mask::ALL,
        }
    }

    /// `blending: translucent`: `src·αs + dst·(1 − αs)`.
    #[must_use]
    pub fn translucent() -> Self {
        Self::uniform(BlendFactor::SrcAlpha, BlendFactor::InvSrcAlpha)
    }

    /// `blending: additive`: `src·αs + dst`.
    #[must_use]
    pub fn additive() -> Self {
        Self::uniform(BlendFactor::SrcAlpha, BlendFactor::One)
    }

    #[must_use]
    pub fn with_write_mask(mut self, mask: u8) -> Self {
        self.write_mask = mask & write_mask::ALL;
        self
    }
}

/// Faces a draw discards.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Cull {
    #[default]
    None,
    Front,
    Back,
}

/// Rasteriser state. `front_ccw` follows wgpu's `FrontFace` (y-up clip space), which is also
/// Direct3D's `FrontCounterClockwise`: `lumenwall-render`'s `FrontFace::Cw` is `false`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct RasterKey {
    pub cull: Cull,
    pub front_ccw: bool,
    /// Clip against the near and far planes (`false` = wgpu's `unclipped_depth`, used for
    /// passes into targets without depth, spec 04 Q4).
    pub depth_clip: bool,
    /// Multisample rasterisation (MSAA targets).
    pub multisample: bool,
    pub scissor: bool,
}

/// Depth test and write of a draw into a target with a depth buffer: "less or equal" or
/// "always", as `lumenwall-render`'s `DepthState`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct DepthKey {
    pub test: bool,
    pub write: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_blend_table() {
        let t = BlendKey::translucent();
        let (c, a) = t.blend.unwrap();
        assert_eq!(
            (c.src, c.dst, c.op),
            (
                BlendFactor::SrcAlpha,
                BlendFactor::InvSrcAlpha,
                BlendOp::Add
            )
        );
        assert_eq!(c, a);
        let (c, _) = BlendKey::additive().blend.unwrap();
        assert_eq!((c.src, c.dst), (BlendFactor::SrcAlpha, BlendFactor::One));
        assert_eq!(BlendKey::OPAQUE.blend, None);
        assert_eq!(
            BlendKey::OPAQUE.with_write_mask(write_mask::RGB).write_mask,
            7
        );
        assert_eq!(BlendKey::OPAQUE.with_write_mask(0xFF).write_mask, 15);
    }

    #[test]
    fn sampler_anisotropy_rule() {
        assert_eq!(
            SamplerKey::new(AddressMode::Repeat, false, true, false).anisotropy,
            8
        );
        assert_eq!(
            SamplerKey::new(AddressMode::Repeat, true, true, false).anisotropy,
            1
        );
        assert_eq!(
            SamplerKey::new(AddressMode::Repeat, false, true, true).anisotropy,
            1
        );
    }
}
