//! Lumenwall's design system: colour tokens (dark default, light), spacing and radius
//! scales, three elevation levels, a type scale, and the style of every widget.
//! Views never pick raw colours or sizes; they use these.

use std::sync::OnceLock;

use iced::border::Radius;
use iced::widget::{button, container, pick_list, scrollable, slider, svg, text_input, toggler};
use iced::{
    Background, Border, Color, Font, Gradient, Shadow, Theme, Vector, color, font, gradient,
};

/// Spacing scale (logical pixels).
pub mod space {
    pub const XS: f32 = 4.0;
    pub const S: f32 = 8.0;
    pub const M: f32 = 12.0;
    pub const L: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const XXL: f32 = 32.0;
}

/// Corner radii.
pub mod radius {
    pub const S: f32 = 6.0;
    pub const M: f32 = 10.0;
    pub const L: f32 = 14.0;
    pub const PILL: f32 = 999.0;
}

/// Type scale.
pub mod size {
    pub const DISPLAY: f32 = 28.0;
    pub const TITLE: f32 = 20.0;
    /// On the 4 px rhythm (16/24 line).
    pub const HEADING: f32 = 16.0;
    pub const BODY: f32 = 14.0;
    pub const CAPTION: f32 = 12.5;
    pub const MICRO: f32 = 11.0;
}

/// Icon sizes: four steps keep the 2-unit stroke of the 24-unit grid between
/// 1.17 and 2 px (see `docs/BRAND.md`, Iconography).
pub mod icon {
    /// Status bar, badges, dense rows.
    pub const S: f32 = 14.0;
    /// Buttons, chips, fields.
    pub const M: f32 = 16.0;
    /// Navigation, notices.
    pub const L: f32 = 20.0;
    /// Headers (part of the scale; no view uses it yet).
    #[allow(dead_code)]
    pub const XL: f32 = 24.0;
    /// The brand mark in the sidebar, nothing else.
    pub const BRAND: f32 = 28.0;
}

/// Named durations (iced's `Animation` presets) and the system's "reduce
/// animations" setting: when it is on, every duration is zero, so an animation
/// ends as soon as it starts and no frame subscription is created for it.
pub mod motion {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use iced::Animation;
    use iced::animation::Easing;

    /// Press, small colour changes (ease-out). Part of the scale; iced styles
    /// change instantly today, so no view uses it yet.
    #[allow(dead_code)]
    pub const FAST: Duration = Duration::from_millis(100);
    /// Hover wash, chips, toggles (ease-in-out).
    pub const BASE: Duration = Duration::from_millis(200);
    /// Details panel (ease-out entering, ease-in leaving).
    pub const PANEL: Duration = Duration::from_millis(220);
    /// Toasts (ease-in-out).
    pub const SLOW: Duration = Duration::from_millis(320);

    static REDUCED: AtomicBool = AtomicBool::new(false);

    /// Follows the system setting (read at start and when the window regains focus).
    pub fn set_reduced(reduced: bool) {
        REDUCED.store(reduced, Ordering::Relaxed);
    }

    pub fn reduced() -> bool {
        REDUCED.load(Ordering::Relaxed)
    }

    /// `d`, or zero when the system asks for reduced motion.
    pub fn duration(d: Duration) -> Duration {
        if reduced() { Duration::ZERO } else { d }
    }

    /// Hover wash of a card, starting from `from` and going to `to` now.
    pub fn hover(from: bool, to: bool, now: Instant) -> Animation<bool> {
        Animation::new(from)
            .duration(duration(BASE))
            .easing(Easing::EaseInOut)
            .go(to, now)
    }

    /// The details panel at rest, open or closed.
    pub fn panel_at(open: bool) -> Animation<bool> {
        Animation::new(open).duration(duration(PANEL))
    }

    /// Slides the panel towards `open` from wherever it is.
    pub fn panel_go(anim: &Animation<bool>, open: bool, now: Instant) -> Animation<bool> {
        anim.clone()
            .duration(duration(PANEL))
            .easing(if open {
                Easing::EaseOut
            } else {
                Easing::EaseIn
            })
            .go(open, now)
    }

