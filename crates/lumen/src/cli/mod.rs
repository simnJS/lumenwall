//! Command line (spec 07 §2).
//!
//! Arguments are walked in order because several options apply to "the last designated
//! output" (§2.3). Unknown arguments are ignored (§2.1). The option table and the help text
//! come from the clap schema in [`schema`].

mod os;
mod schema;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::sync::OnceLock;

use clap::CommandFactory;

use crate::settings::{
    Clamp, HwDecode, Layer, RenderDebug, Scaling, WallpaperRef, WallpaperSource, WindowGeometry,
    clamp_volume, parse_value_enum, validate_fps, validate_screenshot_delay,
};

/// What the command line asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum CliOutcome {
    /// `-h`/`--help`: print this text on stdout, exit 0.
    Help(String),
    /// `--version` (Lumen extension): print this text on stdout, exit 0.
    Version(String),
    /// Run with these arguments.
    Run(Box<CliArgs>),
}

/// Parsed command line, before merging with the configuration file and resolving wallpapers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CliArgs {
    /// All raw arguments, `argv[0]` included (for the "running with" line, §1.1).
    pub raw: Vec<OsString>,
    /// Unknown arguments that were ignored (§2.1), for a debug log.
    pub ignored: Vec<String>,
    pub layout: CliLayout,
    pub settings: CliSettings,
    /// Every wallpaper and playlist named on the command line, in order. All are resolved,
    /// even when a later option overrides them, so that a typo is always fatal (§3.1).
    pub mentioned: Vec<WallpaperRef>,
    pub config: ConfigChoice,
}

/// Where the configuration file comes from (Lumen extension).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ConfigChoice {
    /// The default path, if the file exists.
    #[default]
    Default,
    /// `--config FILE`: the file must exist.
    File(PathBuf),
    /// `--no-config`.
    Disabled,
}

/// Output mode selected by the command line (§4.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CliMode {
    /// No `-w`, `-r` nor `--screen-span`: normal window, unless the configuration file
    /// describes a layout.
    #[default]
    Unset,
    /// `-w XxYxWxH`.
    Window(WindowGeometry),
    /// At least one `-r` or `--screen-span`.
    Desktop,
}

/// Per-target values; `None` means "inherit" (§4.3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TargetSettings {
    pub wallpaper: Option<WallpaperRef>,
    pub playlist: Option<String>,
    /// Copied from the global value when the target is named, then overridden by a
    /// targeted `--scaling` (§2.3).
    pub scaling: Option<Scaling>,
    pub clamp: Option<Clamp>,
}

/// `-r NAME`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliScreen {
    pub name: String,
    pub settings: TargetSettings,
}

/// `--screen-span A,B`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CliSpan {
    /// Synthetic key `span:` + raw argument (§2.3), e.g. `span:A,B`.
    pub key: String,
    /// Output names in the given order, empty items dropped.
    pub outputs: Vec<String>,
    pub settings: TargetSettings,
}

/// Outputs and wallpapers as designated on the command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CliLayout {
    pub mode: CliMode,
    /// In command-line order.
    pub screens: Vec<CliScreen>,
    /// In command-line order.
    pub spans: Vec<CliSpan>,
    /// Last of {positional, `--bg`}, or the first playlist item (§2.3).
    pub default_wallpaper: Option<WallpaperRef>,
    /// `--playlist` given before any target.
    pub window_playlist: Option<String>,
    /// `--scaling` given while no target was designated.
    pub global_scaling: Option<Scaling>,
    pub global_clamp: Option<Clamp>,
}

impl CliLayout {
    /// True when the command line chose the default wallpaper or the window playlist.
    pub fn has_default_content(&self) -> bool {
        self.default_wallpaper.is_some() || self.window_playlist.is_some()
    }
}

