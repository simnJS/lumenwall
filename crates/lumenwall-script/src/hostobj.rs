//! Host object handles: `ILayer` and the sub-objects of `lib.sceneScript.d.ts`
//! (`IEffect`, `IMaterial`, `IAnimation`, `IAnimationLayer`, `ITextureAnimation`,
//! `IVideoTexture`, `IParticleSystemInstance`, `IModelData`, `IAssetHandle`).
//!
//! Each kind is a native class whose prototype (null-prototyped, spec 06 §6.4) holds one
//! accessor per documented property and one function per documented method, all
//! forwarded to [`crate::SceneHost`]. Unsupported methods return a neutral value, so a
//! missing engine subsystem never throws.

use std::rc::Rc;

use rquickjs::class::{JsClass, Trace, Tracer, Writable};
use rquickjs::function::Params;
use rquickjs::object::Accessor;
use rquickjs::{
    Array, Class, Constructor, Ctx, JsLifetime, Object, Persistent, Result, Value as JsValue,
};

use crate::convert::{from_js, to_js};
use crate::shared::{Shared, Target};
use crate::util::{RawFn, method, number, type_error, undefined};
use crate::value::{LayerId, ObjectRef};
use crate::vector::is_native;

pub(crate) const LAYER: u8 = 0;
pub(crate) const EFFECT: u8 = 1;
pub(crate) const MATERIAL: u8 = 2;
pub(crate) const ANIMATION: u8 = 3;
pub(crate) const ANIMATION_LAYER: u8 = 4;
pub(crate) const TEXTURE_ANIMATION: u8 = 5;
pub(crate) const VIDEO_TEXTURE: u8 = 6;
pub(crate) const PARTICLE_INSTANCE: u8 = 7;
pub(crate) const MODEL_DATA: u8 = 8;
pub(crate) const ASSET: u8 = 9;

/// Result of a method the host does not implement.
#[derive(Clone, Copy)]
enum Neutral {
    Undefined,
    False,
    Zero,
    MinusOne,
    EmptyArray,
}

use Neutral::{EmptyArray, False, MinusOne, Undefined as Undef, Zero};

struct KindSpec {
    name: &'static str,
    /// Read-write properties: (JS name, host name).
    props: &'static [(&'static str, &'static str)],
    /// Read-only properties (writes are ignored).
    readonly: &'static [&'static str],
    /// String constants.
    consts: &'static [(&'static str, &'static str)],
    methods: &'static [(&'static str, Neutral)],
}

const ANIMATION_CONTROL: [(&str, Neutral); 6] = [
    ("play", Undef),
    ("pause", Undef),
    ("stop", Undef),
    ("isPlaying", False),
    ("getFrame", Zero),
    ("setFrame", Undef),
];

