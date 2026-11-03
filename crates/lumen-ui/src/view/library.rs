//! Library page: header, instant search, filter chips, sort, and the virtualized
//! grid (only the rows on screen are built).

use std::collections::BTreeMap;

use iced::widget::{
    Id, column, container, mouse_area, pick_list, responsive, row, scrollable, space, stack,
    text_input,
};
use iced::{Alignment, Color, Element, Length, Padding, Size, mouse};

use super::widgets::{self, chip, label, strong};
use crate::app::{App, GRID_ID, Message, SEARCH_ID, grid_geom};
use crate::catalog::{Blocker, Kind, ResClass, Wallpaper};
use crate::config::SortKey;
use crate::daemon::Link;
use crate::i18n::K;
use crate::icons::{self, Icon, Illustration};
use crate::theme::{self, Tokens, layout, radius, size, space as sp};

pub fn kind_label(kind: Kind) -> K {
    match kind {
        Kind::Scene => K::KindScene,
        Kind::Video => K::KindVideo,
        Kind::Web => K::KindWeb,
        Kind::Application => K::KindApplication,
        Kind::Unknown => K::KindUnknown,
    }
}

pub fn kind_icon(kind: Kind) -> Icon {
    match kind {
        Kind::Scene => Icon::Scene,
        Kind::Video => Icon::Video,
        Kind::Web => Icon::Web,
        Kind::Application | Kind::Unknown => Icon::Alert,
    }
}

pub fn res_label(r: ResClass) -> K {
    match r {
        ResClass::UpTo1080 => K::ResHd,
        ResClass::Qhd => K::ResQhd,
        ResClass::Uhd => K::ResUhd,
        ResClass::Unknown => K::ResUnknown,
    }
}

/// A pick-list entry carrying its value and display text.
#[derive(Debug, Clone, PartialEq)]
struct Choice<T> {
    value: T,
    text: String,
}