    /// A toast appearing.
    pub fn toast(now: Instant) -> Animation<bool> {
        Animation::new(false)
            .duration(duration(SLOW))
            .easing(Easing::EaseOut)
            .go(true, now)
    }
}

/// Layout constants shared by views and by the update logic (visibility maths).
pub mod layout {
    pub const SIDEBAR_W: f32 = 224.0;
    pub const PANEL_W: f32 = 380.0;
    pub const STATUS_H: f32 = 44.0;
    pub const CARD_MIN_W: f32 = 260.0;
    pub const CARD_GAP: f32 = 16.0;
    pub const PAGE_PAD: f32 = 24.0;
}

#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    pub dark: bool,
    pub bg: Color,
    pub sidebar: Color,
    pub surface: Color,
    pub raised: Color,
    pub overlay: Color,
    pub border: Color,
    /// The window's 1 px outline on Linux, where the app draws its own frame (`chrome.rs`).
    /// Opaque: a translucent outline over the transparent window lets the desktop show
    /// through, and the compositor brightens it (see `chrome.rs`).
    pub frame: Color,
    pub text: Color,
    pub muted: Color,
    /// Placeholders, hints, secondary labels: still AA (≥ 4.5:1) on every surface.
    pub faint: Color,
    /// Fills and outlines: buttons, chips, selection, sliders.
    pub accent: Color,
    pub accent_hover: Color,
    pub accent_soft: Color,
    /// The accent used as text or as an icon on a surface (selected navigation,
    /// values, links): darker than `accent` in the light theme to stay AA.
    pub accent_text: Color,
    pub on_accent: Color,
    pub success: Color,
    pub warning: Color,
    /// Text on a `warning` fill (badges).
    pub on_warning: Color,
    pub danger: Color,
    /// Text on a `danger` fill (error toasts, red badges).
    pub on_danger: Color,
}

pub const DARK: Tokens = Tokens {
    dark: true,
    bg: color!(0x111217),
    sidebar: color!(0x0c0d11),
    surface: color!(0x181a21),
    raised: color!(0x20232c),
    overlay: color!(0x2a2e39),
    border: color!(0x2c303b),
    // `border`: the panels' edge colour, 1.4:1 on the sidebar.
    frame: color!(0x2c303b),
    text: color!(0xeeeff4),
    muted: color!(0xa3a7b7),
    faint: color!(0x878c9e),
    accent: color!(0xffb347),
    accent_hover: color!(0xffc46e),
    accent_soft: Color {
        r: 1.0,
        g: 0.70,
        b: 0.28,
        a: 0.14,
    },
    accent_text: color!(0xffb347),
    on_accent: color!(0x221605),
    success: color!(0x5cd68d),
    warning: color!(0xf5c542),
    on_warning: color!(0x221605),
    danger: color!(0xff7a7a),
    on_danger: color!(0x2a0a0a),
};

pub const LIGHT: Tokens = Tokens {
    dark: false,
    bg: color!(0xf6f6f9),
    sidebar: color!(0xececf1),
    surface: color!(0xffffff),
    raised: color!(0xffffff),
    overlay: color!(0xf0f0f4),
    border: color!(0xdedfe6),
    // A step darker than `border` (1.5:1 on the sidebar) so the window stays outlined
    // over white windows behind it.
    frame: color!(0xc6c8d2),
    text: color!(0x16171c),
    muted: color!(0x4d5060),
    faint: color!(0x676a7b),
    accent: color!(0xb35d00),
    // In the light theme hover darkens (white label: 6.1:1).
    accent_hover: color!(0x9a4d00),
    accent_soft: Color {
        r: 0.70,
        g: 0.36,
        b: 0.0,
        a: 0.10,
    },
    accent_text: color!(0x9a4d00),
    on_accent: color!(0xffffff),
    success: color!(0x187a41),
    warning: color!(0x8a6200),
    on_warning: color!(0xffffff),
    danger: color!(0xb91c1c),
    on_danger: color!(0xffffff),
};

