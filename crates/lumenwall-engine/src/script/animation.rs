//! Property animations played by scripts (`getAnimation()`, `IAnimation`, spec 06).
//!
//! A setting whose JSON carries an `animation` timeline (`lumenwall-scene`
//! [`PropertyAnimation`]) can be played by a script: `thisObject.getAnimation()` in the script of
//! that value (or `layer.getAnimation('alpha')`, `material.getAnimation('multiply')`…) returns an
//! `IAnimation`. Nothing exists for a scene whose scripts never ask for one: the player of a
//! timeline is created by its first `getAnimation()`, and only a *playing* animation costs
//! anything per frame (one bezier evaluation and a setting write).

use lumenwall_scene::{Project, PropertyAnimation, SettingId, Value as SValue, ValueTag};
use lumenwall_script::Value;

/// Player of one timeline.
#[derive(Debug)]
struct Player {
    setting: SettingId,
    playing: bool,
    /// Frames elapsed since the start, before the end mode (loop, mirror) is applied.
    elapsed: f32,
    rate: f32,
}

/// Every player created by scripts.
#[derive(Debug, Default)]
pub(crate) struct Animations {
    players: Vec<Player>,
    playing: usize,
}

/// Value of a setting built from the components of a timeline, in the type the setting has.
fn setting_value(current: &SValue, c: [f32; 4]) -> SValue {
    match current.tag() {
        ValueTag::Int => SValue::from_int(c[0].round() as i32),
        ValueTag::Bool => SValue::from_bool(c[0] >= 0.5),
        ValueTag::Vec2 => SValue::from_vec2(glam::Vec2::new(c[0], c[1])),
        ValueTag::Vec3 => SValue::from_vec3(glam::Vec3::new(c[0], c[1], c[2])),
        ValueTag::Vec4 => SValue::from_vec4(glam::Vec4::from_array(c)),
        _ => SValue::from_float(c[0]),
    }
}

impl Animations {
    /// Whether an animation is playing (frames must keep coming).
    pub(crate) fn playing(&self) -> bool {
        self.playing > 0
    }

    /// Player of the timeline of `setting`, created on first use; `None` without a timeline.
    pub(crate) fn acquire(&mut self, project: &Project, setting: SettingId) -> Option<u32> {
        project.settings.animation(setting)?;
        if let Some(i) = self.players.iter().position(|p| p.setting == setting) {
            return Some(i as u32);
        }
        let start_paused = project
            .settings
            .animation(setting)
            .is_some_and(|a| a.start_paused);
        self.players.push(Player {
            setting,
            // A timeline that does not start paused plays from the start.
            playing: !start_paused,
            elapsed: 0.0,
            rate: 1.0,
        });
        if !start_paused {
            self.playing += 1;
        }
        Some(self.players.len() as u32 - 1)
    }

