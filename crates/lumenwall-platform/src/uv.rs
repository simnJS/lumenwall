//! Texture coordinates used to copy the wallpaper's off-screen image onto an output (spec 07 §4.4, §4.5).
//!
//! The mouse mapping of [`crate::mouse`] needs the same UVs, so they live here as pure functions.
//! All arithmetic is `f32` like the reference; "new" sizes are truncated to integers as the spec states.

/// Scaling mode (`--scaling`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScalingMode {
    /// Distort to the viewport.
    Stretch,
    /// Fit inside, bands outside `[0,1]`.
    Fit,
    /// Cover, cropping.
    Fill,
    /// Orientation-based rule of the reference.
    #[default]
    Default,
}

/// Texture coordinate rectangle: `u` from `u0` to `u1`, `v` from `v0` to `v1` (may leave `[0,1]`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UvRect {
    /// U at the left edge.
    pub u0: f32,
    /// U at the right edge.
    pub u1: f32,
    /// V at the first edge (bottom in GL terms when `vflip`).
    pub v0: f32,
    /// V at the opposite edge.
    pub v1: f32,
}

impl UvRect {
    /// Identity mapping for the given vertical flip.
    pub const fn reset(vflip: bool) -> Self {
        if vflip {
            Self {
                u0: 0.0,
                u1: 1.0,
                v0: 0.0,
                v1: 1.0,
            }
        } else {
            Self {
                u0: 0.0,
                u1: 1.0,
                v0: 1.0,
                v1: 0.0,
            }
        }
    }
}

/// Horizontal crop primitive: the image is resized to height `V_h`, centred, and cut to `V_l`.
fn crop_horizontal(uv: &mut UvRect, vw: f32, vh: f32, pw: f32, ph: f32) {
    // parity: new width truncated to an integer, centre computed as a float (spec: c = nl/2).
    let nl = (vh / ph * pw).trunc();
    let c = nl / 2.0;
    uv.u0 = (c - vw / 2.0) / nl;
    uv.u1 = (c + vw / 2.0) / nl;
}

/// Vertical crop primitive.
fn crop_vertical(uv: &mut UvRect, vw: f32, vh: f32, pw: f32, ph: f32, vflip: bool) {
    // parity: new height truncated to an integer.
    let nh = (vw / pw * ph).trunc();
    let c = nh / 2.0;
    let low = (c - vh / 2.0) / nh;
    let high = (c + vh / 2.0) / nh;
    if vflip {
        uv.v0 = low;
        uv.v1 = high;
    } else {
        uv.v0 = high;
        uv.v1 = low;
    }
}

/// UVs for a viewport of `viewport` physical pixels showing a wallpaper projected at `projection` pixels.
///
/// `vflip` is true for windows and Wayland, false for the reference X11 root-pixmap path (§4.5).
pub fn compute_uv(
    mode: ScalingMode,
    viewport: (f32, f32),
    projection: (f32, f32),
    vflip: bool,
) -> UvRect {
    let (vw, vh) = viewport;
    let (pw, ph) = projection;
    let mut uv = UvRect::reset(vflip);
    if vw <= 0.0 || vh <= 0.0 || pw <= 0.0 || ph <= 0.0 {
        return uv;
    }
    match mode {
        ScalingMode::Stretch => {}
        ScalingMode::Fill | ScalingMode::Fit => {
            let (sx, sy) = (vw / pw, vh / ph);
            let m = if mode == ScalingMode::Fill {
                sx.max(sy)
            } else {
                sx.min(sy)
            };
            // parity: scaled sizes truncated, then compared with the viewport for exact equality.
            let nw = (pw * m).trunc();
            let nh = (ph * m).trunc();
            if nw != vw {
                crop_horizontal(&mut uv, vw, vh, nw, nh);
            } else if nh != vh {
                crop_vertical(&mut uv, vw, vh, nw, nh, vflip);
            }
        }
        ScalingMode::Default => {
            // parity: orientation table of §4.5, including the asymmetric `>=` / `>` comparisons.
            if (vh > vw && pw >= ph) || (vw > vh && ph > pw) {
                crop_horizontal(&mut uv, vw, vh, pw, ph);
            }
            if (vw > vh && pw >= ph) || (vh > vw && ph > pw) {
                crop_vertical(&mut uv, vw, vh, pw, ph, vflip);
            }
        }
    }
    uv
}