impl<T> std::fmt::Display for Choice<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl App {
    pub(super) fn library_view(&self, t: &'static Tokens) -> Element<'_, Message> {
        let mut page = column![]
            .spacing(sp::L)
            .padding(Padding::from([layout::PAGE_PAD, layout::PAGE_PAD]).bottom(0.0));
        if matches!(self.daemon.link, Link::Offline(_)) {
            page = page.push(widgets::notice(
                t,
                theme::Notice::Info,
                self.t(K::EngineStopped),
                self.t(K::DemoBanner),
                Some(
                    widgets::primary_button(
                        t,
                        Some(Icon::Play),
                        self.t(K::StartEngine),
                        Some(Message::StartEngine),
                    )
                    .into(),
                ),
            ));
        }
        page = page.push(self.header(t));
        if self
            .library
            .as_ref()
            .is_some_and(|l| !l.wallpapers.is_empty())
        {
            page = page.push(self.filters(t));
        }
        let Some(lib) = &self.library else {
            return page
                .push(
                    container(label(self.t(K::Scanning), size::BODY, t.muted)).center(Length::Fill),
                )
                .into();
        };
        if lib.wallpapers.is_empty() {
            return page.push(self.empty_library(t)).into();
        }
        if !self.has_we_assets()
            && self.cfg.filter_kind != Some(Kind::Video)
            && lib.wallpapers.iter().any(|w| w.project.kind == Kind::Scene)
        {
            page = page.push(widgets::notice(
                t,
                theme::Notice::Warning,
                self.t(K::NoWeTitle),
                self.t(K::NoWeBody),
                None,
            ));
        }
        if self.filtered.is_empty() {
            return page
                .push(widgets::empty_state(
                    t,
                    Illustration::Search,
                    self.t(K::NoMatch),
                    self.t(K::NoMatchBody),
                    vec![widgets::secondary_button(
                        t,
                        Some(Icon::Close),
                        self.t(K::ClearFilters),
                        Some(Message::ClearFilters),
                    )],
                ))
                .into();
        }
        page.push(responsive(move |size| self.grid(t, size))).into()
    }

    fn header(&self, t: &'static Tokens) -> Element<'_, Message> {
        let total = self.library.as_ref().map_or(0, |l| l.wallpapers.len());
        let subtitle = if self.filtered.len() == total {
            self.t(K::LibrarySubtitle)
                .replace("{n}", &total.to_string())
        } else {
            self.t(K::CountShown)
                .replace("{shown}", &self.filtered.len().to_string())
                .replace("{n}", &total.to_string())
        };
        let search = container(
            row![
                icons::icon(Icon::Search, theme::icon::M, t.faint),
                text_input(self.t(K::SearchHint), &self.search)
                    .id(Id::new(SEARCH_ID))
                    .on_input(Message::Search)
                    .on_submit(Message::SearchSubmit)
                    .padding(Padding::from([8.0, 0.0]))
                    .size(size::BODY)
                    .font(theme::regular())
                    .style(theme::search_bare),
            ]
            .spacing(sp::S)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([0.0, 14.0]))
        .width(300)
        .style(theme::search_box);
        let sort_choices: Vec<Choice<SortKey>> = [
            (SortKey::Name, K::SortName),
            (SortKey::Recent, K::SortRecent),
            (SortKey::Kind, K::SortKind),
        ]
        .into_iter()
        .map(|(v, k)| Choice {
            value: v,
            text: k.get(self.lang).to_owned(),
        })
        .collect();
        let sort_sel = sort_choices
            .iter()
            .find(|c| c.value == self.cfg.sort)
            .cloned();
        row![
            column![
                strong(self.t(K::NavLibrary), size::DISPLAY, t.text),
                label(subtitle, size::BODY, t.muted)
            ]
            .spacing(2),
            space().width(Length::Fill),
            label(self.t(K::SortBy), size::CAPTION, t.faint),
            pick_list(sort_choices, sort_sel, |c: Choice<SortKey>| Message::Sort(
                c.value
            ))
            .padding(Padding::from([8.0, 12.0]))
            .text_size(size::CAPTION)
            .font(theme::medium())
            .style(theme::pick_list_style),
            search,
        ]
        .spacing(sp::S)
        .align_y(Alignment::Center)
        .into()
    }

    fn filters(&self, t: &'static Tokens) -> Element<'_, Message> {
        let kind = self.cfg.filter_kind;
        let mut chips = row![].spacing(sp::S).align_y(Alignment::Center);
        for (k, label_key) in [
            (None, K::All),
            (Some(Kind::Scene), K::KindScene),
            (Some(Kind::Video), K::KindVideo),
            (Some(Kind::Web), K::KindWeb),
        ] {
            chips = chips.push(chip(self.t(label_key), kind == k, Message::FilterKind(k)));
        }
        // Tags, most used first.
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        if let Some(lib) = &self.library {
            for w in &lib.wallpapers {
                for tag in &w.project.tags {
                    *counts.entry(tag.as_str()).or_default() += 1;
                }
            }
        }
        let mut tags: Vec<(&str, usize)> = counts.into_iter().collect();
        tags.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let mut tag_choices = vec![Choice {
            value: None,
            text: self.t(K::AnyTag).to_owned(),
        }];
        tag_choices.extend(tags.into_iter().map(|(tag, n)| Choice {
            value: Some(tag.to_owned()),
            text: format!("{tag} ({n})"),
        }));
        let tag_sel = tag_choices
            .iter()
            .find(|c| c.value == self.cfg.filter_tag)
            .cloned();
        let mut res_choices = vec![Choice {
            value: None,
            text: self.t(K::AnyResolution).to_owned(),
        }];
        res_choices.extend(ResClass::ALL.map(|r| Choice {
            value: Some(r),
            text: res_label(r).get(self.lang).to_owned(),
        }));
        let res_sel = res_choices
            .iter()
            .find(|c| c.value == self.cfg.filter_res)
            .cloned();
        let pl = |_: f32| Padding::from([5.0, 12.0]);
        let mut r = row![
            chips,
            space().width(sp::S),
            pick_list(tag_choices, tag_sel, |c: Choice<Option<String>>| {
                Message::FilterTag(c.value)
            })
            .padding(pl(0.0))
            .text_size(size::CAPTION)
            .font(theme::medium())
            .style(theme::pick_list_style),
            pick_list(res_choices, res_sel, |c: Choice<Option<ResClass>>| {
                Message::FilterRes(c.value)
            })
            .padding(pl(0.0))
            .text_size(size::CAPTION)
            .font(theme::medium())
            .style(theme::pick_list_style),
        ]
        .spacing(sp::S)
        .align_y(Alignment::Center);
        if self.filters_active() {
            r = r.push(widgets::ghost_button(
                t,
                Some(Icon::Close),
                self.t(K::ClearFilters),
                Some(Message::ClearFilters),
            ));
        }
        r.into()
    }

    fn empty_library(&self, t: &'static Tokens) -> Element<'_, Message> {
        let Some(lib) = &self.library else {
            return space().into();
        };
        if lib.steam.roots.is_empty() {
            let searched = column(
                lib.steam
                    .searched
                    .iter()
                    .take(8)
                    .map(|p| label(widgets::tilde(p), size::MICRO, t.faint).into()),
            )
            .spacing(2)
            .align_x(Alignment::Center);
            return column![
                widgets::empty_state(
                    t,
                    Illustration::Folder,
                    self.t(K::NoSteamTitle),
                    self.t(K::NoSteamBody),
                    vec![
                        widgets::primary_button(
                            t,
                            Some(Icon::Folder),
                            self.t(K::AddFolder),
                            Some(Message::PickFolder)
                        )
                        .into()
                    ],
                ),
                container(
                    column![
                        label(self.t(K::SearchedIn), size::CAPTION, t.muted),
                        searched
                    ]
                    .spacing(sp::XS)
                    .align_x(Alignment::Center)
                )
                .center_x(Length::Fill),
            ]
            .spacing(sp::L)
            .into();
        }
        widgets::empty_state(
            t,
            Illustration::Screen,
            self.t(K::NoWallpapersTitle),
            self.t(K::NoWallpapersBody),
            vec![
                widgets::primary_button(
                    t,
                    Some(Icon::External),
                    self.t(K::OpenWorkshop),
                    Some(Message::Open("steam://url/SteamWorkshopPage/431960".into())),
                )
                .into(),
                widgets::secondary_button(
                    t,
                    Some(Icon::Folder),
                    self.t(K::AddFolder),
                    Some(Message::PickFolder),
                ),
            ],
        )
    }

    fn grid(&self, t: &'static Tokens, avail: Size) -> Element<'_, Message> {
        let g = grid_geom(avail.width - 12.0);
        let Some(lib) = &self.library else {
            return space().into();
        };
        let n = self.filtered.len();
        let rows = n.div_ceil(g.cols);
        let first = ((self.scroll_y / g.row_h).floor().max(0.0) as usize).min(rows);
        let last = (((self.scroll_y + avail.height) / g.row_h).ceil() as usize + 1).min(rows);
        let mut col = column![space().height(first as f32 * g.row_h)];
        for r in first..last {
            let mut cards = row![].spacing(layout::CARD_GAP);
            for c in 0..g.cols {
                let Some(&wi) = self.filtered.get(r * g.cols + c) else {
                    break;
                };
                cards = cards.push(self.card(t, &lib.wallpapers[wi], g.card_w, g.card_h));
            }
            col = col.push(container(cards).height(g.row_h));
        }
        col = col.push(space().height((rows - last) as f32 * g.row_h));
        scrollable(container(col).padding(Padding::default().right(12.0).bottom(sp::L)))
            .id(Id::new(GRID_ID))
            .on_scroll(Message::Scrolled)
            .style(theme::scroll_style)
            .height(Length::Fill)
            .into()
    }

    fn hover_amount(&self, w: &Wallpaper) -> f32 {
        let now = std::time::Instant::now();
        if self.hovered.as_ref() == Some(&w.dir) {
            self.hover_in.interpolate(0.0, 1.0, now)
        } else if self.hover_prev.as_ref() == Some(&w.dir) {
            self.hover_out.interpolate(0.0, 1.0, now)
        } else {
            0.0
        }
    }

    fn card<'a>(
        &'a self,
        t: &'static Tokens,
        w: &'a Wallpaper,
        cw: f32,
        ch: f32,
    ) -> Element<'a, Message> {
        let selected = self.selected.as_ref() == Some(&w.dir);
        let hover = self.hover_amount(w);
        let r = radius::L;
        let img = widgets::preview(self, w.preview.as_deref(), cw, ch, r);

        // Bottom scrim with title and a short subtitle.
        let mut sub: Vec<String> = Vec::new();
        if let Some((rw, rh)) = w.resolution {
            sub.push(format!("{rw}×{rh}"));
        }
        sub.extend(w.project.tags.iter().take(2).cloned());
        let caption = column![
            strong(w.title(), size::HEADING, Color::WHITE)
                .wrapping(iced::widget::text::Wrapping::None),
            label(
                sub.join(" · "),
                size::CAPTION,
                Color {
                    a: 0.78,
                    ..Color::WHITE
                }
            )
            .wrapping(iced::widget::text::Wrapping::None),
        ]
        .spacing(1)
        .clip(true);
        let scrim = container(caption)
            .width(Length::Fill)
            .height(ch * 0.46)
            .padding(Padding::from([10.0, 14.0]))
            .align_y(Alignment::End)
            .style(theme::scrim);

        // Type badge (glass) and "on screen" badge (accent).
        let kind = container(
            row![
                icons::icon(kind_icon(w.project.kind), theme::icon::S, Color::WHITE),
                label(
                    kind_label(w.project.kind).get(self.lang),
                    size::MICRO,
                    Color::WHITE
                )
            ]
            .spacing(5)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([3.0, 9.0]))
        .style(theme::glass_badge);
        let mut top = row![kind, space().width(Length::Fill)].align_y(Alignment::Center);
        let screens = self.screen_numbers(&w.dir);
        if screens.is_empty() && self.ws.new_items.contains(&w.dir) {
            top = top.push(
                container(
                    row![
                        icons::icon(Icon::Sparkle, theme::icon::S, t.on_accent),
                        strong(self.t(K::NewBadge), size::MICRO, t.on_accent)
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([3.0, 9.0]))
                .style(theme::accent_badge),
            );
        }
        if !screens.is_empty() {
            let list: Vec<String> = screens.iter().map(usize::to_string).collect();
            top = top.push(
                container(
                    row![
                        icons::icon(Icon::Check, theme::icon::S, t.on_accent),
                        strong(
                            format!("{} {}", self.t(K::OnScreen), list.join(", ")),
                            size::MICRO,
                            t.on_accent
                        )
                    ]
                    .spacing(4)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([3.0, 9.0]))
                .style(theme::accent_badge),
            );
        }
        match w.blocker(self.has_we_assets()) {
            Some(Blocker::NeedsWallpaperEngine) => {
                top = top.push(
                    container(label(self.t(K::NeedsWe), size::MICRO, t.on_warning))
                        .padding(Padding::from([3.0, 9.0]))
                        .style(theme::badge(t.warning, t.on_warning)),
                );
            }
            Some(Blocker::Unsupported) => {
                top = top.push(
                    container(label(self.t(K::Unsupported), size::MICRO, t.on_danger))
                        .padding(Padding::from([3.0, 9.0]))
                        .style(theme::badge(t.danger, t.on_danger)),
                );
            }
            None => {}
        }
        // A soft light wash on hover makes the card feel responsive.
        let wash = container(space())
            .width(Length::Fill)
            .height(Length::Fill)
            .style(move |_| container::Style {
                background: Some(
                    Color {
                        a: 0.06 * hover,
                        ..Color::WHITE
                    }
                    .into(),
                ),
                border: iced::Border {
                    radius: r.into(),
                    ..Default::default()
                },
                ..Default::default()
            });
        let card = stack![
            img,
            wash,
            column![space().height(Length::Fill), scrim],
            container(top).padding(10).width(Length::Fill),
            container(space())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(theme::card_frame(selected, hover)),
        ]
        .width(cw)
        .height(ch);
        mouse_area(card)
            .on_press(Message::CardPress(w.dir.clone()))
            .on_double_click(Message::CardDouble(w.dir.clone()))
            .on_enter(Message::Hover(w.dir.clone(), true))
            .on_exit(Message::Hover(w.dir.clone(), false))
            .interaction(mouse::Interaction::Pointer)
            .into()
    }
}