pub fn tokens(theme: &Theme) -> &'static Tokens {
    if theme.extended_palette().is_dark {
        &DARK
    } else {
        &LIGHT
    }
}

pub fn theme(dark: bool) -> Theme {
    let t = if dark { &DARK } else { &LIGHT };
    Theme::custom(
        if dark {
            "Lumenwall dark"
        } else {
            "Lumenwall light"
        },
        iced::theme::Palette {
            background: t.bg,
            text: t.text,
            primary: t.accent,
            success: t.success,
            warning: t.warning,
            danger: t.danger,
        },
    )
}

/// Soft shadows for three elevation levels (cards, popovers, dialogs).
pub fn elevation(level: u8, t: &Tokens) -> Shadow {
    let a = if t.dark { 1.0 } else { 0.45 };
    let (y, blur, alpha) = match level {
        0 => return Shadow::default(),
        1 => (1.0, 4.0, 0.28),
        2 => (6.0, 18.0, 0.34),
        _ => (14.0, 36.0, 0.42),
    };
    Shadow {
        color: Color {
            a: alpha * a,
            ..Color::BLACK
        },
        offset: Vector::new(0.0, y),
        blur_radius: blur,
    }
}

fn with_alpha(c: Color, a: f32) -> Color {
    Color { a, ..c }
}

// ---------------------------------------------------------------- typography

struct UiFont {
    family: font::Family,
    medium: font::Weight,
    semibold: font::Weight,
}

static UI_FONT: OnceLock<UiFont> = OnceLock::new();

/// Picks the UI font among installed families, given each family's static weights
/// (Segoe UI on Windows; Inter, Ubuntu, Lato, Noto Sans… on Linux). A family needs
/// a real bold: variable fonts expose a single face to the text engine, and faking
/// bold with another family looks broken. Call once before the app starts.
pub fn init_font(installed: &[(String, Vec<u16>)]) -> Font {
    const PREFERRED: &[&str] = &[
        "Inter",
        "Segoe UI",
        "Ubuntu",
        "Lato",
        "Noto Sans",
        "Roboto",
        "Open Sans",
        "Fira Sans",
        "Cantarell",
        "DejaVu Sans",
    ];
    let chosen = PREFERRED.iter().find_map(|name| {
        let (_, weights) = installed
            .iter()
            .find(|(f, _)| f.eq_ignore_ascii_case(name))?;
        let has = |w: u16| weights.contains(&w);
        if !has(400) || !(has(600) || has(700)) {
            return None;
        }
        Some(UiFont {
            family: font::Family::Name(name),
            medium: if has(500) {
                font::Weight::Medium
            } else {
                font::Weight::Normal
            },
            semibold: if has(600) {
                font::Weight::Semibold
            } else {
                font::Weight::Bold
            },
        })
    });
    let ui = chosen.unwrap_or(UiFont {
        family: font::Family::SansSerif,
        medium: font::Weight::Normal,
        semibold: font::Weight::Bold,
    });
    tracing::info!("UI font: {:?}", ui.family);
    let _ = UI_FONT.set(ui);
    regular()
}

fn ui_font() -> &'static UiFont {
    UI_FONT.get_or_init(|| UiFont {
        family: font::Family::SansSerif,
        medium: font::Weight::Normal,
        semibold: font::Weight::Bold,
    })
}

pub fn regular() -> Font {
    Font {
        family: ui_font().family,
        ..Font::DEFAULT
    }
}

pub fn medium() -> Font {
    Font {
        family: ui_font().family,
        weight: ui_font().medium,
        ..Font::DEFAULT
    }
}

pub fn semibold() -> Font {
    Font {
        family: ui_font().family,
        weight: ui_font().semibold,
        ..Font::DEFAULT
    }
}

// ---------------------------------------------------------------- containers

pub fn app_bg(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.bg.into()),
        text_color: Some(t.text),
        ..Default::default()
    }
}

pub fn sidebar(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.sidebar.into()),
        border: Border {
            color: t.border,
            width: 0.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

pub fn panel(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.surface.into()),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 0.0.into(),
        },
        shadow: elevation(2, t),
        ..Default::default()
    }
}

