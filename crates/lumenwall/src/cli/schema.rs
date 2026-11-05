//! Declarative option table (spec 07 §2.2): names, aliases, value names and help text.
//!
//! clap renders `--help` from this struct and provides the option metadata. The values are
//! *not* read through clap: options are order-dependent (spec 07 §2.3) and unknown
//! arguments must be ignored (spec 07 §2.1), so `super::parse` walks the arguments itself.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;

use crate::settings::{Clamp, HwDecode, Layer, Scaling};

const WALLPAPER: &str = "Wallpaper";
const PERFORMANCE: &str = "Performance";
const QUALITY: &str = "Quality";
const SOUND: &str = "Sound";
const SCREENSHOT: &str = "Screenshot";
const CONTENT: &str = "Content";
const WALLPAPER_CONFIG: &str = "Wallpaper configuration";
const DEBUG: &str = "Debug";
const LUMEN: &str = "Lumenwall";

const EXAMPLES: &str = "\
Examples:
  lumenwall 2317494988
      Preview a Workshop wallpaper in a window.
  lumenwall -r HDMI-1 --bg 2317494988 --scaling fill --clamp border
      Wallpaper on HDMI-1, cropped to fill the output, transparent borders.
  lumenwall -r HDMI-1 -r HDMI-2 2317494988
      The same wallpaper on two outputs.
  lumenwall -r HDMI-1 --bg 2317494988 -r HDMI-2 --bg ./my_wallpaper
      A different wallpaper on each output (a value with '/' is a path).
  lumenwall --screen-span HDMI-1,HDMI-2 --bg 2317494988 --scaling fill
      One wallpaper stretched across two outputs.

Options marked 'per output' apply to the output named by the last --screen-root or
--screen-span. Before any of them, they set the default used by the window and by the
outputs named afterwards: write --scaling/--clamp after the -r they are meant for.

Configuration file: <config dir>/lumenwall/config.toml (command line options take precedence).
Logging filter: LUMENWALL_LOG or RUST_LOG (e.g. LUMENWALL_LOG=debug).";

/// Lumenwall: animated wallpaper engine for Wallpaper Engine wallpapers.
#[derive(Parser, Debug)]
#[command(
    name = "lumenwall",
    about = "Animated wallpaper engine for Wallpaper Engine wallpapers",
    after_help = EXAMPLES,
    disable_version_flag = true,
    max_term_width = 100
)]
#[allow(dead_code)] // Schema only: values are read in order by `cli::parse`.
pub(crate) struct CliSchema {
    /// Workshop ID or path of the default wallpaper, used by the window and by outputs
    /// without --bg. A value without a path separator ('/') is a Workshop ID.
    #[arg(value_name = "ID_OR_PATH", help_heading = WALLPAPER)]
    background: Option<OsString>,

    /// Explicit window mode: position X,Y and size WxH (borderless, always on top).
    #[arg(short = 'w', long = "window", value_name = "XxYxWxH", help_heading = WALLPAPER)]
    window: Option<String>,

    /// Desktop wallpaper mode on this output; the per-output options that follow target it.
    /// Repeatable.
    #[arg(short = 'r', long = "screen-root", value_name = "OUTPUT", help_heading = WALLPAPER)]
    screen_root: Vec<String>,

    /// Desktop wallpaper mode: one wallpaper stretched across these outputs (at least two).
    /// Repeatable.
    #[arg(long = "screen-span", value_name = "A,B[,C...]", help_heading = WALLPAPER)]
    screen_span: Vec<String>,

    /// Wallpaper of the last named output or span group; also becomes the default
    /// wallpaper. Repeatable.
    #[arg(short = 'b', long = "bg", value_name = "ID_OR_PATH", help_heading = WALLPAPER)]
    bg: Vec<OsString>,

    /// Wallpaper Engine playlist (from its config.json) for the last named output, or for
    /// the window. Repeatable.
    #[arg(long = "playlist", value_name = "NAME", help_heading = WALLPAPER)]
    playlist: Vec<String>,

