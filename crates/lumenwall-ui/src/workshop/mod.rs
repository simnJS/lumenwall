//! Steam Workshop for Wallpaper Engine: browsing through the public Steam Web API,
//! subscriptions through the Steam client, arrival of new items through file events.
//!
//! Lumenwall is not a Steam application: it never uses Steamworks and never subscribes on
//! the user's behalf. "Subscribe" opens the item in the Steam client, which downloads
//! it into the Wallpaper Engine Workshop folder (owning Wallpaper Engine is required);
//! the folder watcher then adds it to the library.

pub mod api;
pub mod bbcode;
pub mod key;
pub mod mock;
pub mod state;
pub mod watch;

use serde_json::Value;

use crate::catalog::Kind;

/// Wallpaper Engine's Steam app id: whose Workshop is browsed. Lumenwall does not
/// present itself as this app; the id only selects public Workshop listings.
pub const WE_APP_ID: &str = "431960";

pub fn steam_page(id: &str) -> String {
    format!("steam://url/CommunityFilePage/{id}")
}

pub fn web_page(id: &str) -> String {
    format!("https://steamcommunity.com/sharedfiles/filedetails/?id={id}")
}

pub const STEAM_WORKSHOP: &str = "steam://url/SteamWorkshopPage/431960";
pub const WEB_WORKSHOP: &str = "https://steamcommunity.com/app/431960/workshop/";
pub const API_KEY_PAGE: &str = "https://steamcommunity.com/dev/apikey";

/// One Workshop item, from `QueryFiles` or `GetPublishedFileDetails`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Item {
    pub id: String,
    pub title: String,
    /// SteamID64 of the author.
    pub creator: String,
    /// Persona name, when known (needs the API key).
    pub author: Option<String>,
    pub preview_url: Option<String>,
    pub file_size: Option<u64>,
    pub subscriptions: Option<u64>,
    pub favorited: Option<u64>,
    pub time_updated: Option<i64>,
    pub tags: Vec<String>,
    /// 0.0 to 1.0, from vote data.
    pub score: Option<f32>,
    pub votes: Option<u64>,
    /// Steam BBCode.
    pub description: String,
    pub screenshots: Vec<String>,
}

/// Wallpaper Engine's age-rating tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rating {
    Everyone,
    Questionable,
    Mature,
}

impl Item {
    pub fn kind(&self) -> Option<Kind> {
        self.tags
            .iter()
            .find_map(|t| match t.to_ascii_lowercase().as_str() {
                "scene" => Some(Kind::Scene),
                "video" => Some(Kind::Video),
                "web" => Some(Kind::Web),
                "application" => Some(Kind::Application),
                _ => None,
            })
    }

    pub fn rating(&self) -> Rating {
        if self.tags.iter().any(|t| t.eq_ignore_ascii_case("mature")) {
            Rating::Mature
        } else if self
            .tags
            .iter()
            .any(|t| t.eq_ignore_ascii_case("questionable"))
        {
            Rating::Questionable
        } else {
            Rating::Everyone
        }
    }

    /// Resolution tag such as `3840 x 2160`.
    pub fn resolution(&self) -> Option<&str> {
        self.tags
            .iter()
            .map(String::as_str)
            .find(|t| RESOLUTIONS.contains(t) || t.contains(" x "))
    }

    /// Genre tags (not type, rating or resolution).
    pub fn genres(&self) -> impl Iterator<Item = &str> {
        self.tags.iter().map(String::as_str).filter(|t| {
            !matches!(
                t.to_ascii_lowercase().as_str(),
                "scene"
                    | "video"
                    | "web"
                    | "application"
                    | "everyone"
                    | "questionable"
                    | "mature"
                    | "preset"
                    | "approved"
            ) && !t.contains(" x ")
        })
    }

    /// Parses one entry of `publishedfiledetails`. Numbers may come as strings
    /// (64-bit values) or numbers depending on the endpoint.
    pub fn from_json(v: &Value) -> Option<Item> {
        if let Some(r) = v.get("result").and_then(Value::as_i64)
            && r != 1
        {
            return None;
        }
        let id = text(v.get("publishedfileid")?)?;
        let num = |k: &str| v.get(k).and_then(number);
        let tags = v
            .get("tags")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|t| t.get("tag").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let vote = v.get("vote_data");
        let votes_up = vote.and_then(|d| d.get("votes_up")).and_then(number);
        let votes_down = vote.and_then(|d| d.get("votes_down")).and_then(number);
        let screenshots = v
            .get("previews")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|p| p.get("preview_type").and_then(Value::as_i64).unwrap_or(0) == 0)
                    .filter_map(|p| p.get("url").and_then(Value::as_str).map(str::to_owned))
                    .take(8)
                    .collect()
            })
            .unwrap_or_default();
        Some(Item {
            id,
            title: v
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_owned(),
            creator: v.get("creator").and_then(text).unwrap_or_default(),
            author: None,
            preview_url: v
                .get("preview_url")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            file_size: num("file_size"),
            subscriptions: num("subscriptions").or_else(|| num("lifetime_subscriptions")),
            favorited: num("favorited"),
            time_updated: num("time_updated").map(|t| t as i64),
            tags,
            score: vote
                .and_then(|d| d.get("score"))
                .and_then(Value::as_f64)
                .map(|s| s as f32),
            votes: match (votes_up, votes_down) {
                (Some(u), Some(d)) => Some(u + d),
                (Some(u), None) => Some(u),
                _ => None,
            },
            description: v
                .get("file_description")
                .or_else(|| v.get("description"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            screenshots,
        })
    }
}

