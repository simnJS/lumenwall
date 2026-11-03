//! Host-side values exchanged between the scene and scripts (spec 06 §2, §4).

/// Identifier of a scene object (spec 06 §6.3: 32-bit object id).
pub type LayerId = i32;

/// Handle of a JavaScript callback retained by the engine on behalf of the host
/// (for example `IVideoTexture.addEndedCallback`). Fire it with
/// [`crate::ScriptEngine::invoke_callback`], free it with
/// [`crate::ScriptEngine::release_callback`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CallbackId(pub u32);

/// Reference to a host object exposed to scripts.
///
/// Every SceneScript object other than vectors and matrices is a thin handle: reads,
/// writes and method calls are forwarded to [`crate::SceneHost::get`],
/// [`crate::SceneHost::set`] and [`crate::SceneHost::call`] with one of these.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObjectRef {
    /// The scene itself (`thisScene`).
    Scene,
    /// A layer (`ILayer`).
    Layer(LayerId),
    /// An effect of an image or text layer (`IEffect`), by index.
    Effect { layer: LayerId, effect: u32 },
    /// A material of an effect (`IMaterial`).
    Material {
        layer: LayerId,
        effect: u32,
        material: u32,
    },
    /// A timeline property animation (`IAnimation`), host-assigned id.
    Animation(u32),
    /// A puppet/model animation layer (`IAnimationLayer`).
    AnimationLayer { layer: LayerId, index: u32 },
    /// The sprite-sheet animation of an image layer (`ITextureAnimation`).
    TextureAnimation(LayerId),
    /// The video texture of an image layer (`IVideoTexture`).
    VideoTexture(LayerId),
    /// Per-layer particle instance modifiers (`IParticleSystemInstance`).
    ParticleInstance(LayerId),
    /// Custom model geometry (`IModelData`), host-assigned id.
    ModelData(u32),
    /// A registered asset (`IAssetHandle`), host-assigned id.
    Asset(u32),
}

/// A value crossing the host/script boundary.
///
/// The first eight variants are the dynamic value types of spec 06 §2.1 (components
/// stored as `f32`). The others only appear as method arguments and results.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// JavaScript `undefined` (method results and missing arguments only).
    Undefined,
    Null,
    Bool(bool),
    Int(i32),
    Float(f32),
    String(String),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    /// Column-major 3x3 matrix (`Mat3.m`).
    Mat3([f32; 9]),
    /// Column-major 4x4 matrix (`Mat4.m`).
    Mat4([f32; 16]),
    /// A host object handle (layer, effect, animation…).
    Object(ObjectRef),
    Array(Vec<Value>),
    /// A plain JavaScript object (configuration objects, camera transforms…).
    Record(Vec<(String, Value)>),
    /// A JavaScript function passed to the host.
    Callback(CallbackId),
}

impl Value {
    /// `vec4` projection (spec 06 §2.2).
    pub fn vec4(&self) -> [f32; 4] {
        match *self {
            Value::Float(f) => [f; 4],
            Value::Int(i) => [i as f32; 4],
            Value::Bool(b) => [f32::from(u8::from(b)); 4],
            Value::Vec2([x, y]) => [x, y, 0.0, 0.0],
            Value::Vec3([x, y, z]) => [x, y, z, 0.0],
            Value::Vec4(v) => v,
            _ => [0.0; 4],
        }
    }

    /// `vec3` projection (spec 06 §2.2).
    pub fn vec3(&self) -> [f32; 3] {
        let [x, y, z, _] = self.vec4();
        [x, y, z]
    }

    /// `vec2` projection (spec 06 §2.2).
    pub fn vec2(&self) -> [f32; 2] {
        let [x, y, _, _] = self.vec4();
        [x, y]
    }

    /// `float` projection (spec 06 §2.2).
    pub fn float(&self) -> f32 {
        self.vec4()[0]
    }

    /// `int` projection (spec 06 §2.2): truncation toward zero.
    pub fn int(&self) -> i32 {
        match *self {
            Value::Int(i) => i,
            _ => self.float() as i32,
        }
    }

    /// `bool` projection (spec 06 §2.2).
    pub fn bool(&self) -> bool {
        match *self {
            Value::Bool(b) => b,
            Value::Int(i) => i != 0,
            Value::Float(f) => f as i32 != 0,
            Value::Vec2(_) | Value::Vec3(_) | Value::Vec4(_) => self.float() != 0.0,
            _ => false,
        }
    }

