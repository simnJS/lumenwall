//! Prints the assembled sources, the emitted GLSL 4.50 and the reflection of a
//! shader: `cargo run -p lumen-shader --example shaderdump -- <shader> [DIR...] [-DNAME=V...]`
//! Directories are searched in order (wallpaper folder, then assets).

use lumen_shader::{CompileOptions, DirFs, LayeredFs, PassCombos, PassRequest, ShaderCompiler, ShaderFs};

fn main() {
    let mut args = std::env::args().skip(1);
    let shader = args.next().expect("usage: shaderdump <shader> [DIR...] [-DNAME=V...]");
    let mut dirs = Vec::new();
    let mut combos = PassCombos::default();
    for a in args {
        if let Some(d) = a.strip_prefix("-D") {
            let (n, v) = d.split_once('=').unwrap_or((d, "1"));
            combos.material.insert(n.to_string(), v.parse().unwrap_or(1));
        } else {
            dirs.push(DirFs(a.into()));
        }
    }
    let fs = LayeredFs(dirs.iter().map(|d| d as &dyn ShaderFs).collect());
    let c = ShaderCompiler::new(CompileOptions::default(), None);
    let p = c.prepare(&fs, &PassRequest { shader: &shader, combos: &combos }).expect("prepare");
    for w in &p.warnings {
        eprintln!("warning: {w}");
    }
    match lumen_shader::translate_to_glsl(&p.vertex_source, &p.fragment_source, c.options()) {
        Ok((v, f)) => println!("==== vertex (GLSL 4.50)\n{v}\n==== fragment (GLSL 4.50)\n{f}"),
        Err(e) => {
            println!("==== assembled vertex\n{}\n==== assembled fragment\n{}", p.vertex_source, p.fragment_source);
            println!("error: {e}");
            return;
        }
    }
    match c.compile(p) {
        Ok(s) => println!("==== reflection\n{:#?}", s.reflection()),
        Err(e) => println!("error: {e}"),
    }
}
