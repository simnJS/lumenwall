//! Blending modes, colour write masks and face culling (spec 04 §11.2, §11.5,
//! §11.6, §14; spec 03 §11.2).

use wgpu::{BlendComponent, BlendFactor, BlendOperation, BlendState, ColorWrites, Face, FrontFace};

/// Value of the material `blending` key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BlendMode {
    /// `src·1 + dst·0` for colour and alpha (default, and any unknown value).
    #[default]
    Normal,
    /// `src·αs + dst·(1 − αs)` for colour and alpha.
    Translucent,
    /// `src·αs + dst·1` for colour and alpha.
    Additive,
}

impl BlendMode {
    /// Parse a material value. Unknown values map to `Normal` and return
    /// `known = false` so the caller can log them (spec 04 §11.2).
    #[must_use]
    pub fn parse(value: &str) -> (Self, bool) {
        match value {
            "normal" => (Self::Normal, true),
            "translucent" => (Self::Translucent, true),
            "additive" => (Self::Additive, true),
            _ => (Self::Normal, false),
        }
    }

    /// Exact factors of the spec table: `(src, dst)`, identical for colour and
    /// alpha, operation = add.
    #[must_use]
    pub fn factors(self) -> (BlendFactor, BlendFactor) {
        match self {
            Self::Normal => (BlendFactor::One, BlendFactor::Zero),
            Self::Translucent => (BlendFactor::SrcAlpha, BlendFactor::OneMinusSrcAlpha),
            Self::Additive => (BlendFactor::SrcAlpha, BlendFactor::One),
        }
    }

    /// wgpu blend state.
    ///
    /// `Normal` returns `None` (blending disabled): `src·1 + dst·0` equals `src`
    /// bit for bit on the UNORM targets Lumen uses, and skipping the blend unit
    /// is never slower.
    #[must_use]
    pub fn state(self) -> Option<BlendState> {
        if self == Self::Normal {
            return None;
        }
        let (src_factor, dst_factor) = self.factors();
        let c = BlendComponent {
            src_factor,
            dst_factor,
            operation: BlendOperation::Add,
        };
        Some(BlendState { color: c, alpha: c })
    }

    /// Whether drawing with this mode depends on the destination content
    /// (used to detect frame-to-frame accumulation, spec 04 §15).
    #[must_use]
    pub fn reads_destination(self) -> bool {
        self != Self::Normal
    }
}

/// Colour write mask of a pass (spec 04 §11.6).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum WriteMask {
    /// R, G, B and A.
    #[default]
    All,
    /// R, G, B only: the last pass of every image keeps the destination alpha.
    Rgb,
}

impl WriteMask {
    #[must_use]
    pub fn writes(self) -> ColorWrites {
        match self {
            Self::All => ColorWrites::ALL,
            Self::Rgb => ColorWrites::COLOR,
        }
    }
}

/// Value of the material `cullmode` key.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum CullMode {
    /// `nocull` (default).
    #[default]
    None,
    /// `normal`: back faces are discarded.
    Back,
}

impl CullMode {
    /// Parse a material value; unknown values map to the default (`nocull`).
    #[must_use]
    pub fn parse(value: &str) -> (Self, bool) {
        match value {
            "nocull" => (Self::None, true),
            "normal" => (Self::Back, true),
            _ => (Self::None, false),
        }
    }

    #[must_use]
    pub fn face(self) -> Option<Face> {
        match self {
            Self::None => None,
            Self::Back => Some(Face::Back),
        }
    }
}

/// Front face used by every pass pipeline.
///
/// Front-facing triangles are clockwise in storage coordinates (rows growing
/// down, spec 04 §11.5). After the `y → −y` clip fix they are clockwise in
/// wgpu's y-up NDC, hence `Cw` (spec 04 §24.3, verified by a headless test).
pub const FRONT_FACE: FrontFace = FrontFace::Cw;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blend_table_is_exact() {
        assert_eq!(BlendMode::parse("normal"), (BlendMode::Normal, true));
        assert_eq!(
            BlendMode::parse("translucent"),
            (BlendMode::Translucent, true)
        );
        assert_eq!(BlendMode::parse("additive"), (BlendMode::Additive, true));
        assert_eq!(BlendMode::parse("multiply"), (BlendMode::Normal, false));
        assert_eq!(
            BlendMode::Normal.factors(),
            (BlendFactor::One, BlendFactor::Zero)
        );
        assert_eq!(BlendMode::Normal.state(), None);
        let t = BlendMode::Translucent.state().unwrap();
        assert_eq!(t.color.src_factor, BlendFactor::SrcAlpha);
        assert_eq!(t.color.dst_factor, BlendFactor::OneMinusSrcAlpha);
        assert_eq!(t.alpha, t.color, "alpha uses the colour factors");
        let a = BlendMode::Additive.state().unwrap();
        assert_eq!(a.color.src_factor, BlendFactor::SrcAlpha);
        assert_eq!(a.color.dst_factor, BlendFactor::One);
        assert_eq!(a.alpha, a.color);
        for m in [BlendMode::Translucent, BlendMode::Additive] {
            assert_eq!(m.state().unwrap().color.operation, BlendOperation::Add);
        }
    }

    #[test]
    fn masks_and_culling() {
        assert_eq!(
            WriteMask::Rgb.writes(),
            ColorWrites::RED | ColorWrites::GREEN | ColorWrites::BLUE
        );
        assert_eq!(WriteMask::All.writes(), ColorWrites::ALL);
        assert_eq!(CullMode::parse("normal"), (CullMode::Back, true));
        assert_eq!(CullMode::parse("nocull"), (CullMode::None, true));
        assert_eq!(CullMode::parse("weird"), (CullMode::None, false));
        assert_eq!(CullMode::Back.face(), Some(Face::Back));
    }
}