const KINDS: [KindSpec; 10] = [
    // ILayer and the interfaces it extends (IImageLayer, ISoundLayer, IEffectLayer,
    // ITextLayer, IParticleSystem, IModelLayer, ICamera).
    KindSpec {
        name: "ILayer",
        props: &[
            ("origin", "origin"),
            ("angles", "angles"),
            ("scale", "scale"),
            ("parallaxDepth", "parallaxDepth"),
            ("name", "name"),
            ("visible", "visible"),
            ("alpha", "alpha"),
            ("color", "color"),
            ("alignment", "alignment"),
            ("perspective", "perspective"),
            ("solid", "solid"),
            ("text", "text"),
            ("opaquebackground", "opaquebackground"),
            ("backgroundcolor", "backgroundcolor"),
            ("pointsize", "pointsize"),
            // parity: the reference exposes the point size as `pointSize` (spec 06 §6.4).
            ("pointSize", "pointsize"),
            ("font", "font"),
            ("padding", "padding"),
            ("horizontalalign", "horizontalalign"),
            ("verticalalign", "verticalalign"),
            ("anchor", "anchor"),
            ("limitrows", "limitrows"),
            ("maxrows", "maxrows"),
            ("limitwidth", "limitwidth"),
            ("maxwidth", "maxwidth"),
            ("volume", "volume"),
            ("rootmotion", "rootmotion"),
            ("fov", "fov"),
            ("zoom", "zoom"),
        ],
        readonly: &["size"],
        consts: &[],
        methods: &[
            ("getAnimation", Undef),
            ("getTextureAnimation", Undef),
            ("getVideoTexture", Undef),
            ("getAnimationLayerCount", Zero),
            ("getAnimationLayer", Undef),
            ("createAnimationLayer", Undef),
            ("playSingleAnimation", Undef),
            ("destroyAnimationLayer", False),
            ("getBoneCount", Zero),
            ("getBoneTransform", Undef),
            ("setBoneTransform", Undef),
            ("getLocalBoneTransform", Undef),
            ("setLocalBoneTransform", Undef),
            ("getLocalBoneAngles", Undef),
            ("setLocalBoneAngles", Undef),
            ("getLocalBoneOrigin", Undef),
            ("setLocalBoneOrigin", Undef),
            ("getBoneIndex", MinusOne),
            ("getBoneParentIndex", MinusOne),
            ("applyBonePhysicsImpulse", Undef),
            ("resetBonePhysicsSimulation", Undef),
            ("getBlendShapeIndex", MinusOne),
            ("getBlendShapeWeight", Zero),
            ("setBlendShapeWeight", Undef),
            ("isPlaying", False),
            ("play", Undef),
            ("stop", Undef),
            ("pause", Undef),
            ("emitParticles", Undef),
            ("getEffect", Undef),
            ("getEffectCount", Zero),
            ("transformAttachmentToTexture", Undef),
            ("getTransformMatrix", Undef),
            ("rotateObjectSpace", Undef),
            ("lookAt", Undef),
            ("lookAtYaw", Undef),
            ("setParent", Undef),
            ("getParent", Undef),
            ("getChildren", EmptyArray),
            ("getAttachmentIndex", MinusOne),
            ("getAttachmentMatrix", Undef),
            ("getAttachmentOrigin", Undef),
            ("getAttachmentAngles", Undef),
        ],
    },
    KindSpec {
        name: "IEffect",
        props: &[("visible", "visible"), ("name", "name")],
        readonly: &[],
        consts: &[],
        methods: &[
            ("getMaterial", Undef),
            ("getMaterialCount", Zero),
            ("setMaterialProperty", Undef),
            ("executeMaterialFunction", Undef),
            ("getAnimation", Undef),
        ],
    },
    KindSpec {
        name: "IMaterial",
        props: &[],
        readonly: &[],
        consts: &[],
        methods: &[("getAnimation", Undef)],
    },
    KindSpec {
        name: "IAnimation",
        props: &[("rate", "rate")],
        readonly: &["fps", "frameCount", "duration", "name"],
        consts: &[],
        methods: &ANIMATION_CONTROL,
    },
    KindSpec {
        name: "IAnimationLayer",
        props: &[
            ("name", "name"),
            ("rate", "rate"),
            ("blend", "blend"),
            ("visible", "visible"),
        ],
        readonly: &["fps", "frameCount", "duration"],
        consts: &[],
        methods: &[
            ("play", Undef),
            ("pause", Undef),
            ("stop", Undef),
            ("isPlaying", False),
            ("getFrame", Zero),
            ("setFrame", Undef),
            ("addEndedCallback", Undef),
            ("getAnimation", Undef),
        ],
    },
    KindSpec {
        name: "ITextureAnimation",
        props: &[("rate", "rate")],
        readonly: &["frameCount", "duration"],
        consts: &[],
        methods: &[
            ("play", Undef),
            ("pause", Undef),
            ("stop", Undef),
            ("isPlaying", False),
            ("getFrame", Zero),
            ("setFrame", Undef),
            ("join", Undef),
        ],
    },
    KindSpec {
        name: "IVideoTexture",
        props: &[("rate", "rate"), ("loop", "loop")],
        readonly: &["duration"],
        consts: &[],
        methods: &[
            ("play", Undef),
            ("pause", Undef),
            ("stop", Undef),
            ("isPlaying", False),
            ("getCurrentTime", Zero),
            ("setCurrentTime", Undef),
            ("addEndedCallback", Undef),
        ],
    },
    KindSpec {
        name: "IParticleSystemInstance",
        props: &[
            ("alpha", "alpha"),
            ("size", "size"),
            ("count", "count"),
            ("speed", "speed"),
            ("lifetime", "lifetime"),
            ("rate", "rate"),
            ("colorn", "colorn"),
            ("controlpoint0", "controlpoint0"),
            ("controlpoint1", "controlpoint1"),
            ("controlpoint2", "controlpoint2"),
            ("controlpoint3", "controlpoint3"),
            ("controlpoint4", "controlpoint4"),
            ("controlpoint5", "controlpoint5"),
            ("controlpoint6", "controlpoint6"),
            ("controlpoint7", "controlpoint7"),
        ],
        readonly: &[],
        consts: &[],
        methods: &[],
    },
    KindSpec {
        name: "IModelData",
        props: &[],
        readonly: &[],
        consts: &[
            ("POSITION", "position"),
            ("NORMAL", "normal"),
            ("TANGENT_SIGNED", "tangentSigned"),
            ("UV", "uv"),
            ("COLOR", "color"),
        ],
        methods: &[
            ("applyData", Undef),
            ("replaceData", Undef),
            ("getAnimation", Undef),
        ],
    },
    KindSpec {
        name: "IAssetHandle",
        props: &[],
        readonly: &[],
        consts: &[],
        methods: &[],
    },
];

