//! Reusable view pieces built on the design system.

use iced::widget::{
    button, column, container, image, mouse_area, pin, row, space, stack, text, tooltip,
};
use iced::{Alignment, Color, ContentFit, Element, Length, Padding, mouse};

use crate::app::{App, Message};
use crate::icons::{self, Icon, Illustration};
use crate::theme::{self, Tokens, radius, size, space as sp};

pub fn label<'a>(s: impl text::IntoFragment<'a>, size: f32, color: Color) -> text::Text<'a> {
    text(s).size(size).font(theme::regular()).color(color)
}

pub fn strong<'a>(s: impl text::IntoFragment<'a>, size: f32, color: Color) -> text::Text<'a> {
    text(s).size(size).font(theme::semibold()).color(color)
}

pub fn chip<'a>(s: &'a str, selected: bool, msg: Message) -> Element<'a, Message> {
    button(text(s).size(size::CAPTION).font(theme::medium()))
        .padding(Padding::from([5.0, 14.0]))
        .style(theme::chip(selected))
        .on_press(msg)
        .into()
}

/// Round icon button with a tooltip (its accessible name for sighted users).
pub fn icon_button<'a>(
    t: &Tokens,
    icon: Icon,
    tip: &'a str,
    active: bool,
    msg: Option<Message>,
) -> Element<'a, Message> {
    let color = if active { t.accent } else { t.muted };
    let b = button(container(icons::icon(icon, 18.0, color)).center(Length::Fixed(34.0)))
        .padding(0)
        .style(theme::icon_button(active))
        .on_press_maybe(msg);
    tooltip(
        b,
        container(label(tip, size::CAPTION, t.text))
            .padding([4, 8])
            .style(theme::card),
        tooltip::Position::Top,
    )
    .gap(6)
    .into()
}

pub fn primary_button<'a>(
    t: &Tokens,
    icon: Option<Icon>,
    s: &'a str,
    msg: Option<Message>,
) -> button::Button<'a, Message> {
    let fg = if msg.is_some() { t.on_accent } else { t.faint };
    let mut content = row![].spacing(sp::S).align_y(Alignment::Center);
    if let Some(i) = icon {
        content = content.push(icons::icon(i, 18.0, fg));
    }
    content = content.push(strong(s, size::HEADING, fg));
    button(content)
        .padding(Padding::from([10.0, 18.0]))
        .style(theme::primary)
        .on_press_maybe(msg)
}

/// Full-width primary button with centered content.
pub fn primary_wide<'a>(
    t: &Tokens,
    icon: Option<Icon>,
    s: &'a str,
    msg: Option<Message>,
) -> Element<'a, Message> {
    let fg = if msg.is_some() { t.on_accent } else { t.faint };
    let mut content = row![].spacing(sp::S).align_y(Alignment::Center);
    if let Some(i) = icon {
        content = content.push(icons::icon(i, 18.0, fg));
    }
    content = content.push(strong(s, size::HEADING, fg));
    button(container(content).center_x(Length::Fill))
        .width(Length::Fill)
        .padding(Padding::from([11.0, 18.0]))
        .style(theme::primary)
        .on_press_maybe(msg)
        .into()
}

pub fn secondary_button<'a>(
    t: &Tokens,
    icon: Option<Icon>,
    s: &'a str,
    msg: Option<Message>,
) -> Element<'a, Message> {
    let mut content = row![].spacing(sp::S).align_y(Alignment::Center);
    if let Some(i) = icon {
        content = content.push(icons::icon(i, 16.0, t.muted));
    }
    content = content.push(text(s).size(size::BODY).font(theme::medium()));
    button(content)
        .padding(Padding::from([7.0, 14.0]))
        .style(theme::secondary)
        .on_press_maybe(msg)
        .into()
}

pub fn ghost_button<'a>(
    t: &Tokens,
    icon: Option<Icon>,
    s: &'a str,
    msg: Option<Message>,
) -> Element<'a, Message> {
    let mut content = row![].spacing(sp::S).align_y(Alignment::Center);
    if let Some(i) = icon {
        content = content.push(icons::icon(i, 16.0, t.muted));
    }
    content = content.push(text(s).size(size::BODY).font(theme::medium()));
    button(content)
        .padding(Padding::from([7.0, 10.0]))
        .style(theme::ghost)
        .on_press_maybe(msg)
        .into()
}

