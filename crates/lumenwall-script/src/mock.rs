//! In-memory [`SceneHost`] for tests, benchmarks and examples.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::host::{Clock, Cursor, Display, LogLevel, SceneHost};
use crate::value::{LayerId, ObjectRef, Value};

/// A layer of the mock scene.
#[derive(Clone, Debug)]
pub struct MockLayer {
    pub id: LayerId,
    pub name: String,
    /// Non-scriptable objects are skipped by `thisScene.getLayer` (spec 06 §6.3).
    pub scriptable: bool,
}

/// A scene kept in hash maps. Every member write is accepted and recorded.
#[derive(Debug)]
pub struct MockHost {
    pub clock: Clock,
    pub cursor: Cursor,
    pub display: Display,
    /// Monotonic time base; [`MockHost::advance`] moves `elapsed`.
    pub start: Instant,
    pub elapsed: Duration,
    /// Layers in render order.
    pub layers: Vec<MockLayer>,
    pub members: HashMap<(ObjectRef, String), Value>,
    pub script_properties: HashMap<(u64, String), Value>,
    /// Spectrum returned for every registered resolution (repeated to fill).
    pub audio: Option<(Vec<f32>, Vec<f32>)>,
    pub logs: Vec<(LogLevel, String)>,
    /// Every `call` received: (object, method, arguments).
    pub calls: Vec<(ObjectRef, String, Vec<Value>)>,
    /// Canned results for `call`.
    pub call_results: HashMap<(ObjectRef, String), Value>,
    pub assets: Vec<(String, bool)>,
    pub shortcuts: Vec<String>,
    /// Number of `set` calls.
    pub writes: usize,
}

impl Default for MockHost {
    fn default() -> Self {
        MockHost {
            clock: Clock::default(),
            cursor: Cursor::default(),
            display: Display::default(),
            start: Instant::now(),
            elapsed: Duration::ZERO,
            layers: Vec::new(),
            members: HashMap::new(),
            script_properties: HashMap::new(),
            audio: None,
            logs: Vec::new(),
            calls: Vec::new(),
            call_results: HashMap::new(),
            assets: Vec::new(),
            shortcuts: Vec::new(),
            writes: 0,
        }
    }
}

impl MockHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a scriptable layer at the end of the render order.
    pub fn add_layer(&mut self, id: LayerId, name: &str) -> &mut Self {
        self.layers.push(MockLayer {
            id,
            name: name.to_owned(),
            scriptable: true,
        });
        self.members.insert(
            (ObjectRef::Layer(id), "name".to_owned()),
            Value::String(name.to_owned()),
        );
        self
    }

    /// Sets a member value without counting it as a script write.
    pub fn set_member(&mut self, obj: ObjectRef, name: &str, value: Value) -> &mut Self {
        self.members.insert((obj, name.to_owned()), value);
        self
    }

    pub fn member(&self, obj: ObjectRef, name: &str) -> Option<&Value> {
        self.members.get(&(obj, name.to_owned()))
    }

    /// Advances the scene clock and the monotonic time by `dt` seconds.
    pub fn advance(&mut self, dt: f32) {
        self.clock.runtime += dt;
        self.clock.frametime = dt;
        self.elapsed += Duration::from_secs_f32(dt);
    }

    /// Number of log lines containing `needle`.
    pub fn logged(&self, needle: &str) -> usize {
        self.logs.iter().filter(|(_, m)| m.contains(needle)).count()
    }
}

impl SceneHost for MockHost {
    fn clock(&self) -> Clock {
        self.clock
    }

    fn now(&self) -> Instant {
        self.start + self.elapsed
    }

    fn cursor(&self) -> Cursor {
        self.cursor
    }

    fn display(&self) -> Display {
        self.display
    }

    fn audio_spectrum(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let Some((l, r)) = &self.audio else {
            return false;
        };
        for (i, v) in left.iter_mut().enumerate() {
            *v = l.get(i % l.len().max(1)).copied().unwrap_or(0.0);
        }
        for (i, v) in right.iter_mut().enumerate() {
            *v = r.get(i % r.len().max(1)).copied().unwrap_or(0.0);
        }
        true
    }

    fn get(&self, obj: ObjectRef, name: &str) -> Option<Value> {
        self.members.get(&(obj, name.to_owned())).cloned()
    }

    fn set(&mut self, obj: ObjectRef, name: &str, value: &Value) -> bool {
        self.writes += 1;
        self.members.insert((obj, name.to_owned()), value.clone());
        true
    }

    fn call(&mut self, obj: ObjectRef, method: &str, args: &[Value]) -> Option<Value> {
        self.calls.push((obj, method.to_owned(), args.to_vec()));
        self.call_results.get(&(obj, method.to_owned())).cloned()
    }

    fn layer_exists(&self, id: LayerId) -> bool {
        self.layers.iter().any(|l| l.id == id && l.scriptable)
    }

    fn find_layer(&self, name: &str) -> Option<LayerId> {
        self.layers
            .iter()
            .find(|l| l.name == name && l.scriptable)
            .map(|l| l.id)
    }

    fn layer_count(&self) -> usize {
        self.layers.len()
    }

    fn layer_at(&self, index: usize) -> Option<LayerId> {
        self.layers.get(index).map(|l| l.id)
    }

    fn script_property(&self, binding: u64, name: &str) -> Option<Value> {
        self.script_properties
            .get(&(binding, name.to_owned()))
            .cloned()
    }

    fn set_script_property(&mut self, binding: u64, name: &str, value: &Value) -> bool {
        self.script_properties
            .insert((binding, name.to_owned()), value.clone());
        true
    }

    fn register_asset(&mut self, path: &str, precache: bool) -> Option<u32> {
        self.assets.push((path.to_owned(), precache));
        Some(self.assets.len() as u32 - 1)
    }

    fn open_user_shortcut(&mut self, name: &str) -> bool {
        self.shortcuts.push(name.to_owned());
        true
    }

    fn log(&mut self, level: LogLevel, message: &str) {
        self.logs.push((level, message.to_owned()));
    }
}