/// Native handle of kind `K`.
pub(crate) struct HostObj<const K: u8> {
    pub obj: ObjectRef,
}

// SAFETY: holds no JS values.
unsafe impl<'js, const K: u8> JsLifetime<'js> for HostObj<K> {
    type Changed<'to> = HostObj<K>;
}

impl<'js, const K: u8> Trace<'js> for HostObj<K> {
    fn trace<'a>(&self, _tracer: Tracer<'a, 'js>) {}
}

impl<'js, const K: u8> JsClass<'js> for HostObj<K> {
    const NAME: &'static str = KINDS[K as usize].name;
    type Mutable = Writable;

    fn prototype(ctx: &Ctx<'js>) -> Result<Option<Object<'js>>> {
        Object::new_proto(ctx.clone(), None).map(Some)
    }

    fn constructor(_ctx: &Ctx<'js>) -> Result<Option<Constructor<'js>>> {
        Ok(None)
    }
}

fn this_ref<const K: u8>(sh: &Shared, p: &Params<'_, '_>) -> Result<ObjectRef> {
    let this = p.this();
    if is_native(sh, &this)
        && let Some(c) = this.as_object().and_then(Class::<HostObj<K>>::from_object)
    {
        return Ok(c.borrow().obj);
    }
    Err(type_error(
        p.ctx(),
        &format!("'this' is not an {}", KINDS[K as usize].name),
    ))
}

fn neutral<'js>(ctx: &Ctx<'js>, n: Neutral) -> Result<JsValue<'js>> {
    Ok(match n {
        Neutral::Undefined => undefined(ctx),
        Neutral::False => JsValue::new_bool(ctx.clone(), false),
        Neutral::Zero => number(ctx, 0.0),
        Neutral::MinusOne => number(ctx, -1.0),
        Neutral::EmptyArray => Array::new(ctx.clone())?.into_value(),
    })
}

/// Generic method forwarding to [`crate::SceneHost::call`].
pub(crate) fn host_call<'js>(
    sh: &Shared,
    p: &Params<'_, 'js>,
    obj: ObjectRef,
    name: &str,
) -> Result<Option<JsValue<'js>>> {
    let ctx = p.ctx();
    let mut args = Vec::with_capacity(p.len());
    for i in 0..p.len() {
        args.push(from_js(ctx, sh, p.arg(i).expect("argument in range"))?);
    }
    // we-doc: `thisObject.getAnimation()` in a property script is the timeline of the value the
    // script is bound to. The host gets the script's property (`multiply@effect1.pass0`) as the
    // name of the animated member.
    if name == "getAnimation"
        && args.is_empty()
        && let Some(id) = sh.current.get()
        && let Some(s) = sh.scripts.borrow().get(id.0 as usize)
    {
        args.push(crate::value::Value::String(s.property.clone()));
    }
    let r = sh
        .host
        .with(|h| h.call(obj, name, &args))
        .ok_or_else(|| crate::util::plain_error(ctx, "scene host unavailable"))?;
    r.map(|v| to_js(ctx, sh, &v, None)).transpose()
}

