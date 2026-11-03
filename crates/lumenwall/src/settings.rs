//! Value types shared by the command line, the configuration file and the execution plan
//! (spec 07 §2.2, §2.4, §3.1, §4.5, §18.1, §20).

use std::fmt;
use std::path::{Path, PathBuf};

use clap::ValueEnum;
use serde::Deserialize;

/// How the wallpaper's offscreen image is mapped onto an output (spec 07 §4.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scaling {
    /// Fill the output, distorting the image.
    Stretch,
    /// Show the whole image; the bands outside it follow --clamp.
    Fit,
    /// Cover the output, cropping the image.
    Fill,
    /// Same as `Fill` for desktop wallpapers (Wallpaper Engine covers the screen; the reference fit the width or the height).
    #[default]
    Default,
}

/// Texture addressing outside the image, visible in the bands of `fit`/`default` (spec 07 §4.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Clamp {
    /// Repeat the edge pixels.
    #[default]
    Clamp,
    /// Transparent black.
    Border,
    /// Tile the image.
    Repeat,
}

/// Wayland layer-shell layer (spec 07 §6). No effect on Windows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layer {
    Background,
    #[default]
    Bottom,
    Top,
    Overlay,
}

/// Hardware video decoding (spec 07 §14).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, ValueEnum, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HwDecode {
    /// Use the hardware decoder when available.
    #[default]
    Auto,
    /// NVDEC with a copy back.
    NvdecCopy,
    /// Software decoding only.
    No,
}

macro_rules! display_via_value_enum {
    ($($t:ty),*) => {$(
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let value = self.to_possible_value().expect("no skipped variants");
                f.write_str(value.get_name())
            }
        }
    )*};
}
display_via_value_enum!(Scaling, Clamp, Layer, HwDecode);

/// Parses a value-list option with exact, case-sensitive matching.
pub(crate) fn parse_value_enum<T: ValueEnum>(value: &str) -> Result<T, String> {
    T::from_str(value, false).map_err(|_| {
        let allowed: Vec<String> = T::value_variants()
            .iter()
            .filter_map(|v| v.to_possible_value().map(|p| p.get_name().to_owned()))
            .collect();
        format!("expected one of: {}", allowed.join(", "))
    })
}

/// A wallpaper as written by the user: Workshop ID or path (spec 07 §3.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WallpaperSource {
    /// A value without any path separator, even if it is not numeric.
    WorkshopId(String),
    /// A value with a path separator, used as is (relative to the working directory).
    Path(PathBuf),
}

impl WallpaperSource {
    /// Classifies a raw value with the rules of the current platform.
    pub fn classify(raw: impl Into<PathBuf>) -> Self {
        Self::classify_with(raw, cfg!(windows))
    }

    /// Spec 07 §3.1: a value without `/` is a Workshop ID (`mon_fond` is an ID,
    /// `./mon_fond` a path). On Windows, `\` and a drive prefix (`C:`) also mark a path.
    pub fn classify_with(raw: impl Into<PathBuf>, windows: bool) -> Self {
        let raw: PathBuf = raw.into();
        let is_path = {
            let bytes = raw.as_os_str().as_encoded_bytes();
            bytes.contains(&b'/')
                || (windows
                    && (bytes.contains(&b'\\')
                        || (bytes.len() >= 2
                            && bytes[1] == b':'
                            && bytes[0].is_ascii_alphabetic())))
        };
        if is_path {
            Self::Path(raw)
        } else {
            // Without a separator only non-UTF-8 IDs lose information, and Workshop IDs are ASCII.
            Self::WorkshopId(raw.to_string_lossy().into_owned())
        }
    }
}

impl fmt::Display for WallpaperSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WorkshopId(id) => write!(f, "workshop:{id}"),
            Self::Path(path) => write!(f, "{}", path.display()),
        }
    }
}

/// What fills a wallpaper slot before resolution.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum WallpaperRef {
    Source(WallpaperSource),
    /// First item of a named Wallpaper Engine playlist (spec 07 §2.3, §17).
    PlaylistFirst(String),
}

impl fmt::Display for WallpaperRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source(source) => source.fmt(f),
            Self::PlaylistFirst(name) => write!(f, "playlist:{name}"),
        }
    }
}

