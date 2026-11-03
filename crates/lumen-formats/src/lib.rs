//! Readers for Wallpaper Engine binary formats: `.pkg` archives, `.tex` textures and
//! `MDLV` puppet meshes.
//!
//! Behaviour follows `specs/01-formats-binaires.md` (sections 0 to 3). Every reader
//! treats its input as untrusted: reads are bounds-checked, sizes are validated
//! before allocating, and no input can make them panic.
//!
//! - [`pkg`]: memory-mapped, zero-copy archive reader with an index built once.
//! - [`tex`]: `.tex` parser and conversion into a GPU-agnostic upload description.
//! - [`mdl`]: the heuristic `MDLV` puppet mesh reader.

pub mod mdl;
pub mod pkg;
mod reader;
pub mod tex;

pub use reader::UnexpectedEof;
