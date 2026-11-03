//! Workshop page: browse (with the user's key) or open Steam (without), recent
//! arrivals, installed items, and the details panel of a Workshop item.

use iced::widget::{
    Id, button, column, container, mouse_area, pick_list, responsive, rich_text, row, scrollable,
    space, span, stack, text_input,
};
use iced::{Alignment, Color, Element, Length, Padding, Size, mouse};

use super::library::{kind_icon, kind_label};
use super::widgets::{self, chip, label, strong};
use crate::app::{App, Message, Page, grid_geom};
use crate::catalog::Kind;
use crate::i18n::K;
use crate::icons::{self, Icon, Illustration};
use crate::theme::{self, Tokens, layout, radius, size, space as sp};
use crate::workshop::api::{self, NetError};
use crate::workshop::bbcode::{self, BlockKind};
use crate::workshop::state::{Fetch, WS_GRID_ID, WsMsg};
use crate::workshop::{self as ws, Item, Sort};

fn msg(m: WsMsg) -> Message {
    Message::Ws(m)
}

#[derive(Debug, Clone, PartialEq)]
struct Choice {
    value: Option<String>,
    text: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

/// Five stars from a 0..1 score (halves included).
fn stars<'a>(t: &Tokens, score: f32, size: f32) -> Element<'a, Message> {
    let halves = (score.clamp(0.0, 1.0) * 10.0).round() as u32;
    let mut r = row![].spacing(1).align_y(Alignment::Center);
    for i in 0..5 {
        let icon = match halves.saturating_sub(i * 2) {
            0 => Icon::StarEmpty,
            1 => Icon::StarHalf,
            _ => Icon::Star,
        };
        r = r.push(icons::icon(icon, size, t.warning));
    }
    r.into()
}

impl App {
    /// One decimal, with the language's separator.
    fn decimal(&self, v: f32) -> String {
        let s = format!("{v:.1}");
        if self.lang == crate::i18n::Lang::Fr {
            s.replace('.', ",")
        } else {
            s
        }
    }

    fn author_of(&self, it: &Item) -> Option<String> {
        it.author
            .clone()
            .or_else(|| self.ws.names.get(&it.creator).cloned())
    }

