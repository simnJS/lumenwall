//! StatusNotifierItem + com.canonical.dbusmenu, implemented directly on zbus
//! (KDE Plasma, most panels, GNOME with the AppIndicator extension).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::mpsc::{self, Sender};

use lumen_ipc::Endpoint;
use zbus::blocking::Connection;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Structure, StructureBuilder, Type, Value};

use super::{Action, Msg, TrayError, TrayState, labels, tooltip};
use crate::i18n::Lang;

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";

type Pixmap = (i32, i32, Vec<u8>);

fn pixmaps() -> Vec<Pixmap> {
    [22u32, 32, 48]
        .iter()
        .map(|&size| {
            // SNI wants ARGB32 in network byte order.
            let rgba = crate::icons::logo_rgba(size, [0xff, 0xb3, 0x47]);
            let argb = rgba
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[3], p[0], p[1], p[2]])
                .collect();
            (size as i32, size as i32, argb)
        })
        .collect()
}

struct Item {
    tx: Mutex<Sender<Msg>>,
    tooltip: String,
    icon: Vec<Pixmap>,
}

impl Item {
    fn send(&self, a: Action) {
        let _ = self.tx.lock().map(|tx| tx.send(Msg::Action(a)));
    }
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        "lumen"
    }
    #[zbus(property)]
    fn title(&self) -> &str {
        "Lumen"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "Active"
    }
    #[zbus(property)]
    fn icon_name(&self) -> &str {
        ""
    }
    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<Pixmap> {
        self.icon.clone()
    }
    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<Pixmap>, String, String) {
        (
            String::new(),
            Vec::new(),
            self.tooltip.clone(),
            String::new(),
        )
    }
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn menu(&self) -> ObjectPath<'_> {
        ObjectPath::from_static_str_unchecked(MENU_PATH)
    }
    /// Left click: open the window.
    fn activate(&self, _x: i32, _y: i32) {
        self.send(Action::Open);
    }
    /// Middle click: pause or resume.
    fn secondary_activate(&self, _x: i32, _y: i32) {
        self.send(Action::TogglePause);
    }
    fn context_menu(&self, _x: i32, _y: i32) {}
    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
}

struct Menu {
    tx: Mutex<Sender<Msg>>,
    entries: Vec<(Action, String, bool)>,
    revision: u32,
}

/// Menu item ids: 0 is the root, entries are 1-based, separators are 100+.
fn action_of(entries: &[(Action, String, bool)], id: i32) -> Option<Action> {
    let i = usize::try_from(id).ok()?.checked_sub(1)?;
    entries.get(i).map(|e| e.0)
}

/// The `(ia{sv}av)` layout node returned by `GetLayout`; children are the same
/// structure wrapped in variants.
#[derive(Debug, serde::Serialize, Type)]
struct Layout {
    id: i32,
    props: HashMap<String, OwnedValue>,
    children: Vec<OwnedValue>,
}

fn node(
    id: i32,
    props: HashMap<String, OwnedValue>,
    children: Vec<OwnedValue>,
) -> Structure<'static> {
    let children: Vec<Value<'static>> = children.into_iter().map(Value::from).collect();
    StructureBuilder::new()
        .add_field(id)
        .add_field(props)
        .append_field(Value::Array(zbus::zvariant::Array::from(children)))
        .build()
        .expect("valid dbusmenu node")
}

fn prop(v: impl Into<Value<'static>>) -> OwnedValue {
    OwnedValue::try_from(v.into()).expect("plain values convert")
}

impl Menu {
    fn item_props(&self, id: i32) -> HashMap<String, OwnedValue> {
        let mut m = HashMap::new();
        if id >= 100 {
            m.insert("type".to_owned(), prop("separator"));
        } else if let Some((_, label, enabled)) = usize::try_from(id - 1)
            .ok()
            .and_then(|i| self.entries.get(i))
        {
            m.insert("label".to_owned(), prop(label.clone()));
            m.insert("enabled".to_owned(), prop(*enabled));
        }
        m
    }

