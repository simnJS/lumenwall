//! `--demo-library`: a generated library of made-up wallpapers with abstract,
//! procedurally drawn previews. Lets the UI be shown, tested and screenshotted
//! without any Workshop content (which may not be redistributed).

use std::fs::File;
use std::path::{Path, PathBuf};

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, Rgba, RgbaImage};

use crate::catalog::{Discovery, SteamInstall};

pub(crate) struct Spec {
    pub title: &'static str,
    pub kind: &'static str,
    pub tags: &'static [&'static str],
    pub hue: f32,
    pub animated: bool,
    props: &'static str,
}

pub(crate) const SPECS: &[Spec] = &[
    Spec {
        title: "Aurora Drift",
        kind: "scene",
        tags: &["Nature", "Relaxing"],
        hue: 0.45,
        animated: true,
        props: r#""schemecolor":{"order":0,"text":"ui_browse_properties_scheme_color","type":"color","value":"0.2 0.8 0.6"},"speed":{"order":1,"text":"Wave speed","type":"slider","min":0,"max":5,"fraction":true,"value":1.5},"stars":{"order":2,"text":"Stars","type":"bool","value":true}"#,
    },
    Spec {
        title: "Neon Rain",
        kind: "scene",
        tags: &["Cyberpunk", "Rain"],
        hue: 0.85,
        animated: true,
        props: r#""rain":{"order":0,"text":"Rain","type":"bool","value":true},"density":{"order":1,"text":"Density","type":"slider","min":1,"max":10,"fraction":false,"value":6},"mode":{"order":2,"text":"Time of day","type":"combo","value":"night","options":[{"label":"Day","value":"day"},{"label":"Dusk","value":"dusk"},{"label":"Night","value":"night"}]}"#,
    },
    Spec {
        title: "Quiet Harbor",
        kind: "video",
        tags: &["Landscape"],
        hue: 0.58,
        animated: true,
        props: "",
    },
    Spec {
        title: "Pixel Forest",
        kind: "scene",
        tags: &["Pixel art", "Nature"],
        hue: 0.3,
        animated: true,
        props: r#""fireflies":{"order":0,"text":"Fireflies","type":"bool","value":false}"#,
    },
    Spec {
        title: "Clockwork Desk",
        kind: "web",
        tags: &["Technology", "Clock"],
        hue: 0.1,
        animated: false,
        props: r#""clock24":{"order":0,"text":"24-hour clock","type":"bool","value":true},"label":{"order":1,"text":"Greeting","type":"textinput","value":"Hello"}"#,
    },
    Spec {
        title: "Deep Space",
        kind: "video",
        tags: &["Space"],
        hue: 0.68,
        animated: true,
        props: "",
    },
    Spec {
        title: "Sakura Wind",
        kind: "scene",
        tags: &["Anime", "Relaxing"],
        hue: 0.95,
        animated: true,
        props: r#""petals":{"order":0,"text":"Petals","type":"slider","min":0,"max":100,"fraction":false,"value":40}"#,
    },
    Spec {
        title: "Synthwave Road",
        kind: "video",
        tags: &["Retro", "Music"],
        hue: 0.78,
        animated: true,
        props: "",
    },
    Spec {
        title: "Ocean Glass",
        kind: "scene",
        tags: &["Abstract"],
        hue: 0.52,
        animated: false,
        props: "",
    },
    Spec {
        title: "Ember Field",
        kind: "scene",
        tags: &["Abstract", "Fire"],
        hue: 0.04,
        animated: true,
        props: "",
    },
    Spec {
        title: "Paper Cranes",
        kind: "web",
        tags: &["Minimal"],
        hue: 0.15,
        animated: false,
        props: "",
    },
    Spec {
        title: "Old Arcade",
        kind: "application",
        tags: &["Retro"],
        hue: 0.62,
        animated: false,
        props: "",
    },
];

fn hsv(h: f32, s: f32, v: f32) -> [u8; 3] {
    let h = (h.fract() + 1.0).fract() * 6.0;
    let i = h.floor();
    let f = h - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    let (r, g, b) = match i as u32 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8]
}

/// Soft gradient with drifting glowing blobs.
fn frame(spec: &Spec, t: f32, size: u32) -> RgbaImage {
    art(spec.hue, t, size, size)
}

