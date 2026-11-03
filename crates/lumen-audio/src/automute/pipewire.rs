//! PipeWire native automute (spec 07 §12.1) from registry and node events.
//!
//! Tracks `Stream/Output/Audio` nodes: "another application plays" ⇔ one of them carries an
//! `application.process.id` different from ours and a non-zero average channel volume (the
//! PipeWire view of the reference's sink-input rule). Volumes come from `Props` parameter
//! events; nothing is polled. Not compiled on the development machine (see README).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::{Rc, Weak};
use std::sync::atomic::AtomicBool;

use pipewire as pw;
use pw::spa;
use pw::types::ObjectType;
use spa::param::ParamType;
use spa::pod::deserialize::PodDeserializer;
use spa::pod::{Pod, Value, ValueArray};

use super::is_own_pid;
use super::{ChangeCallback, PlaybackWatcher, PlayingFlag};
use crate::capture::pipewire::{CONNECT_TIMEOUT, PwThread, connect, retry_loop};

/// PipeWire watcher thread.
pub struct PipeWireWatcher {
    flag: PlayingFlag,
    _thread: Option<PwThread>,
}

impl PipeWireWatcher {
    /// Starts watching; never blocks.
    pub fn new(on_change: Option<ChangeCallback>) -> Self {
        let flag = PlayingFlag::new(on_change);
        let f = flag.clone();
        let body = move |ml: &pw::main_loop::MainLoopRc, stop: &AtomicBool| {
            let lost = f.clone();
            retry_loop(ml, stop, |ml| session(ml, &f), || lost.set_connected(false));
        };
        let thread = PwThread::spawn("lumen-automute", body)
            .map_err(|e| tracing::error!("automute: cannot spawn thread: {e}"))
            .ok();
        Self {
            flag,
            _thread: thread,
        }
    }
}

impl PlaybackWatcher for PipeWireWatcher {
    fn other_app_playing(&self) -> bool {
        self.flag.playing()
    }
    fn is_connected(&self) -> bool {
        self.flag.connected()
    }
}

struct Entry {
    pid: Option<String>,
    audible: bool,
    _node: pw::node::Node,
    _listener: pw::node::NodeListener,
}

struct Nodes {
    map: HashMap<u32, Entry>,
    own_pid: u32,
    flag: PlayingFlag,
}

impl Nodes {
    fn recompute(&self) {
        // parity: PID present and different from ours, non-zero average volume; paused or muted
        // streams still count (QO-13).
        let playing = self.map.values().any(|e| {
            e.audible
                && e.pid
                    .as_deref()
                    .is_some_and(|p| !is_own_pid(p, self.own_pid))
        });
        self.flag.set_playing(playing);
    }
}

/// Average of `channelVolumes` in a `Props` pod; `None` if absent.
fn average_volume(pod: &Pod) -> Option<f32> {
    let (_, value) = PodDeserializer::deserialize_any_from(pod.as_bytes()).ok()?;
    let Value::Object(obj) = value else {
        return None;
    };
    obj.properties
        .iter()
        .find(|p| p.key == spa::sys::SPA_PROP_channelVolumes)
        .and_then(|p| match &p.value {
            Value::ValueArray(ValueArray::Float(v)) if !v.is_empty() => {
                Some(v.iter().sum::<f32>() / v.len() as f32)
            }
            _ => None,
        })
}

fn session(mainloop: &pw::main_loop::MainLoopRc, flag: &PlayingFlag) -> bool {
    let Some((_context, core, _core_listener, alive)) = connect(mainloop) else {
        return false;
    };
    let Ok(registry) = core.get_registry_rc() else {
        return false;
    };
    let nodes = Rc::new(RefCell::new(Nodes {
        map: HashMap::new(),
        own_pid: std::process::id(),
        flag: flag.clone(),
    }));
    let weak = registry.downgrade();
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let nodes = nodes.clone();
            move |global| {
                if global.type_ != ObjectType::Node {
                    return;
                }
                let Some(props) = global.props else { return };
                if props.get(*pw::keys::MEDIA_CLASS) != Some("Stream/Output/Audio") {
                    return;
                }
                let Some(registry) = weak.upgrade() else {
                    return;
                };
                let Ok(node) = registry.bind::<pw::node::Node, _>(global) else {
                    return;
                };
                let id = global.id;
                let listener = node
                    .add_listener_local()
                    .param({
                        let nodes: Weak<RefCell<Nodes>> = Rc::downgrade(&nodes);
                        move |_seq, kind, _index, _next, param| {
                            if kind != ParamType::Props {
                                return;
                            }
                            let Some(avg) = param.and_then(average_volume) else {
                                return;
                            };
                            let Some(nodes) = nodes.upgrade() else { return };
                            let mut n = nodes.borrow_mut();
                            if let Some(e) = n.map.get_mut(&id) {
                                e.audible = avg > 0.0;
                            }
                            n.recompute();
                        }
                    })
                    .register();
                node.subscribe_params(&[ParamType::Props]);
                let pid = props.get(*pw::keys::APP_PROCESS_ID).map(str::to_owned);
                let mut n = nodes.borrow_mut();
                n.map.insert(
                    id,
                    Entry {
                        pid,
                        audible: true,
                        _node: node,
                        _listener: listener,
                    },
                );
                n.recompute();
            }
        })
        .global_remove({
            let nodes = nodes.clone();
            move |id| {
                let removed = nodes.borrow_mut().map.remove(&id);
                if removed.is_some() {
                    nodes.borrow().recompute();
                }
                drop(removed);
            }
        })
        .register();
    // Connected once the daemon answers; give up after the timeout (§12.2).
    let timeout = mainloop.loop_().add_timer({
        let ml = mainloop.clone();
        let alive = alive.clone();
        let flag = flag.clone();
        move |_| {
            if alive.get() {
                flag.set_connected(true);
            } else {
                ml.quit();
            }
        }
    });
    let _ = timeout.update_timer(Some(CONNECT_TIMEOUT), None);
    mainloop.run();
    nodes.borrow_mut().map.clear();
    alive.get()
}
