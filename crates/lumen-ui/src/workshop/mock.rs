//! `--fake-steam`: a local HTTP server that answers like the documented Steam Web API
//! endpoints Lumen uses, with made-up items and generated images. For tests, demos
//! and screenshots: no real Workshop content, no real key, no Internet.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::demo;

/// The only key the mock accepts (not a real key; never stored).
pub const MOCK_KEY: &str = "00000000000000000000000000000000";

#[derive(Default)]
pub struct Hits {
    pub query_files: AtomicUsize,
    pub file_details: AtomicUsize,
    pub player_summaries: AtomicUsize,
    pub images: AtomicUsize,
}

impl Hits {
    pub fn total(&self) -> usize {
        self.query_files.load(Ordering::Relaxed)
            + self.file_details.load(Ordering::Relaxed)
            + self.player_summaries.load(Ordering::Relaxed)
            + self.images.load(Ordering::Relaxed)
    }
}

pub struct MockSteam {
    pub base: String,
    pub hits: Arc<Hits>,
}

struct MockItem {
    id: String,
    title: String,
    creator: usize,
    tags: Vec<String>,
    hue: f32,
    subs: u64,
    favs: u64,
    up: u64,
    down: u64,
    size: u64,
    created: i64,
    updated: i64,
    trend: u64,
}

const AUTHORS: [&str; 8] = [
    "Nadia Lumière",
    "pixelfox",
    "Kenji Arai",
    "Studio Brume",
    "mira.design",
    "Oskar W.",
    "lowpoly_lena",
    "Atlas Loop",
];
const WORDS_A: [&str; 12] = [
    "Quiet", "Electric", "Golden", "Hidden", "Silver", "Endless", "Velvet", "Crystal", "Lunar",
    "Wild", "Paper", "Midnight",
];
const WORDS_B: [&str; 12] = [
    "Garden", "Skyline", "Harbor", "Forest", "Signal", "Orbit", "Valley", "Lagoon", "Arcade",
    "Comet", "Tide", "Atelier",
];
const RES: [&str; 3] = ["1920 x 1080", "2560 x 1440", "3840 x 2160"];

fn items() -> Vec<MockItem> {
    let mut v = Vec::new();
    for (i, spec) in demo::SPECS.iter().enumerate() {
        let kind = match spec.kind {
            "video" => "Video",
            "web" => "Web",
            "application" => "Application",
            _ => "Scene",
        };
        v.push(make(
            i,
            spec.title.to_owned(),
            kind,
            spec.tags.iter().map(|t| (*t).to_owned()).collect(),
            spec.hue,
        ));
    }
    for i in v.len()..48 {
        let title = format!("{} {}", WORDS_A[(i * 7) % 12], WORDS_B[(i * 5 + 3) % 12]);
        let kind = ["Scene", "Video", "Scene", "Web"][i % 4];
        let genres: Vec<String> = [demo_genre(i), demo_genre(i * 3 + 1)]
            .into_iter()
            .map(str::to_owned)
            .collect();
        v.push(make(i, title, kind, genres, (i as f32 * 0.137).fract()));
    }
    // A few items above "Everyone", to check the content filter.
    for (i, rating) in [(14, "Questionable"), (27, "Mature"), (33, "Questionable")] {
        if let Some(it) = v.get_mut(i) {
            it.tags.retain(|t| t != "Everyone");
            it.tags.push(rating.into());
        }
    }
    v
}

fn demo_genre(i: usize) -> &'static str {
    super::GENRES[i % super::GENRES.len()]
}

fn make(i: usize, title: String, kind: &'static str, mut tags: Vec<String>, hue: f32) -> MockItem {
    let seed = (i as u64).wrapping_mul(2_654_435_761) % 1000;
    tags.push(kind.into());
    tags.push("Everyone".into());
    tags.push(RES[i % 3].into());
    MockItem {
        id: demo::demo_id(i),
        title,
        creator: i % AUTHORS.len(),
        tags,
        hue,
        subs: 2_000 + seed * 913 % 480_000,
        favs: 100 + seed * 37 % 20_000,
        up: 300 + seed * 17 % 9_000,
        down: 10 + seed * 3 % 600,
        size: 4_000_000 + seed * 91_731,
        created: 1_760_000_000 + (i as i64) * 86_400 * 3,
        updated: 1_790_000_000 + (seed as i64) * 1_800,
        trend: (seed * 7919) % 1000,
    }
}

fn creator_id(i: usize) -> String {
    format!("{}", 76_561_198_000_000_000u64 + i as u64)
}

