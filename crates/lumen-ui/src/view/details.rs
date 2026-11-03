//! Details panel: big preview, metadata, Apply (screen map, scaling, primary
//! button) and Customize (user properties, live on the desktop through the daemon).

use iced::widget::{
    button, column, container, pick_list, row, scrollable, slider, space, text_input, toggler,
};
use iced::{Alignment, Color, Element, Length, Padding};
use lumen_ipc::{PropertyMap, PropertyValue, Scaling};

use super::library::{kind_label, res_label};
use super::widgets::{self, MapMode, chip, label, strong};
use crate::app::{App, Message};
use crate::catalog::{Blocker, Kind, Origin, PropertyDef, PropertyKind, ResClass, Wallpaper};
use crate::i18n::K;
use crate::icons::{self, Icon};
use crate::theme::{self, Tokens, layout, radius, size, space as sp};

#[derive(Debug, Clone, PartialEq)]
struct ComboChoice {
    index: usize,
    text: String,
}

impl std::fmt::Display for ComboChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.text)
    }
}

impl App {
    pub(super) fn details(&self, t: &'static Tokens) -> Element<'_, Message> {
        let Some(w) = self.selected_wallpaper() else {
            return container(space())
                .style(theme::panel)
                .height(Length::Fill)
                .into();
        };
        let inner_w = layout::PANEL_W - 2.0 * sp::XL;
        let head = row![
            label(self.t(K::Details), size::CAPTION, t.faint),
            space().width(Length::Fill),
            widgets::icon_button(
                t,
                Icon::Close,
                self.t(K::Close),
                false,
                Some(Message::ClosePanel)
            ),
        ]
        .align_y(Alignment::Center);
        let preview = widgets::preview(
            self,
            w.preview.as_deref(),
            inner_w,
            inner_w * 9.0 / 16.0,
            radius::L,
        );

        let mut meta = vec![kind_label(w.project.kind).get(self.lang).to_owned()];
        meta.push(match w.resolution {
            Some((rw, rh)) => format!("{rw}×{rh}"),
            None => res_label(ResClass::Unknown).get(self.lang).to_owned(),
        });
        meta.push(
            match w.origin {
                Origin::Workshop => self.t(K::OriginWorkshop),
                Origin::WallpaperEngine => self.t(K::OriginWe),
                Origin::Local => self.t(K::OriginLocal),
            }
            .to_owned(),
        );
        let mut info = column![
            strong(w.title(), size::TITLE, t.text),
            label(meta.join(" · "), size::CAPTION, t.muted)
        ]
        .spacing(sp::XS);
        if !w.project.tags.is_empty() {
            let tags = row(w.project.tags.iter().map(|tag| {
                let on = self.cfg.filter_tag.as_deref() == Some(tag.as_str());
                chip(
                    tag.as_str(),
                    on,
                    Message::FilterTag(if on { None } else { Some(tag.clone()) }),
                )
            }))
            .spacing(sp::XS)
            .wrap();
            info = info.push(container(tags).padding(Padding::default().top(sp::XS)));
        }

        let mut col = column![head, preview, info].spacing(sp::L);
        match w.blocker(self.has_we_assets()) {
            Some(Blocker::NeedsWallpaperEngine) => {
                col = col.push(widgets::notice(
                    t,
                    theme::Notice::Warning,
                    self.t(K::NoWeTitle),
                    self.t(K::NoWeBody),
                    None,
                ));
            }
            Some(Blocker::Unsupported) => {
                let body = if w.project.kind == Kind::Application {
                    self.t(K::UnsupportedApp)
                } else {
                    self.t(K::UnsupportedUnknown)
                };
                col = col.push(widgets::notice(
                    t,
                    theme::Notice::Danger,
                    self.t(K::Unsupported),
                    body,
                    None,
                ));
            }
            None => {}
        }
        col = col.push(widgets::section(
            t,
            self.t(K::Apply),
            self.apply_block(t, w, inner_w - 2.0 * sp::L),
        ));
        col = col.push(widgets::section(
            t,
            self.t(K::Customize),
            self.properties_block(t, w),
        ));

        let mut actions = row![widgets::ghost_button(
            t,
            Some(Icon::Folder),
            self.t(K::OpenFolder),
            Some(Message::Open(w.dir.to_string_lossy().into_owned()))
        )]
        .spacing(sp::XS);
        if let Some(id) = &w.project.workshop_id {
            actions = actions.push(widgets::ghost_button(
                t,
                Some(Icon::External),
                self.t(K::OpenInSteam),
                Some(Message::Open(format!("steam://url/CommunityFilePage/{id}"))),
            ));
        }
        col = col.push(actions);
        let desc = w.project.description.trim();
        if !desc.is_empty() {
            let full = desc.chars().count() > 240;
            let shown: String = if self.desc_open || !full {
                desc.chars().take(4000).collect()
            } else {
                format!("{}…", desc.chars().take(240).collect::<String>())
            };
            let mut d = column![
                strong(self.t(K::Description), size::CAPTION, t.faint),
                label(shown, size::CAPTION, t.muted)
            ]
            .spacing(sp::XS);
            if full {
                let more = if self.desc_open {
                    self.t(K::LessText)
                } else {
                    self.t(K::MoreText)
                };
                d = d.push(widgets::ghost_button(
                    t,
                    None,
                    more,
                    Some(Message::ToggleDescription),
                ));
            }
            col = col.push(d);
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

    fn apply_block(&self, t: &'static Tokens, w: &Wallpaper, width: f32) -> Element<'_, Message> {
        let mut col = column![].spacing(sp::M);
        if self.daemon.is_online() && !self.daemon.outputs.is_empty() {
            col = col.push(widgets::monitor_map(self, t, width, 120.0, MapMode::Apply));
            if self.daemon.outputs.len() > 1 {
                col = col.push(
                    row![
                        chip(
                            self.t(K::AllScreens),
                            self.targets.is_empty(),
                            Message::AllScreens
                        ),
                        label(self.t(K::MultiSelectHint), size::CAPTION, t.faint),
                    ]
                    .spacing(sp::S)
                    .align_y(Alignment::Center),
                );
            }
        }
        let seg = |s: Scaling, k: K, help: K| -> Element<'_, Message> {
            iced::widget::tooltip(
                chip(self.t(k), self.cfg.scaling == s, Message::SetScaling(s)),
                container(label(self.t(help), size::CAPTION, t.text))
                    .padding([4, 8])
                    .style(theme::card),
                iced::widget::tooltip::Position::Top,
            )
            .into()
        };
        col = col.push(
            row![
                label(self.t(K::Scaling), size::CAPTION, t.muted),
                space().width(Length::Fill),
                seg(Scaling::Fill, K::Fill, K::FillHelp),
                seg(Scaling::Fit, K::Fit, K::FitHelp),
                seg(Scaling::Stretch, K::Stretch, K::StretchHelp),
            ]
            .spacing(sp::XS)
            .align_y(Alignment::Center),
        );
        let reason = if !self.daemon.is_online() {
            Some(self.t(K::NeedEngine))
        } else {
            match w.blocker(self.has_we_assets()) {
                Some(Blocker::NeedsWallpaperEngine) => Some(self.t(K::NeedsWe)),
                Some(Blocker::Unsupported) => Some(self.t(K::Unsupported)),
                None => None,
            }
        };
        let on_screens = self.screen_numbers(&w.dir);
        let label_text = if !on_screens.is_empty() && reason.is_none() {
            self.t(K::ApplyAgain)
        } else {
            self.t(K::Apply)
        };
        col = col.push(widgets::primary_wide(
            t,
            Some(Icon::Check),
            label_text,
            reason.is_none().then_some(Message::Apply),
        ));
        col = col.push(label(
            reason.unwrap_or(self.t(K::DragHint)),
            size::CAPTION,
            t.faint,
        ));
        col.into()
    }

    fn properties_block(&self, t: &'static Tokens, w: &Wallpaper) -> Element<'_, Message> {
        let defs: Vec<&PropertyDef> = w.project.properties.iter().collect();
        if defs.is_empty() {
            return label(self.t(K::NoProperties), size::BODY, t.muted).into();
        }
        let showing = self.outputs_showing(&w.dir);
        let live = !showing.is_empty();
        // Current values: defaults, then what the daemon reports, then local edits.
        let mut values: PropertyMap = defs
            .iter()
            .filter_map(|d| d.default.clone().map(|v| (d.key.clone(), v)))
            .collect();
        if let Some(first) = showing.first()
            && let Some(remote) = self.daemon.properties.get(first)
        {
            values.extend(remote.clone());
        }
        if let Some(local) = self.edits.get(&w.dir) {
            values.extend(local.clone());
        }
        let mut col = column![
            row![
                icons::icon(
                    if live { Icon::Sparkle } else { Icon::Check },
                    theme::icon::S,
                    if live { t.accent_text } else { t.faint }
                ),
                label(
                    if live {
                        self.t(K::LiveHint)
                    } else {
                        self.t(K::NotAppliedHint)
                    },
                    size::CAPTION,
                    t.muted
                ),
            ]
            .spacing(sp::S)
            .align_y(Alignment::Center)
        ]
        .spacing(sp::M);
        for def in defs {
            let name = if def.label == "scheme color" {
                self.t(K::SchemeColor).to_owned()
            } else {
                def.label.clone()
            };
            if matches!(def.kind, PropertyKind::Label) {
                col = col.push(strong(name, size::CAPTION, t.faint));
                continue;
            }
            col = col.push(self.property_row(t, def, name, values.get(&def.key)));
        }
        col = col.push(row![
            space().width(Length::Fill),
            widgets::ghost_button(
                t,
                Some(Icon::Refresh),
                self.t(K::Reset),
                Some(Message::ResetProps)
            )
        ]);
        col.into()
    }

    fn property_row(
        &self,
        t: &'static Tokens,
        def: &PropertyDef,
        name: String,
        value: Option<&PropertyValue>,
    ) -> Element<'_, Message> {
        let key = def.key.clone();
        let title = label(name, size::BODY, t.text).width(Length::Fill);
        match &def.kind {
            PropertyKind::Bool => {
                let on = matches!(value, Some(PropertyValue::Bool(true)));
                row![
                    title,
                    toggler(on)
                        .on_toggle(move |b| Message::Prop(key.clone(), PropertyValue::Bool(b)))
                        .size(20)
                        .style(theme::toggler_style)
                ]
                .align_y(Alignment::Center)
                .into()
            }
            PropertyKind::Slider {
                min,
                max,
                step,
                integer,
            } => {
                let v = match value {
                    Some(PropertyValue::Number(n)) => *n,
                    _ => *min,
                };
                let step = if *step > 0.0 {
                    *step
                } else {
                    (max - min) / 100.0
                };
                let shown = if *integer {
                    format!("{v:.0}")
                } else {
                    format!("{v:.2}")
                };
                column![
                    row![title, strong(shown, size::CAPTION, t.accent_text)]
                        .align_y(Alignment::Center),
                    slider(*min..=*max, v, move |n| Message::Prop(
                        key.clone(),
                        PropertyValue::Number(n)
                    ))
                    .step(step)
                    .style(theme::slider_style),
                ]
                .spacing(sp::XS)
                .into()
            }
            PropertyKind::Color => {
                let c = match value {
                    Some(PropertyValue::Color(c)) => *c,
                    _ => [1.0, 1.0, 1.0],
                };
                let open = self.color_open.as_deref() == Some(def.key.as_str());
                let swatch = button(
                    container(space())
                        .width(36)
                        .height(22)
                        .style(theme::swatch(Color::from_rgb(c[0], c[1], c[2]))),
                )
                .padding(0)
                .style(theme::bare)
                .on_press(Message::ColorEditor(if open {
                    None
                } else {
                    Some(def.key.clone())
                }));
                let mut col =
                    column![row![title, swatch].align_y(Alignment::Center)].spacing(sp::S);
                if open {
                    col = col.push(color_editor(t, def.key.clone(), c));
                }
                col.into()
            }
            PropertyKind::Combo { options } => {
                let choices: Vec<ComboChoice> = options
                    .iter()
                    .enumerate()
                    .map(|(index, (l, _))| ComboChoice {
                        index,
                        text: l.clone(),
                    })
                    .collect();
                let current = options
                    .iter()
                    .position(|(_, v)| Some(v) == value)
                    .and_then(|i| choices.get(i).cloned());
                let opts = options.clone();
                row![
                    title,
                    pick_list(choices, current, move |c: ComboChoice| Message::Prop(
                        key.clone(),
                        opts[c.index].1.clone()
                    ))
                    .text_size(size::CAPTION)
                    .padding(Padding::from([5.0, 12.0]))
                    .style(theme::pick_list_style)
                ]
                .align_y(Alignment::Center)
                .into()
            }
            PropertyKind::Text => {
                let s = match value {
                    Some(PropertyValue::Text(s)) => s.clone(),
                    Some(PropertyValue::Number(n)) => n.to_string(),
                    _ => String::new(),
                };
                column![
                    title,
                    text_input("", &s)
                        .on_input(move |v| Message::Prop(key.clone(), PropertyValue::Text(v)))
                        .padding(Padding::from([7.0, 10.0]))
                        .size(size::BODY)
                        .style(theme::search)
                ]
                .spacing(sp::XS)
                .into()
            }
            PropertyKind::File | PropertyKind::Directory => {
                let current = match value {
                    Some(PropertyValue::Text(s)) if !s.is_empty() => std::path::Path::new(s)
                        .file_name()
                        .map_or(s.clone(), |n| n.to_string_lossy().into_owned()),
                    _ => self.t(K::NoneValue).to_owned(),
                };
                let dir = matches!(def.kind, PropertyKind::Directory);
                row![
                    column![title, label(current, size::CAPTION, t.muted)]
                        .spacing(2)
                        .width(Length::Fill),
                    widgets::secondary_button(
                        t,
                        Some(Icon::Folder),
                        self.t(K::Browse),
                        Some(Message::PickProp(def.key.clone(), dir))
                    ),
                ]
                .align_y(Alignment::Center)
                .into()
            }
            PropertyKind::Label => space().into(),
        }
    }
}

/// Inline colour editor: presets, RGB sliders and a hex field.
fn color_editor<'a>(t: &'static Tokens, key: String, c: [f32; 3]) -> Element<'a, Message> {
    const PRESETS: [[f32; 3]; 8] = [
        [1.0, 1.0, 1.0],
        [0.12, 0.12, 0.14],
        [1.0, 0.70, 0.28],
        [0.94, 0.33, 0.31],
        [0.85, 0.36, 0.78],
        [0.40, 0.48, 0.98],
        [0.22, 0.78, 0.88],
        [0.36, 0.84, 0.55],
    ];
    let presets = row(PRESETS.iter().map(|p| {
        let k = key.clone();
        let p = *p;
        button(
            container(space())
                .width(22)
                .height(22)
                .style(theme::swatch(Color::from_rgb(p[0], p[1], p[2]))),
        )
        .padding(0)
        .style(theme::bare)
        .on_press(Message::Prop(k, PropertyValue::Color(p)))
        .into()
    }))
    .spacing(6);
    let channel = |i: usize, name: &'static str| -> Element<'a, Message> {
        let k = key.clone();
        row![
            label(name, size::CAPTION, t.faint).width(14),
            slider(0.0..=1.0, c[i], move |v| {
                let mut n = c;
                n[i] = v;
                Message::Prop(k.clone(), PropertyValue::Color(n))
            })
            .step(1.0_f32 / 255.0)
            .style(theme::slider_style),
            label(format!("{:.0}", c[i] * 255.0), size::CAPTION, t.muted).width(30),
        ]
        .spacing(sp::S)
        .align_y(Alignment::Center)
        .into()
    };
    let hex = format!(
        "#{:02x}{:02x}{:02x}",
        (c[0] * 255.0).round() as u8,
        (c[1] * 255.0).round() as u8,
        (c[2] * 255.0).round() as u8
    );
    let k = key.clone();
    let hex_input = text_input("#rrggbb", &hex)
        .on_input(move |s| match parse_hex(&s) {
            Some(n) => Message::Prop(k.clone(), PropertyValue::Color(n)),
            None => Message::Noop,
        })
        .width(96)
        .padding(Padding::from([5.0, 8.0]))
        .size(size::CAPTION)
        .style(theme::search);
    container(
        column![
            presets,
            channel(0, "R"),
            channel(1, "G"),
            channel(2, "B"),
            row![label("Hex", size::CAPTION, t.faint), hex_input]
                .spacing(sp::S)
                .align_y(Alignment::Center)
        ]
        .spacing(sp::S),
    )
    .padding(sp::M)
    .style(theme::preview_placeholder)
    .into()
}

fn parse_hex(s: &str) -> Option<[f32; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some([
        ((v >> 16) & 255) as f32 / 255.0,
        ((v >> 8) & 255) as f32 / 255.0,
        (v & 255) as f32 / 255.0,
    ])
}

#[cfg(test)]
mod tests {
    #[test]
    fn hex_colors() {
        assert_eq!(super::parse_hex("#ff8000"), Some([1.0, 128.0 / 255.0, 0.0]));
        assert_eq!(super::parse_hex("12345"), None);
    }
}
