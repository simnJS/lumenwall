//! Lumen's own icon set: 24×24 outline icons (2 px round strokes), tinted at draw
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
        Icon::Logo => {
            r#"<circle cx="12" cy="13" r="4.2" fill="black" stroke="none"/><path d="M12 4.5v2M5.6 7.1l1.4 1.4M18.4 7.1L17 8.5M3 13h2M19 13h2M3 18.5h18"/>"#
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
        Icon::Sparkle => r#"<path d="M12 3l2 6.5L20.5 12 14 14l-2 7-2-7-6.5-2L10 9.5z"/>"#,
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
    /// A monitor with a glowing horizon: "no wallpapers yet".
    Screen,
    /// A magnifier over cards: "nothing matches".
    Search,
    /// A plug: "engine not running".
    Engine,
}

pub fn illustration<'a, M: 'a>(kind: Illustration, t: &theme::Tokens) -> Element<'a, M> {
    let (a, m, s) = (hex(t.accent), hex(t.muted), hex(t.overlay));
    let body = match kind {
        Illustration::Screen => format!(
            r#"<rect x="20" y="22" width="120" height="78" rx="10" fill="{s}" stroke="{m}" stroke-width="3"/>
               <circle cx="80" cy="70" r="16" fill="{a}" opacity="0.9"/>
               <path d="M32 82h96" stroke="{a}" stroke-width="3" stroke-linecap="round"/>
               <path d="M80 44v-8M60 52l-6-6M100 52l6-6" stroke="{a}" stroke-width="3" stroke-linecap="round"/>
               <path d="M62 116h36M80 100v16" stroke="{m}" stroke-width="3" stroke-linecap="round"/>
               <path d="M132 20l3 8 8 3-8 3-3 8-3-8-8-3 8-3z" fill="{a}"/>"#
        ),
        Illustration::Search => format!(
            r#"<rect x="18" y="30" width="54" height="40" rx="8" fill="{s}" stroke="{m}" stroke-width="3"/>
               <rect x="82" y="30" width="54" height="40" rx="8" fill="{s}" stroke="{m}" stroke-width="3" opacity="0.6"/>
               <rect x="18" y="78" width="54" height="40" rx="8" fill="{s}" stroke="{m}" stroke-width="3" opacity="0.6"/>
               <circle cx="104" cy="96" r="20" fill="none" stroke="{a}" stroke-width="5"/>
               <path d="M118 110l16 16" stroke="{a}" stroke-width="6" stroke-linecap="round"/>"#
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

/// Logo as RGBA pixels, for the window and tray icons.
pub fn logo_rgba(size: u32, color: [u8; 3]) -> Vec<u8> {
    let mut px = vec![0u8; (size * size * 4) as usize];
    let s = size as f32;
    let (cx, cy) = (s / 2.0, s / 2.0 + s * 0.04);
    for y in 0..size {
        for x in 0..size {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            // Disc, rays above the horizon, horizon line.
            let mut a: f32 = (s * 0.2 - d + 0.5).clamp(0.0, 1.0);
            let angle = (fy - cy).atan2(fx - cx);
            if fy < cy && d > s * 0.28 && d < s * 0.44 {
                let k = (angle + std::f32::consts::PI) / (std::f32::consts::PI / 6.0);
                let ray = (1.0 - (k - k.round()).abs() * 10.0).clamp(0.0, 1.0);
                a = a.max(ray);
            }
            if (fy - (s / 2.0 + s * 0.28)).abs() < s * 0.05 && (fx - cx).abs() < s * 0.44 {
                a = 1.0;
            }
            let i = ((y * size + x) * 4) as usize;
            px[i..i + 3].copy_from_slice(&color);
            px[i + 3] = (a * 255.0) as u8;
        }
    }
    px
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
}