fn description(it: &MockItem) -> String {
    format!(
        "[h1]{}[/h1]A generated [b]demo[/b] wallpaper by [i]{}[/i], made for Lumen's screenshots.\n\n\
         [list][*]Smooth loop at [b]30 fps[/b][*]Customizable colour scheme[*]Works on every resolution[/list]\n\
         [quote]Nothing here comes from the real Workshop.[/quote]\nMore at [url=https://example.org/lumen]example.org[/url].",
        it.title, AUTHORS[it.creator]
    )
}

impl MockItem {
    fn json(&self, base: &str, legacy: bool) -> Value {
        let tags: Vec<Value> = self
            .tags
            .iter()
            .map(|t| json!({"tag": t, "display_name": t}))
            .collect();
        if legacy {
            return json!({
                "publishedfileid": self.id, "result": 1, "creator": creator_id(self.creator),
                "creator_app_id": 431960, "consumer_app_id": 431960, "file_size": self.size,
                "preview_url": format!("{base}/img/{}.gif", self.id), "title": self.title,
                "description": description(self), "time_created": self.created, "time_updated": self.updated,
                "visibility": 0, "subscriptions": self.subs, "favorited": self.favs,
                "lifetime_subscriptions": self.subs + 1000, "views": self.subs * 4, "tags": tags,
            });
        }
        json!({
            "result": 1, "publishedfileid": self.id, "creator": creator_id(self.creator),
            "creator_appid": 431960, "consumer_appid": 431960, "file_size": self.size.to_string(),
            "preview_url": format!("{base}/img/{}.gif", self.id), "title": self.title,
            "file_description": description(self), "time_created": self.created, "time_updated": self.updated,
            "visibility": 0, "subscriptions": self.subs, "favorited": self.favs, "views": self.subs * 4,
            "tags": tags,
            "previews": [
                {"previewid": "1", "sortorder": 1, "url": format!("{base}/img/{}-1.png", self.id), "preview_type": 0},
                {"previewid": "2", "sortorder": 2, "url": format!("{base}/img/{}-2.png", self.id), "preview_type": 0},
                {"previewid": "3", "sortorder": 3, "youtubevideoid": "none", "preview_type": 1},
            ],
            "vote_data": {"score": self.up as f64 / (self.up + self.down) as f64, "votes_up": self.up, "votes_down": self.down},
        })
    }
}

pub fn start() -> std::io::Result<MockSteam> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let base = format!("http://{}", listener.local_addr()?);
    let hits = Arc::new(Hits::default());
    let state = Arc::new(State {
        items: items(),
        images: Mutex::new(HashMap::new()),
        hits: Arc::clone(&hits),
        base: base.clone(),
    });
    std::thread::Builder::new()
        .name("lumen-ui-mock-steam".into())
        .spawn(move || {
            for conn in listener.incoming().flatten() {
                let state = Arc::clone(&state);
                let _ = std::thread::Builder::new()
                    .name("lumen-ui-mock-conn".into())
                    .spawn(move || {
                        let _ = serve(conn, &state);
                    });
            }
        })?;
    tracing::info!(%base, "fake Steam Web API listening");
    Ok(MockSteam { base, hits })
}