/// Explicit window geometry from `--window XxYxWxH` (spec 07 §2.2, §7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl WindowGeometry {
    /// Parses `XxYxWxH`. Each field is read with C-like numeric prefix tolerance: leading
    /// blanks and a sign are accepted, the first non-digit ends the number, no digit gives 0.
    pub fn parse(value: &str) -> Result<Self, String> {
        let fields: Vec<&str> = value.splitn(4, 'x').collect();
        if value.is_empty() || fields.len() != 4 {
            return Err("expected format XxYxWxH".into());
        }
        let geometry = Self {
            x: numeric_prefix(fields[0]),
            y: numeric_prefix(fields[1]),
            width: numeric_prefix(fields[2]),
            height: numeric_prefix(fields[3]),
        };
        // Deviation: the reference does not check the size; a zero-sized window cannot render.
        if geometry.width <= 0 || geometry.height <= 0 {
            return Err("width and height must be positive".into());
        }
        Ok(geometry)
    }
}

impl fmt::Display for WindowGeometry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}x{}x{}", self.x, self.y, self.width, self.height)
    }
}

/// Decimal integer prefix of `s`, saturating at the `i32` range (atoi-like).
pub(crate) fn numeric_prefix(s: &str) -> i32 {
    let s = s.trim_start();
    let (negative, digits) = match s.as_bytes().first() {
        Some(b'-') => (true, &s[1..]),
        Some(b'+') => (false, &s[1..]),
        _ => (false, s),
    };
    let mut value: i64 = 0;
    for b in digits.bytes().take_while(u8::is_ascii_digit) {
        value = (value * 10 + i64::from(b - b'0')).min(i64::from(i32::MAX) + 1);
    }
    let value = if negative { -value } else { value };
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Encoded format of a screenshot, chosen by extension (spec 07 §18.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ScreenshotFormat {
    Bmp,
    Png,
    /// Quality 100 (spec 07 §18.3).
    Jpeg,
}

impl ScreenshotFormat {
    /// Case-sensitive extension check: `.bmp`, `.png`, `.jpg`, `.jpeg`.
    pub fn from_path(path: &Path) -> Result<Self, String> {
        match path.extension().and_then(|e| e.to_str()) {
            Some("bmp") => Ok(Self::Bmp),
            Some("png") => Ok(Self::Png),
            Some("jpg" | "jpeg") => Ok(Self::Jpeg),
            Some(other) => Err(format!(
                "unknown screenshot extension '.{other}' (expected .bmp, .png, .jpg or .jpeg)"
            )),
            None => Err("cannot determine the screenshot format: the path has no extension".into()),
        }
    }
}

/// Upper bound of `--screenshot-delay` (spec 07 §2.2).
pub const MAX_SCREENSHOT_DELAY: u32 = 5;
/// Upper bound of `--volume` (spec 07 §2.2).
pub const MAX_VOLUME: u8 = 128;
/// Defaults of spec 07 §2.4.
pub const DEFAULT_FPS: u32 = 30;
pub const DEFAULT_SCREENSHOT_DELAY: u32 = 5;
/// Default volume: 64 of 128, the 50 % the settings window shows. Deviation: the reference's
/// default is [`REFERENCE_DEFAULT_VOLUME`] (12 %), barely audible on most devices.
pub const DEFAULT_VOLUME: u8 = 64;
/// The reference's `--volume` default (spec 07 §2.4).
pub const REFERENCE_DEFAULT_VOLUME: u8 = 15;
/// Default of `automute`: off. Deviation: the reference mutes by default; see the README.
pub const DEFAULT_AUTOMUTE: bool = false;
/// Default of `gpu-release-delay`: seconds every output stays paused before the GPU is
/// released (Lumenwall extension; 0 = never).
pub const DEFAULT_GPU_RELEASE_DELAY: u32 = 300;

/// Clamps a parsed volume to `[0, 128]` (spec 07 §2.2).
pub fn clamp_volume(volume: i64) -> u8 {
    volume.clamp(0, i64::from(MAX_VOLUME)) as u8
}

/// `--volume` / `volume` (0..=128) as the fraction the protocol and the UI use (0.0..=1.0;
/// the UI shows it × 100 %): 64 → 0.5, 128 → 1.0.
pub fn volume_fraction(volume: u8) -> f32 {
    f32::from(volume.min(MAX_VOLUME)) / f32::from(MAX_VOLUME)
}

/// A fraction from a client (0.0..=1.0) as a `volume` value, rounded: 0.5 → 64.
pub fn volume_from_fraction(fraction: f32) -> u8 {
    let f = if fraction.is_finite() { fraction } else { 0.0 };
    (f.clamp(0.0, 1.0) * f32::from(MAX_VOLUME)).round() as u8
}

/// Validates a frame-rate cap. Deviation (QO-1): 0 and negative values are rejected instead of
/// producing an infinite frame time or an uncapped busy loop.
pub fn validate_fps(fps: i64) -> Result<u32, String> {
    if fps <= 0 {
        return Err("the frame rate must be a positive integer".into());
    }
    u32::try_from(fps).map_err(|_| "the frame rate is too large".to_string())
}

/// Validates a screenshot delay: negative values are rejected, values above 5 are clamped.
pub fn validate_screenshot_delay(delay: i64) -> Result<u32, String> {
    if delay < 0 {
        return Err("the screenshot delay must be zero or positive".into());
    }
    Ok(delay.min(i64::from(MAX_SCREENSHOT_DELAY)) as u32)
}

/// Validates `gpu-release-delay` (seconds, 0 = never).
pub fn validate_gpu_release_delay(secs: i64) -> Result<u32, String> {
    if secs < 0 {
        return Err("the delay must be zero (never) or a number of seconds".into());
    }
    u32::try_from(secs).map_err(|_| "the delay is too large".to_string())
}

/// Rendering debug switches (spec 07 §20).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct RenderDebug {
    /// `base-only`: no effects nor extra blend passes.
    pub base_only: bool,
    /// `no-solid-final`: layers marked solid are not drawn in the final pass.
    pub no_solid_final: bool,
    /// `pass-log`: log every render pass.
    pub pass_log: bool,
    /// `object=<id>`: render only this object (last one wins).
    pub only_object: Option<u32>,
    /// `skip-object=<id>` (cumulative).
    pub skip_objects: Vec<u32>,
    /// `skip-effect=<id>` (cumulative).
    pub skip_effects: Vec<u32>,
}

impl RenderDebug {
    /// Applies one `--render-debug` value.
    pub fn apply(&mut self, value: &str) -> Result<(), String> {
        fn id(raw: &str) -> Result<u32, String> {
            raw.parse::<u32>()
                .map_err(|_| format!("'{raw}' is not a valid object or effect identifier"))
        }
        match value {
            "base-only" => self.base_only = true,
            "no-solid-final" => self.no_solid_final = true,
            "pass-log" => self.pass_log = true,
            _ => match value.split_once('=') {
                Some(("object", raw)) => self.only_object = Some(id(raw)?),
                Some(("skip-object", raw)) => self.skip_objects.push(id(raw)?),
                Some(("skip-effect", raw)) => self.skip_effects.push(id(raw)?),
                _ => {
                    return Err(
                        "expected base-only, no-solid-final, pass-log, object=<id>, \
                         skip-object=<id> or skip-effect=<id>"
                            .into(),
                    );
                }
            },
        }
        Ok(())
    }

    pub fn is_enabled(&self) -> bool {
        *self != Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workshop_id_or_path_posix() {
        let id = |s: &str| WallpaperSource::WorkshopId(s.into());
        let path = |s: &str| WallpaperSource::Path(s.into());
        assert_eq!(
            WallpaperSource::classify_with("2317494988", false),
            id("2317494988")
        );
        assert_eq!(
            WallpaperSource::classify_with("mon_fond", false),
            id("mon_fond")
        );
        assert_eq!(
            WallpaperSource::classify_with("./mon_fond", false),
            path("./mon_fond")
        );
        assert_eq!(
            WallpaperSource::classify_with("/abs/dir", false),
            path("/abs/dir")
        );
        assert_eq!(WallpaperSource::classify_with("a\\b", false), id("a\\b"));
        assert_eq!(WallpaperSource::classify_with("C:x", false), id("C:x"));
    }

    #[test]
    fn workshop_id_or_path_windows() {
        let path = |s: &str| WallpaperSource::Path(s.into());
        assert_eq!(WallpaperSource::classify_with("a\\b", true), path("a\\b"));
        assert_eq!(WallpaperSource::classify_with("C:x", true), path("C:x"));
        assert_eq!(WallpaperSource::classify_with("./x", true), path("./x"));
        assert_eq!(
            WallpaperSource::classify_with("123", true),
            WallpaperSource::WorkshopId("123".into())
        );
    }

    #[test]
    fn window_geometry() {
        let g = |x, y, width, height| WindowGeometry {
            x,
            y,
            width,
            height,
        };
        assert_eq!(WindowGeometry::parse("0x0x800x600"), Ok(g(0, 0, 800, 600)));
        assert_eq!(
            WindowGeometry::parse("-10x+20x800x600"),
            Ok(g(-10, 20, 800, 600))
        );
        assert_eq!(
            WindowGeometry::parse("10px 5x 800x600pt"),
            Ok(g(10, 5, 800, 600))
        );
        assert_eq!(WindowGeometry::parse("1x2x3x4x5"), Ok(g(1, 2, 3, 4)));
        assert_eq!(WindowGeometry::parse("ax0x10x10"), Ok(g(0, 0, 10, 10)));
        assert!(WindowGeometry::parse("").is_err());
        assert!(WindowGeometry::parse("1x2x3").is_err());
        assert!(WindowGeometry::parse("1X2X3X4").is_err());
        assert!(WindowGeometry::parse("0x0x0x600").is_err());
        assert!(WindowGeometry::parse("0x0x800x-1").is_err());
        assert_eq!(
            WindowGeometry::parse("0x0x99999999999x1").unwrap().width,
            i32::MAX
        );
    }

    #[test]
    fn numeric_prefix_saturates() {
        assert_eq!(numeric_prefix("-99999999999"), i32::MIN);
        assert_eq!(numeric_prefix("  42abc"), 42);
        assert_eq!(numeric_prefix("-"), 0);
    }

    #[test]
    fn screenshot_extensions_are_case_sensitive() {
        let f = |s: &str| ScreenshotFormat::from_path(Path::new(s));
        assert_eq!(f("a.bmp"), Ok(ScreenshotFormat::Bmp));
        assert_eq!(f("/tmp/a.png"), Ok(ScreenshotFormat::Png));
        assert_eq!(f("a.jpg"), Ok(ScreenshotFormat::Jpeg));
        assert_eq!(f("a.jpeg"), Ok(ScreenshotFormat::Jpeg));
        assert!(f("a.PNG").is_err());
        assert!(f("a.gif").is_err());
        assert!(f("noext").is_err());
    }

    #[test]
    fn bounds() {
        assert_eq!(clamp_volume(-5), 0);
        assert_eq!(clamp_volume(200), 128);
        assert_eq!(clamp_volume(15), 15);
        assert_eq!(validate_fps(60), Ok(60));
        assert!(validate_fps(0).is_err());
        assert!(validate_fps(-1).is_err());
        assert!(validate_fps(i64::MAX).is_err());
        assert_eq!(validate_screenshot_delay(9), Ok(5));
        assert_eq!(validate_screenshot_delay(0), Ok(0));
        assert!(validate_screenshot_delay(-1).is_err());
    }

    #[test]
    fn volume_conversions() {
        assert_eq!(volume_fraction(DEFAULT_VOLUME), 0.5);
        assert_eq!(volume_fraction(128), 1.0);
        assert_eq!(volume_fraction(0), 0.0);
        assert_eq!(volume_fraction(200), 1.0);
        assert_eq!(volume_from_fraction(0.5), DEFAULT_VOLUME);
        assert_eq!(volume_from_fraction(1.0), 128);
        assert_eq!(volume_from_fraction(2.0), 128);
        assert_eq!(volume_from_fraction(-1.0), 0);
        assert_eq!(volume_from_fraction(f32::NAN), 0);
        // The reference default shows as 12 %.
        assert_eq!(
            (volume_fraction(REFERENCE_DEFAULT_VOLUME) * 100.0).round(),
            12.0
        );
        // Every percent the UI can send survives the round trip.
        for p in 0..=100u8 {
            let v = volume_from_fraction(f32::from(p) / 100.0);
            assert_eq!((volume_fraction(v) * 100.0).round() as u8, p, "{p} %");
        }
    }

    #[test]
    fn render_debug_values() {
        let mut d = RenderDebug::default();
        assert!(!d.is_enabled());
        for v in [
            "base-only",
            "no-solid-final",
            "pass-log",
            "object=3",
            "object=7",
        ] {
            d.apply(v).unwrap();
        }
        d.apply("skip-object=1").unwrap();
        d.apply("skip-object=2").unwrap();
        d.apply("skip-effect=9").unwrap();
        assert_eq!(
            d,
            RenderDebug {
                base_only: true,
                no_solid_final: true,
                pass_log: true,
                only_object: Some(7),
                skip_objects: vec![1, 2],
                skip_effects: vec![9],
            }
        );
        for bad in [
            "",
            "wireframe",
            "object",
            "object=",
            "object=x",
            "object=-1",
            "object=4294967296",
        ] {
            assert!(RenderDebug::default().apply(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn value_enums_are_exact() {
        assert_eq!(parse_value_enum::<Scaling>("fill"), Ok(Scaling::Fill));
        assert!(parse_value_enum::<Scaling>("Fill").is_err());
        assert_eq!(
            parse_value_enum::<HwDecode>("nvdec-copy"),
            Ok(HwDecode::NvdecCopy)
        );
        assert_eq!(Scaling::Default.to_string(), "default");
        assert_eq!(HwDecode::NvdecCopy.to_string(), "nvdec-copy");
        let err = parse_value_enum::<Clamp>("wrap").unwrap_err();
        assert_eq!(err, "expected one of: clamp, border, repeat");
    }
}
