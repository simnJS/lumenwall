//! The window's title bar, drawn by the app on Linux.
//!
//! On GNOME Wayland the title bar is the client's job, and winit's is a grey
//! band in neither the app's colours nor the desktop's. So on Linux the window
//! opens undecorated and transparent, and the app draws its own frame:
//!
//! - a band at the top, in the sidebar's colour, with the brand mark, the page
//!   title and the window buttons the desktop shows, on its side
//!   (`org.gnome.desktop.wm.preferences button-layout`, else GTK's
//!   `gtk-decoration-layout`);
//! - a press on the band moves the window, two within 400 ms maximise or
//!   restore it, a right press opens the desktop's window menu;
//! - 4 px edges and 12 px corners resize it;
//! - a 1 px outline in the theme's `frame` colour and, where the window can be
//!   see-through, rounded corners (12 px, as GNOME's own windows); both are
//!   dropped while the window is maximised or full screen.
//!
//! The outline is opaque, painted under the content. A translucent one drawn over
//! the transparent window let the desktop show through it, and came out as a
//! light line on every desktop: iced blends in linear light and hands the
//! compositor premultiplied sRGB values, so a 10 % white edge is composited as
//! about 35 % (measured on GNOME 50: +80 levels instead of +16). Only the
//! antialiased arc of the corners is left partly transparent.
//!
//! Transparency is only asked for where the corners really come out clear: on
//! Wayland with the GPU renderer (premultiplied alpha). The CPU renderer
//! presents opaque buffers (corners would be black), and X11 needs a running
//! compositor: there the window keeps square corners.
//!
//! Windows and macOS keep their native title bar.

use std::time::{Duration, Instant};

use iced::border::Radius;
use iced::mouse::Interaction;
use iced::widget::{button, column, container, mouse_area, row, space, tooltip};
use iced::window::{self, Direction};
use iced::{Alignment, Background, Border, Color, Element, Length, Padding, Task, Theme};

use crate::app::{App, Message, Page};
use crate::i18n::K;
use crate::icons::{self, Icon};
use crate::theme::{self, Tokens, layout, size, space as sp};

/// Whether the app draws the window's title bar and frame.
pub const DRAWN: bool = cfg!(target_os = "linux");

/// Whether the window is created see-through, for rounded corners: Linux, a Wayland
/// session, and the GPU renderer (read once; `main` settles `ICED_BACKEND` first).
pub fn transparent() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        let var = |k: &str| std::env::var(k).unwrap_or_default();
        DRAWN
            && wants_transparency(
                std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()),
                &var("WINIT_UNIX_BACKEND"),
                &var("ICED_BACKEND"),
            )
    })
}

/// [`transparent`] from the environment it reads.
fn wants_transparency(wayland_display: bool, winit_backend: &str, iced_backend: &str) -> bool {
    let wayland = wayland_display && !winit_backend.eq_ignore_ascii_case("x11");
    // `ICED_BACKEND` lists the renderers to try; the first one wins.
    let cpu = iced_backend
        .split(',')
        .next()
        .is_some_and(|b| b.trim().eq_ignore_ascii_case("tiny-skia"));
    wayland && !cpu
}

/// Height of the band.
pub const BAND_H: f32 = 46.0;
/// Radius of the window's corners (libadwaita's).
pub const RADIUS: f32 = 12.0;
/// How far in from the edge a press resizes, and how far a corner reaches.
const EDGE: f32 = 4.0;
const CORNER: f32 = 12.0;
/// Two presses closer than this are a double press (GTK's default).
const DOUBLE_PRESS: Duration = Duration::from_millis(400);
/// Window buttons: a round 26 px target.
const BUTTON: f32 = 26.0;

/// A window button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Minimize,
    Maximize,
    Close,
}

/// Which buttons the desktop shows, on which side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonLayout {
    pub start: Vec<Control>,
    pub end: Vec<Control>,
}

impl Default for ButtonLayout {
    /// When the desktop says nothing: the three, on the right.
    fn default() -> Self {
        Self {
            start: Vec::new(),
            end: vec![Control::Minimize, Control::Maximize, Control::Close],
        }
    }
}

