//! Screens page: the monitor layout drawn to scale, then one tile per screen.

use iced::widget::{column, container, responsive, row, scrollable, space};
use iced::{Alignment, Element, Length, Padding, Size};

use super::widgets::{self, MapMode, label, strong};
use crate::app::{App, Message};
use crate::i18n::K;
use crate::icons::{Icon, Illustration};
use crate::theme::{self, Tokens, layout, radius, size, space as sp};

impl App {
    pub(super) fn screens_view(&self, t: &'static Tokens) -> Element<'_, Message> {
        let header = column![
            strong(self.t(K::NavScreens), size::DISPLAY, t.text),
            label(self.t(K::ScreensSubtitle), size::BODY, t.muted)
        ]
        .spacing(2);
        let mut page = column![header].spacing(sp::XL).padding(layout::PAGE_PAD);
        if !self.daemon.is_online() {
            return page
                .push(widgets::empty_state(
                    t,
                    Illustration::Engine,
                    self.t(K::EngineStopped),
                    self.t(K::ScreensOffline),
                    vec![
                        widgets::primary_button(
                            t,
                            Some(Icon::Play),
                            self.t(K::StartEngine),
                            Some(Message::StartEngine),
                        )
                        .into(),
                    ],
                ))
                .into();
        }
        if self.daemon.outputs.is_empty() {
            return page
                .push(widgets::empty_state(
                    t,
                    Illustration::Screen,
                    self.t(K::NoScreens),
                    "",
                    vec![],
                ))
                .into();
        }
        page = page.push(responsive(move |size| self.screens_content(t, size)));
        page.into()
    }

    fn screens_content(&self, t: &'static Tokens, avail: Size) -> Element<'_, Message> {
        let map = container(widgets::monitor_map(
            self,
            t,
            avail.width.min(900.0),
            260.0,
            MapMode::Screens,
        ))
        .center_x(Length::Fill)
        .padding(sp::L)
        .style(theme::card);
        let tile_w = 300.0;
        let cols = ((avail.width + sp::L) / (tile_w + sp::L)).floor().max(1.0) as usize;
        let stats = self.daemon.stats.as_ref();
        let state = self.daemon.state.clone().unwrap_or_default();
        let mut grid = column![].spacing(sp::L);
        let mut current = row![].spacing(sp::L);
        for (i, o) in self.daemon.outputs.iter().enumerate() {
            let os = state.outputs.iter().find(|x| x.output == o.id);
            let w = self.wallpaper_on(&o.id);
            let fps = stats
                .and_then(|s| s.outputs.iter().find(|x| x.output == o.id))
                .map(|x| x.fps);
            let inner = tile_w - 2.0 * sp::L;
            let mut title_row = row![strong(
                format!("{}  {}", i + 1, o.id),
                size::HEADING,
                t.text
            )]
            .spacing(sp::S)
            .align_y(Alignment::Center);
            if o.primary {
                title_row = title_row.push(
                    container(label(self.t(K::Primary), size::MICRO, t.on_accent))
                        .padding(Padding::from([2.0, 8.0]))
                        .style(theme::accent_badge),
                );
            }
            let hz = if o.refresh_mhz > 0 {
                format!(" · {:.0} Hz", o.refresh_mhz as f32 / 1000.0)
            } else {
                String::new()
            };
            let mut col = column![
                widgets::preview(
                    self,
                    w.and_then(|w| w.preview.as_deref()),
                    inner,
                    inner * 9.0 / 16.0,
                    radius::M
                ),
                title_row,
                label(
                    format!("{} · {}×{}{hz}", o.name, o.width, o.height),
                    size::CAPTION,
                    t.muted
                ),
                label(
                    w.map_or(self.t(K::EmptyScreen), |w| w.title()),
                    size::BODY,
                    t.text
                ),
            ]
            .spacing(sp::S);
            if let Some(os) = os {
                let (status, color) = match os.status {
                    lumenwall_ipc::WallpaperStatus::Running => (self.t(K::Playing), t.success),
                    lumenwall_ipc::WallpaperStatus::Loading => {
                        (self.t(K::StatusLoading), t.warning)
                    }
                    lumenwall_ipc::WallpaperStatus::Paused => (self.t(K::Paused), t.muted),
                    lumenwall_ipc::WallpaperStatus::Error => (self.t(K::StatusError), t.danger),
                    lumenwall_ipc::WallpaperStatus::Unsupported => {
                        (self.t(K::Unsupported), t.danger)
                    }
                    _ => (self.t(K::EmptyScreen), t.faint),
                };
                let scaling = match os.scaling {
                    lumenwall_ipc::Scaling::Fill => self.t(K::Fill),
                    lumenwall_ipc::Scaling::Fit => self.t(K::Fit),
                    lumenwall_ipc::Scaling::Stretch => self.t(K::Stretch),
                };
                let mut line = row![
                    label(status, size::CAPTION, color),
                    label(format!("· {scaling}"), size::CAPTION, t.faint)
                ]
                .spacing(sp::XS);
                if let Some(f) = fps {
                    line = line.push(label(format!("· {f:.0} fps"), size::CAPTION, t.faint));
                }
                col = col.push(line);
                if let Some(m) = &os.message {
                    col = col.push(label(m.clone(), size::CAPTION, t.danger));
                }
            }
            let mut buttons = row![widgets::secondary_button(
                t,
                Some(Icon::Library),
                self.t(K::ChangeWallpaper),
                Some(Message::TargetOnly(o.id.clone()))
            )]
            .spacing(sp::S);
            if w.is_some() {
                buttons = buttons.push(widgets::ghost_button(
                    t,
                    Some(Icon::Close),
                    self.t(K::RemoveWallpaper),
                    Some(Message::ClearScreen(o.id.clone())),
                ));
            }
            col = col.push(buttons);
            current = current.push(
                container(col)
                    .padding(sp::L)
                    .width(tile_w)
                    .style(theme::card),
            );
            if (i + 1) % cols == 0 {
                grid = grid.push(current);
                current = row![].spacing(sp::L);
            }
        }
        grid = grid.push(current);
        scrollable(column![map, grid, space().height(sp::L)].spacing(sp::XL))
            .style(theme::scroll_style)
            .height(Length::Fill)
            .into()
    }
}