pub fn status_bar(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.sidebar.into()),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 0.0.into(),
        },
        ..Default::default()
    }
}

/// Raised surface for grouped settings and details.
pub fn card(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.raised.into()),
        border: Border {
            color: with_alpha(t.border, 0.7),
            width: 1.0,
            radius: radius::L.into(),
        },
        shadow: elevation(1, t),
        ..Default::default()
    }
}

/// Placeholder behind a preview that is still decoding.
pub fn preview_placeholder(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.overlay.into()),
        border: Border {
            radius: radius::L.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Dark gradient at the bottom of a card, under the title.
pub fn scrim(_theme: &Theme) -> container::Style {
    let g = gradient::Linear::new(std::f32::consts::PI)
        .add_stop(
            0.0,
            Color {
                a: 0.0,
                ..Color::BLACK
            },
        )
        .add_stop(
            0.55,
            Color {
                a: 0.45,
                ..Color::BLACK
            },
        )
        .add_stop(
            1.0,
            Color {
                a: 0.82,
                ..Color::BLACK
            },
        );
    container::Style {
        background: Some(Background::Gradient(Gradient::Linear(g))),
        border: Border {
            radius: Radius::default().bottom(radius::L),
            ..Default::default()
        },
        text_color: Some(Color::WHITE),
        ..Default::default()
    }
}

/// Outline drawn over a card: accent when selected, subtle when hovered.
pub fn card_frame(selected: bool, hover: f32) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tokens(theme);
        let (color, width) = if selected {
            (t.accent, 2.5)
        } else {
            (with_alpha(Color::WHITE, 0.10 + 0.25 * hover), 1.0)
        };
        container::Style {
            border: Border {
                color,
                width,
                radius: radius::L.into(),
            },
            shadow: if selected {
                Shadow {
                    color: with_alpha(t.accent, 0.35),
                    offset: Vector::ZERO,
                    blur_radius: 18.0,
                }
            } else {
                elevation(if hover > 0.5 { 2 } else { 1 }, t)
            },
            ..Default::default()
        }
    }
}

/// Small pill on top of a preview (type, status).
pub fn badge(bg: Color, fg: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(bg.into()),
        text_color: Some(fg),
        border: Border {
            radius: radius::PILL.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn glass_badge(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(
            Color {
                a: 0.55,
                ..Color::BLACK
            }
            .into(),
        ),
        text_color: Some(Color::WHITE),
        border: Border {
            radius: radius::PILL.into(),
            color: with_alpha(Color::WHITE, 0.15),
            width: 1.0,
        },
        ..Default::default()
    }
}

pub fn accent_badge(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.accent.into()),
        text_color: Some(t.on_accent),
        border: Border {
            radius: radius::PILL.into(),
            ..Default::default()
        },
        shadow: elevation(1, t),
        ..Default::default()
    }
}

/// Coloured notice (warning, error, info).
pub fn notice(kind: Notice) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tokens(theme);
        let c = match kind {
            Notice::Info => t.accent,
            Notice::Warning => t.warning,
            Notice::Danger => t.danger,
        };
        container::Style {
            background: Some(with_alpha(c, if t.dark { 0.10 } else { 0.08 }).into()),
            text_color: Some(t.text),
            border: Border {
                color: with_alpha(c, 0.35),
                width: 1.0,
                radius: radius::M.into(),
            },
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Notice {
    Info,
    Warning,
    Danger,
}

/// A monitor in the screen map.
pub fn monitor(selected: bool, drop_target: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tokens(theme);
        let (color, width) = if drop_target {
            (t.accent_hover, 3.0)
        } else if selected {
            (t.accent, 2.5)
        } else {
            (t.border, 1.0)
        };
        container::Style {
            background: Some(t.overlay.into()),
            border: Border {
                color,
                width,
                radius: radius::S.into(),
            },
            shadow: if drop_target {
                Shadow {
                    color: with_alpha(t.accent, 0.5),
                    offset: Vector::ZERO,
                    blur_radius: 16.0,
                }
            } else {
                elevation(1, t)
            },
            ..Default::default()
        }
    }
}

