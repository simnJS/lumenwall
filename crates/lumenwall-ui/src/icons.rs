//! Lumenwall's own icon set: 24×24 outline icons (2 px round strokes), tinted at draw
//! time, plus a few two-tone illustrations for empty states. Drawn for this project;
//! no third-party icon set or Wallpaper Engine artwork.

use iced::widget::svg::{self, Svg};
use iced::{Color, Element, Length};

use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Library,
    Screens,
    Settings,
    Play,
    Pause,
    Volume,
    VolumeOff,
    Search,
    Folder,
    Check,
    Close,
    Scene,
    Video,
    Web,
    Alert,
    Logo,
    Refresh,
    External,
    Plus,
    Gauge,
    Sparkle,
    Workshop,
    Download,
    Star,
    StarHalf,
    StarEmpty,
    Users,
    Clock,
    Key,
    Package,
    Heart,
    Eye,
    Store,
    /// Window buttons (the title bar the app draws on Linux).
    Minimize,
    Maximize,
    Restore,
}

fn path(icon: Icon) -> &'static str {
    match icon {
        Icon::Library => {
            r#"<rect x="3" y="3" width="7.5" height="7.5" rx="2"/><rect x="13.5" y="3" width="7.5" height="7.5" rx="2"/><rect x="3" y="13.5" width="7.5" height="7.5" rx="2"/><rect x="13.5" y="13.5" width="7.5" height="7.5" rx="2"/>"#
        }
        Icon::Screens => {
            r#"<rect x="2.5" y="4" width="19" height="12.5" rx="2"/><path d="M9 20.5h6M12 16.5v4"/>"#
        }
        Icon::Settings => {
            r#"<path d="M4 6.5h9M17 6.5h3M4 12h3M11 12h9M4 17.5h11M19 17.5h1"/><circle cx="15" cy="6.5" r="2"/><circle cx="9" cy="12" r="2"/><circle cx="17" cy="17.5" r="2"/>"#
        }
        Icon::Play => r#"<path d="M7 4.5v15l12-7.5z" fill="black"/>"#,
        Icon::Pause => {
            r#"<rect x="6" y="4.5" width="4" height="15" rx="1" fill="black"/><rect x="14" y="4.5" width="4" height="15" rx="1" fill="black"/>"#
        }
        Icon::Volume => {
            r#"<path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z" fill="black"/><path d="M15.5 9a4 4 0 0 1 0 6M18.5 6.5a8 8 0 0 1 0 11"/>"#
        }
        Icon::VolumeOff => {
            r#"<path d="M4 9.5h3.5L12 5.5v13l-4.5-4H4z" fill="black"/><path d="M16 9.5l5 5M21 9.5l-5 5"/>"#
        }
        Icon::Search => r#"<circle cx="10.5" cy="10.5" r="6.5"/><path d="M15.5 15.5L20.5 20.5"/>"#,
        Icon::Folder => {
            r#"<path d="M3 7.5a2 2 0 0 1 2-2h4l2 2.5h8a2 2 0 0 1 2 2v7.5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>"#
        }
        Icon::Check => r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#,
        Icon::Close => r#"<path d="M6 6l12 12M18 6L6 18"/>"#,
        Icon::Scene => {
            r#"<path d="M12 3.5l9 4.5-9 4.5-9-4.5z"/><path d="M3 12l9 4.5 9-4.5M3 16l9 4.5 9-4.5"/>"#
        }
        Icon::Video => {
            r#"<rect x="3" y="5" width="18" height="14" rx="2.5"/><path d="M10 9.5v5l4.5-2.5z" fill="black"/>"#
        }
        Icon::Web => {
            r#"<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3c2.6 2.6 3.8 5.6 3.8 9s-1.2 6.4-3.8 9c-2.6-2.6-3.8-5.6-3.8-9S9.4 5.6 12 3z"/>"#
        }
        Icon::Alert => r#"<path d="M12 3.5L21.5 20h-19z"/><path d="M12 10v4.5M12 17.2v.3"/>"#,
        // The brand symbol (`assets/brand/logo/lumenwall-symbol-mono.svg`): sun cut at
        // the horizon, horizon, reflection.
        Icon::Logo => {
            r#"<path d="M6.523 12.5A5.75 5.75 0 1 1 17.477 12.5Z" fill="black" stroke="none"/><path d="M2.5 15h19M7.5 18.5h9"/>"#
        }
        Icon::Refresh => {
            r#"<path d="M20 11a8 8 0 0 0-14.5-4.5M4 5v4h4M4 13a8 8 0 0 0 14.5 4.5M20 19v-4h-4"/>"#
        }
        Icon::External => {
            r#"<path d="M14 4h6v6M20 4l-9 9M18 14v4a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h4"/>"#
        }
        Icon::Plus => r#"<path d="M12 5v14M5 12h14"/>"#,
        Icon::Gauge => {
            r#"<path d="M4.5 17a8.5 8.5 0 1 1 15 0"/><path d="M12 13l3.5-4"/><circle cx="12" cy="13" r="1.2" fill="black"/>"#
        }
        Icon::Workshop => {
            r#"<circle cx="12" cy="12" r="9"/><path d="M15.5 8.5l-2 5-5 2 2-5z" fill="black"/>"#
        }
        Icon::Download => r#"<path d="M12 4v11M7 10.5l5 5 5-5M5 20h14"/>"#,
        Icon::Star => {
            r#"<path d="M12 3.5l2.6 5.5 6 .8-4.4 4.1 1.1 5.9L12 17l-5.3 2.8 1.1-5.9L3.4 9.8l6-.8z" fill="black"/>"#
        }
        Icon::StarHalf => {
            r#"<path d="M12 3.5l2.6 5.5 6 .8-4.4 4.1 1.1 5.9L12 17l-5.3 2.8 1.1-5.9L3.4 9.8l6-.8z"/><path d="M12 3.5V17l-5.3 2.8 1.1-5.9L3.4 9.8l6-.8z" fill="black"/>"#
        }
        Icon::StarEmpty => {
            r#"<path d="M12 3.5l2.6 5.5 6 .8-4.4 4.1 1.1 5.9L12 17l-5.3 2.8 1.1-5.9L3.4 9.8l6-.8z"/>"#
        }
        Icon::Users => {
            r#"<circle cx="9" cy="8.5" r="3.5"/><path d="M3 19.5c.8-3.3 3.1-5 6-5s5.2 1.7 6 5M16 5.5a3.5 3.5 0 0 1 0 6.5M18 14.8c1.6.8 2.6 2.4 3 4.7"/>"#
        }
        Icon::Clock => r#"<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3.5 2"/>"#,
        Icon::Key => {
            r#"<circle cx="7.5" cy="15.5" r="4"/><path d="M10.5 12.5L20 3M16 7l3 3M14 9l2 2"/>"#
        }
        Icon::Package => {
            r#"<path d="M3.5 7.5L12 3l8.5 4.5v9L12 21l-8.5-4.5z"/><path d="M3.5 7.5L12 12l8.5-4.5M12 12v9"/>"#
        }
        Icon::Heart => {
            r#"<path d="M12 20s-7.5-4.6-7.5-10A4.3 4.3 0 0 1 12 7.2 4.3 4.3 0 0 1 19.5 10c0 5.4-7.5 10-7.5 10z"/>"#
        }
        Icon::Store => r#"<path d="M5 8h14l-1.2 12H6.2z"/><path d="M9 8V6.5a3 3 0 0 1 6 0V8"/>"#,
        Icon::Eye => {
            r#"<path d="M2.5 12S6 5.5 12 5.5 21.5 12 21.5 12 18 18.5 12 18.5 2.5 12 2.5 12z"/><circle cx="12" cy="12" r="3"/>"#
        }
        Icon::Sparkle => r#"<path d="M12 3l2 6.5L20.5 12 14 14l-2 7-2-7-6.5-2L10 9.5z"/>"#,
        Icon::Minimize => r#"<path d="M6 12h12"/>"#,
        Icon::Maximize => r#"<rect x="6" y="6" width="12" height="12" rx="2"/>"#,
        Icon::Restore => {
            r#"<rect x="5" y="9" width="10" height="10" rx="2"/><path d="M9 9V7a2 2 0 0 1 2-2h6a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2h-2"/>"#
        }
    }
}

