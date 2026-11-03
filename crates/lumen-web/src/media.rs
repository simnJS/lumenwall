//! Media integration events (`wallpaperRegisterMedia*Listener`).
//!
//! Field names follow the public Wallpaper Engine documentation.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Playback state; numeric values exposed as `wallpaperMediaIntegration.PLAYBACK_*`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlaybackState {
    #[default]
    Stopped,
    Playing,
    Paused,
}

impl PlaybackState {
    /// Value of `event.state`, equal to the matching `wallpaperMediaIntegration` constant.
    pub fn js_value(self) -> u8 {
        match self {
            Self::Stopped => 0,
            Self::Playing => 1,
            Self::Paused => 2,
        }
    }
}

/// Which media listener an event targets.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Status,
    Properties,
    Thumbnail,
    Playback,
    Timeline,
}

impl MediaKind {
    pub const ALL: [MediaKind; 5] = [
        Self::Status,
        Self::Properties,
        Self::Thumbnail,
        Self::Playback,
        Self::Timeline,
    ];

    /// Key used by the injected bridge.
    pub fn key(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Properties => "properties",
            Self::Thumbnail => "thumbnail",
            Self::Playback => "playback",
            Self::Timeline => "timeline",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }

    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// Set of media listeners the page registered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaListeners(u8);

impl MediaListeners {
    pub fn contains(self, kind: MediaKind) -> bool {
        self.0 & kind.bit() != 0
    }

    pub fn set(&mut self, kind: MediaKind, on: bool) {
        if on {
            self.0 |= kind.bit();
        } else {
            self.0 &= !kind.bit();
        }
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// One media event, as delivered to the page.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum MediaEvent {
    /// Media integration switched on or off.
    Status {
        enabled: bool,
    },
    /// Track metadata. Optional fields are empty strings when unknown.
    Properties {
        title: String,
        artist: String,
        #[serde(rename = "subTitle")]
        sub_title: String,
        #[serde(rename = "albumTitle")]
        album_title: String,
        #[serde(rename = "albumArtist")]
        album_artist: String,
        /// Comma-separated.
        genres: String,
        /// `music`, `video` or `image`.
        #[serde(rename = "contentType")]
        content_type: String,
    },
    /// Album art as a `data:image/png;base64,…` URL plus colours derived from it.
    Thumbnail {
        thumbnail: String,
        #[serde(rename = "primaryColor")]
        primary_color: String,
        #[serde(rename = "secondaryColor")]
        secondary_color: String,
        #[serde(rename = "tertiaryColor")]
        tertiary_color: String,
        #[serde(rename = "textColor")]
        text_color: String,
        #[serde(rename = "highContrastColor")]
        high_contrast_color: String,
    },
    Playback {
        state: PlaybackState,
    },
    /// Seconds.
    Timeline {
        position: f64,
        duration: f64,
    },
}

impl MediaEvent {
    pub fn kind(&self) -> MediaKind {
        match self {
            Self::Status { .. } => MediaKind::Status,
            Self::Properties { .. } => MediaKind::Properties,
            Self::Thumbnail { .. } => MediaKind::Thumbnail,
            Self::Playback { .. } => MediaKind::Playback,
            Self::Timeline { .. } => MediaKind::Timeline,
        }
    }

    /// The `event` object the page's callback receives.
    pub fn to_js_event(&self) -> Value {
        match self {
            Self::Status { enabled } => json!({ "enabled": enabled }),
            Self::Playback { state } => json!({ "state": state.js_value() }),
            Self::Timeline { position, duration } => {
                json!({ "position": position, "duration": duration })
            }
            other => {
                let mut v = serde_json::to_value(other).unwrap_or(Value::Null);
                if let Some(o) = v.as_object_mut() {
                    o.remove("kind");
                }
                v
            }
        }
    }

    /// Builds a thumbnail event from PNG bytes and the decoded RGBA8 pixels of the same image.
    pub fn thumbnail_from_png(png: &[u8], rgba: &[u8]) -> Self {
        let palette = Palette::from_rgba(rgba);
        Self::Thumbnail {
            thumbnail: png_data_url(png),
            primary_color: css_hex(palette.primary),
            secondary_color: css_hex(palette.secondary),
            tertiary_color: css_hex(palette.tertiary),
            text_color: css_hex(palette.text),
            high_contrast_color: css_hex(high_contrast(palette.primary)),
        }
    }
}

/// `data:image/png;base64,…`
pub fn png_data_url(png: &[u8]) -> String {
    let mut out = String::with_capacity(22 + png.len().div_ceil(3) * 4);
    out.push_str("data:image/png;base64,");
    base64_encode_into(png, &mut out);
    out
}

fn base64_encode_into(data: &[u8], out: &mut String) {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
}

fn css_hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Black or white, whichever contrasts most with `c` (WCAG relative luminance).
pub fn high_contrast(c: [u8; 3]) -> [u8; 3] {
    fn lin(v: u8) -> f64 {
        let s = f64::from(v) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }
    let l = 0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2]);
    // Contrast with white (1.05 / (l + 0.05)) vs black ((l + 0.05) / 0.05).
    if 1.05 / (l + 0.05) >= (l + 0.05) / 0.05 {
        [255, 255, 255]
    } else {
        [0, 0, 0]
    }
}