/// Discreet one-line explanation bar.
pub fn hint_bar(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.accent_soft.into()),
        text_color: Some(t.text),
        border: Border {
            color: with_alpha(t.accent, 0.25),
            width: 1.0,
            radius: radius::M.into(),
        },
        ..Default::default()
    }
}

/// A toast, `alpha` from 0 (hidden) to 1 while it appears or leaves.
pub fn toast(error: bool, alpha: f32) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tokens(theme);
        let bg = if error { t.danger } else { t.overlay };
        let fg = if error { t.on_danger } else { t.text };
        let mut shadow = elevation(3, t);
        shadow.color.a *= alpha;
        container::Style {
            background: Some(with_alpha(bg, alpha).into()),
            text_color: Some(with_alpha(fg, alpha)),
            border: Border {
                radius: radius::M.into(),
                color: with_alpha(t.border, alpha),
                width: if error { 0.0 } else { 1.0 },
            },
            shadow,
            ..Default::default()
        }
    }
}

pub fn swatch(c: Color) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = tokens(theme);
        container::Style {
            background: Some(c.into()),
            border: Border {
                color: t.border,
                width: 1.0,
                radius: radius::S.into(),
            },
            ..Default::default()
        }
    }
}

/// A dialog over the window (elevation 3).
pub fn dialog(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.raised.into()),
        text_color: Some(t.text),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: radius::L.into(),
        },
        shadow: elevation(3, t),
        ..Default::default()
    }
}

/// Dims the window behind a dialog.
pub fn modal_backdrop(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(
            Color {
                a: if t.dark { 0.55 } else { 0.32 },
                ..Color::BLACK
            }
            .into(),
        ),
        ..Default::default()
    }
}

pub fn ghost_drag(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.raised.into()),
        text_color: Some(t.text),
        border: Border {
            color: t.accent,
            width: 1.5,
            radius: radius::M.into(),
        },
        shadow: elevation(3, t),
        ..Default::default()
    }
}

// ---------------------------------------------------------------- buttons

fn base_button(bg: Option<Color>, fg: Color, border: Border, shadow: Shadow) -> button::Style {
    button::Style {
        background: bg.map(Background::Color),
        text_color: fg,
        border,
        shadow,
        snap: true,
    }
}

pub fn primary(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let bg = match status {
        button::Status::Hovered => t.accent_hover,
        button::Status::Pressed => Color {
            a: 0.85,
            ..t.accent
        },
        button::Status::Disabled => t.overlay,
        button::Status::Active => t.accent,
    };
    let fg = if status == button::Status::Disabled {
        t.faint
    } else {
        t.on_accent
    };
    let shadow = if status == button::Status::Disabled {
        Shadow::default()
    } else {
        elevation(1, t)
    };
    base_button(
        Some(bg),
        fg,
        Border {
            radius: radius::M.into(),
            ..Default::default()
        },
        shadow,
    )
}

pub fn secondary(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let bg = match status {
        button::Status::Hovered => t.overlay,
        button::Status::Pressed => t.border,
        _ => t.raised,
    };
    let fg = if status == button::Status::Disabled {
        t.faint
    } else {
        t.text
    };
    base_button(
        Some(bg),
        fg,
        Border {
            color: t.border,
            width: 1.0,
            radius: radius::M.into(),
        },
        Shadow::default(),
    )
}

pub fn ghost(theme: &Theme, status: button::Status) -> button::Style {
    let t = tokens(theme);
    let bg = match status {
        button::Status::Hovered => Some(t.overlay),
        button::Status::Pressed => Some(t.border),
        _ => None,
    };
    let fg = if status == button::Status::Disabled {
        t.faint
    } else {
        t.muted
    };
    base_button(
        bg,
        fg,
        Border {
            radius: radius::M.into(),
            ..Default::default()
        },
        Shadow::default(),
    )
}

pub fn nav(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let bg = if selected {
            Some(t.accent_soft)
        } else if matches!(status, button::Status::Hovered | button::Status::Pressed) {
            Some(t.overlay)
        } else {
            None
        };
        base_button(
            bg,
            if selected { t.accent_text } else { t.muted },
            Border {
                radius: radius::M.into(),
                ..Default::default()
            },
            Shadow::default(),
        )
    }
}

