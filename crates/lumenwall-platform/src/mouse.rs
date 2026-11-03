//! Pointer state and projection to scene / web coordinates (spec 07 §10).
//!
//! Backends deliver [`PointerEvent`]s in surface pixels (Y down). The renderer works in "GL" pixels
//! (origin bottom-left, Y up), then projects into scene space with the UVs of [`crate::uv`].
//! Everything here is pure arithmetic and allocation-free per event.

use std::cell::Cell;

use crate::types::{OutputId, PointerButton, PointerEvent, PointerKind};
use crate::uv::UvRect;

/// Converts a Y measured from the top of a surface of height `height` into GL convention.
///
/// Windowed mode, X11 and Wayland all use this (§10.2, tests of §21.1: H=1080 maps 0→1080, 1080→0, 540→540).
pub fn gl_y_from_top(y: f64, height: f64) -> f64 {
    // parity: Y_GL = H − Y, no clamping and no half-pixel offset.
    height - y
}

/// Viewport of a scene in GL pixels: `(x, y)` is the bottom-left corner.
///
/// For a span group the viewport is `(0, 0, width, height)` of the bounding box (§10.3).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Viewport {
    /// Left edge.
    pub x: f32,
    /// Bottom edge.
    pub y: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

/// Cursor projected into scene space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScenePointer {
    /// Position exposed to scripts as the screen cursor position (`(u0, v0)` at the bottom-left).
    pub normalized: [f32; 2],
    /// Value of the `g_PointerPosition` uniform and input of the parallax (`(u0, v1)` at the bottom-left,
    /// so `(0,0)` is the top-left of the visible region with standard UVs).
    pub pointer: [f32; 2],
}

/// Projects a GL position onto a scene drawn in `viewport` with texture coordinates `uv` (§10.3).
pub fn scene_pointer(gl_x: f64, gl_y: f64, viewport: Viewport, uv: UvRect) -> ScenePointer {
    // parity: relative position clamped to [0,1] before applying the UVs, so the bands of `fit` and the
    // cropped parts of `fill` map to the visible region exactly like the reference.
    let mx = clamp01(((gl_x as f32) - viewport.x) / viewport.width);
    let ny = clamp01(((gl_y as f32) - viewport.y) / viewport.height);
    let u = uv.u0 + mx * (uv.u1 - uv.u0);
    ScenePointer {
        normalized: [u, uv.v0 + ny * (uv.v1 - uv.v0)],
        pointer: [u, uv.v0 + (1.0 - ny) * (uv.v1 - uv.v0)],
    }
}

/// Projects a GL position into page coordinates of a web wallpaper drawn in `viewport` (§10.4).
pub fn web_pointer(gl_x: f64, gl_y: f64, viewport: Viewport) -> (i32, i32) {
    let vw = viewport.width as i32;
    let vh = viewport.height as i32;
    // parity: integer truncation, clamping to [0, size] (inclusive), Y flipped back to top-left origin.
    let x = ((gl_x - f64::from(viewport.x)) as i32).clamp(0, vw.max(0));
    let y = vh - ((gl_y - f64::from(viewport.y)) as i32).clamp(0, vh.max(0));
    (x, y)
}

fn clamp01(v: f32) -> f32 {
    if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) }
}

/// Pointer state handed to the renderer of one output.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PointerState {
    /// X in GL pixels of the output.
    pub x: f64,
    /// Y in GL pixels of the output (from the bottom).
    pub y: f64,
    /// Left button held.
    pub left: bool,
    /// Right button held.
    pub right: bool,
}

#[derive(Clone, Debug)]
struct Entry {
    output: OutputId,
    gl: (f64, f64),
    left: bool,
    right: bool,
    /// Presses not reported by [`PointerTracker::state`] yet (bit 0 left, bit 1 right).
    unseen: Cell<u8>,
}

const UNSEEN_LEFT: u8 = 1;
const UNSEEN_RIGHT: u8 = 2;

/// Per-output pointer bookkeeping with the focus rules of §10.2.
///
/// Feed it every [`PointerEvent`]; ask [`PointerTracker::state`] when rendering an output. A press is reported
/// by at least one `state` call even when its release came first (a click shorter than a frame still gives
/// scripts their `cursorDown`, then `cursorUp`/`cursorClick`).
#[derive(Clone, Debug)]
pub struct PointerTracker {
    enabled: bool,
    focus: Option<OutputId>,
    entries: Vec<Entry>,
}

