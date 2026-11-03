//! Tolerant GLSL front-end: C preprocessor, parser, typer implementing the
//! Wallpaper Engine relaxations (spec 03 §10.2), linker and Vulkan GLSL 4.50
//! emitter. Pure Rust; the output is consumed by naga's GLSL front-end.

pub mod ast;
pub mod builtins;
pub mod emit;
pub mod lexer;
pub mod link;
pub mod parser;
pub mod pp;
pub mod typer;
pub mod types;

use std::fmt;

pub use link::ProgramLayout;
pub use typer::Stage;

/// Error of the front-end, located in the assembled source of one stage.
#[derive(Debug, Clone)]
pub struct FrontendError {
    pub stage: Option<Stage>,
    pub line: u32,
    pub msg: String,
}

impl fmt::Display for FrontendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.stage {
            Some(s) => write!(f, "{} shader, line {}: {}", s.name(), self.line, self.msg),
            None => write!(f, "link: {}", self.msg),
        }
    }
}

/// Output of the front-end: strict GLSL 4.50 for both stages plus the layout.
pub struct Translated {
    pub vertex: String,
    pub fragment: String,
    pub layout: ProgramLayout,
    pub warnings: Vec<String>,
}

fn stage_unit(src: &str, stage: Stage, int: &mut types::Interner) -> Result<typer::TypedUnit, FrontendError> {
    let toks = pp::Preprocessor::new(int)
        .run(src)
        .map_err(|e| FrontendError { stage: Some(stage), line: e.line, msg: e.msg })?;
    let unit =
        parser::parse(&toks, int).map_err(|e| FrontendError { stage: Some(stage), line: e.line, msg: e.msg })?;
    typer::type_unit(unit, int, stage).map_err(|e| FrontendError { stage: Some(stage), line: e.line, msg: e.msg })
}

/// Translates the assembled sources (spec 03 §9) of a vertex/fragment pair.
/// `frame_globals`: serve the engine globals from the renderer's frame block.
pub fn translate(vertex_src: &str, fragment_src: &str, frame_globals: bool) -> Result<Translated, FrontendError> {
    let mut int = types::Interner::new();
    let vs = stage_unit(vertex_src, Stage::Vertex, &mut int)?;
    let fs = stage_unit(fragment_src, Stage::Fragment, &mut int)?;
    let layout = link::link(&vs, &fs, &int, frame_globals).map_err(|e| FrontendError { stage: None, line: 0, msg: e.0 })?;
    let vertex = emit::emit_stage(&vs, &layout, &int);
    let fragment = emit::emit_stage(&fs, &layout, &int);
    let mut warnings = Vec::new();
    for (tu, s) in [(&vs, "vertex"), (&fs, "fragment")] {
        warnings.extend(tu.warnings.iter().map(|w| format!("{s}: {w}")));
    }
    Ok(Translated { vertex, fragment, layout, warnings })
}