    fn timeline<'p>(
        &self,
        project: &'p Project,
        id: u32,
    ) -> Option<(&Player, &'p PropertyAnimation)> {
        let p = self.players.get(id as usize)?;
        Some((p, project.settings.animation(p.setting)?))
    }

    /// Writes the value of the current frame into the setting. Returns whether it changed.
    fn apply(&self, project: &mut Project, id: u32) -> bool {
        let Some((p, a)) = self.timeline(project, id) else {
            return false;
        };
        let (frame, _) = a.frame_at(p.elapsed);
        let next = setting_value(project.settings.value(p.setting), a.value_at(frame));
        let setting = p.setting;
        if *project.settings.value(setting) == next {
            return false;
        }
        project.settings.set(setting, next);
        true
    }

    fn set_playing(&mut self, id: u32, on: bool) {
        if let Some(p) = self.players.get_mut(id as usize)
            && p.playing != on
        {
            p.playing = on;
            if on {
                self.playing += 1;
            } else {
                self.playing -= 1;
            }
        }
    }

    /// Advances every playing animation by `dt` seconds and writes the values. Returns whether
    /// a setting changed.
    pub(crate) fn advance(&mut self, project: &mut Project, dt: f32) -> bool {
        if self.playing == 0 {
            return false;
        }
        let mut changed = false;
        for id in 0..self.players.len() as u32 {
            let p = &self.players[id as usize];
            if !p.playing {
                continue;
            }
            let Some(a) = project.settings.animation(p.setting) else {
                continue;
            };
            let elapsed = p.elapsed + dt * a.fps * p.rate;
            let ended = a.frame_at(elapsed).1;
            self.players[id as usize].elapsed = elapsed;
            changed |= self.apply(project, id);
            if ended {
                self.set_playing(id, false);
            }
        }
        changed
    }

    /// `IAnimation` member read.
    pub(crate) fn get(&self, project: &Project, id: u32, name: &str) -> Option<Value> {
        let (p, a) = self.timeline(project, id)?;
        match name {
            "rate" => Some(Value::Float(p.rate)),
            "fps" => Some(Value::Float(a.fps)),
            "frameCount" => Some(Value::Int(a.length.round() as i32)),
            "duration" => Some(Value::Float(a.duration())),
            "name" => Some(Value::String(String::new())),
            _ => None,
        }
    }

    /// `IAnimation.rate = …`.
    pub(crate) fn set(&mut self, id: u32, name: &str, value: &Value) -> bool {
        match (name, self.players.get_mut(id as usize)) {
            ("rate", Some(p)) => {
                p.rate = value.float();
                true
            }
            _ => false,
        }
    }

    /// `IAnimation` methods. Returns the result and whether a setting changed.
    pub(crate) fn call(
        &mut self,
        project: &mut Project,
        id: u32,
        method: &str,
        args: &[Value],
    ) -> (Option<Value>, bool) {
        let Some(length) = self.timeline(project, id).map(|(_, a)| a.length) else {
            return (None, false);
        };
        match method {
            "play" => {
                // A finished single-run timeline plays again from the start.
                let ended = self
                    .timeline(project, id)
                    .is_some_and(|(p, a)| a.frame_at(p.elapsed).1);
                if ended {
                    self.players[id as usize].elapsed = 0.0;
                }
                self.set_playing(id, true);
                (Some(Value::Undefined), false)
            }
            "pause" => {
                self.set_playing(id, false);
                (Some(Value::Undefined), false)
            }
            // Stops and goes back to the first frame, whose value is shown at once.
            "stop" => {
                self.set_playing(id, false);
                self.players[id as usize].elapsed = 0.0;
                let changed = self.apply(project, id);
                (Some(Value::Undefined), changed)
            }
            "isPlaying" => (Some(Value::Bool(self.players[id as usize].playing)), false),
            "getFrame" => {
                let p = &self.players[id as usize];
                let frame = project
                    .settings
                    .animation(p.setting)
                    .map_or(0.0, |a| a.frame_at(p.elapsed).0);
                (Some(Value::Float(frame)), false)
            }
            "setFrame" => {
                let frame = args.first().map_or(0.0, Value::float).clamp(0.0, length);
                self.players[id as usize].elapsed = frame;
                let changed = self.apply(project, id);
                (Some(Value::Undefined), changed)
            }
            _ => (None, false),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use lumenwall_scene::{PropertyOverrides, load_project};

    use super::*;

    /// A video-less project is enough: the animated value is a scene setting.
    fn project() -> (Project, SettingId) {
        let mut f: HashMap<String, Vec<u8>> = HashMap::new();
        f.insert(
            "project.json".into(),
            br#"{"type":"scene","title":"t","file":"scene.json"}"#.to_vec(),
        );
        f.insert(
            "scene.json".into(),
            br#"{"camera":{"center":"0 0 0","eye":"0 0 1","up":"0 1 0"},
                 "general":{"orthogonalprojection":{"width":100,"height":100}},
                 "objects":[{"id":1,"name":"a","image":"models/m.json","origin":"0 0 0","size":"10 10",
                   "alpha":{"value":0.5,"animation":{"c0":[
                     {"frame":0,"value":1},{"frame":15,"value":0}],
                     "options":{"fps":15,"length":15,"mode":"single","startpaused":true}}}}]}"#
                .to_vec(),
        );
        f.insert(
            "models/m.json".into(),
            br#"{"material":"materials/m.json"}"#.to_vec(),
        );
        let p = load_project(&f, PropertyOverrides::new()).expect("loads");
        let id = p.scene.as_ref().unwrap().objects[0]
            .as_image()
            .unwrap()
            .alpha;
        (p, id)
    }

    #[test]
    fn plays_stops_and_ends() {
        let (mut p, alpha) = project();
        let mut a = Animations::default();
        assert!(!a.playing());
        let id = a.acquire(&p, alpha).expect("timeline");
        assert_eq!(a.acquire(&p, alpha), Some(id), "one player per timeline");
        assert!(!a.playing(), "startpaused");
        assert_eq!(
            p.settings.value(alpha).float(),
            0.5,
            "untouched until played"
        );
        assert_eq!(a.get(&p, id, "fps"), Some(Value::Float(15.0)));
        assert_eq!(a.get(&p, id, "frameCount"), Some(Value::Int(15)));
        // The script's `anim.stop(); anim.play();`.
        let (_, changed) = a.call(&mut p, id, "stop", &[]);
        assert!(changed);
        assert_eq!(p.settings.value(alpha).float(), 1.0);
        a.call(&mut p, id, "play", &[]);
        assert!(a.playing());
        assert!(a.advance(&mut p, 0.5), "half a second: frame 7.5");
        assert!((p.settings.value(alpha).float() - 0.5).abs() < 1e-5);
        assert_eq!(
            a.call(&mut p, id, "getFrame", &[]).0,
            Some(Value::Float(7.5))
        );
        assert!(a.advance(&mut p, 1.0), "past the end");
        assert_eq!(p.settings.value(alpha).float(), 0.0);
        assert!(!a.playing(), "a single run stops on its last frame");
        assert_eq!(
            a.call(&mut p, id, "isPlaying", &[]).0,
            Some(Value::Bool(false))
        );
        assert!(!a.advance(&mut p, 1.0), "nothing playing: nothing to do");
        // Rate and setFrame.
        a.set(id, "rate", &Value::Float(2.0));
        a.call(&mut p, id, "setFrame", &[Value::Float(0.0)]);
        a.call(&mut p, id, "play", &[]);
        a.advance(&mut p, 0.25);
        assert_eq!(
            a.call(&mut p, id, "getFrame", &[]).0,
            Some(Value::Float(7.5))
        );
    }

    #[test]
    fn a_setting_without_timeline_has_no_player() {
        let (p, _) = project();
        let mut a = Animations::default();
        let other = p.scene.as_ref().unwrap().objects[0]
            .as_image()
            .unwrap()
            .brightness;
        assert_eq!(a.acquire(&p, other), None);
    }
}