fn number(v: &Value) -> Option<u64> {
    match v {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Resolution tags Wallpaper Engine offers on the Workshop (most common first).
pub const RESOLUTIONS: [&str; 6] = [
    "1920 x 1080",
    "2560 x 1440",
    "3840 x 2160",
    "1280 x 720",
    "3440 x 1440",
    "Dual 3840 x 1080",
];

/// Genre tags of the Wallpaper Engine Workshop.
pub const GENRES: [&str; 22] = [
    "Abstract",
    "Animal",
    "Anime",
    "Cartoon",
    "CGI",
    "Cyberpunk",
    "Fantasy",
    "Game",
    "Girls",
    "Guys",
    "Landscape",
    "Medieval",
    "Memes",
    "MMD",
    "Music",
    "Nature",
    "Pixel art",
    "Relaxing",
    "Retro",
    "Sci-Fi",
    "Technology",
    "Vehicle",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Sort {
    #[default]
    Trend,
    Popular,
    Recent,
}

/// What the Workshop page asks for. Search text switches to text ranking.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct Query {
    pub sort: Sort,
    pub search: String,
    pub kind: Option<Kind>,
    pub tag: Option<String>,
    pub resolution: Option<String>,
    pub questionable: bool,
    pub mature: bool,
}

impl Query {
    /// Query parameters for `IPublishedFileService/QueryFiles/v1` (without the key).
    pub fn params(&self, cursor: &str, per_page: u32) -> Vec<(String, String)> {
        let search = self.search.trim();
        // EPublishedFileQueryType: 3 trend, 9 total unique subscriptions,
        // 1 publication date, 12 text search.
        let query_type = if !search.is_empty() {
            "12"
        } else {
            match self.sort {
                Sort::Trend => "3",
                Sort::Popular => "9",
                Sort::Recent => "1",
            }
        };
        let mut p: Vec<(String, String)> = vec![
            ("appid".into(), WE_APP_ID.into()),
            ("query_type".into(), query_type.into()),
            ("cursor".into(), cursor.into()),
            ("numperpage".into(), per_page.to_string()),
            ("return_tags".into(), "true".into()),
            ("return_previews".into(), "true".into()),
            ("return_vote_data".into(), "true".into()),
            ("match_all_tags".into(), "true".into()),
        ];
        if self.sort == Sort::Trend && search.is_empty() {
            p.push(("days".into(), "7".into()));
        }
        if !search.is_empty() {
            p.push(("search_text".into(), search.into()));
        }
        let mut required: Vec<String> = Vec::new();
        if let Some(k) = self.kind {
            required.push(
                match k {
                    Kind::Scene => "Scene",
                    Kind::Video => "Video",
                    Kind::Web => "Web",
                    Kind::Application => "Application",
                    Kind::Unknown => "Scene",
                }
                .into(),
            );
        }
        required.extend(self.tag.clone());
        required.extend(self.resolution.clone());
        for (i, t) in required.into_iter().enumerate() {
            p.push((format!("requiredtags[{i}]"), t));
        }
        // "Everyone" content unless the user explicitly allowed more in Settings.
        let mut excluded = Vec::new();
        if !self.questionable {
            excluded.push("Questionable");
        }
        if !self.mature {
            excluded.push("Mature");
        }
        for (i, t) in excluded.into_iter().enumerate() {
            p.push((format!("excludedtags[{i}]"), t.into()));
        }
        p
    }
}

/// Short date in the UI language (`3 nov. 2026` / `Nov 3, 2026`).
pub fn format_date(unix: i64, lang: crate::i18n::Lang) -> String {
    let days = unix.div_euclid(86_400);
    // Civil-from-days (H. Hinnant), proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    const FR: [&str; 12] = [
        "janv.", "févr.", "mars", "avr.", "mai", "juin", "juil.", "août", "sept.", "oct.", "nov.",
        "déc.",
    ];
    const EN: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let mi = (m - 1) as usize;
    match lang {
        crate::i18n::Lang::Fr => format!("{d} {} {y}", FR[mi]),
        crate::i18n::Lang::En => format!("{} {d}, {y}", EN[mi]),
    }
}

/// Compact count: `987`, `12.3k`, `1.2M` (`12,3 k` in French).
pub fn format_count(n: u64, lang: crate::i18n::Lang) -> String {
    let (v, unit) = if n >= 1_000_000 {
        (n as f64 / 1_000_000.0, "M")
    } else if n >= 1_000 {
        (n as f64 / 1_000.0, "k")
    } else {
        return n.to_string();
    };
    let s = if v >= 100.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    };
    let s = s.trim_end_matches(".0").to_owned();
    match lang {
        crate::i18n::Lang::Fr => format!("{} {unit}", s.replace('.', ",")),
        crate::i18n::Lang::En => format!("{s}{unit}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::Lang;

    #[test]
    fn parses_query_files_and_legacy_details() {
        let v: Value = serde_json::from_str(
            r#"{"result":1,"publishedfileid":"3018340632","creator":"76561198000000000","title":" Rain ",
               "file_size":"38240591","preview_url":"https://images.steamusercontent.com/ugc/1/x.gif",
               "time_updated":1700000000,"subscriptions":12345,"favorited":"99",
               "tags":[{"tag":"Scene","display_name":"Scene"},{"tag":"Everyone"},{"tag":"3840 x 2160"},{"tag":"Cyberpunk"}],
               "vote_data":{"score":0.93,"votes_up":900,"votes_down":100},
               "previews":[{"url":"https://images.steamusercontent.com/ugc/2/a.jpg","preview_type":0},{"youtubevideoid":"x","preview_type":1}],
               "file_description":"[b]Hi[/b]"}"#,
        )
        .unwrap();
        let item = Item::from_json(&v).unwrap();
        assert_eq!(item.title, "Rain");
        assert_eq!(item.file_size, Some(38_240_591));
        assert_eq!(item.favorited, Some(99));
        assert_eq!(item.kind(), Some(Kind::Scene));
        assert_eq!(item.rating(), Rating::Everyone);
        assert_eq!(item.resolution(), Some("3840 x 2160"));
        assert_eq!(item.genres().collect::<Vec<_>>(), ["Cyberpunk"]);
        assert_eq!(item.votes, Some(1000));
        assert_eq!(item.screenshots.len(), 1);
        // Legacy ISteamRemoteStorage shape: numbers as numbers, `description`.
        let v: Value = serde_json::from_str(
            r#"{"publishedfileid":"1","result":1,"description":"d","creator":76561198000000001}"#,
        )
        .unwrap();
        let item = Item::from_json(&v).unwrap();
        assert_eq!(item.description, "d");
        assert_eq!(item.creator, "76561198000000001");
        assert!(Item::from_json(&serde_json::json!({"publishedfileid":"2","result":9})).is_none());
    }

    #[test]
    fn query_parameters() {
        let q = Query {
            kind: Some(Kind::Video),
            tag: Some("Anime".into()),
            ..Query::default()
        };
        let p = q.params("*", 30);
        let get = |k: &str| p.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
        assert_eq!(get("query_type"), Some("3"));
        assert_eq!(get("days"), Some("7"));
        assert_eq!(get("requiredtags[0]"), Some("Video"));
        assert_eq!(get("requiredtags[1]"), Some("Anime"));
        assert_eq!(get("excludedtags[0]"), Some("Questionable"));
        assert_eq!(get("excludedtags[1]"), Some("Mature"));
        assert!(get("key").is_none());
        let q = Query {
            search: " rain ".into(),
            questionable: true,
            mature: true,
            ..Query::default()
        };
        let p = q.params("abc", 30);
        let get = |k: &str| p.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str());
        assert_eq!(get("query_type"), Some("12"));
        assert_eq!(get("search_text"), Some("rain"));
        assert_eq!(get("cursor"), Some("abc"));
        assert!(get("excludedtags[0]").is_none());
    }

    #[test]
    fn dates_and_counts() {
        assert_eq!(format_date(0, Lang::En), "Jan 1, 1970");
        assert_eq!(format_date(1_793_664_000, Lang::Fr), "3 nov. 2026");
        assert_eq!(format_count(987, Lang::En), "987");
        assert_eq!(format_count(12_345, Lang::En), "12.3k");
        assert_eq!(format_count(12_345, Lang::Fr), "12,3 k");
        assert_eq!(format_count(2_000_000, Lang::En), "2M");
    }
}
