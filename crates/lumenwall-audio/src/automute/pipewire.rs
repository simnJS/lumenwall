//! PipeWire native automute (spec 07 §12.1) from registry and node events.
//!
//! Tracks `Stream/Output/Audio` nodes: "another application plays" ⇔ one of them is media
//! being played ([`classify`]): node running, not muted, audible average channel volume, not
//! ours, not a call. States come from node `info` events, volumes and mute from `Props`
//! parameter events; nothing is polled. Not compiled on the development machine (see README).

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

use super::{ChangeCallback, PlaybackWatcher, PlayingFlag, Stream, Verdict, classify, is_own_pid};
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
        let thread = PwThread::spawn("lumenwall-automute", body)
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
    app_name: Option<String>,
    binary: Option<String>,
    role: Option<String>,
    /// Node state is `running` (an idle or suspended stream plays nothing).
    running: bool,
    muted: bool,
    volume: f32,
    _node: pw::node::Node,
    _listener: pw::node::NodeListener,
}

impl Entry {
    fn stream(&self, own_pid: u32) -> Stream<'_> {
        Stream {
            ours: self.pid.as_deref().is_some_and(|p| is_own_pid(p, own_pid)),
            has_owner: self.pid.is_some(),
            app_name: self.app_name.as_deref(),
            binary: self.binary.as_deref(),
            role: self.role.as_deref(),
            paused: !self.running,
            muted: self.muted,
            volume: self.volume,
        }
    }
}

struct Nodes {
    map: HashMap<u32, Entry>,
    own_pid: u32,
    flag: PlayingFlag,
}

impl Nodes {
    fn recompute(&self) {
        let media = self.map.values().find_map(|e| {
            let s = e.stream(self.own_pid);
            (classify(&s) == Verdict::Media).then(|| super::describe(&s))
        });
        self.flag.set_media(media);
    }
}

/// Average of `channelVolumes` and the `mute` flag of a `Props` pod, each `None` if absent.
fn volume_and_mute(pod: &Pod) -> (Option<f32>, Option<bool>) {
    let Ok((_, Value::Object(obj))) = PodDeserializer::deserialize_any_from(pod.as_bytes()) else {
        return (None, None);
    };
    let mut volume = None;
    let mut mute = None;
    for p in &obj.properties {
        if p.key == spa::sys::SPA_PROP_channelVolumes {
            if let Value::ValueArray(ValueArray::Float(v)) = &p.value
                && !v.is_empty()
            {
                volume = Some(v.iter().sum::<f32>() / v.len() as f32);
            }
        } else if p.key == spa::sys::SPA_PROP_mute
            && let Value::Bool(m) = &p.value
        {
            mute = Some(*m);
        }
    }
    (volume, mute)
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
                    .info({
                        let nodes: Weak<RefCell<Nodes>> = Rc::downgrade(&nodes);
                        move |info| {
                            let Some(nodes) = nodes.upgrade() else { return };
                            let mut n = nodes.borrow_mut();
                            if let Some(e) = n.map.get_mut(&id) {
                                // Raw state: `NodeInfoRef::state` panics on unknown values.
                                e.running = info.as_raw().state
                                    == pw::sys::pw_node_state_PW_NODE_STATE_RUNNING;
                                // Some clients set their role after the node is announced.
                                if let Some(role) =
                                    info.props().and_then(|p| p.get(*pw::keys::MEDIA_ROLE))
                                {
                                    e.role = Some(role.to_owned());
                                }
                            }
                            n.recompute();
                        }
                    })
                    .param({
                        let nodes: Weak<RefCell<Nodes>> = Rc::downgrade(&nodes);
                        move |_seq, kind, _index, _next, param| {
                            if kind != ParamType::Props {
                                return;
                            }
                            let Some((volume, mute)) = param.map(volume_and_mute) else {
                                return;
                            };
                            let Some(nodes) = nodes.upgrade() else { return };
                            let mut n = nodes.borrow_mut();
                            if let Some(e) = n.map.get_mut(&id) {
                                if let Some(v) = volume {
                                    e.volume = v;
                                }
                                if let Some(m) = mute {
                                    e.muted = m;
                                }
                            }
                            n.recompute();
                        }
                    })
                    .register();
                node.subscribe_params(&[ParamType::Props]);
                let text = |key: &str| props.get(key).map(str::to_owned);
                let mut n = nodes.borrow_mut();
                n.map.insert(
                    id,
                    Entry {
                        pid: text(*pw::keys::APP_PROCESS_ID),
                        app_name: text(*pw::keys::APP_NAME),
                        binary: text(*pw::keys::APP_PROCESS_BINARY),
                        role: text(*pw::keys::MEDIA_ROLE),
                        // Counted once the node reports that it runs.
                        running: false,
                        muted: false,
                        volume: 1.0,
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