/// Soft gradient with drifting glowing blobs, `w × h`, at loop position `t`.
pub(crate) fn art(hue: f32, t: f32, w: u32, h: u32) -> RgbaImage {
    let s = w.max(h) as f32;
    let blobs = [
        (0.3, 0.35, 0.22, 0.0),
        (0.7, 0.6, 0.28, 2.1),
        (0.5, 0.8, 0.18, 4.2),
    ];
    RgbaImage::from_fn(w, h, |x, y| {
        let (u, v) = (x as f32 / s, y as f32 / s);
        let mut glow = 0.0;
        for (bx, by, r, ph) in blobs {
            let a = t * std::f32::consts::TAU + ph;
            let (cx, cy) = (bx + 0.08 * a.cos(), by + 0.06 * (a * 1.3).sin());
            let d2 = (u - cx).powi(2) + (v - cy).powi(2);
            glow += (-d2 / (r * r * 0.35)).exp();
        }
        let base = hsv(hue + 0.08 * v, 0.65, 0.18 + 0.25 * (1.0 - v));
        let hot = hsv(hue + 0.12 * glow, 0.55, 1.0);
        let k = (glow * 0.8).min(1.0);
        let mix = |a: u8, b: u8| (f32::from(a) * (1.0 - k) + f32::from(b) * k) as u8;
        Rgba([
            mix(base[0], hot[0]),
            mix(base[1], hot[1]),
            mix(base[2], hot[2]),
            255,
        ])
    })
}

/// Workshop id of the `i`-th demo wallpaper (also served by the fake Steam server).
pub(crate) fn demo_id(i: usize) -> String {
    format!("{}", 3_100_000_000u64 + i as u64)
}

/// Creates (once) the demo library under the cache directory and returns its root.
pub fn ensure(root: &Path) -> std::io::Result<PathBuf> {
    let ws = root.join("workshop");
    for (i, spec) in SPECS.iter().enumerate() {
        let dir = ws.join(format!("demo-{i:02}"));
        if dir.join("project.json").is_file() {
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        let preview = if spec.animated {
            "preview.gif"
        } else {
            "preview.png"
        };
        if spec.animated {
            let mut enc = GifEncoder::new_with_speed(File::create(dir.join(preview))?, 10);
            enc.set_repeat(Repeat::Infinite)
                .map_err(std::io::Error::other)?;
            let frames = (0..24).map(|f| {
                Frame::from_parts(
                    frame(spec, f as f32 / 24.0, 192),
                    0,
                    0,
                    Delay::from_numer_denom_ms(60, 1),
                )
            });
            enc.encode_frames(frames).map_err(std::io::Error::other)?;
        } else {
            frame(spec, 0.2, 256)
                .save(dir.join(preview))
                .map_err(std::io::Error::other)?;
        }
        let tags: Vec<String> = spec.tags.iter().map(|t| format!("\"{t}\"")).collect();
        let file = match spec.kind {
            "video" => "loop.mp4",
            "web" => "index.html",
            "application" => "game.exe",
            _ => "scene.json",
        };
        let json = format!(
            r#"{{"title":"{}","type":"{}","file":"{file}","preview":"{preview}","tags":[{}],"workshopid":"{}","description":"Demo wallpaper generated by Lumen.","general":{{"properties":{{{}}}}}}}"#,
            spec.title,
            spec.kind,
            tags.join(","),
            demo_id(i),
            spec.props
        );
        std::fs::write(dir.join("project.json"), json)?;
        if spec.kind == "scene" {
            let (w, h) = if i % 3 == 0 {
                (3840, 2160)
            } else {
                (2560, 1440)
            };
            std::fs::write(
                dir.join("scene.json"),
                format!(r#"{{"general":{{"orthogonalprojection":{{"width":{w},"height":{h}}}}}}}"#),
            )?;
        }
    }
    std::fs::create_dir_all(root.join("wallpaper_engine/assets"))?;
    Ok(ws)
}

/// Discovery that returns only the demo library.
pub struct DemoDiscovery {
    pub root: PathBuf,
}

impl Discovery for DemoDiscovery {
    fn discover(&self) -> SteamInstall {
        let ws = self.root.join("workshop");
        SteamInstall {
            roots: vec![self.root.clone()],
            libraries: vec![self.root.clone()],
            workshop_dirs: vec![ws],
            we_dir: Some(self.root.join("wallpaper_engine")),
            searched: vec![self.root.clone()],
        }
    }
}