fn source(icon: Icon) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
        path(icon)
    )
}

pub fn handle(icon: Icon) -> svg::Handle {
    svg::Handle::from_memory(source(icon).into_bytes())
}

/// An icon tinted with `color`.
pub fn icon<'a, M: 'a>(icon: Icon, size: f32, color: Color) -> Element<'a, M> {
    Svg::new(handle(icon))
        .width(size)
        .height(size)
        .style(theme::icon_tint(color))
        .into()
}

fn hex(c: Color) -> String {
    let [r, g, b, _] = c.into_rgba8();
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Two-tone illustrations for empty states.
#[derive(Debug, Clone, Copy)]
pub enum Illustration {
    /// A monitor with the sun at the horizon: "no wallpapers yet".
    Screen,
    /// A magnifier over cards: "nothing matches".
    Search,
    /// A plug: "engine not running".
    Engine,
    /// A folder with an alert mark: "Steam was not found".
    Folder,
    /// A globe of cards with a key: "browse the Workshop".
    Workshop,
    /// A cloud with a slash: "Steam unreachable".
    Offline,
}

pub fn illustration<'a, M: 'a>(kind: Illustration, t: &theme::Tokens) -> Element<'a, M> {
    let (a, m, s, w) = (hex(t.accent), hex(t.muted), hex(t.overlay), hex(t.warning));
    let body = match kind {
        // The brand symbol's geometry (sun cut at the horizon, horizon, reflection),
        // inside the screen.
        Illustration::Screen => format!(
            r#"<rect x="20" y="20" width="120" height="80" rx="10" fill="{s}" stroke="{m}" stroke-width="3"/>
               <path d="M64.76 64A16 16 0 1 1 95.24 64Z" fill="{a}"/>
               <path d="M53.6 71h52.8M67.5 80.7h25" stroke="{a}" stroke-width="5.5" stroke-linecap="round"/>
               <path d="M62 116h36M80 100v16" stroke="{m}" stroke-width="3" stroke-linecap="round"/>"#
        ),
        Illustration::Folder => format!(
            r#"<path d="M24 38a8 8 0 0 1 8-8h26l10 11h56a8 8 0 0 1 8 8v52a8 8 0 0 1-8 8H32a8 8 0 0 1-8-8z" fill="{s}" stroke="{m}" stroke-width="3" stroke-linejoin="round"/>
               <path d="M24 54h108" stroke="{m}" stroke-width="3"/>
               <path d="M112 76l20 34h-40z" fill="{s}" stroke="{w}" stroke-width="4" stroke-linejoin="round"/>
               <path d="M112 88v9M112 103v.5" stroke="{w}" stroke-width="4" stroke-linecap="round"/>"#
        ),
        Illustration::Search => format!(
            r#"<rect x="18" y="30" width="54" height="40" rx="8" fill="{s}" stroke="{m}" stroke-width="3"/>
               <rect x="82" y="30" width="54" height="40" rx="8" fill="{s}" stroke="{m}" stroke-width="3" opacity="0.6"/>
               <rect x="18" y="78" width="54" height="40" rx="8" fill="{s}" stroke="{m}" stroke-width="3" opacity="0.6"/>
               <circle cx="104" cy="96" r="20" fill="none" stroke="{a}" stroke-width="5"/>
               <path d="M118 110l16 16" stroke="{a}" stroke-width="6" stroke-linecap="round"/>"#
        ),
        Illustration::Workshop => format!(
            r#"<circle cx="72" cy="64" r="40" fill="{s}" stroke="{m}" stroke-width="3"/>
               <path d="M32 64h80M72 24c14 12 18 26 18 40s-4 28-18 40c-14-12-18-26-18-40s4-28 18-40z" fill="none" stroke="{m}" stroke-width="3"/>
               <rect x="96" y="70" width="40" height="26" rx="5" fill="{s}" stroke="{a}" stroke-width="3"/>
               <path d="M108 83l6 6 10-11" stroke="{a}" stroke-width="3" fill="none" stroke-linecap="round" stroke-linejoin="round"/>
               <circle cx="122" cy="34" r="9" fill="none" stroke="{a}" stroke-width="3"/>
               <path d="M115 41l-14 14M106 50l4 4" stroke="{a}" stroke-width="3" stroke-linecap="round"/>"#
        ),
        Illustration::Offline => format!(
            r#"<path d="M44 92h70a22 22 0 0 0 0-44 30 30 0 0 0-57-6 24 24 0 0 0-13 50z" fill="{s}" stroke="{m}" stroke-width="3"/>
               <path d="M40 28l84 84" stroke="{a}" stroke-width="5" stroke-linecap="round"/>"#
        ),
        Illustration::Engine => format!(
            r#"<path d="M30 70h28M102 70h28" stroke="{m}" stroke-width="4" stroke-linecap="round"/>
               <rect x="58" y="50" width="22" height="40" rx="6" fill="{s}" stroke="{m}" stroke-width="3"/>
               <rect x="88" y="50" width="22" height="40" rx="6" fill="{s}" stroke="{a}" stroke-width="3"/>
               <path d="M80 60h8M80 80h8" stroke="{m}" stroke-width="3" stroke-linecap="round" stroke-dasharray="2 4"/>
               <path d="M84 26l-6 14h10l-6 14" stroke="{a}" stroke-width="3" fill="none" stroke-linecap="round" stroke-linejoin="round"/>"#
        ),
    };
    let src =
        format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 160 130">{body}</svg>"#);
    Svg::new(svg::Handle::from_memory(src.into_bytes()))
        .width(Length::Fixed(176.0))
        .height(Length::Fixed(143.0))
        .into()
}

