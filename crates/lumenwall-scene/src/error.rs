//! Load errors (spec 02 E1), warnings (W) and JSON paths.

use std::borrow::Cow;
use std::fmt;

use crate::asset::AssetError;
use crate::raw::JsonKind;
use crate::text::ColorError;

/// Location of a value inside a JSON document, e.g. `$.objects[3].effects[0].file`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JsonPath(Vec<Segment>);

/// One step of a [`JsonPath`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    /// Object key.
    Key(Cow<'static, str>),
    /// Array index.
    Index(usize),
}

impl JsonPath {
    /// Path segments from the document root.
    pub fn segments(&self) -> &[Segment] {
        &self.0
    }
}

impl fmt::Display for JsonPath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("$")?;
        for s in &self.0 {
            match s {
                Segment::Key(k) => write!(f, ".{k}")?,
                Segment::Index(i) => write!(f, "[{i}]")?,
            }
        }
        Ok(())
    }
}

/// What the data should have been.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    /// A JSON string.
    String,
    /// A number or boolean converted to a 32-bit integer.
    Integer,
    /// A number or boolean converted to a float.
    Float,
    /// A JSON boolean.
    Bool,
    /// A number or a 2-component vector string.
    Vec2,
    /// A number or a 3-component vector string.
    Vec3,
    /// A JSON object.
    Object,
    /// A JSON array.
    Array,
    /// A string or a number.
    StringOrNumber,
    /// A string or an object (`user` of a setting).
    StringOrObject,
    /// A colour string.
    Color,
}

impl fmt::Display for Expected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::String => "a string",
            Self::Integer => "an integer",
            Self::Float => "a number",
            Self::Bool => "a boolean",
            Self::Vec2 => "a number or a \"x y\" string",
            Self::Vec3 => "a number or a \"x y z\" string",
            Self::Object => "an object",
            Self::Array => "an array",
            Self::StringOrNumber => "a string or a number",
            Self::StringOrObject => "a string or an object",
            Self::Color => "a colour string",
        })
    }
}

/// Why a value was rejected.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum DataError {
    /// A required key is absent.
    #[error("required value is missing")]
    Missing,
    /// Wrong JSON type.
    #[error("expected {expected}, found {found}")]
    Type {
        /// Accepted types.
        expected: Expected,
        /// Type found.
        found: JsonKind,
    },
    /// Vector string with the wrong number of components (strict reads).
    #[error("expected a vector of {expected} components, found {found}")]
    VectorComponents {
        /// Accepted count (`"2"`, `"3"`, `"at least 4"`).
        expected: &'static str,
        /// Components counted (§0.6).
        found: usize,
    },
    /// Invalid colour string.
    #[error("invalid colour {value:?}: {reason}")]
    Color {
        /// The colour text.
        value: String,
        /// What is wrong with it.
        reason: ColorError,
    },
    /// `type` of `project.json` is not `scene`, `video` or `web`.
    #[error("unknown project type {0:?}")]
    ProjectType(String),
    /// A combo property's `options` is not an array.
    #[error("combo options must be an array")]
    ComboOptions,
}

/// Fatal load error (spec 02 E1): the wallpaper does not load.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// A required file could not be read.
    #[error("cannot read `{path}`: {source}")]
    Asset {
        /// Asset path.
        path: String,
        /// Underlying error.
        #[source]
        source: AssetError,
    },
    /// The file is not valid JSON.
    #[error("`{file}`: invalid JSON at line {line}, column {column}: {message}")]
    Syntax {
        /// Asset path.
        file: String,
        /// 1-based line.
        line: usize,
        /// 1-based column.
        column: usize,
        /// Parser message.
        message: String,
    },
    /// The JSON is well formed but a value is wrong.
    #[error("`{file}` at {path}: {error}")]
    Data {
        /// Asset path.
        file: String,
        /// Location in the document.
        path: JsonPath,
        /// What is wrong.
        #[source]
        error: DataError,
    },
    /// [`crate::Project::build`] was called on a video or web project.
    #[error("project has no scene")]
    NotAScene,
}