impl ButtonLayout {
    /// `left:right`, each a comma-separated list (`appmenu:minimize,maximize,close`).
    /// Names that are not buttons (`appmenu`, `icon`, `menu`, `spacer`) are
    /// skipped; text without a colon is all on the left, as GTK reads it.
    pub fn parse(text: &str) -> Self {
        let text = text.trim().trim_matches(|c| c == '\'' || c == '"');
        let (start, end) = text.split_once(':').unwrap_or((text, ""));
        let side = |names: &str| {
            names
                .split(',')
                .filter_map(|n| match n.trim() {
                    "minimize" => Some(Control::Minimize),
                    "maximize" => Some(Control::Maximize),
                    "close" => Some(Control::Close),
                    _ => None,
                })
                .collect()
        };
        Self {
            start: side(start),
            end: side(end),
        }
    }
}

/// Reads the desktop's button layout (off the UI thread): GNOME's setting,
/// then GTK 3's `settings.ini`.
pub async fn read_layout() -> Option<ButtonLayout> {
    let (tx, rx) = iced::futures::channel::oneshot::channel();
    std::thread::Builder::new()
        .name("lumenwall-ui-layout".into())
        .spawn(move || {
            let _ = tx.send(read_layout_blocking());
        })
        .ok()?;
    rx.await.ok().flatten()
}

fn read_layout_blocking() -> Option<ButtonLayout> {
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.wm.preferences", "button-layout"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    if let Ok(o) = out
        && o.status.success()
    {
        let text = String::from_utf8_lossy(&o.stdout);
        if !text.trim().is_empty() {
            return Some(ButtonLayout::parse(&text));
        }
    }
    let ini = dirs::config_dir()?.join("gtk-3.0/settings.ini");
    std::fs::read_to_string(ini)
        .ok()?
        .lines()
        .filter_map(|l| l.split_once('='))
        .find(|(k, _)| k.trim() == "gtk-decoration-layout")
        .map(|(_, v)| ButtonLayout::parse(v))
}

/// What the app knows of its window frame.
#[derive(Debug, Default)]
pub struct Chrome {
    pub window: Option<window::Id>,
    pub layout: ButtonLayout,
    /// Maximised or full screen: square corners, no outline, no resize edges.
    pub maximized: bool,
    last_press: Option<Instant>,
}

#[derive(Debug, Clone)]
pub enum Msg {
    /// A press on the band where no control took it.
    Band,
    /// A right press on the band.
    Menu,
    Control(Control),
    Edge(Direction),
    Maximized(bool),
    Layout(Option<ButtonLayout>),
}

impl App {
    /// Tasks to run once the window exists and whenever it is resized.
    pub(crate) fn chrome_window_event(
        &mut self,
        id: window::Id,
        ev: &window::Event,
    ) -> Task<Message> {
        self.chrome.window = Some(id);
        if !DRAWN {
            return Task::none();
        }
        match ev {
            window::Event::Opened { .. } => Task::batch([
                ask_maximized(id),
                Task::perform(read_layout(), |l| Message::Chrome(Msg::Layout(l))),
            ]),
            window::Event::Resized(_) => ask_maximized(id),
            _ => Task::none(),
        }
    }

    pub(crate) fn chrome_update(&mut self, msg: Msg) -> Task<Message> {
        let Some(id) = self.chrome.window else {
            return Task::none();
        };
        match msg {
            Msg::Band => {
                let now = Instant::now();
                if self
                    .chrome
                    .last_press
                    .is_some_and(|t| now.saturating_duration_since(t) <= DOUBLE_PRESS)
                {
                    self.chrome.last_press = None;
                    window::toggle_maximize(id)
                } else {
                    self.chrome.last_press = Some(now);
                    window::drag(id)
                }
            }
            Msg::Menu => window::show_system_menu(id),
            Msg::Control(Control::Minimize) => window::minimize(id, true),
            Msg::Control(Control::Maximize) => window::toggle_maximize(id),
            // As the system's close button: remember the window, then close.
            Msg::Control(Control::Close) => {
                self.update(Message::Window(id, window::Event::CloseRequested))
            }
            Msg::Edge(direction) => window::drag_resize(id, direction),
            Msg::Maximized(m) => {
                self.chrome.maximized = m;
                Task::none()
            }
            Msg::Layout(l) => {
                if let Some(l) = l {
                    self.chrome.layout = l;
                }
                Task::none()
            }
        }
    }

    /// Whether the window has its outline now (not maximised or full screen).
    pub fn framed(&self) -> bool {
        DRAWN && !self.chrome.maximized
    }

    /// Whether the window's corners are rounded now.
    pub fn rounded(&self) -> bool {
        self.framed() && transparent()
    }