// ---------------------------------------------------------------- brand assets
//
// Generated by `assets/brand/build.py` and embedded as they are: the app shows the
// same mark as the packages, and small sizes use their hinted masters.

const SYMBOL_ON_DARK: &[u8] =
    include_bytes!("../../../assets/brand/logo/lumenwall-symbol-color.svg");
const SYMBOL_ON_LIGHT: &[u8] =
    include_bytes!("../../../assets/brand/logo/lumenwall-symbol-color-light.svg");
const LOGO_ON_DARK: &[u8] = include_bytes!("../../../assets/brand/logo/lumenwall-logo-on-dark.svg");
const LOGO_ON_LIGHT: &[u8] =
    include_bytes!("../../../assets/brand/logo/lumenwall-logo-on-light.svg");

/// Width / height of an SVG, from its `viewBox` (the logo is regenerated by
/// `build.py`, its proportions are not hard-coded here).
fn svg_ratio(src: &[u8]) -> Option<f32> {
    let s = std::str::from_utf8(src).ok()?;
    let rest = &s[s.find("viewBox=\"")? + 9..];
    let v: Vec<f32> = rest[..rest.find('"')?]
        .split_whitespace()
        .filter_map(|n| n.parse().ok())
        .collect();
    match v[..] {
        [_, _, w, h] if w > 0.0 && h > 0.0 => Some(w / h),
        _ => None,
    }
}

