//! Binding reflection of a compiled program, so the renderer can create bind
//! group layouts once and write uniforms by byte offset (no name lookups per
//! frame).

use serde::{Deserialize, Serialize};

// Binding contract of lumen-render (crates/lumen-render/README.md, "Shaders"):
// @group(0) @binding(0) pass block, @group(0) @binding(1) frame block,
// @group(1) @binding(2N) / (2N + 1) texture / filtering sampler of slot N.

/// Bind group of the uniform blocks.
pub const BIND_GROUP: u32 = 0;
/// Binding of the pass uniform block (when the program has uniforms).
pub const UNIFORM_BLOCK_BINDING: u32 = 0;
/// Binding of the renderer's frame block (when used).
pub const FRAME_BLOCK_BINDING: u32 = 1;
/// Bind group of the textures and samplers.
pub const TEXTURE_GROUP: u32 = 1;
/// Entry point names in the merged module.
pub const VERTEX_ENTRY: &str = "vs_main";
pub const FRAGMENT_ENTRY: &str = "fs_main";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ScalarKind {
    Float,
    Int,
    Uint,
    /// Stored as a 32-bit unsigned integer (0 or 1).
    Bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UniformType {
    Scalar(ScalarKind),
    Vector(ScalarKind, u8),
    /// Column-major float matrix; every column starts on a 16-byte boundary.
    Matrix { columns: u8, rows: u8 },
    /// Aggregate the renderer does not interpret.
    Other,
}

impl UniformType {
    pub const FLOAT: UniformType = UniformType::Scalar(ScalarKind::Float);
    pub const INT: UniformType = UniformType::Scalar(ScalarKind::Int);
    pub const VEC2: UniformType = UniformType::Vector(ScalarKind::Float, 2);
    pub const VEC3: UniformType = UniformType::Vector(ScalarKind::Float, 3);
    pub const VEC4: UniformType = UniformType::Vector(ScalarKind::Float, 4);
    pub const MAT3: UniformType = UniformType::Matrix { columns: 3, rows: 3 };
    pub const MAT4: UniformType = UniformType::Matrix { columns: 4, rows: 4 };

    /// Byte distance between matrix columns.
    pub const MATRIX_COLUMN_STRIDE: u32 = 16;

    /// Annotation type names of spec 03 §6.4.
    pub fn from_glsl_name(s: &str) -> Option<UniformType> {
        Some(match s {
            "float" => Self::FLOAT,
            "int" => Self::INT,
            "vec2" => Self::VEC2,
            "vec3" => Self::VEC3,
            "vec4" => Self::VEC4,
            _ => return None,
        })
    }
}

/// Shader stages that reference a resource.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Stages {
    pub vertex: bool,
    pub fragment: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UniformMember {
    pub name: String,
    pub ty: UniformType,
    /// Byte offset in the uniform block.
    pub offset: u32,
    /// Element count, 0 when the uniform is not an array.
    pub array_len: u32,
    /// Byte distance between array elements: 4 for packed scalar arrays
    /// (`float[N]` is stored tightly), 16 otherwise.
    pub array_stride: u32,
    pub stages: Stages,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UniformBlock {
    pub group: u32,
    pub binding: u32,
    /// Block size in bytes (multiple of 16).
    pub size: u32,
    pub members: Vec<UniformMember>,
}

/// A sampled slot: `texture_2d<f32>` (`texture_3d<f32>` for a `sampler3D`) at
/// `texture_binding`, filtering sampler at `sampler_binding`, both in [`TEXTURE_GROUP`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextureBinding {
    /// Texture unit / slot (spec 03 §8.1): N for `g_Texture0`…`g_Texture7`;
    /// any other sampler name (including `g_Texture8`/`9`) reads unit 0.
    pub unit: u32,
    /// Sampler uniforms of the shader that read this unit.
    pub names: Vec<String>,
    pub group: u32,
    /// `2 * unit`.
    pub texture_binding: u32,
    /// `2 * unit + 1`.
    pub sampler_binding: u32,
    pub stages: Stages,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VertexAttribute {
    pub name: String,
    pub location: u32,
    pub ty: UniformType,
}

macro_rules! engine_uniforms {
    ($($v:ident = $name:literal : $ty:expr, $len:literal;)*) => {
        /// Uniforms provided by the engine (spec 03 §12.2, §12.3).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum EngineUniform { $($v,)* }
        impl EngineUniform {
            pub const ALL: &'static [EngineUniform] = &[$(EngineUniform::$v,)*];
            pub const COUNT: usize = Self::ALL.len();
            pub fn glsl_name(self) -> &'static str {
                match self { $(EngineUniform::$v => $name,)* }
            }
            /// Declared type the engine writes (a uniform declared with another
            /// type is not updated, spec 03 §12.4) and array length (0 = scalar).
            pub fn expected(self) -> (UniformType, u32) {
                match self { $(EngineUniform::$v => ($ty, $len),)* }
            }
            pub fn from_name(s: &str) -> Option<EngineUniform> {
                match s { $($name => Some(EngineUniform::$v),)* _ => None }
            }
        }
    };
}

engine_uniforms! {
    Time = "g_Time": UniformType::FLOAT, 0;
    Daytime = "g_Daytime": UniformType::FLOAT, 0;
    Texture0Resolution = "g_Texture0Resolution": UniformType::VEC4, 0;
    Texture1Resolution = "g_Texture1Resolution": UniformType::VEC4, 0;
    Texture2Resolution = "g_Texture2Resolution": UniformType::VEC4, 0;
    Texture3Resolution = "g_Texture3Resolution": UniformType::VEC4, 0;
    Texture4Resolution = "g_Texture4Resolution": UniformType::VEC4, 0;
    Texture5Resolution = "g_Texture5Resolution": UniformType::VEC4, 0;
    Texture6Resolution = "g_Texture6Resolution": UniformType::VEC4, 0;
    Texture7Resolution = "g_Texture7Resolution": UniformType::VEC4, 0;
    Texture0Rotation = "g_Texture0Rotation": UniformType::VEC4, 0;
    Texture0Translation = "g_Texture0Translation": UniformType::VEC2, 0;
    TextureReductionScale = "g_TextureReductionScale": UniformType::FLOAT, 0;
    Brightness = "g_Brightness": UniformType::FLOAT, 0;
    UserAlpha = "g_UserAlpha": UniformType::FLOAT, 0;
    Alpha = "g_Alpha": UniformType::FLOAT, 0;
    Color = "g_Color": UniformType::VEC3, 0;
    Color4 = "g_Color4": UniformType::VEC4, 0;
    CompositeColor = "g_CompositeColor": UniformType::VEC3, 0;
    LightAmbientColor = "g_LightAmbientColor": UniformType::VEC3, 0;
    LightSkylightColor = "g_LightSkylightColor": UniformType::VEC3, 0;
    ModelViewProjectionMatrix = "g_ModelViewProjectionMatrix": UniformType::MAT4, 0;
    ModelViewProjectionMatrixInverse = "g_ModelViewProjectionMatrixInverse": UniformType::MAT4, 0;
    EffectModelViewProjectionMatrix = "g_EffectModelViewProjectionMatrix": UniformType::MAT4, 0;
    ModelMatrix = "g_ModelMatrix": UniformType::MAT4, 0;
    EffectModelMatrix = "g_EffectModelMatrix": UniformType::MAT4, 0;
    ViewProjectionMatrix = "g_ViewProjectionMatrix": UniformType::MAT4, 0;
    NormalModelMatrix = "g_NormalModelMatrix": UniformType::MAT3, 0;
    EffectTextureProjectionMatrix = "g_EffectTextureProjectionMatrix": UniformType::MAT4, 0;
    EffectTextureProjectionMatrixInverse = "g_EffectTextureProjectionMatrixInverse": UniformType::MAT4, 0;
    PointerPosition = "g_PointerPosition": UniformType::VEC2, 0;
    PointerPositionLast = "g_PointerPositionLast": UniformType::VEC2, 0;
    TexelSize = "g_TexelSize": UniformType::VEC2, 0;
    TexelSizeHalf = "g_TexelSizeHalf": UniformType::VEC2, 0;
    AudioSpectrum16Left = "g_AudioSpectrum16Left": UniformType::FLOAT, 16;
    AudioSpectrum16Right = "g_AudioSpectrum16Right": UniformType::FLOAT, 16;
    AudioSpectrum32Left = "g_AudioSpectrum32Left": UniformType::FLOAT, 32;
    AudioSpectrum32Right = "g_AudioSpectrum32Right": UniformType::FLOAT, 32;
    AudioSpectrum64Left = "g_AudioSpectrum64Left": UniformType::FLOAT, 64;
    AudioSpectrum64Right = "g_AudioSpectrum64Right": UniformType::FLOAT, 64;
    ModelMatrixInverse = "g_ModelMatrixInverse": UniformType::MAT4, 0;
    OrientationUp = "g_OrientationUp": UniformType::VEC3, 0;
    OrientationRight = "g_OrientationRight": UniformType::VEC3, 0;
    OrientationForward = "g_OrientationForward": UniformType::VEC3, 0;
    ViewUp = "g_ViewUp": UniformType::VEC3, 0;
    ViewRight = "g_ViewRight": UniformType::VEC3, 0;
    EyePosition = "g_EyePosition": UniformType::VEC3, 0;
    RenderVar0 = "g_RenderVar0": UniformType::VEC4, 0;
    RenderVar1 = "g_RenderVar1": UniformType::VEC4, 0;
    RefractAmount = "g_RefractAmount": UniformType::FLOAT, 0;
}

/// Everything the renderer needs to bind and feed a compiled program.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProgramReflection {
    pub uniform_block: Option<UniformBlock>,
    /// Sampled slots, by unit.
    pub textures: Vec<TextureBinding>,
    pub attributes: Vec<VertexAttribute>,
    /// Engine globals read from the frame block (`@group(0) @binding(1)`)
    /// instead of the pass block.
    pub frame_uniforms: Vec<String>,
    pub vertex_entry: String,
    pub fragment_entry: String,
    /// Index into `uniform_block.members` for each [`EngineUniform`], when the
    /// shader declares it with the type the engine writes.
    engine: Vec<Option<u32>>,
}