    fn layout(&self) -> Layout {
        let mut children = Vec::new();
        for (i, (action, _, _)) in self.entries.iter().enumerate() {
            let id = i as i32 + 1;
            if *action == Action::Quit {
                children.push(
                    OwnedValue::try_from(Value::from(node(100, self.item_props(100), vec![])))
                        .expect("node"),
                );
            }
            children.push(
                OwnedValue::try_from(Value::from(node(id, self.item_props(id), vec![])))
                    .expect("node"),
            );
        }
        let mut props = HashMap::new();
        props.insert("children-display".to_owned(), prop("submenu"));
        Layout {
            id: 0,
            props,
            children,
        }
    }
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl Menu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    fn get_layout(
        &self,
        _parent_id: i32,
        _recursion_depth: i32,
        _property_names: Vec<String>,
    ) -> (u32, Layout) {
        (self.revision, self.layout())
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        _property_names: Vec<String>,
    ) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        ids.into_iter()
            .map(|id| (id, self.item_props(id)))
            .collect()
    }

    fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedValue> {
        self.item_props(id)
            .remove(&name)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no property {name} on {id}")))
    }

    fn event(&self, id: i32, event_id: &str, _data: Value<'_>, _timestamp: u32) {
        if event_id == "clicked"
            && let Some(a) = action_of(&self.entries, id)
        {
            let _ = self.tx.lock().map(|tx| tx.send(Msg::Action(a)));
        }
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        for (id, event_id, _, _) in &events {
            if event_id == "clicked"
                && let Some(a) = action_of(&self.entries, *id)
            {
                let _ = self.tx.lock().map(|tx| tx.send(Msg::Action(a)));
            }
        }
        Vec::new()
    }

    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    fn about_to_show_group(&self, _ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (Vec::new(), Vec::new())
    }

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;
}

fn other(e: impl std::fmt::Display) -> TrayError {
    TrayError::Other(e.to_string())
}

pub fn run(lang: Lang, endpoint: Endpoint) -> Result<(), TrayError> {
    let conn = Connection::session().map_err(other)?;
    let dbus = zbus::blocking::fdo::DBusProxy::new(&conn).map_err(other)?;
    let watcher = zbus::names::BusName::try_from("org.kde.StatusNotifierWatcher").map_err(other)?;
    if !dbus.name_has_owner(watcher).map_err(other)? {
        return Err(TrayError::NoHost);
    }
    let (tx, rx) = mpsc::channel();
    let mut state = TrayState::default();
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    let conn = zbus::blocking::connection::Builder::session()
        .map_err(other)?
        .name(name.as_str())
        .map_err(other)?
        .serve_at(
            ITEM_PATH,
            Item {
                tx: Mutex::new(tx.clone()),
                tooltip: tooltip(lang, &state),
                icon: pixmaps(),
            },
        )
        .map_err(other)?
        .serve_at(
            MENU_PATH,
            Menu {
                tx: Mutex::new(tx.clone()),
                entries: labels(lang, &state),
                revision: 1,
            },
        )
        .map_err(other)?
        .build()
        .map_err(other)?;
    conn.call_method(
        Some("org.kde.StatusNotifierWatcher"),
        "/StatusNotifierWatcher",
        Some("org.kde.StatusNotifierWatcher"),
        "RegisterStatusNotifierItem",
        &(name.as_str(),),
    )
    .map_err(other)?;
    tracing::info!("tray icon registered as {name}");

    let wake = || {};
    let mut client = super::connect(&endpoint, &tx, wake);
    if client.is_none() {
        let _ = tx.send(Msg::Daemon(None));
    }
    let item = conn
        .object_server()
        .interface::<_, Item>(ITEM_PATH)
        .map_err(other)?;
    let menu = conn
        .object_server()
        .interface::<_, Menu>(MENU_PATH)
        .map_err(other)?;
    while let Ok(msg) = rx.recv() {
        match msg {
            Msg::Action(a) => {
                if !super::perform(a, &mut client, &state, &endpoint, &tx, wake) {
                    break;
                }
            }
            Msg::Daemon(s) => {
                if s.is_none() {
                    client = None;
                }
                let new = TrayState::from_daemon(s.as_ref());
                if new == state {
                    continue;
                }
                state = new;
                let revision = {
                    let mut m = menu.get_mut();
                    m.entries = labels(lang, &state);
                    m.revision += 1;
                    m.revision
                };
                item.get_mut().tooltip = tooltip(lang, &state);
                let _ = zbus::block_on(Menu::layout_updated(menu.signal_emitter(), revision, 0));
                let _ = zbus::block_on(Item::new_tool_tip(item.signal_emitter()));
            }
        }
    }
    Ok(())
}
