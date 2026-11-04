//! Log records of the engine process, written by the daemon.
//!
//! The engine process installs [`Forwarder`] as its `tracing` subscriber: each event goes to the
//! daemon as a [`FromHost::Log`](super::wire::FromHost::Log) line (level, target, formatted
//! fields), filtered by the most verbose level the daemon's filter lets through. The daemon
//! re-emits it with [`emit`] through a callsite of the same target and level, so its own filter,
//! log file and console treat it exactly like an event of the daemon (`RUST_LOG` per target
//! included).

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Mutex, OnceLock, PoisonError};

use tracing::callsite::{Callsite, Identifier};
use tracing::field::{Field, FieldSet, Value, Visit};
use tracing::level_filters::LevelFilter;
use tracing::metadata::Kind;
use tracing::span::{Attributes, Id, Record};
use tracing::subscriber::Interest;
use tracing::{Event, Level, Metadata, Subscriber};

/// `Level` ↔ the wire's 1 (error) … 5 (trace); 0 is "off".
pub(super) fn level_number(level: Level) -> u8 {
    match level {
        Level::ERROR => 1,
        Level::WARN => 2,
        Level::INFO => 3,
        Level::DEBUG => 4,
        Level::TRACE => 5,
    }
}

fn level_of(n: u8) -> Option<Level> {
    Some(match n {
        1 => Level::ERROR,
        2 => Level::WARN,
        3 => Level::INFO,
        4 => Level::DEBUG,
        5 => Level::TRACE,
        _ => return None,
    })
}

/// The most verbose level this process's subscriber records, for the engine process.
pub(super) fn current_level() -> u8 {
    match LevelFilter::current().into_level() {
        Some(l) => level_number(l),
        None => 0,
    }
}

/// Receives (level, target, formatted fields) of each forwarded event.
pub(super) type Sink = Box<dyn Fn(u8, &str, String) + Send + Sync>;

/// Subscriber of the engine process: every enabled event is formatted and handed to `send`.
pub(super) struct Forwarder {
    max: LevelFilter,
    send: Sink,
}

impl Forwarder {
    pub(super) fn new(level: u8, send: Sink) -> Self {
        Self {
            max: level_of(level).map_or(LevelFilter::OFF, LevelFilter::from_level),
            send,
        }
    }
}

/// `message` first, then the other fields as ` name=value`, like the daemon's formatter.
#[derive(Default)]
struct Fields {
    message: String,
    rest: String,
}

/// Fields that tracing-log adds to a `log` record (`log.target`, `log.file`…): the target is
/// already the event's own, the rest the daemon's formatter does not show either.
fn is_log_metadata(field: &Field) -> bool {
    field.name().starts_with("log.")
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message.push_str(value);
        } else if is_log_metadata(field) {
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.message, "{value:?}");
        } else if is_log_metadata(field) {
        } else {
            let _ = write!(self.rest, " {}={value:?}", field.name());
        }
    }
}

impl Subscriber for Forwarder {
    fn register_callsite(&self, meta: &'static Metadata<'static>) -> Interest {
        if self.enabled(meta) {
            Interest::always()
        } else {
            Interest::never()
        }
    }

    fn enabled(&self, meta: &Metadata<'_>) -> bool {
        meta.is_event() && self.max >= *meta.level()
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(self.max)
    }

    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }

    fn record(&self, _: &Id, _: &Record<'_>) {}

    fn record_follows_from(&self, _: &Id, _: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let meta = event.metadata();
        fields.message.push_str(&fields.rest);
        (self.send)(level_number(*meta.level()), meta.target(), fields.message);
    }

    fn enter(&self, _: &Id) {}

    fn exit(&self, _: &Id) {}
}

/// A callsite made at run time for one (target, level) of the engine process.
struct Dynamic {
    meta: OnceLock<Metadata<'static>>,
}

impl Callsite for Dynamic {
    fn set_interest(&self, _: Interest) {}

    fn metadata(&self) -> &Metadata<'_> {
        self.meta.get().expect("set before registration")
    }
}

static FIELDS: &[&str] = &["message"];