struct State {
    items: Vec<MockItem>,
    images: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    hits: Arc<Hits>,
    base: String,
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn form(s: &str) -> Vec<(String, String)> {
    s.split('&')
        .filter(|p| !p.is_empty())
        .map(|p| p.split_once('=').unwrap_or((p, "")))
        .map(|(k, v)| (decode(k), decode(v)))
        .collect()
}

fn serve(stream: TcpStream, state: &State) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let mut len = 0usize;
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 || h == "\r\n" {
            break;
        }
        if let Some((k, v)) = h.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; len.min(1 << 20)];
    reader.read_exact(&mut body)?;
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let mut params = form(query);
    params.extend(form(&String::from_utf8_lossy(&body)));
    let get = |k: &str| param(&params, k).map(str::to_owned);
    let list = |prefix: &str| param_list(&params, prefix);
    tracing::info!(
        method,
        path,
        total = state.hits.total() + 1,
        "fake Steam request"
    );
    let (status, ctype, payload): (&str, &str, Arc<Vec<u8>>) = match path {
        "/IPublishedFileService/QueryFiles/v1/" => {
            state.hits.query_files.fetch_add(1, Ordering::Relaxed);
            if get("key").as_deref() != Some(MOCK_KEY) {
                (
                    "403 Forbidden",
                    "text/html",
                    Arc::new(b"<html>Forbidden</html>".to_vec()),
                )
            } else {
                (
                    "200 OK",
                    "application/json",
                    Arc::new(query_files(state, &params).to_string().into_bytes()),
                )
            }
        }
        "/ISteamRemoteStorage/GetPublishedFileDetails/v1/" if method == "POST" => {
            state.hits.file_details.fetch_add(1, Ordering::Relaxed);
            let ids = list("publishedfileids[");
            let details: Vec<Value> = ids
                .iter()
                .map(|id| match state.items.iter().find(|it| &it.id == id) {
                    Some(it) => it.json(&state.base, true),
                    None => json!({"publishedfileid": id, "result": 9}),
                })
                .collect();
            let v = json!({"response": {"result": 1, "resultcount": details.len(), "publishedfiledetails": details}});
            (
                "200 OK",
                "application/json",
                Arc::new(v.to_string().into_bytes()),
            )
        }
        "/ISteamUser/GetPlayerSummaries/v2/" => {
            state.hits.player_summaries.fetch_add(1, Ordering::Relaxed);
            let joined = get("steamids").unwrap_or_default();
            let ids: Vec<&str> = joined.split(',').collect();
            let players: Vec<Value> = (0..AUTHORS.len())
                .filter(|i| ids.contains(&creator_id(*i).as_str()))
                .map(|i| json!({"steamid": creator_id(i), "personaname": AUTHORS[i]}))
                .collect();
            (
                "200 OK",
                "application/json",
                Arc::new(
                    json!({"response": {"players": players}})
                        .to_string()
                        .into_bytes(),
                ),
            )
        }
        p if p.starts_with("/img/") => {
            state.hits.images.fetch_add(1, Ordering::Relaxed);
            match image(state, &p[5..]) {
                Some((ctype, bytes)) => ("200 OK", ctype, bytes),
                None => (
                    "404 Not Found",
                    "text/plain",
                    Arc::new(b"not found".to_vec()),
                ),
            }
        }
        _ => (
            "404 Not Found",
            "text/plain",
            Arc::new(b"not found".to_vec()),
        ),
    };
    let mut out = stream;
    write!(
        out,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    )?;
    out.write_all(&payload)?;
    out.flush()
}

fn param<'a>(params: &'a [(String, String)], k: &str) -> Option<&'a str> {
    params.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
}

fn param_list(params: &[(String, String)], prefix: &str) -> Vec<String> {
    params
        .iter()
        .filter(|(k, _)| k.starts_with(prefix))
        .map(|(_, v)| v.clone())
        .collect()
}

fn query_files(state: &State, params: &[(String, String)]) -> Value {
    let get = |k: &str| param(params, k);
    let required = param_list(params, "requiredtags[");
    let excluded = param_list(params, "excludedtags[");
    let search = get("search_text").unwrap_or("").to_lowercase();
    let has = |it: &MockItem, t: &str| it.tags.iter().any(|x| x.eq_ignore_ascii_case(t));
    let mut matched: Vec<&MockItem> = state
        .items
        .iter()
        .filter(|it| required.iter().all(|t| has(it, t)))
        .filter(|it| !excluded.iter().any(|t| has(it, t)))
        .filter(|it| {
            search.is_empty()
                || it.title.to_lowercase().contains(&search)
                || it.tags.iter().any(|t| t.to_lowercase() == search)
        })
        .collect();
    match get("query_type").unwrap_or("3") {
        "1" => matched.sort_by_key(|it| std::cmp::Reverse(it.created)),
        "9" | "12" => matched.sort_by_key(|it| std::cmp::Reverse(it.subs)),
        _ => matched.sort_by_key(|it| std::cmp::Reverse(it.trend)),
    }
    let per: usize = get("numperpage")
        .and_then(|n| n.parse().ok())
        .unwrap_or(30)
        .clamp(1, 100);
    let offset: usize = get("cursor")
        .and_then(|c| c.strip_prefix("o"))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    let page: Vec<Value> = matched
        .iter()
        .skip(offset)
        .take(per)
        .map(|it| it.json(&state.base, false))
        .collect();
    let next = if offset + per < matched.len() {
        format!("o{}", offset + per)
    } else {
        String::new()
    };
    json!({"response": {"total": matched.len(), "publishedfiledetails": page, "next_cursor": next}})
}