impl ProgramReflection {
    pub(crate) fn new(
        uniform_block: Option<UniformBlock>,
        textures: Vec<TextureBinding>,
        attributes: Vec<VertexAttribute>,
        frame_uniforms: Vec<String>,
    ) -> Self {
        let mut engine = vec![None; EngineUniform::COUNT];
        if let Some(b) = &uniform_block {
            for (i, m) in b.members.iter().enumerate() {
                if let Some(e) = EngineUniform::from_name(&m.name)
                    && e.expected() == (m.ty, m.array_len)
                {
                    engine[e as usize] = Some(i as u32);
                }
            }
        }
        ProgramReflection {
            uniform_block,
            textures,
            attributes,
            frame_uniforms,
            vertex_entry: VERTEX_ENTRY.into(),
            fragment_entry: FRAGMENT_ENTRY.into(),
            engine,
        }
    }

    /// Engine uniform of the pass block, O(1). Globals moved to the frame
    /// block are not listed here (see [`Self::frame_uniforms`]).
    pub fn engine(&self, u: EngineUniform) -> Option<&UniformMember> {
        let i = self.engine.get(u as usize).copied().flatten()?;
        self.uniform_block.as_ref().map(|b| &b.members[i as usize])
    }

    /// Uniform by name (load time only).
    pub fn uniform(&self, name: &str) -> Option<&UniformMember> {
        self.uniform_block.as_ref()?.members.iter().find(|m| m.name == name)
    }

    /// Slot read by a sampler uniform.
    pub fn texture(&self, name: &str) -> Option<&TextureBinding> {
        self.textures.iter().find(|t| t.names.iter().any(|n| n == name))
    }

    pub fn uses_frame_block(&self) -> bool {
        !self.frame_uniforms.is_empty()
    }

    pub fn attribute(&self, name: &str) -> Option<&VertexAttribute> {
        self.attributes.iter().find(|a| a.name == name)
    }
}
