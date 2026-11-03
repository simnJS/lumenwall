//! Views: a pure function of the state.

mod details;
mod drawn;
mod library;
mod screens;
mod settings;
pub mod widgets;
mod workshop;

use iced::widget::{button, column, container, pin, row, slider, space, stack, text};
use iced::{Alignment, Color, Element, Length, Padding};
use lumen_ipc::PauseReason;

use crate::app::{App, Message, Page};
use crate::daemon::{Link, Offline};
use crate::i18n::K;
use crate::icons::{self, Icon};
use crate::theme::{self, Tokens, layout, size, space as sp};
use widgets::{icon_button, label, strong};

impl App {
    pub fn tokens(&self) -> &'static Tokens {
        theme::tokens(&self.theme())
    }

    pub fn view(&self) -> Element<'_, Message> {
        let t = self.tokens();
        let page: Element<'_, Message> = match self.page {
            Page::Library => self.library_view(t),
            Page::Workshop => self.workshop_view(t),
            Page::Screens => self.screens_view(t),
            Page::Settings => self.settings_view(t),
        };
        let mut body = row![
            self.sidebar(t),
            container(page).width(Length::Fill).height(Length::Fill)
        ];
        if matches!(self.page, Page::Library | Page::Workshop) {
            let has_selection = if self.page == Page::Library {
                self.selected.is_some()
            } else {
                self.ws.selected.is_some()
            };
            // The wall clock, not the last frame time: if frames stop (hidden
            // window), the panel still ends fully open or closed.
            let now = std::time::Instant::now();
            let open = if self.panel.is_animating(now) {
                self.panel.interpolate(0.0, 1.0, now)
            } else if has_selection {
                1.0
            } else {
                0.0
            };
            if open > 0.0 {
                body = body.push(
                    container(if self.page == Page::Library {
                        self.details(t)
                    } else {
                        self.ws_details(t)
                    })
                    .width(layout::PANEL_W * open)
                    .height(Length::Fill)
                    .clip(true),
                );
            }
        }
        let root = column![body.height(Length::Fill), self.status_bar(t)];
        let mut layers = stack![
            container(root)
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::app_bg)
        ];
        if let Some(d) = &self.drag
            && d.active
            && let Some(w) = self.wallpaper(&d.path)
        {
            let ghost = container(
                row![
                    widgets::preview(self, w.preview.as_deref(), 64.0, 36.0, 6.0),
                    column![
                        strong(w.title(), size::CAPTION, t.text),
                        label(self.t(K::DropOnScreen), size::MICRO, t.muted)
                    ]
                    .spacing(2),
                ]
                .spacing(sp::S)
                .align_y(Alignment::Center),
            )
            .padding(sp::S)
            .max_width(300)
            .style(theme::ghost_drag);
            layers = layers.push(pin(ghost).x(d.pos.x + 14.0).y(d.pos.y + 14.0));
        }
        if !self.toasts.is_empty() {
            let list = column(self.toasts.iter().map(|toast| {
                let fg = if toast.error { Color::WHITE } else { t.text };
                container(
                    row![
                        icons::icon(
                            if toast.error {
                                Icon::Alert
                            } else {
                                Icon::Check
                            },
                            16.0,
                            if toast.error { Color::WHITE } else { t.success }
                        ),
                        label(toast.text.as_str(), size::BODY, fg),
                    ]
                    .spacing(sp::S)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([10.0, 14.0]))
                .max_width(420)
                .style(theme::toast(toast.error))
                .into()
            }))
            .spacing(sp::S)
            .align_x(Alignment::Center);
            layers = layers.push(
                container(list)
                    .center_x(Length::Fill)
                    .align_bottom(Length::Fill)
                    .padding(Padding::default().bottom(layout::STATUS_H + sp::L)),
            );
        }
        drawn::drawn(layers, &self.drawn)
    }

    fn sidebar(&self, t: &'static Tokens) -> Element<'_, Message> {
        let logo = row![
            icons::icon(Icon::Logo, 26.0, t.accent),
            strong(self.t(K::AppName), size::TITLE, t.text)
        ]
        .spacing(sp::S)
        .align_y(Alignment::Center);
        let nav = |page: Page, icon: Icon, k: K| -> Element<'_, Message> {
            let selected = self.page == page;
            let color = if selected { t.accent } else { t.muted };
            button(
                row![
                    icons::icon(icon, 18.0, color),
                    text(self.t(k)).size(size::BODY).font(theme::medium())
                ]
                .spacing(sp::M)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding(Padding::from([9.0, 12.0]))
            .style(theme::nav(selected))
            .on_press(Message::Nav(page))
            .into()
        };
        let col = column![
            container(logo).padding(Padding::from([sp::XL, sp::S])),
            nav(Page::Library, Icon::Library, K::NavLibrary),
            nav(Page::Workshop, Icon::Workshop, K::NavWorkshop),
            nav(Page::Screens, Icon::Screens, K::NavScreens),
            nav(Page::Settings, Icon::Settings, K::NavSettings),
            space().height(Length::Fill),
            self.shortcuts(t),
        ]
        .spacing(sp::XS);
        container(
            col.padding(Padding::from([0.0, sp::M]))
                .push(space().height(sp::M)),
        )
        .width(layout::SIDEBAR_W)
        .height(Length::Fill)
        .style(theme::sidebar)
        .into()
    }

    fn status_bar(&self, t: &'static Tokens) -> Element<'_, Message> {
        let (dot, status) = match &self.daemon.link {
            Link::Online(_) => {
                let s = self.daemon.state.clone().unwrap_or_default();
                let text = match (s.paused, s.pause_reason) {
                    (false, _) => self.t(K::EngineRunning),
                    (true, Some(PauseReason::Fullscreen)) => self.t(K::PausedFullscreen),
                    (true, Some(PauseReason::Battery)) => self.t(K::PausedBattery),
                    (true, Some(PauseReason::Locked)) => self.t(K::PausedLocked),
                    (true, Some(PauseReason::Hidden)) => self.t(K::PausedHidden),
                    (true, _) => self.t(K::Paused),
                };
                (if s.paused { t.warning } else { t.success }, text)
            }
            Link::Connecting => (t.faint, self.t(K::EngineConnecting)),
            Link::Starting => (t.warning, self.t(K::EngineStarting)),
            Link::Offline(Offline::Incompatible(_)) => (t.danger, self.t(K::EngineIncompatible)),
            Link::Offline(Offline::NoBinary) => (t.danger, self.t(K::EngineMissing)),
            Link::Offline(_) => (t.faint, self.t(K::EngineStopped)),
        };
        let dot = container(space())
            .width(8)
            .height(8)
            .style(move |_| container::Style {
                background: Some(dot.into()),
                border: iced::Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
                ..Default::default()
            });
        let mut bar = row![dot, label(status, size::CAPTION, t.muted)]
            .spacing(sp::S)
            .align_y(Alignment::Center);

        // Engine metrics: the product's selling point, always in view.
        let metric = |name: &'static str, value: String| -> Element<'_, Message> {
            row![
                label(name, size::CAPTION, t.faint),
                strong(value, size::CAPTION, t.text)
            ]
            .spacing(5)
            .align_y(Alignment::Center)
            .into()
        };
        let mut metrics = row![].spacing(sp::L).align_y(Alignment::Center);
        if let (Link::Online(_), Some(s)) = (&self.daemon.link, &self.daemon.stats) {
            let rendering = s.outputs.iter().filter(|o| o.rendering).count();
            let fps = s
                .outputs
                .iter()
                .map(|o| o.fps)
                .fold(None::<f32>, |m, f| Some(m.map_or(f, |m| m.max(f))));
            let fps = format!(
                "{:.0}{}",
                fps.unwrap_or(s.fps),
                if rendering > 1 {
                    format!(" × {rendering}")
                } else {
                    String::new()
                }
            );
            metrics = metrics.push(
                row![
                    icons::icon(Icon::Gauge, 15.0, t.accent),
                    strong(fps, size::CAPTION, t.accent),
                    label("fps", size::CAPTION, t.faint)
                ]
                .spacing(5)
                .align_y(Alignment::Center),
            );
            metrics = metrics.push(metric("CPU", format!("{:.1} %", s.cpu_percent)));
            if let Some(g) = s.gpu_percent {
                metrics = metrics.push(metric("GPU", format!("{g:.1} %")));
            }
            metrics = metrics.push(metric(
                self.t(K::PerfEngineRam),
                widgets::format_bytes(s.rss_bytes, self.lang),
            ));
        } else if self.daemon.is_online() {
            metrics = metrics.push(label(self.t(K::PerfNoData), size::CAPTION, t.faint));
        }
        if let Some(rss) = self.own_rss() {
            metrics = metrics.push(metric(
                self.t(K::PerfAppRam),
                widgets::format_bytes(rss, self.lang),
            ));
        }
        bar = bar
            .push(space().width(sp::XL))
            .push(metrics)
            .push(space().width(Length::Fill));

        if let (Link::Online(_), Some(s)) = (&self.daemon.link, &self.daemon.state) {
            let (pi, pl) = if s.paused {
                (Icon::Play, self.t(K::Resume))
            } else {
                (Icon::Pause, self.t(K::Pause))
            };
            let (mi, ml) = if s.muted {
                (Icon::VolumeOff, self.t(K::Unmute))
            } else {
                (Icon::Volume, self.t(K::Mute))
            };
            let vol = self.volume_drag.unwrap_or(s.volume);
            bar = bar
                .push(icon_button(t, pi, pl, s.paused, Some(Message::Pause)))
                .push(icon_button(t, mi, ml, s.muted, Some(Message::Mute)))
                .push(
                    slider(0.0..=1.0, vol, Message::Volume)
                        .step(0.01_f32)
                        .on_release(Message::VolumeRelease)
                        .width(110)
                        .style(theme::slider_style),
                )
                .push(label(format!("{:.0} %", vol * 100.0), size::CAPTION, t.muted).width(40));
        } else if let Link::Offline(reason) = &self.daemon.link {
            let hint = match reason {
                Offline::NoBinary => self.t(K::EngineMissingHelp),
                Offline::NoAnswer => self.t(K::EngineNoAnswer),
                _ => self.t(K::EngineOfflineHint),
            };
            bar = bar
                .push(label(hint, size::CAPTION, t.faint))
                .push(space().width(sp::M))
                .push(
                    button(
                        row![
                            icons::icon(Icon::Play, 14.0, t.on_accent),
                            text(self.t(K::StartEngine))
                                .size(size::CAPTION)
                                .font(theme::semibold())
                        ]
                        .spacing(6)
                        .align_y(Alignment::Center),
                    )
                    .padding(Padding::from([5.0, 12.0]))
                    .style(theme::primary)
                    .on_press(Message::StartEngine),
                );
        }
        container(
            bar.padding(Padding::from([0.0, sp::L]))
                .height(layout::STATUS_H),
        )
        .width(Length::Fill)
        .style(theme::status_bar)
        .into()
    }

    /// Keyboard shortcuts reminder at the bottom of the sidebar.
    fn shortcuts(&self, t: &'static Tokens) -> Element<'_, Message> {
        let key = |k: &'static str| {
            container(label(k, size::MICRO, t.muted))
                .padding(Padding::from([1.0, 6.0]))
                .style(move |_| container::Style {
                    background: Some(t.overlay.into()),
                    border: iced::Border {
                        color: t.border,
                        width: 1.0,
                        radius: 4.0.into(),
                    },
                    ..Default::default()
                })
        };
        let line = |k: &'static str, what: K| {
            row![key(k), label(self.t(what), size::MICRO, t.faint)]
                .spacing(sp::S)
                .align_y(Alignment::Center)
        };
        let (enter, esc) = (self.t(K::KeyEnter), self.t(K::KeyEscape));
        container(
            column![
                line("Ctrl F", K::KeySearch),
                line("← ↑ → ↓", K::KeyNavigate),
                line(enter, K::KeyApply),
                line(esc, K::KeyClose),
            ]
            .spacing(6),
        )
        .padding(Padding::from([sp::M, sp::S]))
        .into()
    }
}