/// Uppercase caption above a raised card.
pub fn section<'a>(
    t: &Tokens,
    title: &str,
    content: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    column![
        container(strong(title.to_uppercase(), size::MICRO, t.faint))
            .padding(Padding::default().left(4.0)),
        container(content)
            .padding(sp::L)
            .width(Length::Fill)
            .style(theme::card),
    ]
    .spacing(sp::S)
    .into()
}

/// A settings row: title and explanation on the left, control on the right.
pub fn setting_row<'a>(
    t: &Tokens,
    title: &'a str,
    caption: Option<&'a str>,
    control: impl Into<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut left = column![label(title, size::BODY, t.text)].spacing(2);
    if let Some(c) = caption {
        left = left.push(label(c, size::CAPTION, t.muted));
    }
    row![left.width(Length::Fill), control.into()]
        .spacing(sp::L)
        .align_y(Alignment::Center)
        .into()
}

pub fn divider<'a>(t: &Tokens) -> Element<'a, Message> {
    let c = t.border;
    container(space().height(1))
        .width(Length::Fill)
        .height(1)
        .style(move |_| container::Style {
            background: Some(c.into()),
            ..Default::default()
        })
        .into()
}

/// Notice with an icon, a title, a body and an optional action.
pub fn notice<'a>(
    t: &Tokens,
    kind: theme::Notice,
    title: &'a str,
    body: &'a str,
    action: Option<Element<'a, Message>>,
) -> Element<'a, Message> {
    let color = match kind {
        theme::Notice::Info => t.accent,
        theme::Notice::Warning => t.warning,
        theme::Notice::Danger => t.danger,
    };
    let mut texts = column![strong(title, size::BODY, t.text)].spacing(2);
    if !body.is_empty() {
        texts = texts.push(label(body, size::CAPTION, t.muted));
    }
    let mut r = row![
        icons::icon(Icon::Alert, 20.0, color),
        texts.width(Length::Fill)
    ]
    .spacing(sp::M)
    .align_y(Alignment::Center);
    if let Some(a) = action {
        r = r.push(a);
    }
    container(r)
        .padding(Padding::from([10.0, 14.0]))
        .width(Length::Fill)
        .style(theme::notice(kind))
        .into()
}

/// Centered illustration, title, explanation and actions.
pub fn empty_state<'a>(
    t: &Tokens,
    illus: Illustration,
    title: &'a str,
    body: &'a str,
    actions: Vec<Element<'a, Message>>,
) -> Element<'a, Message> {
    let mut col = column![
        icons::illustration(illus, t),
        strong(title, size::TITLE, t.text),
    ]
    .spacing(sp::M)
    .align_x(Alignment::Center)
    .max_width(520);
    if !body.is_empty() {
        col = col.push(label(body, size::BODY, t.muted).align_x(Alignment::Center));
    }
    if !actions.is_empty() {
        col = col.push(row(actions).spacing(sp::S));
    }
    container(col)
        .center_x(Length::Fill)
        .padding(Padding::from([sp::XXL * 2.0, sp::XXL]))
        .into()
}