impl PointerTracker {
    /// `enabled = false` reproduces `--disable-mouse`: the position stays at (0,0), buttons released (§10.5).
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            focus: None,
            entries: Vec::with_capacity(8),
        }
    }

    fn entry_mut(&mut self, output: OutputId) -> &mut Entry {
        let idx = match self.entries.iter().position(|e| e.output == output) {
            Some(i) => i,
            None => {
                self.entries.push(Entry {
                    output,
                    gl: (0.0, 0.0),
                    left: false,
                    right: false,
                    unseen: Cell::new(0),
                });
                self.entries.len() - 1
            }
        };
        &mut self.entries[idx]
    }

    /// Records an event. `surface_height` is the physical height of `event.output`.
    pub fn handle(&mut self, event: &PointerEvent, surface_height: f64) {
        if !self.enabled {
            return;
        }
        match event.kind {
            // parity: enter only moves the focus; leave is ignored (focus and last position stay).
            PointerKind::Enter => {
                self.focus = Some(event.output);
                self.entry_mut(event.output);
            }
            PointerKind::Leave => {}
            PointerKind::Motion => {
                let gl = (event.x, gl_y_from_top(event.y, surface_height));
                self.entry_mut(event.output).gl = gl;
            }
            PointerKind::Button { button, pressed } => {
                let e = self.entry_mut(event.output);
                let bit = match button {
                    PointerButton::Left => {
                        e.left = pressed;
                        UNSEEN_LEFT
                    }
                    PointerButton::Right => {
                        e.right = pressed;
                        UNSEEN_RIGHT
                    }
                    PointerButton::Middle | PointerButton::Other(_) => 0,
                };
                if pressed {
                    e.unseen.set(e.unseen.get() | bit);
                }
            }
        }
    }

    /// Forgets an output that was removed.
    pub fn remove_output(&mut self, output: OutputId) {
        self.entries.retain(|e| e.output != output);
        if self.focus == Some(output) {
            self.focus = None;
        }
    }

    /// State for rendering `output` whose drawable is `physical_size` pixels. A press released before this
    /// call is still reported as held, once.
    pub fn state(&self, output: OutputId, physical_size: (u32, u32)) -> PointerState {
        if !self.enabled {
            return PointerState::default();
        }
        let center = (
            f64::from(physical_size.0) / 2.0,
            f64::from(physical_size.1) / 2.0,
        );
        match self.entries.iter().find(|e| e.output == output) {
            None => PointerState {
                x: center.0,
                y: center.1,
                left: false,
                right: false,
            },
            Some(e) => {
                // parity: focused output → last position; otherwise last position unless it is exactly (0,0),
                // in which case the centre of the output.
                let pos = if self.focus == Some(output) || e.gl != (0.0, 0.0) {
                    e.gl
                } else {
                    center
                };
                let unseen = e.unseen.replace(0);
                PointerState {
                    x: pos.0,
                    y: pos.1,
                    left: e.left || unseen & UNSEEN_LEFT != 0,
                    right: e.right || unseen & UNSEEN_RIGHT != 0,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VP: Viewport = Viewport {
        x: 0.0,
        y: 0.0,
        width: 1920.0,
        height: 1080.0,
    };

    #[test]
    fn window_to_gl() {
        for (h, cases) in [
            (1080.0, [(0.0, 1080.0), (1080.0, 0.0), (540.0, 540.0)]),
            (1440.0, [(0.0, 1440.0), (1440.0, 0.0), (720.0, 720.0)]),
            (600.0, [(0.0, 600.0), (600.0, 0.0), (300.0, 300.0)]),
        ] {
            for (y, expected) in cases {
                assert_eq!(gl_y_from_top(y, h), expected);
            }
        }
    }

    #[test]
    fn wayland_to_gl_with_scale() {
        // Wayland: (x·s, (lh − y)·s) is the same as converting the scaled position.
        let (s, lh, x, y) = (1.5_f64, 720.0, 100.0, 200.0);
        let reference = (x * s, (lh - y) * s);
        assert_eq!((x * s, gl_y_from_top(y * s, lh * s)), reference);
    }

    #[test]
    fn gl_to_normalized() {
        let uv = UvRect::reset(true);
        for (y, expected) in [(1080.0, 1.0), (0.0, 0.0), (540.0, 0.5)] {
            assert_eq!(scene_pointer(0.0, y, VP, uv).normalized[1], expected);
        }
    }

    #[test]
    fn gl_to_web() {
        for (y, expected) in [(1080.0, 0), (0.0, 1080), (540.0, 540)] {
            assert_eq!(web_pointer(0.0, y, VP).1, expected);
        }
        assert_eq!(web_pointer(5000.0, -20.0, VP), (1920, 1080));
    }

    #[test]
    fn window_chain_top_and_bottom() {
        let uv = UvRect::reset(true);
        let top = scene_pointer(0.0, gl_y_from_top(0.0, 1080.0), VP, uv);
        let bottom = scene_pointer(0.0, gl_y_from_top(1080.0, 1080.0), VP, uv);
        assert_eq!(top.normalized[1], 1.0);
        assert_eq!(bottom.normalized[1], 0.0);
        // g_PointerPosition: (0,0) top-left, (1,1) bottom-right.
        assert_eq!(top.pointer, [0.0, 0.0]);
        assert_eq!(scene_pointer(1920.0, 0.0, VP, uv).pointer, [1.0, 1.0]);
    }

    #[test]
    fn cropped_uvs_and_clamping() {
        let uv = UvRect {
            u0: 0.25,
            u1: 0.75,
            v0: -0.1,
            v1: 1.1,
        };
        let p = scene_pointer(960.0, 540.0, VP, uv);
        assert!((p.pointer[0] - 0.5).abs() < 1e-6);
        assert!((p.pointer[1] - 0.5).abs() < 1e-6);
        // Outside the viewport: clamped to the edges of the UV rectangle.
        let out = scene_pointer(-100.0, 5000.0, VP, uv);
        assert_eq!(out.pointer, [0.25, -0.1]);
    }

    #[test]
    fn disabled_mouse_points_at_u0_v1() {
        let uv = UvRect {
            u0: 0.1,
            u1: 0.9,
            v0: 0.2,
            v1: 0.8,
        };
        let t = PointerTracker::new(false);
        let s = t.state(OutputId(0), (1920, 1080));
        assert_eq!((s.x, s.y, s.left, s.right), (0.0, 0.0, false, false));
        let p = scene_pointer(s.x, s.y, VP, uv);
        assert_eq!(p.pointer, [0.1, 0.8]);
    }

    fn ev(output: u32, x: f64, y: f64, kind: PointerKind) -> PointerEvent {
        PointerEvent {
            output: OutputId(output),
            x,
            y,
            kind,
        }
    }

    #[test]
    fn tracker_focus_rules() {
        let mut t = PointerTracker::new(true);
        let (a, b) = (OutputId(1), OutputId(2));
        // Never visited: centre.
        assert_eq!(
            t.state(b, (1920, 1080)),
            PointerState {
                x: 960.0,
                y: 540.0,
                left: false,
                right: false
            }
        );

        t.handle(&ev(1, 0.0, 0.0, PointerKind::Enter), 1080.0);
        t.handle(&ev(1, 100.0, 80.0, PointerKind::Motion), 1080.0);
        t.handle(
            &ev(
                1,
                100.0,
                80.0,
                PointerKind::Button {
                    button: PointerButton::Left,
                    pressed: true,
                },
            ),
            1080.0,
        );
        let s = t.state(a, (1920, 1080));
        assert_eq!((s.x, s.y, s.left), (100.0, 1000.0, true));

        // Leave is ignored: focus and position stay.
        t.handle(&ev(1, 0.0, 0.0, PointerKind::Leave), 1080.0);
        assert_eq!(t.state(a, (1920, 1080)).x, 100.0);

        // Move to B; A keeps its last non-zero position.
        t.handle(&ev(2, 0.0, 0.0, PointerKind::Enter), 1080.0);
        t.handle(&ev(2, 10.0, 1080.0, PointerKind::Motion), 1080.0);
        assert_eq!(t.state(a, (1920, 1080)).x, 100.0);
        assert_eq!(t.state(b, (1920, 1080)).y, 0.0);
    }

    #[test]
    fn short_click_is_seen_once() {
        let mut t = PointerTracker::new(true);
        let button = |pressed| PointerKind::Button {
            button: PointerButton::Left,
            pressed,
        };
        // Press and release between two frames: one frame with the button held, then released.
        t.handle(&ev(1, 10.0, 10.0, button(true)), 1080.0);
        t.handle(&ev(1, 10.0, 10.0, button(false)), 1080.0);
        assert!(t.state(OutputId(1), (1920, 1080)).left);
        assert!(!t.state(OutputId(1), (1920, 1080)).left);
        // A press seen while held is not repeated after its release.
        t.handle(&ev(1, 10.0, 10.0, button(true)), 1080.0);
        assert!(t.state(OutputId(1), (1920, 1080)).left);
        t.handle(&ev(1, 10.0, 10.0, button(false)), 1080.0);
        assert!(!t.state(OutputId(1), (1920, 1080)).left);
        // Right button too; the middle one is not tracked.
        t.handle(
            &ev(
                1,
                0.0,
                0.0,
                PointerKind::Button {
                    button: PointerButton::Right,
                    pressed: true,
                },
            ),
            1080.0,
        );
        t.handle(
            &ev(
                1,
                0.0,
                0.0,
                PointerKind::Button {
                    button: PointerButton::Right,
                    pressed: false,
                },
            ),
            1080.0,
        );
        let s = t.state(OutputId(1), (1920, 1080));
        assert!(s.right && !s.left);
    }

    #[test]
    fn tracker_zero_position_unfocused_is_centre() {
        let mut t = PointerTracker::new(true);
        t.handle(&ev(1, 0.0, 0.0, PointerKind::Enter), 1080.0);
        // GL (0,0) = bottom-left corner while focused: kept.
        t.handle(&ev(1, 0.0, 1080.0, PointerKind::Motion), 1080.0);
        assert_eq!(t.state(OutputId(1), (1920, 1080)).x, 0.0);
        t.handle(&ev(2, 5.0, 5.0, PointerKind::Enter), 1080.0);
        // No longer focused and exactly (0,0): centre.
        assert_eq!(t.state(OutputId(1), (1920, 1080)).x, 960.0);
        t.remove_output(OutputId(1));
        assert_eq!(t.state(OutputId(1), (100, 100)).x, 50.0);
    }
}