/// Global settings; `None`/`false` means "not given" so the configuration file can supply
/// the value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CliSettings {
    pub layer: Option<Layer>,
    pub fps: Option<u32>,
    pub hwdec: Option<HwDecode>,
    pub no_fullscreen_pause: bool,
    pub fullscreen_pause_only_active: bool,
    /// Non-empty patterns only.
    pub fullscreen_pause_ignore_appids: Vec<String>,
    /// Already clamped to `[0, 128]`.
    pub volume: Option<u8>,
    pub silent: bool,
    pub no_automute: bool,
    pub no_audio_processing: bool,
    pub screenshot: Option<PathBuf>,
    /// Already clamped to `[0, 5]`.
    pub screenshot_delay: Option<u32>,
    pub assets_dir: Option<PathBuf>,
    pub disable_particles: bool,
    pub disable_mouse: bool,
    pub disable_parallax: bool,
    pub list_properties: bool,
    /// `(key, value)` in order; a later duplicate key wins.
    pub properties: Vec<(String, String)>,
    pub dump_structure: bool,
    pub render_debug: RenderDebug,
}

/// Fatal command-line error (exit code 1, §1.3).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum CliError {
    #[error("option '{0}' requires a value")]
    MissingValue(String),
    #[error("option '{0}' does not take a value")]
    UnexpectedValue(String),
    #[error("invalid value '{value}' for '{option}': {reason}")]
    InvalidValue {
        option: String,
        value: String,
        reason: String,
    },
    #[error("the value of '{0}' is not valid Unicode")]
    NotUnicode(String),
    #[error(
        "cannot be both a desktop wallpaper and a window: '--window' cannot be combined with \
         '--screen-root' or '--screen-span'"
    )]
    WindowAndDesktop,
    #[error("'--window' can only be given once")]
    WindowTwice,
    #[error("output '{0}' is configured more than once")]
    DuplicateOutput(String),
    #[error("output '{0}' already belongs to a span group")]
    OutputInSpan(String),
    #[error("span group '{0}' needs at least two output names")]
    SpanTooSmall(String),
    #[error("output '{output}' appears twice in span group '{group}'")]
    DuplicateInSpan { group: String, output: String },
    #[error("'--volume' and '--silent' are mutually exclusive")]
    VolumeAndSilent,
    #[error(
        "only one wallpaper can be given without an option (got '{first}' and '{second}'); \
         use --screen-root and --bg for several outputs"
    )]
    MultiplePositionals { first: String, second: String },
}

/// Parses `args` (`argv[0]` first, as from `std::env::args_os()`).
pub fn parse<I, T>(args: I) -> Result<CliOutcome, CliError>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let raw: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let items = tokenize(raw.get(1..).unwrap_or_default());

    // Help and version win over any error, wherever they appear.
    for item in &items {
        match item {
            Ok(Item::Opt {
                kind: OptKind::Help,
                name,
                ..
            }) => {
                let mut command = schema::CliSchema::command();
                let text = if name == "-h" {
                    command.render_help()
                } else {
                    command.render_long_help()
                };
                return Ok(CliOutcome::Help(text.to_string()));
            }
            Ok(Item::Opt {
                kind: OptKind::Version,
                ..
            }) => {
                return Ok(CliOutcome::Version(format!(
                    "lumen {}\n",
                    env!("CARGO_PKG_VERSION")
                )));
            }
            _ => {}
        }
    }

    let mut state = State::default();
    for item in items {
        match item? {
            Item::Ignored(arg) => state.args.ignored.push(arg),
            Item::Positional(value) => state.positional(value)?,
            Item::Opt { kind, name, value } => state.option(kind, &name, value)?,
        }
    }
    if state.args.settings.volume.is_some() && state.args.settings.silent {
        return Err(CliError::VolumeAndSilent);
    }
    state.args.raw = raw;
    Ok(CliOutcome::Run(Box::new(state.args)))
}

/// The command-line help, as printed by `--help`.
pub fn long_help() -> String {
    schema::CliSchema::command().render_long_help().to_string()
}

// --- Tokenizer ----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OptKind {
    Help,
    Version,
    Window,
    ScreenRoot,
    ScreenSpan,
    Bg,
    Playlist,
    Scaling,
    Clamp,
    Layer,
    Fps,
    Hwdec,
    NoFullscreenPause,
    FullscreenPauseOnlyActive,
    FullscreenPauseIgnoreAppid,
    Volume,
    Silent,
    NoAutomute,
    NoAudioProcessing,
    Screenshot,
    ScreenshotDelay,
    AssetsDir,
    DisableParticles,
    DisableMouse,
    DisableParallax,
    ListProperties,
    SetProperty,
    DumpStructure,
    RenderDebug,
    Config,
    NoConfig,
}