/// `<id>.gif` (animated preview) or `<id>-<n>.png` (screenshot), generated once.
fn image(state: &State, name: &str) -> Option<(&'static str, Arc<Vec<u8>>)> {
    if let Some(bytes) = state.images.lock().ok()?.get(name).cloned() {
        return Some((
            if name.ends_with(".gif") {
                "image/gif"
            } else {
                "image/png"
            },
            bytes,
        ));
    }
    let (stem, ext) = name.rsplit_once('.')?;
    let (id, shot) = match stem.split_once('-') {
        Some((id, n)) => (id, n.parse::<u32>().ok()),
        None => (stem, None),
    };
    let it = state.items.iter().find(|it| it.id == id)?;
    let mut bytes = Vec::new();
    match (ext, shot) {
        ("gif", None) => {
            use image::codecs::gif::{GifEncoder, Repeat};
            let mut enc = GifEncoder::new_with_speed(&mut bytes, 10);
            enc.set_repeat(Repeat::Infinite).ok()?;
            let frames = (0..20).map(|f| {
                image::Frame::from_parts(
                    demo::art(it.hue, f as f32 / 20.0, 256, 144),
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(70, 1),
                )
            });
            enc.encode_frames(frames).ok()?;
        }
        ("png", Some(n)) => {
            let img = demo::art(it.hue + 0.07 * n as f32, 0.3 * n as f32, 480, 270);
            img.write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .ok()?;
        }
        _ => return None,
    }
    let bytes = Arc::new(bytes);
    state
        .images
        .lock()
        .ok()?
        .insert(name.to_owned(), Arc::clone(&bytes));
    Some((
        if ext == "gif" {
            "image/gif"
        } else {
            "image/png"
        },
        bytes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workshop::{Query, Sort, api};

    /// End to end against the mock: query, pagination, filters, details, names,
    /// images, key rejection. One test because the API base is process-wide.
    #[test]
    fn client_against_mock_server() {
        let mock = start().unwrap();
        api::use_mock_server(mock.base.clone());
        api::use_cache_root(
            std::env::temp_dir().join(format!("lumen-ui-api-cache-{}", std::process::id())),
        );
        let q = Query {
            sort: Sort::Popular,
            ..Query::default()
        };
        let page = api::query_files(&q, "*", MOCK_KEY).unwrap();
        assert_eq!(page.items.len(), 30);
        assert!(
            page.items
                .iter()
                .all(|i| i.rating() == crate::workshop::Rating::Everyone)
        );
        assert!(
            page.items
                .windows(2)
                .all(|w| w[0].subscriptions >= w[1].subscriptions)
        );
        let next = page.next_cursor.clone().unwrap();
        let page2 = api::query_files(&q, &next, MOCK_KEY).unwrap();
        assert!(!page2.items.is_empty() && page2.next_cursor.is_none());
        // Same query again: served from the disk cache, no new request.
        let before = mock.hits.query_files.load(Ordering::Relaxed);
        let _ = api::query_files(&q, "*", MOCK_KEY).unwrap();
        assert_eq!(mock.hits.query_files.load(Ordering::Relaxed), before);

        let q = Query {
            kind: Some(crate::catalog::Kind::Video),
            mature: true,
            questionable: true,
            ..Query::default()
        };
        let videos = api::query_files(&q, "*", MOCK_KEY).unwrap();
        assert!(!videos.items.is_empty());
        assert!(
            videos
                .items
                .iter()
                .all(|i| i.kind() == Some(crate::catalog::Kind::Video))
        );

        let bad = api::query_files(
            &Query {
                search: "x".into(),
                ..Query::default()
            },
            "*",
            "ffffffffffffffffffffffffffffffff",
        );
        assert_eq!(bad.unwrap_err(), api::NetError::Forbidden);

        let details = api::file_details(&[demo::demo_id(0), "1".into()]).unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].title, "Aurora Drift");
        assert!(details[0].subscriptions.is_some());

        let names = api::player_names(&[details[0].creator.clone()], MOCK_KEY).unwrap();
        assert_eq!(
            names.get(&details[0].creator).map(String::as_str),
            Some(AUTHORS[0])
        );

        let url = page.items[0].preview_url.clone().unwrap();
        assert!(api::allowed(&url));
        let dest = std::env::temp_dir().join(format!("lumen-ui-mock-{}.gif", std::process::id()));
        let _ = std::fs::remove_file(&dest);
        api::download(&url, &dest).unwrap();
        assert!(std::fs::read(&dest).unwrap().starts_with(b"GIF8"));
        assert_eq!(
            api::download("https://evil.example/x.gif", &dest.with_extension("x")),
            Err(api::NetError::Refused)
        );
        let _ = std::fs::remove_file(&dest);
    }
}