impl LoadError {
    pub(crate) fn syntax(file: &str, e: &serde_json::Error) -> Self {
        let message = e.to_string();
        // serde_json appends " at line L column C"; keep the bare message.
        let message = match message.rfind(" at line ") {
            Some(i) => message[..i].to_owned(),
            None => message,
        };
        Self::Syntax {
            file: file.to_owned(),
            line: e.line(),
            column: e.column(),
            message,
        }
    }
}

/// A recoverable problem (spec 02 W): the faulty data was ignored or replaced by a default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// Asset being read when the problem was found (empty for build-time warnings).
    pub file: String,
    /// Human-readable description.
    pub message: String,
}

impl fmt::Display for Warning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.file.is_empty() {
            f.write_str(&self.message)
        } else {
            write!(f, "`{}`: {}", self.file, self.message)
        }
    }
}

/// Collects warnings and mirrors them to `tracing`.
#[derive(Debug, Default)]
pub(crate) struct Warnings(pub Vec<Warning>);

impl Warnings {
    pub fn push(&mut self, file: &str, message: impl Into<String>) {
        let w = Warning {
            file: file.to_owned(),
            message: message.into(),
        };
        tracing::warn!("{w}");
        self.0.push(w);
    }
}

/// Error inside one document, before the file name is known. The path is stored innermost first
/// and only built on the error path.
#[derive(Debug)]
pub(crate) struct DataErr {
    rev_path: Vec<Segment>,
    error: DataError,
}

impl DataErr {
    pub fn new(error: DataError) -> Self {
        Self {
            rev_path: Vec::new(),
            error,
        }
    }

    pub fn into_load(mut self, file: &str) -> LoadError {
        self.rev_path.reverse();
        LoadError::Data {
            file: file.to_owned(),
            path: JsonPath(self.rev_path),
            error: self.error,
        }
    }
}

impl From<DataError> for DataErr {
    fn from(e: DataError) -> Self {
        Self::new(e)
    }
}

/// Failure while converting a document: either a problem in this document, or a fatal error
/// from a file it references (already complete).
#[derive(Debug)]
pub(crate) enum Fail {
    Data(DataErr),
    Load(Box<LoadError>),
}

impl From<DataErr> for Fail {
    fn from(e: DataErr) -> Self {
        Self::Data(e)
    }
}

impl From<DataError> for Fail {
    fn from(e: DataError) -> Self {
        Self::Data(DataErr::new(e))
    }
}

impl From<LoadError> for Fail {
    fn from(e: LoadError) -> Self {
        Self::Load(Box::new(e))
    }
}

impl Fail {
    pub fn into_load(self, file: &str) -> LoadError {
        match self {
            Self::Data(d) => d.into_load(file),
            Self::Load(l) => *l,
        }
    }
}

/// Adds path segments to errors as they bubble up.
pub(crate) trait At: Sized {
    fn push_seg(self, seg: Segment) -> Self;

    fn at(self, key: &'static str) -> Self {
        self.push_seg(Segment::Key(Cow::Borrowed(key)))
    }

    fn at_key(self, key: &str) -> Self {
        self.push_seg(Segment::Key(Cow::Owned(key.to_owned())))
    }

    fn at_idx(self, i: usize) -> Self {
        self.push_seg(Segment::Index(i))
    }
}

impl<T> At for Result<T, DataErr> {
    fn push_seg(self, seg: Segment) -> Self {
        self.map_err(|mut e| {
            e.rev_path.push(seg);
            e
        })
    }
}

impl<T> At for Result<T, Fail> {
    fn push_seg(self, seg: Segment) -> Self {
        self.map_err(|e| match e {
            Fail::Data(mut d) => {
                d.rev_path.push(seg);
                Fail::Data(d)
            }
            other => other,
        })
    }
}