impl OptKind {
    /// Maps a schema argument id (the field name) to its handler.
    fn from_id(id: &str) -> Option<Self> {
        Some(match id {
            "help" => Self::Help,
            "version" => Self::Version,
            "window" => Self::Window,
            "screen_root" => Self::ScreenRoot,
            "screen_span" => Self::ScreenSpan,
            "bg" => Self::Bg,
            "playlist" => Self::Playlist,
            "scaling" => Self::Scaling,
            "clamp" => Self::Clamp,
            "layer" => Self::Layer,
            "fps" => Self::Fps,
            "hwdec" => Self::Hwdec,
            "no_fullscreen_pause" => Self::NoFullscreenPause,
            "fullscreen_pause_only_active" => Self::FullscreenPauseOnlyActive,
            "fullscreen_pause_ignore_appid" => Self::FullscreenPauseIgnoreAppid,
            "volume" => Self::Volume,
            "silent" => Self::Silent,
            "noautomute" => Self::NoAutomute,
            "no_audio_processing" => Self::NoAudioProcessing,
            "screenshot" => Self::Screenshot,
            "screenshot_delay" => Self::ScreenshotDelay,
            "assets_dir" => Self::AssetsDir,
            "disable_particles" => Self::DisableParticles,
            "disable_mouse" => Self::DisableMouse,
            "disable_parallax" => Self::DisableParallax,
            "list_properties" => Self::ListProperties,
            "set_property" => Self::SetProperty,
            "dump_structure" => Self::DumpStructure,
            "render_debug" => Self::RenderDebug,
            "config" => Self::Config,
            "no_config" => Self::NoConfig,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct OptSpec {
    kind: OptKind,
    takes_value: bool,
}

#[derive(Debug, Default)]
struct OptionTable {
    long: HashMap<String, OptSpec>,
    short: HashMap<char, OptSpec>,
}

fn option_table() -> &'static OptionTable {
    static TABLE: OnceLock<OptionTable> = OnceLock::new();
    TABLE.get_or_init(build_option_table)
}

fn build_option_table() -> OptionTable {
    let mut command = schema::CliSchema::command();
    command.build();
    let mut table = OptionTable::default();
    for arg in command.get_arguments().filter(|a| !a.is_positional()) {
        let id = arg.get_id().as_str();
        let kind = OptKind::from_id(id).unwrap_or_else(|| panic!("no handler for option '{id}'"));
        let spec = OptSpec {
            kind,
            takes_value: arg.get_action().takes_values(),
        };
        let longs = arg
            .get_long()
            .into_iter()
            .chain(arg.get_all_aliases().unwrap_or_default());
        for long in longs {
            table.long.insert(long.to_owned(), spec);
        }
        let shorts = arg
            .get_short()
            .into_iter()
            .chain(arg.get_all_short_aliases().unwrap_or_default());
        for short in shorts {
            table.short.insert(short, spec);
        }
    }
    table
}

#[derive(Clone, Debug, PartialEq)]
enum Item {
    /// A known option; `name` is the spelling used (`--clamping`, `-r`).
    Opt {
        kind: OptKind,
        name: String,
        value: Option<OsString>,
    },
    Positional(OsString),
    Ignored(String),
}

/// Splits the arguments (without `argv[0]`) into options and positionals, getopt-style:
/// `--name value`, `--name=value`, `-x value`, `-xvalue`, `-x=value`, clustered flags
/// (`-sz`), `--` ends the options. A value-taking option always consumes the next
/// argument, even one starting with `-` (so `-v -5` works).
fn tokenize(args: &[OsString]) -> Vec<Result<Item, CliError>> {
    let table = option_table();
    let mut items = Vec::with_capacity(args.len());
    let mut rest = args.iter();
    let mut only_positionals = false;
    while let Some(arg) = rest.next() {
        let bytes = arg.as_encoded_bytes();
        if only_positionals || bytes.len() < 2 || bytes[0] != b'-' {
            items.push(Ok(Item::Positional(arg.clone())));
        } else if bytes == b"--" {
            only_positionals = true;
        } else if bytes.starts_with(b"--") {
            let (name, inline) = os::split_at_eq(arg);
            let spec = name
                .to_str()
                .and_then(|n| table.long.get(&n[2..]).map(|s| (n, *s)));
            let Some((name, spec)) = spec else {
                items.push(Ok(Item::Ignored(arg.to_string_lossy().into_owned())));
                continue;
            };
            let name = name.to_owned();
            items.push(match (spec.takes_value, inline) {
                (true, Some(value)) => Ok(Item::Opt {
                    kind: spec.kind,
                    name,
                    value: Some(value),
                }),
                (true, None) => match rest.next() {
                    Some(value) => Ok(Item::Opt {
                        kind: spec.kind,
                        name,
                        value: Some(value.clone()),
                    }),
                    None => Err(CliError::MissingValue(name)),
                },
                (false, None) => Ok(Item::Opt {
                    kind: spec.kind,
                    name,
                    value: None,
                }),
                (false, Some(_)) => Err(CliError::UnexpectedValue(name)),
            });
        } else {
            tokenize_short_cluster(arg, table, &mut rest, &mut items);
        }
    }
    items
}

fn tokenize_short_cluster<'a>(
    arg: &OsStr,
    table: &OptionTable,
    rest: &mut impl Iterator<Item = &'a OsString>,
    items: &mut Vec<Result<Item, CliError>>,
) {
    let bytes = arg.as_encoded_bytes();
    let mut ignored = String::new();
    let mut pos = 1;
    while pos < bytes.len() && bytes[pos].is_ascii() {
        let letter = char::from(bytes[pos]);
        pos += 1;
        let Some(spec) = table.short.get(&letter) else {
            ignored.push(letter);
            continue;
        };
        let name = format!("-{letter}");
        if !spec.takes_value {
            items.push(Ok(Item::Opt {
                kind: spec.kind,
                name,
                value: None,
            }));
            continue;
        }
        let item = if pos < bytes.len() {
            let start = if bytes[pos] == b'=' { pos + 1 } else { pos };
            Ok(Item::Opt {
                kind: spec.kind,
                name,
                value: Some(os::suffix(arg, start)),
            })
        } else {
            match rest.next() {
                Some(value) => Ok(Item::Opt {
                    kind: spec.kind,
                    name,
                    value: Some(value.clone()),
                }),
                None => Err(CliError::MissingValue(name)),
            }
        };
        items.push(item);
        pos = bytes.len();
    }
    if pos < bytes.len() {
        // A non-ASCII byte cannot start an option letter: drop the rest of the cluster.
        ignored.push_str(&String::from_utf8_lossy(&bytes[pos..]));
    }
    if !ignored.is_empty() {
        items.push(Ok(Item::Ignored(format!("-{ignored}"))));
    }
}

// --- Ordered interpretation (§2.3) --------------------------------------------------------

#[derive(Clone, Copy, Debug)]
enum Target {
    Screen(usize),
    Span(usize),
}

#[derive(Default)]
struct State {
    args: CliArgs,
    target: Option<Target>,
    window_given: bool,
    positional: Option<String>,
}

fn invalid(option: &str, value: &str, reason: impl Into<String>) -> CliError {
    CliError::InvalidValue {
        option: option.to_owned(),
        value: value.to_owned(),
        reason: reason.into(),
    }
}

fn utf8(name: &str, value: &OsStr) -> Result<String, CliError> {
    value
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| CliError::NotUnicode(name.to_owned()))
}

fn non_empty_utf8(name: &str, value: &OsStr) -> Result<String, CliError> {
    let value = utf8(name, value)?;
    if value.is_empty() {
        return Err(invalid(name, "", "the value cannot be empty"));
    }
    Ok(value)
}

fn non_empty_path(name: &str, value: OsString) -> Result<PathBuf, CliError> {
    if value.is_empty() {
        return Err(invalid(name, "", "the value cannot be empty"));
    }
    Ok(PathBuf::from(value))
}

fn integer(name: &str, value: &OsStr) -> Result<i64, CliError> {
    let text = utf8(name, value)?;
    text.parse::<i64>()
        .map_err(|_| invalid(name, &text, "expected an integer"))
}

impl State {
    fn target_settings(&mut self) -> Option<&mut TargetSettings> {
        let layout = &mut self.args.layout;
        match self.target? {
            Target::Screen(i) => Some(&mut layout.screens[i].settings),
            Target::Span(i) => Some(&mut layout.spans[i].settings),
        }
    }