    /// Scaling mode (per output). Repeatable.
    #[arg(long = "scaling", value_name = "MODE", value_enum, default_value_t = Scaling::Default,
          help_heading = WALLPAPER)]
    scaling: Scaling,

    /// Texture edge mode outside the image (per output). Repeatable.
    #[arg(long = "clamp", visible_alias = "clamping", value_name = "MODE", value_enum,
          default_value_t = Clamp::Clamp, help_heading = WALLPAPER)]
    clamp: Clamp,

    /// Wayland layer-shell layer (ignored on Windows).
    #[arg(long = "layer", value_name = "LAYER", value_enum, default_value_t = Layer::Bottom,
          help_heading = WALLPAPER)]
    layer: Layer,

    /// Frame-rate cap (frames per second, at least 1).
    #[arg(short = 'f', long = "fps", value_name = "FPS", default_value_t = 30,
          help_heading = PERFORMANCE)]
    fps: i64,

    /// Hardware video decoding.
    #[arg(long = "hwdec", value_name = "MODE", value_enum, default_value_t = HwDecode::Auto,
          help_heading = PERFORMANCE)]
    hwdec: HwDecode,

    /// Keep rendering while a fullscreen window is shown.
    #[arg(long = "no-fullscreen-pause", help_heading = PERFORMANCE)]
    no_fullscreen_pause: bool,

    /// Wayland and Windows: only pause for fullscreen windows that are active (the foreground window).
    #[arg(long = "fullscreen-pause-only-active", help_heading = PERFORMANCE)]
    fullscreen_pause_only_active: bool,

    /// Wayland: ignore fullscreen windows whose app_id contains this text (case-insensitive).
    /// Repeatable.
    #[arg(long = "fullscreen-pause-ignore-appid", value_name = "TEXT", help_heading = PERFORMANCE)]
    fullscreen_pause_ignore_appid: Vec<String>,

    /// Global volume, clamped to 0..=128 (64 is the 50 % of the settings window: percent =
    /// volume × 100 / 128).
    #[arg(short = 'v', long = "volume", value_name = "VOLUME", default_value_t = 64,
          allow_negative_numbers = true, help_heading = SOUND)]
    volume: i64,

    /// Mute every sound played by wallpapers.
    #[arg(short = 's', long = "silent", help_heading = SOUND)]
    silent: bool,

    /// Keep wallpaper sounds playing while another application plays media (the default;
    /// `automute = true` in the configuration file pauses them while music, video or a game
    /// plays, calls and notifications excepted).
    #[arg(long = "noautomute", help_heading = SOUND)]
    noautomute: bool,

    /// Disable audio capture for audio-reactive wallpapers.
    #[arg(long = "no-audio-processing", help_heading = SOUND)]
    no_audio_processing: bool,

    /// Save a screenshot to this file (.bmp, .png, .jpg or .jpeg) and keep running.
    #[arg(long = "screenshot", value_name = "FILE", help_heading = SCREENSHOT)]
    screenshot: Option<PathBuf>,

    /// Number of frames rendered before the screenshot (at most 5).
    #[arg(long = "screenshot-delay", value_name = "FRAMES", default_value_t = 5,
          help_heading = SCREENSHOT)]
    screenshot_delay: i64,

    /// Wallpaper Engine 'assets' folder (detected automatically by default).
    #[arg(long = "assets-dir", value_name = "DIR", help_heading = CONTENT)]
    assets_dir: Option<PathBuf>,

    /// Anti-aliasing of the scene: off, x2, x4, x8 (the GPU may offer less).
    #[arg(long = "msaa", value_name = "MODE", help_heading = QUALITY)]
    msaa: Option<String>,

    /// Texture size: full, half, quarter.
    #[arg(long = "texture-resolution", value_name = "SIZE", help_heading = QUALITY)]
    texture_resolution: Option<String>,

    /// HDR render targets for the scenes that ask for them: off, scene.
    #[arg(long = "hdr", value_name = "MODE", help_heading = QUALITY)]
    hdr: Option<String>,

    /// Render image chains and effects at the scene's own size even when it is shown smaller.
    #[arg(long = "native-resolution", help_heading = QUALITY)]
    native_resolution: bool,

    /// Do not create particle systems.
    #[arg(long = "disable-particles", help_heading = WALLPAPER_CONFIG)]
    disable_particles: bool,

    /// Disable mouse interaction.
    #[arg(long = "disable-mouse", help_heading = WALLPAPER_CONFIG)]
    disable_mouse: bool,

    /// Disable camera parallax.
    #[arg(long = "disable-parallax", help_heading = WALLPAPER_CONFIG)]
    disable_parallax: bool,

    /// Print the user properties of the wallpapers, then exit.
    #[arg(short = 'l', long = "list-properties", help_heading = WALLPAPER_CONFIG)]
    list_properties: bool,

    /// Override a user property; without '=VALUE' the value is "1". Repeatable.
    #[arg(long = "set-property", visible_alias = "property", value_name = "KEY[=VALUE]",
          help_heading = WALLPAPER_CONFIG)]
    set_property: Vec<String>,

    /// Print the parsed structure of the wallpapers.
    #[arg(short = 'z', long = "dump-structure", help_heading = DEBUG)]
    dump_structure: bool,

    /// Rendering debug mode: base-only, no-solid-final, pass-log, object=<id>,
    /// skip-object=<id>, skip-effect=<id>. Repeatable.
    #[arg(long = "render-debug", value_name = "MODE", help_heading = DEBUG)]
    render_debug: Vec<String>,

    /// Read settings from this TOML file instead of the default one.
    #[arg(long = "config", value_name = "FILE", help_heading = LUMEN)]
    config: Option<PathBuf>,

    /// Ignore the configuration file.
    #[arg(long = "no-config", help_heading = LUMEN)]
    no_config: bool,

    /// Print the version, then exit.
    #[arg(long = "version", help_heading = LUMEN)]
    version: bool,
}
