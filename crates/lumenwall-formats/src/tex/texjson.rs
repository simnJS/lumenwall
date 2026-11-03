//! `.tex-json` companion file (spec 01 §2.9.2).

use serde_json::Value;

use super::Spritesheet;

/// Returns the spritesheet described by the first `spritesheetsequences` element,
/// or `None` when the file must be ignored (invalid JSON, missing or empty array,
/// incompatible types, or values that fail the positivity checks).
pub(super) fn spritesheet_from_json(
    json: &[u8],
    image_width: u32,
    image_height: u32,
) -> Option<Spritesheet> {
    let root: Value = serde_json::from_slice(json).ok()?;
    let first = root.get("spritesheetsequences")?.as_array()?.first()?;

    let frames = match first.get("frames") {
        None => 0,
        Some(Value::Number(n)) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => i,
            (None, Some(u), _) => i64::try_from(u).unwrap_or(i64::MAX),
            // A floating-point value is truncated.
            (None, None, Some(f)) => f as i64,
            _ => return None,
        },
        Some(_) => return None,
    };
    let float = |key: &str, default: f64| match first.get(key) {
        None => Some(default),
        Some(value) => value.as_f64(),
    };
    let width = float("width", 0.0)?;
    let height = float("height", 0.0)?;
    let duration = float("duration", 1.0)?;

    if frames > 0 && width > 0.0 && height > 0.0 && image_width > 0 && image_height > 0 {
        // No `columns × rows >= frames` check here, unlike §2.9.1.
        Some(Spritesheet {
            columns: (f64::from(image_width) / width).round() as u32,
            rows: (f64::from(image_height) / height).round() as u32,
            frames: u32::try_from(frames).unwrap_or(u32::MAX),
            duration: duration as f32,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_first_sequence() {
        let json = br#"{"spritesheetsequences":[{"frames":4,"width":32,"height":16,"duration":2.5},{"frames":9}]}"#;
        let sheet = spritesheet_from_json(json, 64, 32).unwrap();
        assert_eq!(
            sheet,
            Spritesheet {
                columns: 2,
                rows: 2,
                frames: 4,
                duration: 2.5
            }
        );
    }

    #[test]
    fn duration_defaults_to_one_and_float_frames_truncate() {
        let json = br#"{"spritesheetsequences":[{"frames":3.9,"width":10,"height":10}]}"#;
        let sheet = spritesheet_from_json(json, 30, 10).unwrap();
        assert_eq!(sheet.frames, 3);
        assert_eq!(sheet.duration, 1.0);
        // No columns × rows check: 3 × 1 for 3 frames is accepted, so is 1 × 1.
        let json = br#"{"spritesheetsequences":[{"frames":8,"width":10,"height":10}]}"#;
        assert_eq!(spritesheet_from_json(json, 10, 10).unwrap().frames, 8);
    }

    #[test]
    fn errors_are_ignored() {
        for json in [
            &br#"{"spritesheetsequences":[{"frames":"3","width":1,"height":1}]}"#[..],
            br#"{"spritesheetsequences":[]}"#,
            br#"{"spritesheetsequences":{}}"#,
            br#"{"other":1}"#,
            br#"not json"#,
            br#"{"spritesheetsequences":[{"frames":2,"width":"1","height":1}]}"#,
            br#"{"spritesheetsequences":[{"frames":0,"width":1,"height":1}]}"#,
        ] {
            assert_eq!(
                spritesheet_from_json(json, 4, 4),
                None,
                "{}",
                String::from_utf8_lossy(json)
            );
        }
    }
}