/// Preview image covering `w × h`, or a placeholder while it decodes.
pub fn preview<'a>(
    app: &App,
    path: Option<&std::path::Path>,
    w: f32,
    h: f32,
    r: f32,
) -> Element<'a, Message> {
    match path.and_then(|p| app.previews.get(p)) {
        // Previews are already 16:9: fill 16:9 boxes directly. Other shapes (monitor
        // tiles) are covered and clipped by their container, which iced's software
        // renderer honours (it ignores `crop` and does not clip `Cover` images).
        Some((handle, (iw, ih))) => {
            let ratio = |a: f32, b: f32| a / b.max(1.0);
            if (ratio(w, h) - ratio(iw as f32, ih as f32)).abs() < 0.04 {
                image(handle.clone())
                    .content_fit(ContentFit::Fill)
                    .width(w)
                    .height(h)
                    .border_radius(r)
                    .into()
            } else {
                container(
                    image(handle.clone())
                        .content_fit(ContentFit::Cover)
                        .width(w)
                        .height(h)
                        .border_radius(r),
                )
                .width(w)
                .height(h)
                .clip(true)
                .into()
            }
        }
        None => container(space())
            .width(w)
            .height(h)
            .style(move |th| {
                let mut s = theme::preview_placeholder(th);
                s.border.radius = r.into();
                s
            })
            .into(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapMode {
    /// Details panel: click toggles the target, drop applies.
    Apply,
    /// Screens page: click picks a wallpaper for that screen.
    Screens,
}

/// The monitors at their real relative positions and sizes, each showing its
/// current wallpaper. Also the drop target for dragged cards.
pub fn monitor_map<'a>(
    app: &App,
    t: &Tokens,
    width: f32,
    height: f32,
    mode: MapMode,
) -> Element<'a, Message> {
    let outputs = &app.daemon.outputs;
    if outputs.is_empty() {
        return space().height(0).into();
    }
    let min_x = outputs.iter().map(|o| o.x).min().unwrap_or(0) as f32;
    let min_y = outputs.iter().map(|o| o.y).min().unwrap_or(0) as f32;
    let max_x = outputs
        .iter()
        .map(|o| o.x + o.width as i32)
        .max()
        .unwrap_or(1) as f32;
    let max_y = outputs
        .iter()
        .map(|o| o.y + o.height as i32)
        .max()
        .unwrap_or(1) as f32;
    let (span_w, span_h) = ((max_x - min_x).max(1.0), (max_y - min_y).max(1.0));
    let scale = ((width - 8.0) / span_w).min((height - 8.0) / span_h);
    let (off_x, off_y) = (
        (width - span_w * scale) / 2.0,
        (height - span_h * scale) / 2.0,
    );
    let all = app.targets.is_empty();
    let dragging = app.drag.as_ref().is_some_and(|d| d.active);
    let mut layers = stack![space().width(width).height(height)];
    for (i, o) in outputs.iter().enumerate() {
        let gap = 4.0;
        let (x, y) = (
            off_x + (o.x as f32 - min_x) * scale + gap / 2.0,
            off_y + (o.y as f32 - min_y) * scale + gap / 2.0,
        );
        let (w, h) = (
            (o.width as f32 * scale - gap).max(20.0),
            (o.height as f32 * scale - gap).max(14.0),
        );
        let selected = mode == MapMode::Apply && (all || app.targets.contains(&o.id));
        let drop = dragging && app.drop_target.as_deref() == Some(o.id.as_str());
        let wp = app.wallpaper_on(&o.id);
        let img = preview(
            app,
            wp.and_then(|w| w.preview.as_deref()),
            w - 4.0,
            h - 4.0,
            radius::S - 1.0,
        );
        // Screens the wallpaper will not go to are dimmed.
        let dim = if wp.is_some() && !selected && mode == MapMode::Apply {
            0.45
        } else {
            0.0
        };
        let num = container(strong(
            format!("{}", i + 1),
            size::MICRO,
            if selected { t.on_accent } else { Color::WHITE },
        ))
        .center(Length::Fixed(20.0))
        .style(if selected {
            theme::accent_badge
        } else {
            theme::glass_badge
        });
        let caption = if h > 70.0 {
            container(label(
                format!("{} · {}×{}", o.id, o.width, o.height),
                size::MICRO,
                Color::WHITE,
            ))
            .padding([2, 8])
            .style(theme::glass_badge)
        } else {
            container(space())
        };
        let face = stack![
            img,
            container(space())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(
                        Color {
                            a: dim,
                            ..Color::BLACK
                        }
                        .into()
                    ),
                    ..Default::default()
                }),
            container(num).padding(6),
            container(caption)
                .align_bottom(Length::Fill)
                .center_x(Length::Fill)
                .padding(6),
        ];
        let tile = container(face)
            .padding(2)
            .width(w)
            .height(h)
            .style(theme::monitor(selected, drop));
        let msg = match mode {
            MapMode::Apply if all => Message::TargetOnly(o.id.clone()),
            MapMode::Apply => Message::TargetToggle(o.id.clone()),
            MapMode::Screens => Message::TargetOnly(o.id.clone()),
        };
        let area = mouse_area(tile)
            .on_press(msg)
            .on_enter(Message::DropHover(Some(o.id.clone())))
            .on_exit(Message::DropHover(None))
            .interaction(mouse::Interaction::Pointer);
        layers = layers.push(pin(area).x(x).y(y));
    }
    container(layers).width(width).height(height).into()
}

pub fn format_bytes(b: u64, lang: crate::i18n::Lang) -> String {
    let (m, g) = match lang {
        crate::i18n::Lang::Fr => ("Mo", "Go"),
        crate::i18n::Lang::En => ("MB", "GB"),
    };
    let mb = b as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.1} {g}", mb / 1024.0)
    } else {
        format!("{mb:.0} {m}")
    }
}
