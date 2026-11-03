//! TEXS animation block, spritesheet inference and frame selection
//! (spec 01 §2.8, §2.9.1, §2.12).

use super::{Spritesheet, TexError};
use crate::reader::{Reader, escape_bytes};

/// One TEXS frame (fields F0 to F7).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Frame {
    /// F0: image index shown by the frame.
    pub image: u32,
    /// F1: display duration in seconds.
    pub duration: f32,
    /// F2: x origin in the image, in pixels.
    pub x: f32,
    /// F3: y origin in the image, in pixels.
    pub y: f32,
    /// F4: x component of the "width" side of the frame rectangle.
    pub width_x: f32,
    /// F5: y component of the "width" side (0 in TEXS0001).
    pub width_y: f32,
    /// F6: x component of the "height" side (0 in TEXS0001).
    pub height_x: f32,
    /// F7: y component of the "height" side.
    pub height_y: f32,
}

/// Parsed TEXS block.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Animation {
    /// TEXS version (1..=3).
    pub version: u8,
    /// Animation width `LA`.
    pub width: u32,
    /// Animation height `HA`.
    pub height: u32,
    /// Frames in file order.
    pub frames: Vec<Frame>,
}

/// What to bind and which uniforms to set for the current frame (§2.12).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FrameState {
    /// Image bound on texture unit 0.
    pub image: u32,
    /// `g_Texture0Translation`.
    pub translation: [f32; 2],
    /// `g_Texture0Rotation`.
    pub rotation: [f32; 4],
}

const FRAME_SIZE: u64 = 32;

pub(super) fn read_texs(r: &mut Reader<'_>) -> Result<Animation, TexError> {
    let magic: [u8; 9] = r.array()?;
    let version = match &magic {
        b"TEXS0001\0" => 1,
        b"TEXS0002\0" => 2,
        b"TEXS0003\0" => 3,
        _ => return Err(TexError::BadTexs(escape_bytes(&magic))),
    };
    let count = r.u32()?;
    let (mut width, mut height) = (0, 0);
    if version == 3 {
        width = r.u32()?;
        height = r.u32()?;
    }
    // Frames have a fixed size: check the whole table before allocating.
    r.ensure(u64::from(count) * FRAME_SIZE)?;
    let mut frames = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let image = r.u32()?;
        let duration = r.f32()?;
        let frame = if version == 1 {
            // TEXS0001 stores integers; F5 and F6 are ignored (QO-14).
            let x = r.u32()? as f32;
            let y = r.u32()? as f32;
            let width_x = r.u32()? as f32;
            let _f5 = r.u32()?;
            let _f6 = r.u32()?;
            let height_y = r.u32()? as f32;
            Frame {
                image,
                duration,
                x,
                y,
                width_x,
                width_y: 0.0,
                height_x: 0.0,
                height_y,
            }
        } else {
            Frame {
                image,
                duration,
                x: r.f32()?,
                y: r.f32()?,
                width_x: r.f32()?,
                width_y: r.f32()?,
                height_x: r.f32()?,
                height_y: r.f32()?,
            }
        };
        frames.push(frame);
    }
    if version != 3 {
        // LA/HA from the first frame, truncated toward zero; `as` saturates and maps
        // NaN to 0 (QO-17). No frame: 0×0 (QO-11).
        if let Some(first) = frames.first() {
            width = first.width_x as u32;
            height = first.height_y as u32;
        }
    }
    Ok(Animation {
        version,
        width,
        height,
        frames,
    })
}

/// Spritesheet inference from TEXS (§2.9.1).
pub(super) fn infer_spritesheet(
    animation: &Animation,
    image_width: u32,
    image_height: u32,
) -> Spritesheet {
    let Some(first) = animation.frames.first() else {
        return Spritesheet::default();
    };
    if image_width == 0 || image_height == 0 {
        return Spritesheet::default();
    }
    let (lf, hf) = (first.width_x, first.height_y);
    if !(lf > 0.0 && hf > 0.0) {
        return Spritesheet::default();
    }
    // f64::round rounds half away from zero, as the spec requires.
    let columns = (f64::from(image_width) / f64::from(lf)).round() as u32;
    let rows = (f64::from(image_height) / f64::from(hf)).round() as u32;
    let count = animation.frames.len() as u64;
    if columns > 0 && rows > 0 && u64::from(columns) * u64::from(rows) >= count {
        Spritesheet {
            columns,
            rows,
            frames: animation.frames.len() as u32,
            duration: animation.frames.iter().map(|f| f.duration).sum(),
        }
    } else {
        Spritesheet::default()
    }
}

impl Animation {
    /// Total duration `T`, the sum of every F1, in double precision.
    pub fn total_duration(&self) -> f64 {
        self.frames.iter().map(|f| f64::from(f.duration)).sum()
    }

    /// Index of the frame shown at `time` seconds (§2.12), given `total` from
    /// [`total_duration`](Self::total_duration) (computed once per consumer).
    pub fn frame_index_at(&self, total: f64, time: f64) -> Option<usize> {
        // `%` on f64 is C's fmod; with total = 0 it yields NaN.
        let mut t = time % total;
        for (index, frame) in self.frames.iter().enumerate() {
            t -= f64::from(frame.duration);
            // parity: the frame is kept unless `t > 0` holds, so a NaN time (T = 0)
            // selects the first frame and an exact boundary selects the earlier one.
            if t > 0.0 {
                continue;
            }
            return Some(index);
        }
        None
    }

    /// Bound image and uniforms at `time` (§2.12). `image_count` is `NI` and
    /// `texture_size(i)` the §2.11 texture size of image `i`.
    pub fn state_at(
        &self,
        total: f64,
        time: f64,
        image_count: usize,
        texture_size: impl Fn(usize) -> [u32; 2],
    ) -> FrameState {
        let Some(frame) = self
            .frame_index_at(total, time)
            .and_then(|i| self.frames.get(i))
        else {
            return FrameState::default();
        };
        let [w, h] = texture_size(frame.image as usize);
        let (w, h) = (w as f32, h as f32);
        FrameState {
            image: if (frame.image as usize) < image_count {
                frame.image
            } else {
                0
            },
            translation: [frame.x / w, frame.y / h],
            rotation: [
                frame.width_x / w,
                frame.width_y / w,
                frame.height_x / h,
                frame.height_y / h,
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anim(durations: &[f32]) -> Animation {
        Animation {
            version: 3,
            width: 1,
            height: 1,
            frames: durations
                .iter()
                .enumerate()
                .map(|(i, &d)| Frame {
                    image: i as u32,
                    duration: d,
                    ..Frame::default()
                })
                .collect(),
        }
    }

    #[test]
    fn zero_total_selects_first_frame() {
        let a = anim(&[0.0, 0.0]);
        assert_eq!(a.frame_index_at(a.total_duration(), 3.0), Some(0));
    }

    #[test]
    fn no_frames_gives_default_state() {
        let a = anim(&[]);
        let state = a.state_at(a.total_duration(), 1.0, 1, |_| [1, 1]);
        assert_eq!(state, FrameState::default());
    }
}