fn install_kind<'js, const K: u8>(ctx: &Ctx<'js>, shared: &Rc<Shared>) -> Result<()> {
    let spec = &KINDS[K as usize];
    let proto = Class::<HostObj<K>>::prototype(ctx)?.ok_or(rquickjs::Error::Unknown)?;
    for &(js_name, host_name) in spec.props {
        let sh = shared.clone();
        let get = move |p: Params<'_, 'js>| -> Result<JsValue<'js>> {
            let obj = this_ref::<K>(&sh, &p)?;
            // we-doc: a layer member read is a copy (`let o = layer.origin; o.x += 1;
            // layer.origin = o;`). Wallpaper Engine's own Dino Run keeps `initialPosition =
            // thisLayer.origin` fixed while it moves the layer; a live view (spec 06 §6.4)
            // would follow the layer and break its jump.
            let view = (K != LAYER).then_some(Target::Member {
                obj,
                name: host_name,
            });
            match sh.host_get(p.ctx(), obj, host_name)? {
                Some(v) => to_js(p.ctx(), &sh, &v, view),
                None => Ok(undefined(p.ctx())),
            }
        };
        let sh = shared.clone();
        // Réf (spec 06 §6.4): writes never throw; the host applies them when it can
        // (we-doc: `thisLayer.scale = new Vec3(…)` is the documented way to set properties).
        let set = move |p: Params<'_, 'js>| -> Result<JsValue<'js>> {
            let obj = this_ref::<K>(&sh, &p)?;
            if let Some(v) = p.arg(0) {
                let v = from_js(p.ctx(), &sh, v)?;
                sh.host_set(p.ctx(), obj, host_name, &v)?;
            }
            Ok(undefined(p.ctx()))
        };
        proto.prop(js_name, Accessor::new(RawFn(get), RawFn(set)).enumerable())?;
    }
    for &name in spec.readonly {
        let sh = shared.clone();
        let get = move |p: Params<'_, 'js>| -> Result<JsValue<'js>> {
            let obj = this_ref::<K>(&sh, &p)?;
            match sh.host_get(p.ctx(), obj, name)? {
                Some(v) => to_js(p.ctx(), &sh, &v, None),
                None => Ok(undefined(p.ctx())),
            }
        };
        let set = |p: Params<'_, 'js>| -> Result<JsValue<'js>> { Ok(undefined(p.ctx())) };
        proto.prop(name, Accessor::new(RawFn(get), RawFn(set)).enumerable())?;
    }
    for &(name, value) in spec.consts {
        proto.prop(name, rquickjs::object::Property::from(value).enumerable())?;
    }
    for &(name, fallback) in spec.methods {
        let sh = shared.clone();
        method(
            &proto,
            name,
            0,
            move |p: Params<'_, 'js>| -> Result<JsValue<'js>> {
                let obj = this_ref::<K>(&sh, &p)?;
                match host_call(&sh, &p, obj, name)? {
                    Some(v) => Ok(v),
                    None => neutral(p.ctx(), fallback),
                }
            },
        )?;
    }
    if K == LAYER {
        // `instance` (IParticleSystem): per-layer particle modifiers.
        let sh = shared.clone();
        let get = move |p: Params<'_, 'js>| -> Result<JsValue<'js>> {
            let ObjectRef::Layer(id) = this_ref::<K>(&sh, &p)? else {
                return Ok(undefined(p.ctx()));
            };
            instance::<PARTICLE_INSTANCE>(p.ctx(), ObjectRef::ParticleInstance(id))
        };
        let set = |p: Params<'_, 'js>| -> Result<JsValue<'js>> { Ok(undefined(p.ctx())) };
        proto.prop(
            "instance",
            Accessor::new(RawFn(get), RawFn(set)).enumerable(),
        )?;
    }
    Ok(())
}

/// Registers every host object kind.
pub(crate) fn install(ctx: &Ctx<'_>, shared: &Rc<Shared>) -> Result<()> {
    install_kind::<LAYER>(ctx, shared)?;
    install_kind::<EFFECT>(ctx, shared)?;
    install_kind::<MATERIAL>(ctx, shared)?;
    install_kind::<ANIMATION>(ctx, shared)?;
    install_kind::<ANIMATION_LAYER>(ctx, shared)?;
    install_kind::<TEXTURE_ANIMATION>(ctx, shared)?;
    install_kind::<VIDEO_TEXTURE>(ctx, shared)?;
    install_kind::<PARTICLE_INSTANCE>(ctx, shared)?;
    install_kind::<MODEL_DATA>(ctx, shared)?;
    install_kind::<ASSET>(ctx, shared)?;
    Ok(())
}