/// The colour symbol (sun gradient), in its version for the theme's background.
pub fn symbol<'a, M: 'a>(t: &theme::Tokens, size: f32) -> Element<'a, M> {
    let src = if t.dark {
        SYMBOL_ON_DARK
    } else {
        SYMBOL_ON_LIGHT
    };
    Svg::new(svg::Handle::from_memory(src))
        .width(size)
        .height(size)
        .into()
}

/// The horizontal logo (symbol and wordmark) `width` wide, clear space included.
pub fn logo<'a, M: 'a>(t: &theme::Tokens, width: f32) -> Element<'a, M> {
    let src = if t.dark { LOGO_ON_DARK } else { LOGO_ON_LIGHT };
    let ratio = svg_ratio(src).unwrap_or(3.76);
    Svg::new(svg::Handle::from_memory(src))
        .width(width)
        .height(width / ratio)
        .into()
}

const APP_ICON_32: &[u8] = include_bytes!("../../../assets/brand/icon/png/lumenwall-32.png");
const APP_ICON_64: &[u8] = include_bytes!("../../../assets/brand/icon/png/lumenwall-64.png");

/// Decodes an embedded PNG into `(width, height, RGBA pixels)`.
pub fn decode_png(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    match image::load_from_memory_with_format(bytes, image::ImageFormat::Png) {
        Ok(img) => {
            let rgba = img.into_rgba8();
            Some((rgba.width(), rgba.height(), rgba.into_raw()))
        }
        Err(e) => {
            tracing::warn!("embedded icon: {e}");
            None
        }
    }
}

/// The app icon for the window: 64 px (X11 task switchers scale it down), 32 px
/// on Windows, where the hinted sizes of `lumenwall.ico` replace it once the window
/// exists (see `platform::set_window_icon`).
pub fn window_icon() -> Option<iced::window::Icon> {
    let png = if cfg!(windows) {
        APP_ICON_32
    } else {
        APP_ICON_64
    };
    let (w, h, rgba) = decode_png(png)?;
    iced::window::icon::from_rgba(rgba, w, h).ok()
}

/// Ink of the monochrome tray glyph: the panel's text colour, white on dark panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayInk {
    White,
    Black,
}

/// Sizes of the tray glyph masters (100, 125, 150 and 200 % of a 16 px icon).
pub const TRAY_SIZES: [u32; 4] = [16, 20, 24, 32];

macro_rules! tray_png {
    ($ink:literal, $state:literal, $size:literal) => {
        include_bytes!(concat!(
            "../../../assets/brand/tray/png/lumenwall-tray-",
            $ink,
            $state,
            "-",
            $size,
            ".png"
        ))
    };
}