    /// Radius of a region's background at the window corners it touches.
    pub fn corner_radius(&self, top: bool, bottom: bool) -> Radius {
        if !self.rounded() {
            return Radius::default();
        }
        let r = RADIUS - 1.0;
        Radius {
            top_left: if top { r } else { 0.0 },
            top_right: if top { r } else { 0.0 },
            bottom_right: if bottom { r } else { 0.0 },
            bottom_left: if bottom { r } else { 0.0 },
        }
    }

    /// The band at the top of the window: brand mark over the sidebar, page
    /// title over the page, the desktop's window buttons.
    pub fn title_band(&self, t: &'static Tokens) -> Option<Element<'_, Message>> {
        if !DRAWN {
            return None;
        }
        let page = match self.page {
            Page::Library => K::NavLibrary,
            Page::Workshop => K::NavWorkshop,
            Page::Screens => K::NavScreens,
            Page::Settings => K::NavSettings,
        };
        let mut brand = row![].spacing(sp::S).align_y(Alignment::Center);
        if let Some(c) = self.controls(t, &self.chrome.layout.start) {
            brand = brand.push(c).push(space().width(sp::XS));
        }
        brand = brand
            .push(icons::symbol(t, theme::icon::XL))
            .push(widgets_strong(self.t(K::AppName), size::HEADING, t.text));
        let mut end = row![].spacing(sp::S).align_y(Alignment::Center);
        if let Some(c) = self.controls(t, &self.chrome.layout.end) {
            end = end.push(c);
        }
        let bar = row![
            // Same left edge as the navigation icons of the sidebar below.
            container(brand)
                .width(layout::SIDEBAR_W - sp::L)
                .padding(Padding::default().left(sp::L + sp::S)),
            container(
                iced::widget::text(self.t(page))
                    .size(size::BODY)
                    .font(theme::medium())
                    .color(t.muted)
            )
            .center_x(Length::Fill),
            end,
        ]
        .align_y(Alignment::Center)
        .padding(Padding::default().right(sp::M));
        let radius = self.corner_radius(true, false);
        let band = container(bar)
            .width(Length::Fill)
            .height(BAND_H)
            .align_y(Alignment::Center)
            .style(move |_: &Theme| container::Style {
                background: Some(t.sidebar.into()),
                border: Border {
                    radius,
                    ..Border::default()
                },
                ..container::Style::default()
            });
        Some(
            mouse_area(band)
                .on_press(Message::Chrome(Msg::Band))
                .on_right_press(Message::Chrome(Msg::Menu))
                .into(),
        )
    }

    fn controls(&self, t: &'static Tokens, which: &[Control]) -> Option<Element<'_, Message>> {
        if which.is_empty() {
            return None;
        }
        let mut r = row![].spacing(sp::S).align_y(Alignment::Center);
        for &c in which {
            let (glyph, tip) = match c {
                Control::Minimize => (Icon::Minimize, K::WinMinimize),
                Control::Maximize if self.chrome.maximized => (Icon::Restore, K::WinRestore),
                Control::Maximize => (Icon::Maximize, K::WinMaximize),
                Control::Close => (Icon::Close, K::Close),
            };
            let close = c == Control::Close;
            let b =
                button(container(icons::icon(glyph, theme::icon::S, t.text)).center(Length::Fill))
                    .padding(0)
                    .width(BUTTON)
                    .height(BUTTON)
                    .style(move |_: &Theme, status| control_style(t, close, status))
                    .on_press(Message::Chrome(Msg::Control(c)));
            r = r.push(
                tooltip(
                    b,
                    container(crate::view::widgets::label(
                        self.t(tip),
                        size::CAPTION,
                        t.text,
                    ))
                    .padding([4, 8])
                    .style(theme::card),
                    tooltip::Position::Bottom,
                )
                .gap(6),
            );
        }
        Some(r.into())
    }

    /// `frame` with the window's outline (and rounded corners where the window is
    /// see-through), and the resize edges over it, while the window is not maximised.
    pub fn window_frame<'a>(
        &'a self,
        t: &'static Tokens,
        frame: Element<'a, Message>,
    ) -> Element<'a, Message> {
        if !self.framed() {
            return frame;
        }
        let radius = if self.rounded() { RADIUS } else { 0.0 };
        // An opaque plate in the outline colour, the content 1 px inside it (its
        // corners `corner_radius`, concentric): the 1 px ring left visible is the
        // outline, and nothing of the desktop shows through it.
        let outlined = container(frame)
            .padding(1)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_: &Theme| container::Style {
                background: Some(t.frame.into()),
                border: Border {
                    radius: radius.into(),
                    ..Border::default()
                },
                ..container::Style::default()
            });
        iced::widget::stack![outlined, edges()].into()
    }
}