/// Filter chips and segmented controls.
pub fn chip(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let (bg, fg, border) = if selected {
            (t.accent, t.on_accent, t.accent)
        } else if hovered {
            (t.overlay, t.text, t.border)
        } else {
            (t.raised, t.muted, t.border)
        };
        base_button(
            Some(bg),
            fg,
            Border {
                color: border,
                width: 1.0,
                radius: radius::PILL.into(),
            },
            Shadow::default(),
        )
    }
}

pub fn icon_button(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = tokens(theme);
        let bg = match (active, status) {
            (true, _) => Some(t.accent_soft),
            (false, button::Status::Hovered | button::Status::Pressed) => Some(t.overlay),
            _ => None,
        };
        base_button(
            bg,
            if active { t.accent_text } else { t.muted },
            Border {
                radius: radius::PILL.into(),
                ..Default::default()
            },
            Shadow::default(),
        )
    }
}

/// Invisible button wrapping a card (the card draws itself).
pub fn bare(_theme: &Theme, _status: button::Status) -> button::Style {
    base_button(None, Color::WHITE, Border::default(), Shadow::default())
}

// ---------------------------------------------------------------- inputs

pub fn search(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let t = tokens(theme);
    let focused = matches!(status, text_input::Status::Focused { .. });
    text_input::Style {
        background: t.raised.into(),
        border: Border {
            color: if focused { t.accent } else { t.border },
            width: if focused { 1.5 } else { 1.0 },
            radius: radius::M.into(),
        },
        icon: t.muted,
        placeholder: t.faint,
        value: t.text,
        selection: with_alpha(t.accent, 0.35),
    }
}

/// Search field: the frame is drawn by [`search_box`], the input itself is bare.
pub fn search_bare(theme: &Theme, status: text_input::Status) -> text_input::Style {
    let mut s = search(theme, status);
    s.background = Color::TRANSPARENT.into();
    s.border = Border::default();
    s
}

pub fn search_box(theme: &Theme) -> container::Style {
    let t = tokens(theme);
    container::Style {
        background: Some(t.raised.into()),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: radius::PILL.into(),
        },
        shadow: elevation(1, t),
        ..Default::default()
    }
}

pub fn slider_style(theme: &Theme, status: slider::Status) -> slider::Style {
    let t = tokens(theme);
    let handle = if matches!(status, slider::Status::Hovered | slider::Status::Dragged) {
        t.accent_hover
    } else {
        t.accent
    };
    slider::Style {
        rail: slider::Rail {
            backgrounds: (t.accent.into(), t.overlay.into()),
            width: 4.0,
            border: Border {
                radius: radius::PILL.into(),
                ..Default::default()
            },
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: 8.0 },
            background: handle.into(),
            border_width: 3.0,
            border_color: t.raised,
        },
    }
}

pub fn toggler_style(theme: &Theme, status: toggler::Status) -> toggler::Style {
    let t = tokens(theme);
    let on = matches!(
        status,
        toggler::Status::Active { is_toggled: true }
            | toggler::Status::Hovered { is_toggled: true }
    );
    toggler::Style {
        background: if on { t.accent } else { t.overlay }.into(),
        background_border_width: 1.0,
        background_border_color: if on { t.accent } else { t.border },
        foreground: if on { t.on_accent } else { t.muted }.into(),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: Some(t.text),
        border_radius: None,
        padding_ratio: 0.15,
    }
}

pub fn pick_list_style(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let t = tokens(theme);
    let hovered = !matches!(status, pick_list::Status::Active);
    pick_list::Style {
        text_color: t.text,
        placeholder_color: t.faint,
        handle_color: t.muted,
        background: if hovered { t.overlay } else { t.raised }.into(),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: radius::PILL.into(),
        },
    }
}

pub fn scroll_style(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let t = tokens(theme);
    let mut s = scrollable::default(theme, status);
    let rail = || scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: with_alpha(t.muted, 0.35).into(),
            border: Border {
                radius: radius::PILL.into(),
                ..Default::default()
            },
        },
    };
    s.vertical_rail = rail();
    s.horizontal_rail = rail();
    s.container = container::Style::default();
    s
}

