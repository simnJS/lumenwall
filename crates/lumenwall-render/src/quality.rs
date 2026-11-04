//! Quality settings of the scene renderer: anti-aliasing, texture resolution, HDR bloom and the
//! resolution scenes are rendered at.
//!
//! One plain struct, [`QualitySettings`], is handed to the scene builder
//! ([`crate::SceneBuilder::set_quality`]) and to the engine (`lumenwall_engine::EngineSettings`).
//! The default costs exactly what the renderer cost before the settings existed: no
//! multisampling, full-size textures, 8-bit targets, scenes rendered at the displayed size.

use std::fmt;
use std::str::FromStr;

/// Multisample anti-aliasing of the geometry drawn into the scene target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Msaa {
    /// One sample per pixel (default).
    #[default]
    Off,
    X2,
    X4,
    X8,
}

impl Msaa {
    /// All values, from the cheapest.
    pub const ALL: [Msaa; 4] = [Msaa::Off, Msaa::X2, Msaa::X4, Msaa::X8];

    /// Samples per pixel.
    #[must_use]
    pub fn samples(self) -> u32 {
        match self {
            Msaa::Off => 1,
            Msaa::X2 => 2,
            Msaa::X4 => 4,
            Msaa::X8 => 8,
        }
    }

    /// The highest level at or below `self` that the adapter supports for `format`
    /// (`supported` is a bit set of sample counts, wgpu's `TextureFormatFeatureFlags`).
    #[must_use]
    pub fn clamp_to(self, supported: impl Fn(u32) -> bool) -> Msaa {
        Msaa::ALL
            .iter()
            .rev()
            .copied()
            .filter(|m| m.samples() <= self.samples())
            .find(|m| m.samples() == 1 || supported(m.samples()))
            .unwrap_or(Msaa::Off)
    }

    /// Setting name: `off`, `x2`, `x4`, `x8` (the names Wallpaper Engine's settings file uses).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Msaa::Off => "off",
            Msaa::X2 => "x2",
            Msaa::X4 => "x4",
            Msaa::X8 => "x8",
        }
    }
}

impl fmt::Display for Msaa {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Msaa {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "0" | "1" => Ok(Msaa::Off),
            "x2" | "2" => Ok(Msaa::X2),
            "x4" | "4" => Ok(Msaa::X4),
            "x8" | "8" => Ok(Msaa::X8),
            other => Err(format!("unknown anti-aliasing '{other}' (off, x2, x4, x8)")),
        }
    }
}

/// Size of the textures loaded from `.tex` files: the top mip levels are not uploaded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TextureResolution {
    /// Every level (default).
    #[default]
    Full,
    /// Half the width and height (a quarter of the memory).
    Half,
    /// A quarter of the width and height.
    Quarter,
}

impl TextureResolution {
    /// Mip levels dropped from the top of each texture's chain.
    #[must_use]
    pub fn skipped_levels(self) -> u32 {
        match self {
            TextureResolution::Full => 0,
            TextureResolution::Half => 1,
            TextureResolution::Quarter => 2,
        }
    }

    /// Setting name: `full`, `half`, `quarter`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            TextureResolution::Full => "full",
            TextureResolution::Half => "half",
            TextureResolution::Quarter => "quarter",
        }
    }
}

impl fmt::Display for TextureResolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for TextureResolution {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "full" => Ok(TextureResolution::Full),
            "half" => Ok(TextureResolution::Half),
            "quarter" => Ok(TextureResolution::Quarter),
            other => Err(format!(
                "unknown texture resolution '{other}' (full, half, quarter)"
            )),
        }
    }
}

/// Precision of the render targets of a scene whose `general.hdr` is true.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum HdrMode {
    /// 8-bit targets whatever the scene says (default: what Lumenwall always did).
    #[default]
    Off,
    /// 16-bit float targets for scenes that declare `hdr`: bloom and effects keep values above 1.
    Scene,
}

impl HdrMode {
    /// Setting name: `off`, `scene`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            HdrMode::Off => "off",
            HdrMode::Scene => "scene",
        }
    }
}

impl fmt::Display for HdrMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for HdrMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(HdrMode::Off),
            "scene" | "on" => Ok(HdrMode::Scene),
            other => Err(format!("unknown hdr mode '{other}' (off, scene)")),
        }
    }
}

/// Everything the user can trade quality against cost with. `Default` is the cheapest setting
/// and what every scene rendered before these settings existed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct QualitySettings {
    /// Anti-aliasing of the scene target.
    pub msaa: Msaa,
    /// Texture size.
    pub texture_resolution: TextureResolution,
    /// HDR targets for scenes that ask for them.
    pub hdr: HdrMode,
    /// Render image chains and effect targets at the scene's own size even when it is shown
    /// smaller (otherwise at the displayed size). Also switched on by the environment variable
    /// `LUMENWALL_FULL_SIZE_CHAINS=1`.
    pub native_resolution: bool,
}

impl QualitySettings {
    /// Whether any setting costs more than the default.
    #[must_use]
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Drops the top mip levels of `levels` according to the texture resolution, always keeping
    /// at least one level. Returns how many were dropped.
    #[must_use]
    pub fn texture_levels_to_skip(&self, levels: usize) -> usize {
        (self.texture_resolution.skipped_levels() as usize).min(levels.saturating_sub(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_the_cheapest() {
        let q = QualitySettings::default();
        assert_eq!(q.msaa.samples(), 1);
        assert_eq!(q.texture_resolution.skipped_levels(), 0);
        assert_eq!(q.hdr, HdrMode::Off);
        assert!(!q.native_resolution);
        assert!(q.is_default());
    }

    #[test]
    fn names_round_trip() {
        for m in Msaa::ALL {
            assert_eq!(m.to_string().parse::<Msaa>(), Ok(m));
        }
        for t in [
            TextureResolution::Full,
            TextureResolution::Half,
            TextureResolution::Quarter,
        ] {
            assert_eq!(t.to_string().parse::<TextureResolution>(), Ok(t));
        }
        for h in [HdrMode::Off, HdrMode::Scene] {
            assert_eq!(h.to_string().parse::<HdrMode>(), Ok(h));
        }
        assert!("x16".parse::<Msaa>().is_err());
        assert!("tiny".parse::<TextureResolution>().is_err());
    }

    #[test]
    fn msaa_falls_back_to_the_best_supported_level() {
        assert_eq!(Msaa::X8.clamp_to(|s| s <= 4), Msaa::X4);
        assert_eq!(Msaa::X8.clamp_to(|s| s <= 2), Msaa::X2);
        assert_eq!(Msaa::X4.clamp_to(|_| false), Msaa::Off);
        assert_eq!(Msaa::X2.clamp_to(|_| true), Msaa::X2);
        assert_eq!(Msaa::Off.clamp_to(|_| true), Msaa::Off);
    }

    #[test]
    fn texture_levels_keep_at_least_one() {
        let q = QualitySettings {
            texture_resolution: TextureResolution::Quarter,
            ..Default::default()
        };
        assert_eq!(q.texture_levels_to_skip(10), 2);
        assert_eq!(q.texture_levels_to_skip(2), 1);
        assert_eq!(q.texture_levels_to_skip(1), 0);
        assert_eq!(QualitySettings::default().texture_levels_to_skip(10), 0);
    }
}
