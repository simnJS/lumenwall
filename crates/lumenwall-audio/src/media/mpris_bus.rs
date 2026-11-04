//! Linux media source: MPRIS players on the session bus (spec 07 §16).
//!
//! At start: `ListNames`, the owner and `PlaybackStatus` of each `org.mpris.MediaPlayer2.*`
//! name, then `GetAll` of the player picked by [`mpris::pick`]. Afterwards only signals:
//! `PropertiesChanged` of `org.mpris.MediaPlayer2.Player` (a player that starts playing becomes
//! the current one, §16.3; the others are ignored, QO-23), `Seeked` of the current player, and
//! `NameOwnerChanged` for players that appear or quit. The position is read again only when
//! the playback state changes (the reference reads it every frame, QO-23).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::thread;
use std::time::Instant;

use zbus::{Connection, Message, OwnedVariant, Variant};

use super::artwork::{MAX_SOURCE_BYTES, decode_artwork};
use super::mpris::{self, PlayerProps, Prop};
use super::{Artwork, MediaSink, MediaState, NowPlaying, PlaybackState};
use crate::dbus::{Next, Stop, StopOnDrop, next, signal_rule, stream};

const DBUS: &str = "org.freedesktop.DBus";
const DBUS_PATH: &str = "/org/freedesktop/DBus";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

pub(super) fn start(sink: MediaSink) -> Box<dyn Send> {
    let stop = Stop::default();
    let thread_stop = stop.clone();
    let spawned = thread::Builder::new()
        .name("lumenwall-media".into())
        .spawn(move || {
            if let Err(e) = zbus::block_on(run(&sink, &thread_stop)) {
                tracing::info!("media integration: session bus unavailable ({e})");
                sink.publish(MediaState::default());
            }
        });
    if let Err(e) = spawned {
        tracing::warn!("media integration: cannot start its thread: {e}");
    }
    Box::new(StopOnDrop(stop))
}

/// The player followed.
struct Current {
    /// Well-known name (`org.mpris.MediaPlayer2.spotify`).
    name: String,
    /// Unique name of its owner (signals carry it).
    owner: String,
    props: PlayerProps,
    sampled: Instant,
    /// Artwork decoded for `art_url`.
    art_url: String,
    artwork: Option<Arc<Artwork>>,
}

impl Current {
    fn now_playing(&self) -> NowPlaying {
        let p = &self.props;
        NowPlaying {
            title: p.title.clone(),
            artist: p.artist.clone(),
            sub_title: String::new(),
            album_title: p.album.clone(),
            album_artist: p.album_artist.clone(),
            genres: p.genres.clone(),
            content_type: String::new(),
            state: p.state,
            position: p.position_us as f64 / 1e6,
            duration: p.length_us as f64 / 1e6,
            sampled: self.sampled,
            artwork: self.artwork.clone(),
        }
    }

    /// Loads the artwork when its URL changed (local files only).
    fn refresh_artwork(&mut self) {
        if self.props.art_url == self.art_url {
            return;
        }
        self.art_url = self.props.art_url.clone();
        self.artwork = load_artwork(&self.art_url).map(Arc::new);
    }
}

fn load_artwork(url: &str) -> Option<Artwork> {
    if url.is_empty() {
        return None;
    }
    let Some(path) = mpris::file_url_path(url) else {
        tracing::debug!("media artwork {url}: not a local file, not downloaded");
        return None;
    };
    read_file(&path).and_then(|bytes| decode_artwork(&bytes))
}

fn read_file(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_SOURCE_BYTES as u64 {
        return None;
    }
    std::fs::read(path)
        .inspect_err(|e| tracing::debug!("media artwork {}: {e}", path.display()))
        .ok()
}