pub fn icon_tint(color: Color) -> impl Fn(&Theme, svg::Status) -> svg::Style {
    move |_, _| svg::Style { color: Some(color) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG 2.2 relative luminance of an sRGB colour.
    fn luminance(c: Color) -> f32 {
        let lin = |v: f32| {
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
    }

    /// `top` (possibly translucent) composited over an opaque `bottom`.
    fn over(top: Color, bottom: Color) -> Color {
        let mix = |a: f32, b: f32| a * top.a + b * (1.0 - top.a);
        Color::from_rgb(
            mix(top.r, bottom.r),
            mix(top.g, bottom.g),
            mix(top.b, bottom.b),
        )
    }

    fn contrast(fg: Color, bg: Color) -> f32 {
        let (a, b) = (luminance(fg), luminance(bg));
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// Every pair that carries text in the views meets AA (4.5:1), in both themes.
    /// `cargo test -p lumenwall-ui contrast -- --nocapture` prints the table.
    #[test]
    fn text_contrast_meets_wcag_aa() {
        for t in [&DARK, &LIGHT] {
            let selected_nav = over(t.accent_soft, t.sidebar);
            let pairs = [
                ("text / bg", t.text, t.bg),
                ("text / raised", t.text, t.raised),
                ("text / overlay (toast)", t.text, t.overlay),
                ("muted / bg", t.muted, t.bg),
                ("muted / raised", t.muted, t.raised),
                ("muted / sidebar", t.muted, t.sidebar),
                ("muted / surface (panel)", t.muted, t.surface),
                ("faint / bg", t.faint, t.bg),
                ("faint / raised", t.faint, t.raised),
                ("faint / sidebar", t.faint, t.sidebar),
                ("faint / surface (panel)", t.faint, t.surface),
                ("accent_text / bg", t.accent_text, t.bg),
                ("accent_text / raised", t.accent_text, t.raised),
                ("accent_text / sidebar", t.accent_text, t.sidebar),
                ("accent_text / selected nav", t.accent_text, selected_nav),
                ("on_accent / accent", t.on_accent, t.accent),
                ("on_accent / accent_hover", t.on_accent, t.accent_hover),
                ("on_warning / warning", t.on_warning, t.warning),
                ("on_danger / danger", t.on_danger, t.danger),
                ("success / bg", t.success, t.bg),
                ("success / sidebar", t.success, t.sidebar),
                ("success / raised", t.success, t.raised),
                ("warning / bg", t.warning, t.bg),
                ("danger / bg", t.danger, t.bg),
                ("danger / raised", t.danger, t.raised),
            ];
            let theme = if t.dark { "dark" } else { "light" };
            for (name, fg, bg) in pairs {
                let r = contrast(fg, bg);
                println!("{theme:5} {name:28} {r:5.2}");
                assert!(r >= 4.5, "{theme} {name}: {r:.2}:1");
            }
        }
    }

    #[test]
    fn contrast_matches_known_values() {
        assert!((contrast(Color::WHITE, Color::BLACK) - 21.0).abs() < 0.01);
        // BRAND.md: white on the old light accent_hover failed at 3.79:1.
        assert!((contrast(Color::WHITE, color!(0xc96a00)) - 3.79).abs() < 0.02);
    }

    #[test]
    fn reduced_motion_zeroes_durations() {
        assert_eq!(motion::duration(motion::PANEL), motion::PANEL);
        motion::set_reduced(true);
        assert_eq!(motion::duration(motion::SLOW), std::time::Duration::ZERO);
        let now = std::time::Instant::now();
        let anim = motion::panel_go(&motion::panel_at(false), true, now);
        assert!(!anim.is_animating(now));
        assert_eq!(anim.interpolate(0.0, 1.0, now), 1.0);
        motion::set_reduced(false);
        let anim = motion::panel_go(&motion::panel_at(false), true, now);
        assert!(anim.is_animating(now));
    }
}
