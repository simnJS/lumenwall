//! Settings page: engine settings (through the daemon) and app preferences,
//! grouped in cards.

use iced::widget::{column, container, row, scrollable, slider, space, text_input, toggler};
use iced::{Alignment, Element, Length, Padding};
use lumenwall_ipc::Settings;

use super::widgets::{self, chip, label, setting_row, strong};
use crate::app::{App, Message};
use crate::config::{LangPref, PreviewMode, ThemePref};
use crate::i18n::K;
use crate::icons::{self, Icon};
use crate::theme::{self, Tokens, layout, size, space as sp};

impl App {
    pub(super) fn settings_view(&self, t: &'static Tokens) -> Element<'_, Message> {
        let header = column![
            strong(self.t(K::NavSettings), size::DISPLAY, t.text),
            label(self.t(K::SettingsSubtitle), size::BODY, t.muted)
        ]
        .spacing(2);
        let body = column![
            header,
            self.engine_section(t),
            self.audio_section(t),
            self.startup_section(t),
            self.install_section(t),
            self.workshop_section(t),
            self.library_section(t),
            self.appearance_section(t),
            self.updates_section(t),
            self.about_section(t),
            space().height(sp::XL),
        ]
        .spacing(sp::XL)
        .max_width(780);
        scrollable(container(body).padding(layout::PAGE_PAD))
            .style(theme::scroll_style)
            .height(Length::Fill)
            .into()
    }

    fn toggle_row<'a>(
        &self,
        t: &'static Tokens,
        title: &'a str,
        caption: Option<&'a str>,
        on: bool,
        set: Option<impl Fn(bool) -> Message + 'a>,
    ) -> Element<'a, Message> {
        let tg = toggler(on)
            .size(22)
            .style(theme::toggler_style)
            .on_toggle_maybe(set);
        setting_row(t, title, caption, tg)
    }

    fn engine_section(&self, t: &'static Tokens) -> Element<'_, Message> {
        let Some(s) = self.daemon.settings.clone() else {
            return widgets::section(
                t,
                self.t(K::SecPerformance),
                widgets::notice(
                    t,
                    theme::Notice::Info,
                    self.t(K::EngineStopped),
                    self.t(K::EngineSettingsOffline),
                    None,
                ),
            );
        };
        let with = |f: fn(&mut Settings, bool)| {
            let s = s.clone();
            move |b: bool| {
                let mut n = s.clone();
                f(&mut n, b);
                Message::EngineSettings(n)
            }
        };
        let fps = s.fps_limit;
        let s2 = s.clone();
        let fps_control = row![
            slider(5.0..=144.0, fps as f32, move |v| Message::EngineSettings(
                Settings {
                    fps_limit: v.round() as u32,
                    ..s2.clone()
                }
            ))
            .width(200)
            .style(theme::slider_style),
            strong(format!("{fps} fps"), size::BODY, t.accent_text).width(64),
        ]
        .spacing(sp::M)
        .align_y(Alignment::Center);
        let presets = row([15u32, 30, 60].map(|p| {
            let n = Settings {
                fps_limit: p,
                ..s.clone()
            };
            chip(
                match p {
                    15 => "15",
                    30 => "30",
                    _ => "60",
                },
                fps == p,
                Message::EngineSettings(n),
            )
        }))
        .spacing(sp::XS);
        widgets::section(
            t,
            self.t(K::SecPerformance),
            column![
                setting_row(
                    t,
                    self.t(K::FpsLimit),
                    Some(self.t(K::FpsHelp)),
                    fps_control
                ),
                row![space().width(Length::Fill), presets],
                widgets::divider(t),
                self.toggle_row(
                    t,
                    self.t(K::PauseFullscreen),
                    None,
                    s.pause_on_fullscreen,
                    Some(with(|s, b| s.pause_on_fullscreen = b))
                ),
                self.toggle_row(
                    t,
                    self.t(K::PauseBattery),
                    None,
                    s.pause_on_battery,
                    Some(with(|s, b| s.pause_on_battery = b))
                ),
                self.toggle_row(
                    t,
                    self.t(K::PauseLock),
                    None,
                    s.pause_on_lock,
                    Some(with(|s, b| s.pause_on_lock = b))
                ),
            ]
            .spacing(sp::M),
        )
    }

    fn audio_section(&self, t: &'static Tokens) -> Element<'_, Message> {
        let Some(s) = self.daemon.settings.clone() else {
            return space().into();
        };
        let s2 = s.clone();
        let vol = row![
            slider(0.0..=1.0, s.volume, move |v| Message::EngineSettings(
                Settings {
                    volume: v,
                    ..s2.clone()
                }
            ))
            .step(0.01_f32)
            .width(200)
            .style(theme::slider_style),
            strong(
                format!("{:.0} %", s.volume * 100.0),
                size::BODY,
                t.accent_text
            )
            .width(64),
        ]
        .spacing(sp::M)
        .align_y(Alignment::Center);
        let with = |f: fn(&mut Settings, bool)| {
            let s = s.clone();
            move |b: bool| {
                let mut n = s.clone();
                f(&mut n, b);
                Message::EngineSettings(n)
            }
        };
        widgets::section(
            t,
            self.t(K::SecAudio),
            column![
                setting_row(t, self.t(K::Volume), None, vol),
                self.toggle_row(
                    t,
                    self.t(K::Muted),
                    None,
                    s.muted,
                    Some(with(|s, b| s.muted = b))
                ),
                self.toggle_row(
                    t,
                    self.t(K::Automute),
                    Some(self.t(K::AutomuteHint)),
                    s.automute,
                    Some(with(|s, b| s.automute = b))
                ),
            ]
            .spacing(sp::M),
        )
    }

    fn startup_section(&self, t: &'static Tokens) -> Element<'_, Message> {
        let caption = cfg!(target_os = "linux").then(|| self.t(K::TrayHint));
        widgets::section(
            t,
            self.t(K::SecStartup),
            self.toggle_row(
                t,
                self.t(K::Autostart),
                caption,
                self.autostart,
                Some(Message::Autostart),
            ),
        )
    }

    fn library_section(&self, t: &'static Tokens) -> Element<'_, Message> {
        let mut col = column![strong(self.t(K::Folders), size::BODY, t.text)].spacing(sp::M);
        if self.cfg.folders.is_empty() {
            col = col.push(label(self.t(K::NoFolders), size::CAPTION, t.muted));
        }
        for (i, f) in self.cfg.folders.iter().enumerate() {
            let mut r = row![
                icons::icon(Icon::Folder, theme::icon::M, t.muted),
                label(widgets::tilde(f), size::CAPTION, t.text).width(Length::Fill)
            ]
            .spacing(sp::S)
            .align_y(Alignment::Center);
            if !f.is_dir() {
                r = r.push(label(self.t(K::FolderMissing), size::CAPTION, t.danger));
            }
            r = r.push(widgets::ghost_button(
                t,
                Some(Icon::Close),
                self.t(K::Remove),
                Some(Message::RemoveFolder(i)),
            ));
            col = col.push(r);
        }
        col = col.push(
            row![
                text_input(self.t(K::FolderPath), &self.folder_input)
                    .on_input(Message::FolderInput)
                    .on_submit(Message::AddTypedFolder)
                    .padding(Padding::from([7.0, 10.0]))
                    .size(size::BODY)
                    .style(theme::search),
                widgets::secondary_button(
                    t,
                    Some(Icon::Plus),
                    self.t(K::Add),
                    (!self.folder_input.trim().is_empty()).then_some(Message::AddTypedFolder)
                ),
                widgets::secondary_button(
                    t,
                    Some(Icon::Folder),
                    self.t(K::Browse),
                    Some(Message::PickFolder)
                ),
            ]
            .spacing(sp::S)
            .align_y(Alignment::Center),
        );
        col = col.push(widgets::divider(t));
        col = col.push(strong(self.t(K::Detected), size::BODY, t.text));
        if let Some(lib) = &self.library {
            let line = |name: &'static str, value: Option<String>| -> Element<'_, Message> {
                let (dot, text) = match value {
                    Some(v) => (t.success, v),
                    None => (t.danger, self.t(K::NotFound).to_owned()),
                };
                row![
                    container(space())
                        .width(8)
                        .height(8)
                        .style(move |_| container::Style {
                            background: Some(dot.into()),
                            border: iced::Border {
                                radius: 4.0.into(),
                                ..Default::default()
                            },
                            ..Default::default()
                        }),
                    label(name, size::CAPTION, t.muted).width(150),
                    label(text, size::CAPTION, t.text).width(Length::Fill),
                ]
                .spacing(sp::S)
                .align_y(Alignment::Center)
                .into()
            };
            let libs: Vec<String> = lib
                .steam
                .libraries
                .iter()
                .map(|l| widgets::tilde(l))
                .collect();
            let ws: Vec<String> = lib
                .steam
                .workshop_dirs
                .iter()
                .map(|l| widgets::tilde(l))
                .collect();
            col = col
                .push(line(
                    self.t(K::SteamLibraries),
                    (!libs.is_empty()).then(|| libs.join("\n")),
                ))
                .push(line(
                    "Workshop 431960",
                    (!ws.is_empty()).then(|| ws.join("\n")),
                ))
                .push(line(
                    "Wallpaper Engine",
                    lib.steam.we_assets().map(|p| widgets::tilde(&p)),
                ));
        }
        col = col.push(row![
            space().width(Length::Fill),
            widgets::secondary_button(
                t,
                Some(Icon::Refresh),
                self.t(K::Rescan),
                Some(Message::Rescan)
            )
        ]);
        widgets::section(t, self.t(K::SecLibrary), col)
    }

    fn workshop_section(&self, t: &'static Tokens) -> Element<'_, Message> {
        use crate::workshop::state::WsMsg;
        let ws_msg = |m: WsMsg| Message::Ws(m);
        let mut col = column![
            row![
                icons::icon(Icon::Key, theme::icon::L, t.accent_text),
                strong(self.t(K::ApiKey), size::BODY, t.text),
                space().width(Length::Fill),
                widgets::ghost_button(
                    t,
                    Some(Icon::External),
                    self.t(K::WsGetKey),
                    Some(Message::Open(crate::workshop::API_KEY_PAGE.into()))
                ),
            ]
            .spacing(sp::S)
            .align_y(Alignment::Center),
            label(self.t(K::ApiKeyHelp), size::CAPTION, t.muted),
        ]
        .spacing(sp::M);
        match &self.ws.key {
            Some(k) => {
                col = col.push(
                    row![
                        icons::icon(Icon::Check, theme::icon::M, t.success),
                        label(
                            self.t(K::KeyStored)
                                .replace("{k}", &crate::workshop::key::masked(k)),
                            size::BODY,
                            t.text
                        )
                        .width(Length::Fill),
                        widgets::secondary_button(
                            t,
                            Some(Icon::Close),
                            self.t(K::Remove),
                            Some(ws_msg(WsMsg::RemoveKey))
                        ),
                    ]
                    .spacing(sp::S)
                    .align_y(Alignment::Center),
                );
            }
            None => {
                let valid = crate::workshop::key::is_valid(self.ws.key_input.trim());
                col = col.push(
                    row![
                        text_input(self.t(K::ApiKeyPlaceholder), &self.ws.key_input)
                            .secure(!self.ws.key_visible)
                            .on_input(|s| Message::Ws(WsMsg::KeyInput(
                                crate::workshop::state::Secret(s)
                            )))
                            .on_submit(ws_msg(WsMsg::SaveKey))
                            .padding(Padding::from([7.0, 10.0]))
                            .size(size::BODY)
                            .style(theme::search),
                        widgets::ghost_button(
                            t,
                            Some(Icon::Eye),
                            self.t(if self.ws.key_visible {
                                K::HideKey
                            } else {
                                K::ShowKey
                            }),
                            Some(ws_msg(WsMsg::ToggleKeyVisible))
                        ),
                        widgets::secondary_button(
                            t,
                            Some(Icon::Check),
                            self.t(K::Save),
                            valid.then(|| ws_msg(WsMsg::SaveKey))
                        ),
                    ]
                    .spacing(sp::S)
                    .align_y(Alignment::Center),
                );
            }
        }
        col = col.push(widgets::divider(t));
        col = col.push(self.toggle_row(
            t,
            self.t(K::ShowQuestionable),
            Some(self.t(K::ContentHelp)),
            self.cfg.workshop_questionable,
            Some(move |b| Message::Ws(WsMsg::Questionable(b))),
        ));
        col = col.push(self.toggle_row(
            t,
            self.t(K::ShowMature),
            None,
            self.cfg.workshop_mature,
            Some(move |b| Message::Ws(WsMsg::Mature(b))),
        ));
        widgets::section(t, self.t(K::SecWorkshop), col)
    }

    fn appearance_section(&self, t: &'static Tokens) -> Element<'_, Message> {
        let lang = row![
            chip(
                self.t(K::LangSystem),
                self.cfg.language == LangPref::System,
                Message::Lang(LangPref::System)
            ),
            chip(
                "English",
                self.cfg.language == LangPref::En,
                Message::Lang(LangPref::En)
            ),
            chip(
                "Français",
                self.cfg.language == LangPref::Fr,
                Message::Lang(LangPref::Fr)
            ),
        ]
        .spacing(sp::XS);
        let theme_row = row![
            chip(
                self.t(K::LangSystem),
                self.cfg.theme == ThemePref::System,
                Message::ThemePref(ThemePref::System)
            ),
            chip(
                self.t(K::ThemeDark),
                self.cfg.theme == ThemePref::Dark,
                Message::ThemePref(ThemePref::Dark)
            ),
            chip(
                self.t(K::ThemeLight),
                self.cfg.theme == ThemePref::Light,
                Message::ThemePref(ThemePref::Light)
            ),
        ]
        .spacing(sp::XS);
        let preview = row![
            chip(
                self.t(K::PreviewHover),
                self.cfg.preview_mode == PreviewMode::Hover,
                Message::Preview(PreviewMode::Hover)
            ),
            chip(
                self.t(K::PreviewVisible),
                self.cfg.preview_mode == PreviewMode::Visible,
                Message::Preview(PreviewMode::Visible)
            ),
            chip(
                self.t(K::PreviewNever),
                self.cfg.preview_mode == PreviewMode::Never,
                Message::Preview(PreviewMode::Never)
            ),
        ]
        .spacing(sp::XS);
        widgets::section(
            t,
            self.t(K::SecAppearance),
            column![
                setting_row(t, self.t(K::Language), None, lang),
                setting_row(t, self.t(K::Theme), None, theme_row),
                setting_row(
                    t,
                    self.t(K::PreviewAnim),
                    Some(self.t(K::PreviewAnimHelp)),
                    preview
                ),
            ]
            .spacing(sp::M),
        )
    }

    fn about_section(&self, t: &'static Tokens) -> Element<'_, Message> {
        let mut text = column![label(
            self.t(K::AboutText)
                .replace("{v}", env!("CARGO_PKG_VERSION")),
            size::BODY,
            t.text
        )]
        .spacing(sp::XS)
        .width(Length::Fill);
        if let crate::daemon::Link::Online(info) = &self.daemon.link {
            text = text.push(label(
                format!("{} {} · IPC v{}", info.server, info.version, info.protocol),
                size::CAPTION,
                t.faint,
            ));
        }
        // The horizontal logo (its clear space is part of the image).
        let col = row![text, icons::logo(t, 168.0)]
            .spacing(sp::L)
            .align_y(Alignment::Center);
        widgets::section(t, self.t(K::SecAbout), col)
    }
}