    fn mention(&mut self, wallpaper: WallpaperRef) -> WallpaperRef {
        self.args.mentioned.push(wallpaper.clone());
        wallpaper
    }

    fn enter_desktop_mode(&mut self) -> Result<(), CliError> {
        if self.window_given {
            return Err(CliError::WindowAndDesktop);
        }
        self.args.layout.mode = CliMode::Desktop;
        Ok(())
    }

    fn is_screen(&self, name: &str) -> bool {
        self.args.layout.screens.iter().any(|s| s.name == name)
    }

    fn is_in_span(&self, name: &str) -> bool {
        self.args
            .layout
            .spans
            .iter()
            .any(|s| s.outputs.iter().any(|o| o == name))
    }

    fn positional(&mut self, value: OsString) -> Result<(), CliError> {
        let shown = value.to_string_lossy().into_owned();
        // Decision for QO-28: a second positional argument is an error.
        if let Some(first) = &self.positional {
            return Err(CliError::MultiplePositionals {
                first: first.clone(),
                second: shown,
            });
        }
        self.positional = Some(shown);
        if !value.is_empty() {
            let wallpaper = self.mention(WallpaperRef::Source(WallpaperSource::classify(value)));
            self.args.layout.default_wallpaper = Some(wallpaper);
        }
        Ok(())
    }

