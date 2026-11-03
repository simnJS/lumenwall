//! Sprite sheet data (spec 05 §4.3) and texture dimensions used by the uniforms (§12.2).

use serde_json::Value as Json;

/// Sprite sheet layout of the main texture. `frames == 0` means no sprite sheet.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SpriteSheet {
    /// `C`.
    pub columns: u32,
    /// `R`.
    pub rows: u32,
    /// `F`.
    pub frames: u32,
    /// `T_anim`, seconds per cycle as stored in the texture. Not used for timing: a particle
    /// plays its sheet over its own lifetime (Wallpaper Engine documentation).
    pub duration: f32,
}

/// Frames of an animated TEX texture, as read by the texture loader.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AnimatedTexture {
    pub frame0_width: f32,
    pub frame0_height: f32,
    pub frame_count: u32,
    /// Sum of the frame durations, seconds.
    pub total_duration: f32,
}

/// Main texture dimensions, for `g_RenderVar1`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TextureInfo {
    /// GPU texture size (with padding).
    pub gpu_width: u32,
    pub gpu_height: u32,
    /// Real image size (per frame for an animated texture without sprite sheet).
    pub image_width: u32,
    pub image_height: u32,
}

fn round_ratio(a: f32, b: f32) -> u32 {
    // Saturating conversion: negative/NaN → 0.
    (a / b).round() as u32
}

impl SpriteSheet {
    /// True when sprite sheet mode is active (`F > 0`, combo `SPRITESHEET`).
    pub fn active(&self) -> bool {
        self.frames > 0
    }

    /// Resolves the sprite sheet of the main texture (§4.3):
    /// `tex_json` is the parsed `<texture>.tex-json` companion (if any), `animated` the frames of an
    /// animated TEX texture (if any), `image_width/height` the real image size.
    pub fn resolve(
        tex_json: Option<&Json>,
        animated: Option<&AnimatedTexture>,
        image_width: u32,
        image_height: u32,
    ) -> SpriteSheet {
        let (iw, ih) = (image_width as f32, image_height as f32);
        // (2) `spritesheetsequences` of the companion file has priority.
        if let Some(seq) = tex_json
            .and_then(|j| j.get("spritesheetsequences"))
            .and_then(Json::as_array)
            .and_then(|a| a.first())
        {
            let num = |k: &str, d: f64| seq.get(k).and_then(Json::as_f64).unwrap_or(d);
            let frames = num("frames", 0.0);
            let width = num("width", 0.0);
            let height = num("height", 0.0);
            if frames > 0.0 && width > 0.0 && height > 0.0 && image_width > 0 && image_height > 0 {
                return SpriteSheet {
                    columns: round_ratio(iw, width as f32),
                    rows: round_ratio(ih, height as f32),
                    frames: frames as u32,
                    duration: num("duration", 1.0) as f32,
                };
            }
        }
        // (1) Animated texture laid out as a grid.
        if let Some(a) = animated {
            let c = round_ratio(iw, a.frame0_width);
            let r = round_ratio(ih, a.frame0_height);
            if c > 0 && r > 0 && u64::from(c) * u64::from(r) >= u64::from(a.frame_count) {
                return SpriteSheet {
                    columns: c,
                    rows: r,
                    frames: a.frame_count,
                    duration: a.total_duration,
                };
            }
        }
        SpriteSheet::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tex_json_has_priority() {
        let tj = json!({"spritesheetsequences": [{"frames": 6, "width": 64, "height": 32, "duration": 2.5}]});
        let anim = AnimatedTexture {
            frame0_width: 128.0,
            frame0_height: 128.0,
            frame_count: 2,
            total_duration: 1.0,
        };
        let s = SpriteSheet::resolve(Some(&tj), Some(&anim), 256, 128);
        assert_eq!(
            s,
            SpriteSheet {
                columns: 4,
                rows: 4,
                frames: 6,
                duration: 2.5
            }
        );
        // Missing duration defaults to 1.
        let tj = json!({"spritesheetsequences": [{"frames": 4, "width": 100, "height": 100}]});
        let s = SpriteSheet::resolve(Some(&tj), None, 250, 199);
        assert_eq!(
            s,
            SpriteSheet {
                columns: 3,
                rows: 2,
                frames: 4,
                duration: 1.0
            }
        );
    }

    #[test]
    fn animated_texture_grid() {
        let anim = AnimatedTexture {
            frame0_width: 64.0,
            frame0_height: 64.0,
            frame_count: 8,
            total_duration: 0.8,
        };
        assert_eq!(
            SpriteSheet::resolve(None, Some(&anim), 256, 128),
            SpriteSheet {
                columns: 4,
                rows: 2,
                frames: 8,
                duration: 0.8
            }
        );
        // C·R < F: not a sprite sheet (GIF-like animated texture).
        let gif = AnimatedTexture {
            frame0_width: 64.0,
            frame0_height: 64.0,
            frame_count: 9,
            total_duration: 0.8,
        };
        assert!(!SpriteSheet::resolve(None, Some(&gif), 256, 128).active());
        // Invalid companion falls back to (1).
        let tj = json!({"spritesheetsequences": [{"frames": 0, "width": 10, "height": 10}]});
        assert!(SpriteSheet::resolve(Some(&tj), Some(&anim), 256, 128).active());
        assert!(!SpriteSheet::resolve(Some(&tj), None, 256, 128).active());
    }
}