/// One leaked callsite per (target, level) seen: a few dozen for a whole run.
fn callsite(target: &str, level: Level) -> &'static Dynamic {
    static CALLSITES: Mutex<Option<HashMap<(String, u8), &'static Dynamic>>> = Mutex::new(None);
    let mut map = CALLSITES.lock().unwrap_or_else(PoisonError::into_inner);
    let map = map.get_or_insert_with(HashMap::new);
    let key = (target.to_owned(), level_number(level));
    if let Some(cs) = map.get(&key) {
        return cs;
    }
    let cs: &'static Dynamic = Box::leak(Box::new(Dynamic {
        meta: OnceLock::new(),
    }));
    let target: &'static str = Box::leak(target.to_owned().into_boxed_str());
    let _ = cs.meta.set(Metadata::new(
        "event of the engine process",
        target,
        level,
        None,
        None,
        None,
        FieldSet::new(FIELDS, Identifier(cs)),
        Kind::EVENT,
    ));
    tracing::callsite::register(cs);
    map.insert(key, cs);
    cs
}

/// Writes a record of the engine process through this process's subscriber.
pub(super) fn emit(level: u8, target: &str, message: &str) {
    let Some(level) = level_of(level) else {
        return;
    };
    let cs = callsite(target, level);
    let meta: &'static Metadata<'static> = cs.meta.get().expect("set at creation");
    if !tracing::dispatcher::get_default(|d| d.enabled(meta)) {
        return;
    }
    let Some(field) = meta.fields().field("message") else {
        return;
    };
    let values = [(&field, Some(&message as &dyn Value))];
    Event::dispatch(meta, &meta.fields().value_set(&values));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn levels_round_trip() {
        for l in [
            Level::ERROR,
            Level::WARN,
            Level::INFO,
            Level::DEBUG,
            Level::TRACE,
        ] {
            assert_eq!(level_of(level_number(l)), Some(l));
        }
        assert_eq!(level_of(0), None);
    }

    #[test]
    fn the_forwarder_formats_and_filters() {
        let got = Arc::new(Mutex::new(Vec::new()));
        let sink = got.clone();
        let fwd = Forwarder::new(
            3,
            Box::new(move |level, target, message| {
                sink.lock()
                    .unwrap()
                    .push((level, target.to_owned(), message));
            }),
        );
        tracing::subscriber::with_default(fwd, || {
            tracing::info!(target: "lumenwall_engine::engine", output = "DISPLAY1", "scene loaded in {} ms", 98);
            tracing::debug!("filtered out");
        });
        let got = got.lock().unwrap();
        assert_eq!(
            *got,
            vec![(
                3,
                "lumenwall_engine::engine".to_owned(),
                "scene loaded in 98 ms output=\"DISPLAY1\"".to_owned()
            )]
        );
    }

    /// A re-emitted record reaches the daemon's subscriber with its own target and level.
    #[test]
    fn emitted_records_keep_their_target() {
        struct Capture(Arc<Mutex<Vec<(String, Level, String)>>>);
        impl Subscriber for Capture {
            fn enabled(&self, m: &Metadata<'_>) -> bool {
                m.target() != "noisy::crate"
            }
            fn new_span(&self, _: &Attributes<'_>) -> Id {
                Id::from_u64(1)
            }
            fn record(&self, _: &Id, _: &Record<'_>) {}
            fn record_follows_from(&self, _: &Id, _: &Id) {}
            fn event(&self, e: &Event<'_>) {
                let mut f = Fields::default();
                e.record(&mut f);
                self.0.lock().unwrap().push((
                    e.metadata().target().to_owned(),
                    *e.metadata().level(),
                    f.message,
                ));
            }
            fn enter(&self, _: &Id) {}
            fn exit(&self, _: &Id) {}
        }
        let got = Arc::new(Mutex::new(Vec::new()));
        tracing::subscriber::with_default(Capture(got.clone()), || {
            emit(2, "wgpu_hal::vulkan::adapter", "driver warning");
            emit(3, "noisy::crate", "dropped by the filter");
            emit(0, "x", "off");
        });
        assert_eq!(
            *got.lock().unwrap(),
            vec![(
                "wgpu_hal::vulkan::adapter".to_owned(),
                Level::WARN,
                "driver warning".to_owned()
            )]
        );
    }
}