/// Dominant colours of an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub primary: [u8; 3],
    pub secondary: [u8; 3],
    pub tertiary: [u8; 3],
    pub text: [u8; 3],
}

impl Palette {
    /// Most frequent 4-bit-per-channel colour buckets, each at least a minimum distance from the
    /// previous picks. `rgba` is tightly packed RGBA8; transparent pixels are ignored.
    pub fn from_rgba(rgba: &[u8]) -> Self {
        let mut counts = vec![0u32; 4096];
        let mut sums = vec![[0u64; 3]; 4096];
        // Subsample large images: ~16k pixels are plenty for a palette.
        let pixels = rgba.len() / 4;
        let step = (pixels / 16384).max(1);
        let (pixels4, _) = rgba.as_chunks::<4>();
        for px in pixels4.iter().step_by(step) {
            if px[3] < 128 {
                continue;
            }
            let bin = (usize::from(px[0] >> 4) << 8)
                | (usize::from(px[1] >> 4) << 4)
                | usize::from(px[2] >> 4);
            counts[bin] += 1;
            for c in 0..3 {
                sums[bin][c] += u64::from(px[c]);
            }
        }
        let mut order: Vec<usize> = (0..4096).filter(|&b| counts[b] > 0).collect();
        order.sort_by(|&a, &b| counts[b].cmp(&counts[a]).then(a.cmp(&b)));
        let mean = |bin: usize| -> [u8; 3] {
            let n = u64::from(counts[bin]);
            [
                (sums[bin][0] / n) as u8,
                (sums[bin][1] / n) as u8,
                (sums[bin][2] / n) as u8,
            ]
        };
        let dist = |a: [u8; 3], b: [u8; 3]| -> i32 {
            (0..3)
                .map(|i| (i32::from(a[i]) - i32::from(b[i])).abs())
                .sum()
        };
        let mut picks: Vec<[u8; 3]> = Vec::with_capacity(3);
        for &bin in &order {
            let c = mean(bin);
            if picks.iter().all(|&p| dist(p, c) >= 96) {
                picks.push(c);
                if picks.len() == 3 {
                    break;
                }
            }
        }
        let primary = picks.first().copied().unwrap_or([0, 0, 0]);
        let secondary = picks.get(1).copied().unwrap_or(primary);
        let tertiary = picks.get(2).copied().unwrap_or(secondary);
        // Text: the most contrasting pick against the primary, else black/white.
        let text = [secondary, tertiary]
            .into_iter()
            .filter(|&c| dist(c, primary) >= 192)
            .max_by_key(|&c| dist(c, primary))
            .unwrap_or_else(|| high_contrast(primary));
        Self {
            primary,
            secondary,
            tertiary,
            text,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_known_vectors() {
        let enc = |d: &[u8]| {
            let mut s = String::new();
            base64_encode_into(d, &mut s);
            s
        };
        assert_eq!(enc(b""), "");
        assert_eq!(enc(b"f"), "Zg==");
        assert_eq!(enc(b"fo"), "Zm8=");
        assert_eq!(enc(b"foo"), "Zm9v");
        assert_eq!(enc(b"foobar"), "Zm9vYmFy");
        assert_eq!(png_data_url(b"\x89PNG"), "data:image/png;base64,iVBORw==");
    }

    #[test]
    fn js_events_use_documented_names() {
        let e = MediaEvent::Properties {
            title: "T".into(),
            artist: "A".into(),
            sub_title: String::new(),
            album_title: "Al".into(),
            album_artist: String::new(),
            genres: "rock,pop".into(),
            content_type: "music".into(),
        };
        let v = e.to_js_event();
        assert_eq!(v["albumTitle"], "Al");
        assert_eq!(v["contentType"], "music");
        assert!(v.get("kind").is_none());
        assert_eq!(
            MediaEvent::Playback {
                state: PlaybackState::Paused
            }
            .to_js_event(),
            json!({ "state": 2 })
        );
        assert_eq!(
            MediaEvent::Status { enabled: true }.to_js_event(),
            json!({ "enabled": true })
        );
    }

    #[test]
    fn palette_and_contrast() {
        assert_eq!(high_contrast([250, 250, 250]), [0, 0, 0]);
        assert_eq!(high_contrast([10, 10, 60]), [255, 255, 255]);
        let mut img = Vec::new();
        for i in 0..100 {
            let c: [u8; 4] = if i < 60 {
                [200, 30, 30, 255]
            } else if i < 90 {
                [20, 20, 220, 255]
            } else {
                [0, 0, 0, 0]
            };
            img.extend_from_slice(&c);
        }
        let p = Palette::from_rgba(&img);
        assert_eq!(p.primary, [200, 30, 30]);
        assert_eq!(p.secondary, [20, 20, 220]);
        let MediaEvent::Thumbnail { primary_color, .. } =
            MediaEvent::thumbnail_from_png(b"x", &img)
        else {
            unreachable!()
        };
        assert_eq!(primary_color, "#c81e1e");
    }

    #[test]
    fn listener_set() {
        let mut l = MediaListeners::default();
        assert!(l.is_empty());
        l.set(MediaKind::Timeline, true);
        assert!(l.contains(MediaKind::Timeline));
        assert!(!l.contains(MediaKind::Status));
        l.set(MediaKind::Timeline, false);
        assert!(l.is_empty());
        assert_eq!(MediaKind::from_key("thumbnail"), Some(MediaKind::Thumbnail));
    }
}