    /// Preview of an item (installed copy first), or one of its screenshots.
    fn ws_preview<'a>(
        &self,
        it: &Item,
        w: f32,
        h: f32,
        r: f32,
        shot: Option<usize>,
    ) -> Element<'a, Message> {
        let path = match shot {
            Some(i) => it.screenshots.get(i).and_then(|u| api::image_path(u)),
            None => self.ws_preview_file(it).map(|(p, _)| p),
        };
        widgets::preview(self, path.as_deref(), w, h, r)
    }

    pub(super) fn workshop_view(&self, t: &'static Tokens) -> Element<'_, Message> {
        let header = row![
            column![
                strong(self.t(K::NavWorkshop), size::DISPLAY, t.text),
                label(self.t(K::WsSubtitle), size::BODY, t.muted)
            ]
            .spacing(2),
            space().width(Length::Fill),
            widgets::secondary_button(
                t,
                Some(Icon::External),
                self.t(K::WsBrowseSteam),
                Some(Message::Open(ws::STEAM_WORKSHOP.into()))
            ),
        ]
        .spacing(sp::S)
        .align_y(Alignment::Center);
        let mut page = column![header]
            .spacing(sp::L)
            .padding(Padding::from([layout::PAGE_PAD, layout::PAGE_PAD]).bottom(0.0));
        if self.ws.key.is_some() {
            page = page.push(self.ws_controls(t));
            page = page.push(
                container(
                    row![
                        icons::icon(Icon::Store, theme::icon::S, t.accent_text),
                        strong(self.t(K::WsOwnTitle), size::CAPTION, t.text),
                        label(self.t(K::WsOwnShort), size::CAPTION, t.muted).width(Length::Fill),
                    ]
                    .spacing(sp::S)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([7.0, 12.0]))
                .width(Length::Fill)
                .style(theme::hint_bar),
            );
            page = page.push(self.ws_new_strip(t));
            page = page.push(responsive(move |s| self.ws_results(t, s)));
        } else {
            page = page.push(
                scrollable(
                    column![
                        self.ws_onboarding(t),
                        self.ws_new_strip(t),
                        self.ws_installed(t),
                        space().height(sp::XL)
                    ]
                    .spacing(sp::XL),
                )
                .style(theme::scroll_style)
                .height(Length::Fill),
            );
        }
        page.into()
    }

    fn ws_controls(&self, t: &'static Tokens) -> Element<'_, Message> {
        let q = &self.ws.query;
        let searching = !q.search.is_empty();
        let mut sorts = row![].spacing(sp::S).align_y(Alignment::Center);
        for (s, k) in [
            (Sort::Trend, K::WsTrend),
            (Sort::Popular, K::WsPopular),
            (Sort::Recent, K::WsRecent),
        ] {
            sorts = sorts.push(chip(
                self.t(k),
                !searching && q.sort == s,
                msg(WsMsg::Sort(s)),
            ));
        }
        if searching {
            let label_text = self.t(K::WsResultsFor).replace("{q}", &q.search);
            sorts = sorts.push(
                container(label(label_text, size::CAPTION, t.on_accent))
                    .padding(Padding::from([5.0, 14.0]))
                    .style(theme::accent_badge),
            );
        }
        let search = container(
            row![
                icons::icon(Icon::Search, theme::icon::M, t.faint),
                text_input(self.t(K::WsSearchHint), &self.ws.search_input)
                    .id(Id::new("ws-search"))
                    .on_input(|s| msg(WsMsg::Search(s)))
                    .on_submit(msg(WsMsg::SearchSubmit))
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
        let mut kinds = row![].spacing(sp::S);
        for (k, l) in [
            (None, K::All),
            (Some(Kind::Scene), K::KindScene),
            (Some(Kind::Video), K::KindVideo),
            (Some(Kind::Web), K::KindWeb),
        ] {
            kinds = kinds.push(chip(self.t(l), q.kind == k, msg(WsMsg::Kind(k))));
        }
        let mut tags = vec![Choice {
            value: None,
            text: self.t(K::AnyTag).into(),
        }];
        tags.extend(ws::GENRES.iter().map(|g| Choice {
            value: Some((*g).into()),
            text: (*g).into(),
        }));
        let tag_sel = tags.iter().find(|c| c.value == q.tag).cloned();
        let mut res = vec![Choice {
            value: None,
            text: self.t(K::AnyResolution).into(),
        }];
        res.extend(ws::RESOLUTIONS.iter().map(|r| Choice {
            value: Some((*r).into()),
            text: (*r).replace(" x ", "×"),
        }));
        let res_sel = res.iter().find(|c| c.value == q.resolution).cloned();
        let pl = Padding::from([5.0, 12.0]);
        column![
            row![sorts, space().width(Length::Fill), search]
                .spacing(sp::S)
                .align_y(Alignment::Center),
            row![
                kinds,
                space().width(sp::S),
                pick_list(tags, tag_sel, |c: Choice| msg(WsMsg::Tag(c.value)))
                    .padding(pl)
                    .text_size(size::CAPTION)
                    .font(theme::medium())
                    .style(theme::pick_list_style),
                pick_list(res, res_sel, |c: Choice| msg(WsMsg::Resolution(c.value)))
                    .padding(pl)
                    .text_size(size::CAPTION)
                    .font(theme::medium())
                    .style(theme::pick_list_style),
                space().width(Length::Fill),
                label(
                    if self.ws.total > 0 {
                        self.t(K::WsTotal)
                            .replace("{n}", &self.ws.total.to_string())
                    } else {
                        String::new()
                    },
                    size::CAPTION,
                    t.faint
                ),
            ]
            .spacing(sp::S)
            .align_y(Alignment::Center),
        ]
        .spacing(sp::M)
        .into()
    }

    fn ws_error<'a>(&self, t: &'static Tokens, e: &NetError, full: bool) -> Element<'a, Message> {
        let (title, body) = match e {
            NetError::Offline => (self.t(K::WsOffline), self.t(K::WsOfflineBody)),
            NetError::Quota => (self.t(K::WsQuota), self.t(K::WsQuotaBody)),
            NetError::Forbidden => (self.t(K::WsForbidden), self.t(K::WsForbiddenBody)),
            _ => (self.t(K::WsError), self.t(K::WsOfflineBody)),
        };
        let retry = widgets::secondary_button(
            t,
            Some(Icon::Refresh),
            self.t(K::Retry),
            Some(msg(WsMsg::Retry)),
        );
        if full {
            let mut actions = vec![retry];
            if *e == NetError::Forbidden {
                actions.push(widgets::secondary_button(
                    t,
                    Some(Icon::Key),
                    self.t(K::NavSettings),
                    Some(Message::Nav(Page::Settings)),
                ));
            }
            widgets::empty_state(t, Illustration::Offline, title, body, actions)
        } else {
            widgets::notice(t, theme::Notice::Warning, title, body, Some(retry))
        }
    }

    fn ws_results(&self, t: &'static Tokens, avail: Size) -> Element<'_, Message> {
        if self.ws.items.is_empty() {
            if let Some(e) = &self.ws.error {
                return self.ws_error(t, e, true);
            }
            if self.ws.loading || self.ws.loaded.is_none() {
                return self.ws_skeleton(t, avail);
            }
            return widgets::empty_state(
                t,
                Illustration::Search,
                self.t(K::WsNoResults),
                self.t(K::WsNoResultsBody),
                vec![],
            );
        }
        let g = grid_geom(avail.width - 12.0);
        let n = self.ws.items.len();
        let rows = n.div_ceil(g.cols);
        let first = ((self.ws.scroll_y / g.row_h).floor().max(0.0) as usize).min(rows);
        let last = (((self.ws.scroll_y + avail.height) / g.row_h).ceil() as usize + 1).min(rows);
        let mut col = column![space().height(first as f32 * g.row_h)];
        for r in first..last {
            let mut cards = row![].spacing(layout::CARD_GAP);
            for c in 0..g.cols {
                let Some(it) = self.ws.items.get(r * g.cols + c) else {
                    break;
                };
                cards = cards.push(self.ws_card(t, it, g.card_w, g.card_h));
            }
            col = col.push(container(cards).height(g.row_h));
        }
        col = col.push(space().height((rows - last) as f32 * g.row_h));
        let footer: Element<'_, Message> = if let Some(e) = &self.ws.error {
            self.ws_error(t, e, false)
        } else if self.ws.loading {
            label(self.t(K::WsLoading), size::CAPTION, t.muted).into()
        } else if self.ws.cursor.is_some() {
            widgets::secondary_button(
                t,
                Some(Icon::Download),
                self.t(K::WsLoadMore),
                Some(msg(WsMsg::LoadMore)),
            )
        } else {
            label(self.t(K::WsEnd), size::CAPTION, t.faint).into()
        };
        col = col.push(
            container(footer)
                .center_x(Length::Fill)
                .padding(Padding::from([sp::S, 0.0]).bottom(sp::XL)),
        );
        scrollable(container(col).padding(Padding::default().right(12.0)))
            .id(Id::new(WS_GRID_ID))
            .on_scroll(|v| msg(WsMsg::Scrolled(v)))
            .style(theme::scroll_style)
            .height(Length::Fill)
            .into()
    }

    /// Placeholder cards while the first page loads.
    fn ws_skeleton(&self, t: &'static Tokens, avail: Size) -> Element<'_, Message> {
        let g = grid_geom(avail.width - 12.0);
        let mut col = column![].spacing(layout::CARD_GAP);
        for _ in 0..2 {
            let mut r = row![].spacing(layout::CARD_GAP);
            for _ in 0..g.cols {
                r = r.push(
                    container(space())
                        .width(g.card_w)
                        .height(g.card_h)
                        .style(theme::preview_placeholder),
                );
            }
            col = col.push(r);
        }
        column![
            col,
            container(label(self.t(K::WsLoading), size::CAPTION, t.muted)).center_x(Length::Fill)
        ]
        .spacing(sp::L)
        .into()
    }

    fn ws_card(&self, t: &'static Tokens, it: &Item, cw: f32, ch: f32) -> Element<'_, Message> {
        let selected = self.ws.selected.as_deref() == Some(it.id.as_str());
        let hovered = self.ws.hovered.as_deref() == Some(it.id.as_str());
        let r = radius::L;
        let img = self.ws_preview(it, cw, ch, r, None);
        let white = |a: f32| Color { a, ..Color::WHITE };
        let mut meta = row![].spacing(6).align_y(Alignment::Center);
        if let Some(name) = self.author_of(it) {
            meta = meta.push(
                label(
                    self.t(K::By).replace("{name}", &name),
                    size::CAPTION,
                    white(0.8),
                )
                .wrapping(iced::widget::text::Wrapping::None),
            );
        }
        if let Some(s) = it.score {
            meta = meta
                .push(icons::icon(Icon::Star, theme::icon::S, t.warning))
                .push(label(self.decimal(s * 5.0), size::CAPTION, white(0.85)));
        }
        if let Some(n) = it.subscriptions {
            meta = meta
                .push(icons::icon(Icon::Users, theme::icon::S, white(0.8)))
                .push(label(
                    ws::format_count(n, self.lang),
                    size::CAPTION,
                    white(0.85),
                ));
        }
        let caption = column![
            strong(it.title.clone(), size::HEADING, Color::WHITE)
                .wrapping(iced::widget::text::Wrapping::None),
            meta.clip(true),
        ]
        .spacing(2)
        .clip(true);
        let scrim = container(caption)
            .width(Length::Fill)
            .height(ch * 0.5)
            .padding(Padding::from([10.0, 14.0]))
            .align_y(Alignment::End)
            .style(theme::scrim);
        let mut top = row![].align_y(Alignment::Center);
        if let Some(k) = it.kind() {
            top = top.push(
                container(
                    row![
                        icons::icon(kind_icon(k), theme::icon::S, Color::WHITE),
                        label(kind_label(k).get(self.lang), size::MICRO, Color::WHITE)
                    ]
                    .spacing(5)
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([3.0, 9.0]))
                .style(theme::glass_badge),
            );
        }
        top = top.push(space().width(Length::Fill));
        // Visible only once the user allowed such content in Settings.
        let rating = match it.rating() {
            ws::Rating::Everyone => None,
            ws::Rating::Questionable => Some("Questionable"),
            ws::Rating::Mature => Some("Mature"),
        };
        if let Some(r) = rating {
            top = top.push(
                container(label(r, size::MICRO, t.on_warning))
                    .padding(Padding::from([3.0, 9.0]))
                    .style(theme::badge(t.warning, t.on_warning)),
            );
            top = top.push(space().width(sp::XS));
        }
        if let Some(dir) = self.installed_dir(&it.id) {
            let new = self.ws.new_items.contains(&dir);
            top = top.push(
                container(
                    row![
                        icons::icon(Icon::Check, theme::icon::S, t.on_accent),
                        strong(
                            self.t(if new { K::NewBadge } else { K::Installed }),
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
        let hover = if hovered { 1.0 } else { 0.0 };
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
            .on_press(msg(WsMsg::Select(it.id.clone())))
            .on_enter(msg(WsMsg::Hover(it.id.clone(), true)))
            .on_exit(msg(WsMsg::Hover(it.id.clone(), false)))
            .interaction(mouse::Interaction::Pointer)
            .into()
    }

    fn ws_onboarding(&self, t: &'static Tokens) -> Element<'_, Message> {
        let body = column![
            strong(self.t(K::WsKeyTitle), size::TITLE, t.text),
            label(self.t(K::WsKeyBody), size::BODY, t.muted),
            row![
                widgets::primary_button(
                    t,
                    Some(Icon::Key),
                    self.t(K::WsAddKey),
                    Some(Message::Nav(Page::Settings))
                ),
                widgets::secondary_button(
                    t,
                    Some(Icon::External),
                    self.t(K::WsGetKey),
                    Some(Message::Open(ws::API_KEY_PAGE.into()))
                ),
                widgets::ghost_button(
                    t,
                    Some(Icon::Workshop),
                    self.t(K::WsOpenBrowser),
                    Some(Message::Open(ws::WEB_WORKSHOP.into()))
                ),
            ]
            .spacing(sp::S)
            .wrap(),
            container(
                row![
                    icons::icon(Icon::Alert, theme::icon::M, t.accent_text),
                    label(self.t(K::WsOwnBody), size::CAPTION, t.muted)
                ]
                .spacing(sp::S)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([sp::S, 0.0])),
        ]
        .spacing(sp::M)
        .width(Length::Fill);
        container(
            row![icons::illustration(Illustration::Workshop, t), body]
                .spacing(sp::XL)
                .align_y(Alignment::Center),
        )
        .padding(sp::XL)
        .width(Length::Fill)
        .style(theme::card)
        .into()
    }

    /// New subscriptions noticed by the folder watcher (or since last session).
    fn ws_new_strip(&self, t: &'static Tokens) -> Element<'_, Message> {
        let Some(lib) = &self.library else {
            return space().into();
        };
        let mut news: Vec<&crate::catalog::Wallpaper> = lib
            .wallpapers
            .iter()
            .filter(|w| self.ws.new_items.contains(&w.dir))
            .collect();
        if news.is_empty() {
            return space().height(0).into();
        }
        news.sort_by_key(|w| std::cmp::Reverse(w.added));
        let mut r = row![].spacing(sp::M);
        for w in news.into_iter().take(4) {
            let card = row![
                widgets::preview(self, w.preview.as_deref(), 96.0, 54.0, radius::S),
                column![
                    strong(w.title(), size::CAPTION, t.text)
                        .wrapping(iced::widget::text::Wrapping::None),
                    container(strong(self.t(K::NewBadge), size::MICRO, t.on_accent))
                        .padding(Padding::from([1.0, 7.0]))
                        .style(theme::accent_badge),
                ]
                .spacing(4)
                .width(150)
                .clip(true),
            ]
            .spacing(sp::S)
            .align_y(Alignment::Center);
            r = r.push(
                button(container(card).padding(sp::S).style(theme::card))
                    .padding(0)
                    .style(theme::bare)
                    .on_press(Message::ShowInLibrary(w.dir.clone())),
            );
        }
        column![
            strong(self.t(K::WsNewTitle).to_uppercase(), size::MICRO, t.faint),
            r.wrap()
        ]
        .spacing(sp::S)
        .into()
    }

    /// Without a key: the installed Workshop items, enriched by the keyless details.
    fn ws_installed(&self, t: &'static Tokens) -> Element<'_, Message> {
        let ids = self.installed_ids();
        let mut col = column![strong(
            self.t(K::WsInstalledTitle).to_uppercase(),
            size::MICRO,
            t.faint
        )]
        .spacing(sp::M);
        if ids.is_empty() {
            return col
                .push(label(self.t(K::WsInstalledNone), size::BODY, t.muted))
                .into();
        }
        if let Fetch::Failed(e) = &self.ws.installed_fetch {
            col = col.push(self.ws_error(t, e, false));
        }
        let list = self.ws_installed_list();
        let more = ids.len() > list.len();
        let width = self.window.width
            - layout::SIDEBAR_W
            - 2.0 * layout::PAGE_PAD
            - if self.ws.selected.is_some() {
                layout::PANEL_W
            } else {
                0.0
            }
            - 12.0;
        let g = grid_geom(width.max(300.0));
        let mut rows = column![].spacing(layout::CARD_GAP);
        for chunk in list.chunks(g.cols) {
            let mut r = row![].spacing(layout::CARD_GAP);
            for (id, dir) in chunk {
                let item = self.ws.installed.get(id).cloned().unwrap_or_else(|| {
                    let w = self.wallpaper(dir);
                    let mut tags = w.map(|w| w.project.tags.clone()).unwrap_or_default();
                    if let Some(k) = w.map(|w| w.project.kind) {
                        tags.push(format!("{k:?}"));
                    }
                    Item {
                        id: id.clone(),
                        title: w.map(|w| w.title().to_owned()).unwrap_or_default(),
                        tags,
                        ..Item::default()
                    }
                });
                r = r.push(self.ws_installed_card(t, item, g.card_w, g.card_h));
            }
            rows = rows.push(r);
        }
        col = col.push(rows);
        if more {
            col = col.push(widgets::secondary_button(
                t,
                Some(Icon::Library),
                self.t(K::NavLibrary),
                Some(Message::Nav(Page::Library)),
            ));
        }
        col.into()
    }

    fn ws_installed_card(
        &self,
        t: &'static Tokens,
        it: Item,
        cw: f32,
        ch: f32,
    ) -> Element<'_, Message> {
        self.ws_card(t, &it, cw, ch)
    }

    // ------------------------------------------------------------- details panel

    pub(super) fn ws_details(&self, t: &'static Tokens) -> Element<'_, Message> {
        let Some(it) = self.ws.selected.as_deref().and_then(|id| self.ws.item(id)) else {
            return container(space())
                .style(theme::panel)
                .height(Length::Fill)
                .into();
        };
        let inner = layout::PANEL_W - 2.0 * sp::XL;
        let head = row![
            label(self.t(K::NavWorkshop), size::CAPTION, t.faint),
            space().width(Length::Fill),
            widgets::icon_button(
                t,
                Icon::Close,
                self.t(K::Close),
                false,
                Some(msg(WsMsg::Close))
            ),
        ]
        .align_y(Alignment::Center);
        let mut col = column![
            head,
            self.ws_preview(it, inner, inner * 9.0 / 16.0, radius::L, self.ws.shot)
        ]
        .spacing(sp::L);
        if !it.screenshots.is_empty() {
            col = col.push(strong(
                self.t(K::WsScreenshots).to_uppercase(),
                size::MICRO,
                t.faint,
            ));
            let tw = (inner - 3.0 * sp::S) / 4.0;
            let mut shots = row![].spacing(sp::S);
            let main = button(
                container(self.ws_preview(it, tw, tw * 9.0 / 16.0, radius::S, None))
                    .style(theme::monitor(self.ws.shot.is_none(), false)),
            )
            .padding(0)
            .style(theme::bare)
            .on_press(msg(WsMsg::Shot(None)));
            shots = shots.push(main);
            for (i, _) in it.screenshots.iter().enumerate().take(3) {
                shots = shots.push(
                    button(
                        container(self.ws_preview(it, tw, tw * 9.0 / 16.0, radius::S, Some(i)))
                            .style(theme::monitor(self.ws.shot == Some(i), false)),
                    )
                    .padding(0)
                    .style(theme::bare)
                    .on_press(msg(WsMsg::Shot(Some(i)))),
                );
            }
            col = col.push(shots);
        }
        let mut info = column![strong(it.title.as_str(), size::TITLE, t.text)].spacing(sp::XS);
        let mut by = row![].spacing(sp::S).align_y(Alignment::Center);
        if let Some(name) = self.author_of(it) {
            by = by.push(label(
                self.t(K::By).replace("{name}", &name),
                size::BODY,
                t.muted,
            ));
        }
        if !it.creator.is_empty() {
            by = by.push(widgets::ghost_button(
                t,
                Some(Icon::External),
                self.t(K::WsAuthorProfile),
                Some(Message::Open(format!(
                    "https://steamcommunity.com/profiles/{}",
                    it.creator
                ))),
            ));
        }
        info = info.push(by);
        if let Some(s) = it.score {
            let mut rr = row![
                stars(t, s, theme::icon::M),
                strong(self.decimal(s * 5.0), size::BODY, t.text)
            ]
            .spacing(sp::S)
            .align_y(Alignment::Center);
            if let Some(v) = it.votes {
                rr = rr.push(label(
                    self.t(K::WsRatings)
                        .replace("{n}", &ws::format_count(v, self.lang)),
                    size::CAPTION,
                    t.faint,
                ));
            }
            info = info.push(rr);
        }
        col = col.push(info);

        // Stats.
        let stat = |icon: Icon, name: &'static str, value: String| -> Element<'_, Message> {
            column![
                row![
                    icons::icon(icon, theme::icon::S, t.faint),
                    label(name, size::MICRO, t.faint)
                ]
                .spacing(5)
                .align_y(Alignment::Center),
                strong(value, size::BODY, t.text),
            ]
            .spacing(2)
            .width(Length::FillPortion(1))
            .into()
        };
        let mut cells: Vec<Element<'_, Message>> = Vec::new();
        if let Some(n) = it.subscriptions {
            cells.push(stat(
                Icon::Users,
                self.t(K::WsSubscribers),
                ws::format_count(n, self.lang),
            ));
        }
        if let Some(n) = it.favorited {
            cells.push(stat(
                Icon::Heart,
                self.t(K::WsFavorites),
                ws::format_count(n, self.lang),
            ));
        }
        if let Some(n) = it.file_size {
            cells.push(stat(
                Icon::Package,
                self.t(K::WsSize),
                widgets::format_bytes(n, self.lang),
            ));
        }
        if let Some(d) = it.time_updated {
            cells.push(stat(
                Icon::Clock,
                self.t(K::WsUpdatedLabel),
                ws::format_date(d, self.lang),
            ));
        }
        if !cells.is_empty() {
            // Two columns: dates and counts stay on one line in the panel.
            let mut grid = column![].spacing(sp::M);
            let mut it_cells = cells.into_iter();
            while let Some(a) = it_cells.next() {
                let mut r = row![a].spacing(sp::M);
                r = r.push(
                    it_cells
                        .next()
                        .unwrap_or_else(|| space().width(Length::FillPortion(1)).into()),
                );
                grid = grid.push(r);
            }
            col = col.push(
                container(grid)
                    .padding(sp::M)
                    .width(Length::Fill)
                    .style(theme::card),
            );
        }

        // Actions.
        let installed = self.installed_dir(&it.id);
        let has_steam = self
            .library
            .as_ref()
            .is_some_and(|l| !l.steam.roots.is_empty());
        let mut actions = column![].spacing(sp::S);
        match &installed {
            Some(dir) => {
                let can = self.wallpaper(dir).is_some_and(|w| self.can_apply(w));
                actions = actions.push(widgets::primary_wide(
                    t,
                    Some(Icon::Check),
                    self.t(K::Apply),
                    can.then(|| Message::ApplyDir(dir.clone())),
                ));
                actions = actions.push(widgets::secondary_button(
                    t,
                    Some(Icon::Library),
                    self.t(K::WsShowInLibrary),
                    Some(Message::ShowInLibrary(dir.clone())),
                ));
                if !can && !self.daemon.is_online() {
                    actions = actions.push(label(self.t(K::NeedEngine), size::CAPTION, t.faint));
                }
            }
            None => {
                let target = if has_steam {
                    ws::steam_page(&it.id)
                } else {
                    ws::web_page(&it.id)
                };
                actions = actions.push(widgets::primary_wide(
                    t,
                    Some(Icon::Download),
                    self.t(K::WsSubscribe),
                    Some(Message::Open(target)),
                ));
                actions = actions.push(
                    row![
                        icons::icon(Icon::Alert, theme::icon::S, t.accent_text),
                        label(self.t(K::WsOwnShort), size::CAPTION, t.muted)
                    ]
                    .spacing(sp::S)
                    .align_y(Alignment::Center),
                );
            }
        }
        actions = actions.push(widgets::ghost_button(
            t,
            Some(Icon::External),
            self.t(K::WsOpenBrowser),
            Some(Message::Open(ws::web_page(&it.id))),
        ));
        col = col.push(actions);

        // Tags: a click filters the Workshop by that genre.
        let genres: Vec<&str> = it.genres().collect();
        let mut tag_row = row![].spacing(sp::XS);
        if let Some(k) = it.kind() {
            tag_row = tag_row.push(chip(
                kind_label(k).get(self.lang),
                self.ws.query.kind == Some(k),
                msg(WsMsg::Kind(Some(k))),
            ));
        }
        if let Some(r) = it.resolution() {
            tag_row = tag_row.push(chip(
                r,
                self.ws.query.resolution.as_deref() == Some(r),
                msg(WsMsg::Resolution(Some(r.to_owned()))),
            ));
        }
        for g in genres {
            let on = self.ws.query.tag.as_deref() == Some(g);
            tag_row = tag_row.push(chip(
                g,
                on,
                msg(WsMsg::Tag(if on { None } else { Some(g.to_owned()) })),
            ));
        }
        col = col.push(tag_row.wrap());

        if !it.description.trim().is_empty() {
            col = col.push(self.ws_description(t, &it.description));
        }
        container(
            scrollable(container(col).padding(Padding::from([sp::L, sp::XL])))
                .style(theme::scroll_style)
                .height(Length::Fill),
        )
        .width(layout::PANEL_W)
        .height(Length::Fill)
        .style(theme::panel)
        .into()
    }

    /// Steam BBCode rendered as simple rich text (no HTML, no images).
    fn ws_description<'a>(&self, t: &'static Tokens, src: &str) -> Element<'a, Message> {
        let blocks = bbcode::parse(src);
        let total = blocks.len();
        let shown = if self.ws.desc_open {
            total
        } else {
            total.min(6)
        };
        let mut col =
            column![strong(self.t(K::Description), size::CAPTION, t.faint)].spacing(sp::S);
        for b in blocks.into_iter().take(shown) {
            let size_ = if b.kind == BlockKind::Heading {
                size::HEADING
            } else {
                size::CAPTION
            };
            let spans: Vec<iced::widget::text::Span<'a, String, iced::Font>> = b
                .spans
                .into_iter()
                .map(|s| {
                    let font = match (s.bold || b.kind == BlockKind::Heading, s.italic) {
                        (true, _) => theme::semibold(),
                        (false, true) => iced::Font {
                            style: iced::font::Style::Italic,
                            ..theme::regular()
                        },
                        _ => theme::regular(),
                    };
                    let color = if s.link.is_some() {
                        t.accent_text
                    } else if s.spoiler {
                        t.faint
                    } else if b.kind == BlockKind::Heading {
                        t.text
                    } else {
                        t.muted
                    };
                    span(s.text)
                        .font(font)
                        .size(size_)
                        .color(color)
                        .underline(s.underline || s.link.is_some())
                        .strikethrough(s.strike)
                        .link_maybe(s.link)
                })
                .collect();
            let text: Element<'a, Message> = rich_text(spans).on_link_click(Message::Open).into();
            let el: Element<'a, Message> = match b.kind {
                BlockKind::Bullet => row![label("•", size::CAPTION, t.accent_text), text]
                    .spacing(sp::S)
                    .into(),
                BlockKind::Quote | BlockKind::Code => container(text)
                    .padding(Padding::from([sp::S, sp::M]))
                    .width(Length::Fill)
                    .style(theme::notice(theme::Notice::Info))
                    .into(),
                BlockKind::Rule => widgets::divider(t),
                _ => text,
            };
            col = col.push(el);
        }
        if total > 6 {
            let more = if self.ws.desc_open {
                self.t(K::LessText)
            } else {
                self.t(K::MoreText)
            };
            col = col.push(widgets::ghost_button(
                t,
                None,
                more,
                Some(msg(WsMsg::ToggleDescription)),
            ));
        }
        col.into()
    }
}
