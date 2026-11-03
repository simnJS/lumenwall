//! Mouse input for web pages (spec 07 §10.4).

use serde::{Deserialize, Serialize};

/// A mouse button the page can receive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MouseButton {
    Left,
    Right,
}

/// Viewport of the web wallpaper in GL pixels (origin bottom-left).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewport {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Page coordinates (origin top-left) for a GL pointer position (spec 07 §10.4):
/// `x = clamp(floor(px − vx), 0, vw)`, `y = vh − clamp(floor(py − vy), 0, vh)`.
pub fn web_position(viewport: Viewport, gl_x: f64, gl_y: f64) -> (i32, i32) {
    let x = ((gl_x - f64::from(viewport.x)).floor() as i32).clamp(0, viewport.width.max(0));
    let y = viewport.height
        - ((gl_y - f64::from(viewport.y)).floor() as i32).clamp(0, viewport.height.max(0));
    (x, y)
}

/// Turns per-frame pointer state into page events.
///
/// parity: clicks are sent when a button state changes, with click count 1 and the current
/// position. Moves are only sent when the position changes (the reference resends an identical
/// move every frame; Chromium does not need it).
#[derive(Debug, Default, Clone)]
pub struct MouseTracker {
    last_pos: Option<(i32, i32)>,
    left: bool,
    right: bool,
}

/// Event to forward to the browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MouseInput {
    Move {
        x: i32,
        y: i32,
        left_down: bool,
    },
    Button {
        x: i32,
        y: i32,
        button: MouseButton,
        down: bool,
    },
}

impl MouseTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds the state of one frame; pushes the resulting events.
    pub fn update(
        &mut self,
        pos: (i32, i32),
        left: bool,
        right: bool,
        out: &mut impl Extend<MouseInput>,
    ) {
        let (x, y) = pos;
        if self.last_pos != Some(pos) {
            self.last_pos = Some(pos);
            out.extend([MouseInput::Move {
                x,
                y,
                left_down: left,
            }]);
        }
        if left != self.left {
            self.left = left;
            out.extend([MouseInput::Button {
                x,
                y,
                button: MouseButton::Left,
                down: left,
            }]);
        }
        if right != self.right {
            self.right = right;
            out.extend([MouseInput::Button {
                x,
                y,
                button: MouseButton::Right,
                down: right,
            }]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_21_1_web_projection() {
        let vp = Viewport {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert_eq!(web_position(vp, 0.0, 1080.0).1, 0);
        assert_eq!(web_position(vp, 0.0, 0.0).1, 1080);
        assert_eq!(web_position(vp, 0.0, 540.0).1, 540);
        assert_eq!(web_position(vp, -5.0, 2000.0), (0, 0));
        assert_eq!(web_position(vp, 3000.0, 540.5), (1920, 540));
    }

    #[test]
    fn tracker_emits_changes_only() {
        let mut t = MouseTracker::new();
        let mut out = Vec::new();
        t.update((10, 20), false, false, &mut out);
        t.update((10, 20), false, false, &mut out);
        assert_eq!(
            out,
            [MouseInput::Move {
                x: 10,
                y: 20,
                left_down: false
            }]
        );
        out.clear();
        t.update((10, 20), true, false, &mut out);
        t.update((10, 20), false, true, &mut out);
        assert_eq!(
            out,
            [
                MouseInput::Button {
                    x: 10,
                    y: 20,
                    button: MouseButton::Left,
                    down: true
                },
                MouseInput::Button {
                    x: 10,
                    y: 20,
                    button: MouseButton::Left,
                    down: false
                },
                MouseInput::Button {
                    x: 10,
                    y: 20,
                    button: MouseButton::Right,
                    down: true
                },
            ]
        );
    }
}