    /// Number of vector components when the value is a vector.
    pub fn vector_dim(&self) -> Option<usize> {
        match self {
            Value::Vec2(_) => Some(2),
            Value::Vec3(_) => Some(3),
            Value::Vec4(_) => Some(4),
            _ => None,
        }
    }

    /// Builds a vector value of dimension `dim` (2..=4) from the first components.
    pub fn vector(dim: usize, c: [f32; 4]) -> Value {
        match dim {
            2 => Value::Vec2([c[0], c[1]]),
            3 => Value::Vec3([c[0], c[1], c[2]]),
            _ => Value::Vec4(c),
        }
    }
}

/// Media playback state (`MediaPlaybackEvent.state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PlaybackState {
    #[default]
    Stopped = 0,
    Playing = 1,
    Paused = 2,
}

/// `MediaPropertiesEvent` fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaProperties {
    pub title: String,
    pub artist: String,
    pub sub_title: String,
    pub album_title: String,
    pub album_artist: String,
    pub genres: String,
    pub content_type: String,
}

/// `MediaThumbnailEvent` fields. Colors are normalized RGB.
#[derive(Clone, Debug, PartialEq)]
pub struct MediaThumbnail {
    pub has_thumbnail: bool,
    pub primary_color: [f32; 3],
    pub secondary_color: [f32; 3],
    pub tertiary_color: [f32; 3],
    pub text_color: [f32; 3],
    pub high_contrast_color: [f32; 3],
}

impl Default for MediaThumbnail {
    /// The fixed colors of spec 06 §8.4, used until the host extracts real ones.
    fn default() -> Self {
        MediaThumbnail {
            has_thumbnail: false,
            primary_color: [0.12, 0.12, 0.12],
            secondary_color: [0.0, 0.0, 0.0],
            tertiary_color: [0.25, 0.25, 0.25],
            text_color: [1.0, 1.0, 1.0],
            high_contrast_color: [1.0, 1.0, 1.0],
        }
    }
}

/// Everything known about the current media, for [`crate::ScriptEngine::media_changed`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MediaSnapshot {
    pub properties: MediaProperties,
    pub state: PlaybackState,
    /// Seconds (WE `MediaTimelineEvent`; MPRIS microseconds must be converted by the host).
    pub position: f64,
    /// Seconds.
    pub duration: f64,
    pub thumbnail: MediaThumbnail,
}

/// Which cursor event to dispatch (`IComponent.cursor*`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorEventKind {
    Enter,
    Leave,
    Move,
    Down,
    Up,
    Click,
}

/// `CursorEvent` fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CursorEvent {
    pub world_position: [f32; 3],
    pub local_position: [f32; 3],
    /// Puppet-warp hit box name, if any.
    pub hit_box: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn projections_follow_spec_table() {
        assert_eq!(Value::Float(2.7).vec4(), [2.7; 4]);
        assert_eq!(Value::Float(2.7).int(), 2);
        assert!(!Value::Float(0.5).bool());
        assert!(Value::Float(1.5).bool());
        assert_eq!(Value::Int(-3).vec3(), [-3.0; 3]);
        assert_eq!(Value::Bool(true).vec2(), [1.0, 1.0]);
        assert_eq!(Value::Vec2([1.0, 2.0]).vec4(), [1.0, 2.0, 0.0, 0.0]);
        assert_eq!(Value::Vec3([1.0, 2.0, 3.0]).vec4(), [1.0, 2.0, 3.0, 0.0]);
        assert_eq!(Value::Vec4([1.0, 2.0, 3.0, 4.0]).vec2(), [1.0, 2.0]);
        assert_eq!(Value::Vec3([-1.5, 0.0, 0.0]).int(), -1);
        assert!(Value::Vec3([0.5, 0.0, 0.0]).bool());
        assert_eq!(Value::String("x".into()).vec4(), [0.0; 4]);
        assert!(!Value::String("x".into()).bool());
        assert_eq!(Value::Null.float(), 0.0);
        assert_eq!(
            Value::vector(3, [1.0, 2.0, 3.0, 4.0]),
            Value::Vec3([1.0, 2.0, 3.0])
        );
        assert_eq!(Value::Vec4([0.0; 4]).vector_dim(), Some(4));
        assert_eq!(Value::Int(1).vector_dim(), None);
    }

    #[test]
    fn thumbnail_defaults_match_spec() {
        let t = MediaThumbnail::default();
        assert_eq!(t.primary_color, [0.12, 0.12, 0.12]);
        assert_eq!(t.tertiary_color, [0.25, 0.25, 0.25]);
        assert_eq!(t.high_contrast_color, [1.0, 1.0, 1.0]);
    }
}
