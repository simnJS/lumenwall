//! Linux power source: UPower's `OnBattery` on the system bus (`org.freedesktop.UPower`,
//! `/org/freedesktop/UPower`). Read once, then followed through `PropertiesChanged`; read again
//! when UPower (re)appears on the bus. One thread, blocked on the bus socket.

use std::collections::HashMap;
use std::thread;

use zbus::{Connection, Message, OwnedVariant, Variant};

use super::PowerCallback;
use crate::dbus::{Next, Stop, StopOnDrop, next, signal_rule, stream};

const NAME: &str = "org.freedesktop.UPower";
const PATH: &str = "/org/freedesktop/UPower";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

pub(super) fn start(on_change: PowerCallback) -> Option<StopOnDrop> {
    let stop = Stop::default();
    let thread_stop = stop.clone();
    let spawned = thread::Builder::new()
        .name("lumenwall-power".into())
        .spawn(move || {
            if let Err(e) = zbus::block_on(run(&on_change, &thread_stop)) {
                tracing::info!("power source: UPower unavailable ({e}); battery is not followed");
            }
        });
    match spawned {
        Ok(_) => Some(StopOnDrop(stop)),
        Err(e) => {
            tracing::warn!("power source: cannot start its thread: {e}");
            None
        }
    }
}

async fn run(on_change: &PowerCallback, stop: &Stop) -> zbus::Result<()> {
    let conn = Connection::system().await?;
    let mut streams = [
        stream(
            &conn,
            signal_rule(PROPERTIES, "PropertiesChanged", Some(PATH), Some(NAME))?,
        )
        .await?,
        stream(
            &conn,
            signal_rule(
                "org.freedesktop.DBus",
                "NameOwnerChanged",
                Some("/org/freedesktop/DBus"),
                Some(NAME),
            )?,
        )
        .await?,
    ];
    match read(&conn).await {
        Ok(on_battery) => on_change(on_battery),
        // UPower not running yet: its NameOwnerChanged brings it.
        Err(e) => tracing::info!("power source: UPower: {e}"),
    }
    tracing::debug!("power source: following UPower OnBattery");
    loop {
        match next(stop, &mut streams).await {
            Next::Stop => return Ok(()),
            Next::Closed => {
                tracing::info!("power source: system bus connection closed");
                return Ok(());
            }
            Next::Message(0, msg) => {
                if let Some(on_battery) = changed_on_battery(&msg) {
                    on_change(on_battery);
                }
            }
            Next::Message(_, msg) => {
                let appeared = msg
                    .body()
                    .deserialize::<(String, String, String)>()
                    .is_ok_and(|(_, _, new)| !new.is_empty());
                if appeared && let Ok(on_battery) = read(&conn).await {
                    on_change(on_battery);
                }
            }
        }
    }
}

async fn read(conn: &Connection) -> zbus::Result<bool> {
    let reply = conn
        .call_method(
            Some(NAME),
            PATH,
            Some(PROPERTIES),
            "Get",
            &(NAME, "OnBattery"),
        )
        .await?;
    let value: OwnedVariant = reply.body().deserialize()?;
    as_bool(&value).ok_or_else(|| zbus::Error::Failure("OnBattery is not a boolean".into()))
}

/// `OnBattery` in a `PropertiesChanged(s interface, a{sv} changed, as invalidated)`.
fn changed_on_battery(msg: &Message) -> Option<bool> {
    let (interface, changed, _): (String, HashMap<String, OwnedVariant>, Vec<String>) =
        msg.body().deserialize().ok()?;
    if interface != NAME {
        return None;
    }
    changed.get("OnBattery").and_then(|v| as_bool(v))
}

fn as_bool(v: &Variant<'_>) -> Option<bool> {
    match v {
        Variant::Bool(b) => Some(*b),
        Variant::Variant(inner) => as_bool(inner),
        _ => None,
    }
}