    fn option(
        &mut self,
        kind: OptKind,
        name: &str,
        value: Option<OsString>,
    ) -> Result<(), CliError> {
        let value = value.unwrap_or_default();
        let settings = &mut self.args.settings;
        match kind {
            OptKind::Help | OptKind::Version => {}
            OptKind::Window => {
                let text = utf8(name, &value)?;
                let geometry = WindowGeometry::parse(&text).map_err(|e| invalid(name, &text, e))?;
                if self.window_given {
                    return Err(CliError::WindowTwice);
                }
                if self.args.layout.mode == CliMode::Desktop {
                    return Err(CliError::WindowAndDesktop);
                }
                self.window_given = true;
                self.args.layout.mode = CliMode::Window(geometry);
            }
            OptKind::ScreenRoot => {
                let output = non_empty_utf8(name, &value)?;
                self.enter_desktop_mode()?;
                if self.is_screen(&output) {
                    return Err(CliError::DuplicateOutput(output));
                }
                if self.is_in_span(&output) {
                    return Err(CliError::OutputInSpan(output));
                }
                let layout = &mut self.args.layout;
                layout.screens.push(CliScreen {
                    name: output,
                    settings: TargetSettings {
                        scaling: layout.global_scaling,
                        clamp: layout.global_clamp,
                        ..TargetSettings::default()
                    },
                });
                self.target = Some(Target::Screen(layout.screens.len() - 1));
            }
            OptKind::ScreenSpan => {
                let raw = utf8(name, &value)?;
                self.enter_desktop_mode()?;
                let outputs: Vec<String> = raw
                    .split(',')
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect();
                if outputs.len() < 2 {
                    return Err(CliError::SpanTooSmall(raw));
                }
                for (i, output) in outputs.iter().enumerate() {
                    if outputs[..i].contains(output) {
                        return Err(CliError::DuplicateInSpan {
                            group: raw,
                            output: output.clone(),
                        });
                    }
                    if self.is_screen(output) {
                        return Err(CliError::DuplicateOutput(output.clone()));
                    }
                    if self.is_in_span(output) {
                        return Err(CliError::OutputInSpan(output.clone()));
                    }
                }
                let layout = &mut self.args.layout;
                layout.spans.push(CliSpan {
                    key: format!("span:{raw}"),
                    outputs,
                    settings: TargetSettings {
                        scaling: layout.global_scaling,
                        clamp: layout.global_clamp,
                        ..TargetSettings::default()
                    },
                });
                self.target = Some(Target::Span(layout.spans.len() - 1));
            }
            OptKind::Bg => {
                let path = non_empty_path(name, value)?;
                let wallpaper = self.mention(WallpaperRef::Source(WallpaperSource::classify(path)));
                if let Some(target) = self.target_settings() {
                    target.wallpaper = Some(wallpaper.clone());
                }
                self.args.layout.default_wallpaper = Some(wallpaper);
            }
            OptKind::Playlist => {
                let playlist = non_empty_utf8(name, &value)?;
                let first = self.mention(WallpaperRef::PlaylistFirst(playlist.clone()));
                match self.target_settings() {
                    Some(target) => {
                        target.playlist = Some(playlist);
                        target.wallpaper = Some(first.clone());
                    }
                    None => self.args.layout.window_playlist = Some(playlist),
                }
                self.args.layout.default_wallpaper.get_or_insert(first);
            }
            OptKind::Scaling => {
                let text = utf8(name, &value)?;
                let scaling =
                    parse_value_enum::<Scaling>(&text).map_err(|e| invalid(name, &text, e))?;
                match self.target_settings() {
                    Some(target) => target.scaling = Some(scaling),
                    None => self.args.layout.global_scaling = Some(scaling),
                }
            }
            OptKind::Clamp => {
                let text = utf8(name, &value)?;
                let clamp =
                    parse_value_enum::<Clamp>(&text).map_err(|e| invalid(name, &text, e))?;
                match self.target_settings() {
                    Some(target) => target.clamp = Some(clamp),
                    None => self.args.layout.global_clamp = Some(clamp),
                }
            }
            OptKind::Layer => {
                let text = utf8(name, &value)?;
                settings.layer =
                    Some(parse_value_enum(&text).map_err(|e| invalid(name, &text, e))?);
            }
            OptKind::Fps => {
                let fps = integer(name, &value)?;
                settings.fps =
                    Some(validate_fps(fps).map_err(|e| invalid(name, &fps.to_string(), e))?);
            }
            OptKind::Hwdec => {
                let text = utf8(name, &value)?;
                settings.hwdec =
                    Some(parse_value_enum(&text).map_err(|e| invalid(name, &text, e))?);
            }
            OptKind::NoFullscreenPause => settings.no_fullscreen_pause = true,
            OptKind::FullscreenPauseOnlyActive => settings.fullscreen_pause_only_active = true,
            OptKind::FullscreenPauseIgnoreAppid => {
                let pattern = utf8(name, &value)?;
                if !pattern.is_empty() {
                    settings.fullscreen_pause_ignore_appids.push(pattern);
                }
            }
            OptKind::Volume => settings.volume = Some(clamp_volume(integer(name, &value)?)),
            OptKind::Silent => settings.silent = true,
            OptKind::NoAutomute => settings.no_automute = true,
            OptKind::NoAudioProcessing => settings.no_audio_processing = true,
            OptKind::Screenshot => settings.screenshot = Some(non_empty_path(name, value)?),
            OptKind::ScreenshotDelay => {
                let delay = integer(name, &value)?;
                settings.screenshot_delay = Some(
                    validate_screenshot_delay(delay)
                        .map_err(|e| invalid(name, &delay.to_string(), e))?,
                );
            }
            OptKind::AssetsDir => settings.assets_dir = Some(non_empty_path(name, value)?),
            OptKind::DisableParticles => settings.disable_particles = true,
            OptKind::DisableMouse => settings.disable_mouse = true,
            OptKind::DisableParallax => settings.disable_parallax = true,
            OptKind::ListProperties => settings.list_properties = true,
            OptKind::SetProperty => {
                let text = utf8(name, &value)?;
                let (key, val) = text.split_once('=').unwrap_or((&text, "1"));
                if key.is_empty() {
                    return Err(invalid(name, &text, "the property key cannot be empty"));
                }
                settings.properties.push((key.to_owned(), val.to_owned()));
            }
            OptKind::DumpStructure => settings.dump_structure = true,
            OptKind::RenderDebug => {
                let text = utf8(name, &value)?;
                settings
                    .render_debug
                    .apply(&text)
                    .map_err(|e| invalid(name, &text, e))?;
            }
            OptKind::Config => self.args.config = ConfigChoice::File(non_empty_path(name, value)?),
            OptKind::NoConfig => self.args.config = ConfigChoice::Disabled,
        }
        Ok(())
    }
}