async fn run(sink: &MediaSink, stop: &Stop) -> zbus::Result<()> {
    let conn = Connection::session().await?;
    let mut streams = [
        stream(
            &conn,
            signal_rule(
                PROPERTIES,
                "PropertiesChanged",
                Some(mpris::PATH),
                Some(mpris::PLAYER),
            )?,
        )
        .await?,
        stream(
            &conn,
            signal_rule(mpris::PLAYER, "Seeked", Some(mpris::PATH), None)?,
        )
        .await?,
        stream(
            &conn,
            zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface(DBUS)
                .member("NameOwnerChanged")
                .path(DBUS_PATH)
                .arg0ns("org.mpris.MediaPlayer2")
                .build()?,
        )
        .await?,
    ];
    // Well-known player name → unique owner name, in bus order.
    let mut players: Vec<(String, String)> = Vec::new();
    let names: Vec<String> = conn
        .call_method(Some(DBUS), DBUS_PATH, Some(DBUS), "ListNames", &())
        .await?
        .body()
        .deserialize()?;
    for name in names.into_iter().filter(|n| mpris::is_player(n)) {
        if let Ok(owner) = owner_of(&conn, &name).await {
            players.push((name, owner));
        }
    }
    let mut current = choose(&conn, &players).await;
    tracing::debug!(
        "media integration: {} MPRIS player(s), following {:?}",
        players.len(),
        current.as_ref().map(|c| &c.name)
    );
    publish(sink, current.as_ref());
    loop {
        let msg = match next(stop, &mut streams).await {
            Next::Stop => return Ok(()),
            Next::Closed => {
                tracing::info!("media integration: session bus connection closed");
                sink.publish(MediaState::default());
                return Ok(());
            }
            Next::Message(i, msg) => (i, msg),
        };
        let changed = match msg {
            (0, msg) => properties_changed(&conn, &msg, &players, &mut current).await,
            (1, msg) => seeked(&msg, &mut current),
            (_, msg) => owner_changed(&conn, &msg, &mut players, &mut current).await,
        };
        if changed && !publish(sink, current.as_ref()) {
            return Ok(());
        }
    }
}

fn publish(sink: &MediaSink, current: Option<&Current>) -> bool {
    sink.publish(MediaState {
        available: true,
        session: current.map(Current::now_playing),
    })
}

fn sender(msg: &Message) -> Option<String> {
    msg.header().sender().map(|s| s.to_string())
}

async fn owner_of(conn: &Connection, name: &str) -> zbus::Result<String> {
    conn.call_method(Some(DBUS), DBUS_PATH, Some(DBUS), "GetNameOwner", &(name,))
        .await?
        .body()
        .deserialize()
}

/// Reads every Player property of `name`.
async fn get_all(conn: &Connection, name: &str) -> zbus::Result<Vec<(String, Prop)>> {
    let map: HashMap<String, OwnedVariant> = conn
        .call_method(
            Some(name),
            mpris::PATH,
            Some(PROPERTIES),
            "GetAll",
            &(mpris::PLAYER,),
        )
        .await?
        .body()
        .deserialize()?;
    Ok(props(&map))
}

async fn get(conn: &Connection, name: &str, property: &str) -> zbus::Result<Prop> {
    let v: OwnedVariant = conn
        .call_method(
            Some(name),
            mpris::PATH,
            Some(PROPERTIES),
            "Get",
            &(mpris::PLAYER, property),
        )
        .await?
        .body()
        .deserialize()?;
    Ok(prop(&v))
}

/// The player to follow among `players`, fully read.
async fn choose(conn: &Connection, players: &[(String, String)]) -> Option<Current> {
    let mut states = Vec::with_capacity(players.len());
    for (name, _) in players {
        let state = match get(conn, name, "PlaybackStatus").await {
            Ok(Prop::Str(s)) => mpris::playback_state(&s),
            _ => PlaybackState::Stopped,
        };
        states.push(state);
    }
    let (name, owner) = players.get(mpris::pick(&states)?)?;
    load(conn, name, owner).await
}

async fn load(conn: &Connection, name: &str, owner: &str) -> Option<Current> {
    let all = get_all(conn, name)
        .await
        .inspect_err(|e| tracing::debug!("media integration: {name}: {e}"))
        .ok()?;
    let mut props = PlayerProps::default();
    props.apply(&all);
    let mut c = Current {
        name: name.to_owned(),
        owner: owner.to_owned(),
        props,
        sampled: Instant::now(),
        art_url: String::new(),
        artwork: None,
    };
    c.refresh_artwork();
    Some(c)
}