/// The tray glyph (solid sun when playing, outlined when paused or stopped) for
/// the closest master at or above `size` pixels.
pub fn tray_png(ink: TrayInk, paused: bool, size: u32) -> &'static [u8] {
    let i = TRAY_SIZES
        .iter()
        .position(|&s| s >= size)
        .unwrap_or(TRAY_SIZES.len() - 1);
    const WHITE: [&[u8]; 4] = [
        tray_png!("white", "", "16"),
        tray_png!("white", "", "20"),
        tray_png!("white", "", "24"),
        tray_png!("white", "", "32"),
    ];
    const WHITE_PAUSED: [&[u8]; 4] = [
        tray_png!("white", "-paused", "16"),
        tray_png!("white", "-paused", "20"),
        tray_png!("white", "-paused", "24"),
        tray_png!("white", "-paused", "32"),
    ];
    const BLACK: [&[u8]; 4] = [
        tray_png!("black", "", "16"),
        tray_png!("black", "", "20"),
        tray_png!("black", "", "24"),
        tray_png!("black", "", "32"),
    ];
    const BLACK_PAUSED: [&[u8]; 4] = [
        tray_png!("black", "-paused", "16"),
        tray_png!("black", "-paused", "20"),
        tray_png!("black", "-paused", "24"),
        tray_png!("black", "-paused", "32"),
    ];
    match (ink, paused) {
        (TrayInk::White, false) => WHITE[i],
        (TrayInk::White, true) => WHITE_PAUSED[i],
        (TrayInk::Black, false) => BLACK[i],
        (TrayInk::Black, true) => BLACK_PAUSED[i],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_is_valid_xml() {
        for icon in [
            Icon::Library,
            Icon::Screens,
            Icon::Settings,
            Icon::Play,
            Icon::Pause,
            Icon::Volume,
            Icon::VolumeOff,
            Icon::Search,
            Icon::Folder,
            Icon::Check,
            Icon::Close,
            Icon::Scene,
            Icon::Video,
            Icon::Web,
            Icon::Alert,
            Icon::Logo,
            Icon::Refresh,
            Icon::External,
            Icon::Plus,
            Icon::Gauge,
            Icon::Sparkle,
            Icon::Workshop,
            Icon::Download,
            Icon::Star,
            Icon::StarHalf,
            Icon::StarEmpty,
            Icon::Users,
            Icon::Clock,
            Icon::Key,
            Icon::Package,
            Icon::Heart,
            Icon::Eye,
            Icon::Store,
            Icon::Minimize,
            Icon::Maximize,
            Icon::Restore,
        ] {
            let src = source(icon);
            assert_eq!(
                src.matches('<').count(),
                src.matches('>').count(),
                "{icon:?}"
            );
            assert!(src.ends_with("</svg>"));
        }
    }

    #[test]
    fn brand_pngs_decode_at_their_size() {
        let (w, h, px) = decode_png(APP_ICON_64).unwrap();
        assert_eq!((w, h, px.len()), (64, 64, 64 * 64 * 4));
        assert_eq!(decode_png(APP_ICON_32).unwrap().0, 32);
        for ink in [TrayInk::White, TrayInk::Black] {
            for paused in [false, true] {
                for size in TRAY_SIZES {
                    let (w, h, px) = decode_png(tray_png(ink, paused, size)).unwrap();
                    assert_eq!((w, h), (size, size), "{ink:?} {paused} {size}");
                    // Monochrome glyph on a transparent background.
                    let ink_rgb = if ink == TrayInk::White { 255 } else { 0 };
                    assert!(px.chunks(4).all(|p| p[3] == 0 || p[0] == ink_rgb));
                    assert!(px.chunks(4).any(|p| p[3] == 255));
                }
            }
        }
        // Closest master at or above the asked size.
        assert_eq!(tray_png(TrayInk::White, false, 22).len(), {
            tray_png(TrayInk::White, false, 24).len()
        });
        assert_eq!(
            decode_png(tray_png(TrayInk::Black, true, 64)).unwrap().0,
            32
        );
    }

    #[test]
    fn brand_svgs_are_embedded() {
        for src in [SYMBOL_ON_DARK, SYMBOL_ON_LIGHT, LOGO_ON_DARK, LOGO_ON_LIGHT] {
            let s = std::str::from_utf8(src).unwrap();
            assert!(s.starts_with("<svg") && s.trim_end().ends_with("</svg>"));
        }
        let ratio = svg_ratio(LOGO_ON_DARK).unwrap();
        assert!((2.0..6.0).contains(&ratio), "{ratio}");
        assert_eq!(svg_ratio(LOGO_ON_LIGHT), Some(ratio));
        assert_eq!(svg_ratio(SYMBOL_ON_DARK), Some(1.0));
    }
}