/// Sub-rectangle of a span group's UVs shown by one member output (§4.4).
///
/// `bbox` is the union `(x, y, w, h)` of the members, `pos` and `size` the member's global position and
/// logical size, `base` the UVs computed for the whole box.
pub fn span_slice(
    bbox: (f32, f32, f32, f32),
    pos: (f32, f32),
    size: (f32, f32),
    base: UvRect,
) -> UvRect {
    let (bx, by, bw, bh) = bbox;
    let (gx, gy) = pos;
    let (lw, lh) = size;
    if bw <= 0.0 || bh <= 0.0 {
        return base;
    }
    let rl = (gx - bx) / bw;
    let rr = (gx + lw - bx) / bw;
    let rt = (gy - by) / bh;
    let rb = (gy + lh - by) / bh;
    UvRect {
        u0: base.u0 + rl * (base.u1 - base.u0),
        u1: base.u0 + rr * (base.u1 - base.u0),
        v0: base.v0 + rt * (base.v1 - base.v0),
        v1: base.v0 + rb * (base.v1 - base.v0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    fn uv_close(a: UvRect, b: UvRect) -> bool {
        close(a.u0, b.u0) && close(a.u1, b.u1) && close(a.v0, b.v0) && close(a.v1, b.v1)
    }

    #[test]
    fn reset_follows_vflip() {
        assert_eq!(
            UvRect::reset(true),
            UvRect {
                u0: 0.0,
                u1: 1.0,
                v0: 0.0,
                v1: 1.0
            }
        );
        assert_eq!(
            UvRect::reset(false),
            UvRect {
                u0: 0.0,
                u1: 1.0,
                v0: 1.0,
                v1: 0.0
            }
        );
    }

    #[test]
    fn stretch_is_identity() {
        let uv = compute_uv(ScalingMode::Stretch, (1920.0, 1080.0), (800.0, 800.0), true);
        assert_eq!(uv, UvRect::reset(true));
    }

    #[test]
    fn same_aspect_is_identity_in_every_mode() {
        for mode in [ScalingMode::Fill, ScalingMode::Fit, ScalingMode::Default] {
            let uv = compute_uv(mode, (1920.0, 1080.0), (3840.0, 2160.0), true);
            assert!(uv_close(uv, UvRect::reset(true)), "{mode:?}: {uv:?}");
        }
    }

    #[test]
    fn default_landscape_on_landscape_wider_gives_bands() {
        // 1920×1080 viewport, 2560×1080 wallpaper: fitted to width, bands top and bottom.
        let uv = compute_uv(
            ScalingMode::Default,
            (1920.0, 1080.0),
            (2560.0, 1080.0),
            true,
        );
        // nh = trunc(1920/2560*1080) = 810, c = 405
        assert!(close(uv.v0, (405.0 - 540.0) / 810.0));
        assert!(close(uv.v1, (405.0 + 540.0) / 810.0));
        assert_eq!((uv.u0, uv.u1), (0.0, 1.0));
    }

    #[test]
    fn default_portrait_wallpaper_on_landscape_viewport() {
        // Fitted to height: side bands.
        let uv = compute_uv(
            ScalingMode::Default,
            (1920.0, 1080.0),
            (1080.0, 1920.0),
            true,
        );
        // nl = trunc(1080/1920*1080) = 607, c = 303.5
        assert!(close(uv.u0, (303.5 - 960.0) / 607.0));
        assert!(close(uv.u1, (303.5 + 960.0) / 607.0));
        assert_eq!((uv.v0, uv.v1), (0.0, 1.0));
    }

    #[test]
    fn default_landscape_wallpaper_on_portrait_viewport() {
        let uv = compute_uv(
            ScalingMode::Default,
            (1080.0, 1920.0),
            (1920.0, 1080.0),
            true,
        );
        // nl = trunc(1920/1080*1920) = 3413, c = 1706.5: lateral crop
        assert!(close(uv.u0, (1706.5 - 540.0) / 3413.0));
        assert!(close(uv.u1, (1706.5 + 540.0) / 3413.0));
    }

    #[test]
    fn default_square_viewport_is_stretch() {
        let uv = compute_uv(
            ScalingMode::Default,
            (1000.0, 1000.0),
            (1920.0, 1080.0),
            true,
        );
        assert_eq!(uv, UvRect::reset(true));
    }

    #[test]
    fn fill_crops_and_fit_bands() {
        let fill = compute_uv(ScalingMode::Fill, (1920.0, 1080.0), (1080.0, 1920.0), true);
        // m = 1920/1080; nw = 1920, nh = trunc(1920*1.777..) = 3413 → vertical crop
        assert_eq!((fill.u0, fill.u1), (0.0, 1.0));
        assert!(close(fill.v0, (1706.5 - 540.0) / 3413.0));
        assert!(fill.v0 > 0.0 && fill.v1 < 1.0);

        let fit = compute_uv(ScalingMode::Fit, (1920.0, 1080.0), (1080.0, 1920.0), true);
        // m = 1080/1920; nw = trunc(607.5) = 607, nh = 1080 → horizontal crop with nl = 607
        assert!(close(fit.u0, (303.5 - 960.0) / 607.0));
        assert!(fit.u0 < 0.0 && fit.u1 > 1.0);
    }

    #[test]
    fn vertical_crop_without_vflip_swaps_v() {
        let uv = compute_uv(
            ScalingMode::Default,
            (1920.0, 1080.0),
            (2560.0, 1080.0),
            false,
        );
        assert!(uv.v0 > uv.v1);
        assert!(close(uv.v0, (405.0 + 540.0) / 810.0));
    }

    #[test]
    fn degenerate_sizes_do_not_divide_by_zero() {
        let uv = compute_uv(ScalingMode::Fill, (0.0, 1080.0), (1920.0, 1080.0), true);
        assert_eq!(uv, UvRect::reset(true));
    }

    #[test]
    fn span_slices_cover_the_box() {
        let base = UvRect::reset(true);
        let bbox = (0.0, 0.0, 3840.0, 1080.0);
        let left = span_slice(bbox, (0.0, 0.0), (1920.0, 1080.0), base);
        let right = span_slice(bbox, (1920.0, 0.0), (1920.0, 1080.0), base);
        assert_eq!(
            left,
            UvRect {
                u0: 0.0,
                u1: 0.5,
                v0: 0.0,
                v1: 1.0
            }
        );
        assert_eq!(
            right,
            UvRect {
                u0: 0.5,
                u1: 1.0,
                v0: 0.0,
                v1: 1.0
            }
        );
    }
}