async fn properties_changed(
    conn: &Connection,
    msg: &Message,
    players: &[(String, String)],
    current: &mut Option<Current>,
) -> bool {
    let Ok((interface, changed, _)) =
        msg.body()
            .deserialize::<(String, HashMap<String, OwnedVariant>, Vec<String>)>()
    else {
        return false;
    };
    if interface != mpris::PLAYER {
        return false;
    }
    let Some(from) = sender(msg) else {
        return false;
    };
    let changed = props(&changed);
    if current.as_ref().is_some_and(|c| c.owner == from) {
        let Some(c) = current.as_mut() else {
            return false;
        };
        let what = c.props.apply(&changed);
        if what.status {
            // Where the player stands now that it started, paused or stopped.
            if let Ok(Prop::Int(p)) = get(conn, &c.name, "Position").await {
                c.props.position_us = p;
            }
        }
        if what.status || what.position {
            c.sampled = Instant::now();
        }
        if what.metadata {
            c.refresh_artwork();
        }
        return what != Default::default();
    }
    // Another player: it becomes the current one when it starts playing (§16.3).
    let starts = changed
        .iter()
        .any(|(k, v)| k == "PlaybackStatus" && matches!(v, Prop::Str(s) if s == "Playing"));
    if !starts {
        return false;
    }
    let Some((name, owner)) = players.iter().find(|(_, o)| *o == from) else {
        return false;
    };
    match load(conn, name, owner).await {
        Some(c) => {
            tracing::debug!("media integration: following {name}");
            *current = Some(c);
            true
        }
        None => false,
    }
}

fn seeked(msg: &Message, current: &mut Option<Current>) -> bool {
    let (Some(from), Some(c)) = (sender(msg), current.as_mut()) else {
        return false;
    };
    if c.owner != from {
        return false;
    }
    let Ok(position) = msg.body().deserialize::<i64>() else {
        return false;
    };
    c.props.position_us = position;
    c.sampled = Instant::now();
    true
}

async fn owner_changed(
    conn: &Connection,
    msg: &Message,
    players: &mut Vec<(String, String)>,
    current: &mut Option<Current>,
) -> bool {
    let Ok((name, _old, new)) = msg.body().deserialize::<(String, String, String)>() else {
        return false;
    };
    if !mpris::is_player(&name) {
        return false;
    }
    players.retain(|(n, _)| *n != name);
    if !new.is_empty() {
        players.push((name.clone(), new));
    }
    let lost = current.as_ref().is_some_and(|c| c.name == name);
    if !lost && current.is_some() {
        return false;
    }
    // The current player quit (or none was followed): pick again.
    *current = choose(conn, players).await;
    true
}

fn props(map: &HashMap<String, OwnedVariant>) -> Vec<(String, Prop)> {
    map.iter().map(|(k, v)| (k.clone(), prop(v))).collect()
}

/// A D-Bus value as a [`Prop`].
fn prop(v: &Variant<'_>) -> Prop {
    match v {
        Variant::Bool(b) => Prop::Bool(*b),
        Variant::U8(i) => Prop::Int(i64::from(*i)),
        Variant::I16(i) => Prop::Int(i64::from(*i)),
        Variant::U16(i) => Prop::Int(i64::from(*i)),
        Variant::I32(i) => Prop::Int(i64::from(*i)),
        Variant::U32(i) => Prop::Int(i64::from(*i)),
        Variant::I64(i) => Prop::Int(*i),
        Variant::U64(i) => Prop::Int(i64::try_from(*i).unwrap_or(i64::MAX)),
        Variant::F64(f) => Prop::Float(*f),
        Variant::Str(s) => Prop::Str(s.to_string()),
        Variant::ObjectPath(p) => Prop::Str(p.to_string()),
        Variant::Variant(inner) => prop(inner),
        Variant::Array(a) => {
            let mut list = Vec::with_capacity(a.len());
            for item in a.iter() {
                match prop(item) {
                    Prop::Str(s) => list.push(s),
                    _ => return Prop::Other,
                }
            }
            Prop::Strs(list)
        }
        Variant::Dict(d) => Prop::Map(
            d.iter()
                .filter_map(|(k, v)| match prop(k) {
                    Prop::Str(k) => Some((k, prop(v))),
                    _ => None,
                })
                .collect(),
        ),
        _ => Prop::Other,
    }
}