fn widgets_strong<'a>(s: &'a str, size: f32, color: Color) -> iced::widget::Text<'a> {
    crate::view::widgets::strong(s, size, color)
}

/// GNOME-like round buttons on a faint disc of the text colour; close turns
/// red on hover.
fn control_style(t: &Tokens, close: bool, status: button::Status) -> button::Style {
    let hot = matches!(status, button::Status::Hovered | button::Status::Pressed);
    let (bg, fg) = if close && hot {
        (t.danger, t.on_danger)
    } else {
        let a = match status {
            button::Status::Hovered => 0.16,
            button::Status::Pressed => 0.26,
            _ => 0.08,
        };
        (Color { a, ..t.text }, t.text)
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: fg,
        border: Border {
            radius: (BUTTON / 2.0).into(),
            ..Border::default()
        },
        ..button::Style::default()
    }
}

fn ask_maximized(id: window::Id) -> Task<Message> {
    window::is_maximized(id).then(move |maximized| {
        window::mode(id).map(move |mode| {
            Message::Chrome(Msg::Maximized(
                maximized || mode == window::Mode::Fullscreen,
            ))
        })
    })
}

/// Invisible handles along the edges and corners that resize the window.
fn edges<'a>() -> Element<'a, Message> {
    let handle = |d: Direction, w: Length, h: Length| -> Element<'a, Message> {
        let cursor = match d {
            Direction::North | Direction::South => Interaction::ResizingVertically,
            Direction::East | Direction::West => Interaction::ResizingHorizontally,
            Direction::NorthWest | Direction::SouthEast => Interaction::ResizingDiagonallyDown,
            Direction::NorthEast | Direction::SouthWest => Interaction::ResizingDiagonallyUp,
        };
        mouse_area(space().width(w).height(h))
            .interaction(cursor)
            .on_press(Message::Chrome(Msg::Edge(d)))
            .into()
    };
    let (edge, corner, fill) = (Length::Fixed(EDGE), Length::Fixed(CORNER), Length::Fill);
    let along = Length::Fixed(CORNER - EDGE);
    let side = |top: Direction, mid: Direction, bottom: Direction| {
        column![
            handle(top, edge, along),
            handle(mid, edge, fill),
            handle(bottom, edge, along)
        ]
        .height(fill)
    };
    column![
        row![
            handle(Direction::NorthWest, corner, edge),
            handle(Direction::North, fill, edge),
            handle(Direction::NorthEast, corner, edge),
        ],
        row![
            side(Direction::NorthWest, Direction::West, Direction::SouthWest),
            space().width(fill).height(fill),
            side(Direction::NorthEast, Direction::East, Direction::SouthEast),
        ]
        .height(fill),
        row![
            handle(Direction::SouthWest, corner, edge),
            handle(Direction::South, fill, edge),
            handle(Direction::SouthEast, corner, edge),
        ],
    ]
    .width(fill)
    .height(fill)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_desktops_button_layout() {
        assert_eq!(
            ButtonLayout::parse("':minimize,maximize,close'\n"),
            ButtonLayout::default()
        );
        assert_eq!(
            ButtonLayout::parse("'appmenu:close'"),
            ButtonLayout {
                start: vec![],
                end: vec![Control::Close]
            }
        );
        assert_eq!(
            ButtonLayout::parse("close,minimize:icon"),
            ButtonLayout {
                start: vec![Control::Close, Control::Minimize],
                end: vec![]
            }
        );
        // Without a colon, everything is on the left.
        assert_eq!(ButtonLayout::parse("close").start, vec![Control::Close]);
    }

    #[test]
    fn transparency_only_where_corners_come_out_clear() {
        assert!(wants_transparency(true, "", ""));
        assert!(wants_transparency(true, "wayland", "wgpu"));
        assert!(wants_transparency(true, "", "wgpu,tiny-skia"));
        // X11 (no WAYLAND_DISPLAY, or forced): square corners.
        assert!(!wants_transparency(false, "", ""));
        assert!(!wants_transparency(true, "X11", ""));
        // CPU renderer: opaque buffers.
        assert!(!wants_transparency(true, "", "tiny-skia"));
        assert!(!wants_transparency(true, "", " Tiny-Skia ,wgpu"));
    }

    #[test]
    fn outline_is_opaque() {
        for t in [&theme::DARK, &theme::LIGHT] {
            assert_eq!(t.frame.a, 1.0);
        }
    }
}