pub(crate) fn instance<'js, const K: u8>(ctx: &Ctx<'js>, obj: ObjectRef) -> Result<JsValue<'js>> {
    Ok(Class::instance(ctx.clone(), HostObj::<K> { obj })?.into_value())
}

/// A fresh, uncached layer object (M2 local `thisLayer`).
pub(crate) fn new_layer_object<'js>(ctx: &Ctx<'js>, id: LayerId) -> Result<Object<'js>> {
    Ok(Class::instance(
        ctx.clone(),
        HostObj::<LAYER> {
            obj: ObjectRef::Layer(id),
        },
    )?
    .into_inner())
}

/// The cached layer object of `id` (same object for every lookup).
pub(crate) fn layer_object<'js>(ctx: &Ctx<'js>, sh: &Shared, id: LayerId) -> Result<Object<'js>> {
    if let Some(p) = sh.js.borrow().layer_cache.get(&id) {
        return p.clone().restore(ctx);
    }
    let obj = new_layer_object(ctx, id)?;
    sh.js
        .borrow_mut()
        .layer_cache
        .insert(id, Persistent::save(ctx, obj.clone()));
    Ok(obj)
}

/// JS object for a host reference.
pub(crate) fn wrap<'js>(ctx: &Ctx<'js>, sh: &Shared, r: ObjectRef) -> Result<JsValue<'js>> {
    match r {
        ObjectRef::Scene => {
            let scene = sh.js.borrow().scene.clone();
            match scene {
                Some(s) => Ok(s.restore(ctx)?.into_value()),
                None => Ok(undefined(ctx)),
            }
        }
        ObjectRef::Layer(id) => Ok(layer_object(ctx, sh, id)?.into_value()),
        ObjectRef::Effect { .. } => instance::<EFFECT>(ctx, r),
        ObjectRef::Material { .. } => instance::<MATERIAL>(ctx, r),
        ObjectRef::Animation(_) => instance::<ANIMATION>(ctx, r),
        ObjectRef::AnimationLayer { .. } => instance::<ANIMATION_LAYER>(ctx, r),
        ObjectRef::TextureAnimation(_) => instance::<TEXTURE_ANIMATION>(ctx, r),
        ObjectRef::VideoTexture(_) => instance::<VIDEO_TEXTURE>(ctx, r),
        ObjectRef::ParticleInstance(_) => instance::<PARTICLE_INSTANCE>(ctx, r),
        ObjectRef::ModelData(_) => instance::<MODEL_DATA>(ctx, r),
        ObjectRef::Asset(_) => instance::<ASSET>(ctx, r),
    }
}

/// Host reference behind a JS value, if it is a host object (or `thisScene`).
pub(crate) fn as_object_ref<'js>(
    ctx: &Ctx<'js>,
    sh: &Shared,
    v: &JsValue<'js>,
) -> Result<Option<ObjectRef>> {
    if is_native(sh, v) {
        let Some(obj) = v.as_object() else {
            return Ok(None);
        };
        macro_rules! try_kind {
            ($($k:ident),*) => {$(
                if let Some(c) = Class::<HostObj<$k>>::from_object(obj) {
                    return Ok(Some(c.borrow().obj));
                }
            )*};
        }
        try_kind!(
            LAYER,
            EFFECT,
            MATERIAL,
            ANIMATION,
            ANIMATION_LAYER,
            TEXTURE_ANIMATION,
            VIDEO_TEXTURE,
            PARTICLE_INSTANCE,
            MODEL_DATA,
            ASSET
        );
        return Ok(None);
    }
    let scene = sh.js.borrow().scene.clone();
    if let Some(scene) = scene
        && *v == scene.restore(ctx)?.into_value()
    {
        return Ok(Some(ObjectRef::Scene));
    }
    Ok(None)
}

/// Layer referenced by a script argument: layer object, numeric id or name.
pub(crate) fn layer_arg<'js>(
    ctx: &Ctx<'js>,
    sh: &Shared,
    v: &JsValue<'js>,
) -> Result<Option<LayerId>> {
    if let Some(ObjectRef::Layer(id)) = as_object_ref(ctx, sh, v)? {
        return Ok(Some(id));
    }
    if let Some(n) = v.as_number() {
        return Ok(Some(n as i32));
    }
    if let Some(s) = v.as_string() {
        let name = s.to_string()?;
        return Ok(sh.host.with(|h| h.find_layer(&name)).flatten());
    }
    Ok(None)
}
