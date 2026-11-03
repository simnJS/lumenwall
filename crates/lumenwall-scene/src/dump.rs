//! Structure dump (spec 02 §15.2): one tab per level, booleans as 0/1, material modes as numbers.

use std::fmt::Write as _;

use crate::project::{Project, ProjectKind};
use crate::scene::{BindSource, Command, Material, MaterialId, ObjectKind, Scene, TextureMap};

struct Out<'p> {
    s: String,
    p: &'p Project,
}

impl Out<'_> {
    fn line(&mut self, depth: usize, text: std::fmt::Arguments<'_>) {
        for _ in 0..depth {
            self.s.push('\t');
        }
        let _ = self.s.write_fmt(text);
        self.s.push('\n');
    }

    fn str(&self, sym: crate::Sym) -> &str {
        self.p.strings.resolve(sym)
    }

    fn textures(&mut self, d: usize, label: &str, m: &TextureMap) {
        if m.is_empty() {
            return;
        }
        self.line(d, format_args!("{label} count: {}", m.entries().len()));
        for &(slot, name) in m.entries() {
            let n = self.str(name).to_owned();
            self.line(d, format_args!("Texture {slot}: {n}"));
        }
    }

    fn material(&mut self, d: usize, scene: &Scene, id: MaterialId) {
        let m: &Material = scene.material(id);
        let file = self.str(m.file).to_owned();
        self.line(d, format_args!("Material {file}:"));
        self.line(d + 1, format_args!("Passes count: {}", m.passes.len()));
        for p in &m.passes {
            let shader = self.str(p.shader).to_owned();
            self.line(d + 1, format_args!("Pass {shader}:"));
            self.line(d + 2, format_args!("Depth test: {}", p.depth_test as u8));
            self.line(d + 2, format_args!("Depth write: {}", p.depth_write as u8));
            self.line(d + 2, format_args!("Blend mode: {}", p.blending as u8));
            self.line(d + 2, format_args!("Culling mode: {}", p.cull_mode as u8));
            self.textures(d + 2, "Textures", &p.textures);
            self.textures(d + 2, "User textures", &p.user_textures);
            if !p.combos.is_empty() {
                self.line(d + 2, format_args!("Combos count: {}", p.combos.len()));
                for c in &p.combos {
                    let n = self.str(c.name).to_owned();
                    self.line(d + 2, format_args!("Combo {n}: {}", c.value));
                }
            }
        }
    }

    fn bind_name(&self, b: BindSource) -> String {
        match b {
            BindSource::Named(n) => self.str(n).to_owned(),
            BindSource::Previous => "previous".into(),
            BindSource::OwnComposite => "<own composite A>".into(),
        }
    }
}

impl Project {
    /// Textual dump of the loaded structure (§15.2).
    pub fn dump(&self) -> String {
        let mut o = Out {
            s: String::new(),
            p: self,
        };
        match self.kind {
            ProjectKind::Video => o.line(0, format_args!("Video wallpaper: {}", self.file)),
            ProjectKind::Web => o.line(0, format_args!("Web wallpaper: {}", self.file)),
            ProjectKind::Scene => {
                o.line(0, format_args!("Scene wallpaper: "));
                if let Some(scene) = &self.scene {
                    dump_scene(&mut o, scene);
                }
            }
        }
        o.s
    }
}

fn dump_scene(o: &mut Out<'_>, scene: &Scene) {
    o.line(1, format_args!("Objects count: {}", scene.objects.len()));
    for obj in &scene.objects {
        let name = o.str(obj.name).to_owned();
        o.line(1, format_args!("Object {} {name}:", obj.id));
        let deps: String = obj.dependencies.iter().map(|d| format!("{d}, ")).collect();
        o.line(
            2,
            format_args!("Dependencies ({}): {deps}", obj.dependencies.len()),
        );
        match &obj.kind {
            ObjectKind::Image(img) => {
                if let Some(mid) = img.model {
                    let m = scene.model(mid);
                    let file = o.str(m.file).to_owned();
                    o.line(2, format_args!("Model {file}:"));
                    o.line(3, format_args!("Autosize: {}", u8::from(m.autosize)));
                    o.line(3, format_args!("Passthrough: {}", u8::from(m.passthrough)));
                    o.line(3, format_args!("Fullscreen: {}", u8::from(m.fullscreen)));
                    o.line(3, format_args!("No padding: {}", u8::from(m.nopadding)));
                    o.line(3, format_args!("Solid layer: {}", u8::from(m.solid_layer)));
                    if let Some(w) = m.width {
                        o.line(3, format_args!("Width: {w}"));
                    }
                    if let Some(h) = m.height {
                        o.line(3, format_args!("Height: {h}"));
                    }
                    o.material(3, scene, m.material);
                } else {
                    let file = o.str(img.file).to_owned();
                    o.line(2, format_args!("Model {file}: (not loaded)"));
                }
                o.line(
                    2,
                    format_args!("Image effects count: {}", img.effects.len()),
                );
                for ie in &img.effects {
                    o.line(2, format_args!("Image effect {}:", ie.id));
                    let visible = o.p.settings.value(ie.visible).boolean();
                    o.line(
                        3,
                        format_args!("Default visibility status: {}", u8::from(visible)),
                    );
                    let e = scene.effect(ie.effect);
                    o.line(3, format_args!("Effect {}:", e.name));
                    if !e.description.is_empty() {
                        o.line(4, format_args!("Description: {}", e.description));
                    }
                    if !e.dependencies.is_empty() {
                        o.line(
                            4,
                            format_args!("Dependencies count: {}", e.dependencies.len()),
                        );
                    }
                    if !e.group.is_empty() {
                        o.line(4, format_args!("Group: {}", e.group));
                    }
                    if !e.preview.is_empty() {
                        o.line(4, format_args!("Preview: {}", e.preview));
                    }
                    o.line(4, format_args!("Passes count: {}", e.passes.len()));
                    for (i, p) in e.passes.iter().enumerate() {
                        o.line(4, format_args!("Pass {}:", i + 1));
                        if let Some(c) = p.command {
                            o.line(
                                5,
                                format_args!(
                                    "Command: {}",
                                    if c == Command::Copy { "copy" } else { "swap" }
                                ),
                            );
                        }
                        if let Some(t) = p.target {
                            let t = o.str(t).to_owned();
                            o.line(5, format_args!("Target: {t}"));
                        }
                        if let Some(s) = p.source {
                            let s = o.bind_name(s);
                            o.line(5, format_args!("Source: {s}"));
                        }
                        o.line(5, format_args!("Binds count: {}", p.binds.len()));
                        for b in &p.binds {
                            let n = o.bind_name(b.source);
                            o.line(5, format_args!("Bind {}: {n}", b.index));
                        }
                        if let Some(m) = p.material {
                            o.material(5, scene, m);
                        }
                    }
                    o.line(4, format_args!("FBOs count: {}", e.fbos.len()));
                    for f in &e.fbos {
                        let (n, fmt) = (o.str(f.name).to_owned(), o.str(f.format).to_owned());
                        o.line(4, format_args!("FBO {n}:"));
                        o.line(5, format_args!("Format: {fmt}"));
                        o.line(5, format_args!("Scale: {}", crate::text::fmt_g6(f.scale)));
                    }
                    o.line(
                        3,
                        format_args!("Effect overrides count: {}", ie.overrides.len()),
                    );
                    for ov in &ie.overrides {
                        o.line(3, format_args!("Pass override {}:", ov.id));
                        o.line(
                            4,
                            format_args!("Textures count: {}", ov.textures.entries().len()),
                        );
                        for &(slot, n) in ov.textures.entries() {
                            let n = o.str(n).to_owned();
                            o.line(4, format_args!("Texture {slot}: {n}"));
                        }
                        o.line(4, format_args!("Combos count: {}", ov.combos.len()));
                        for c in &ov.combos {
                            let n = o.str(c.name).to_owned();
                            o.line(4, format_args!("Combo {n}={}", c.value));
                        }
                        o.line(4, format_args!("Constants count: {}", ov.constants.len()));
                        for c in &ov.constants {
                            let n = o.str(c.name).to_owned();
                            let v = o.p.settings.value(c.setting).to_string();
                            o.line(4, format_args!("Constant {n}={v}"));
                        }
                    }
                }
            }
            ObjectKind::Sound(s) => {
                o.line(2, format_args!("Sounds: {}", s.files.len()));
                for f in &s.files {
                    let f = o.str(*f).to_owned();
                    o.line(3, format_args!("Filename {f}"));
                }
            }
            ObjectKind::Model3d(m) => {
                let f = o.str(m.file).to_owned();
                o.line(2, format_args!("Model {f} (skin {})", m.skin));
            }
            ObjectKind::Sprite(sp) => {
                let f = o.str(sp.file).to_owned();
                o.line(2, format_args!("Sprite {f}"));
            }
            ObjectKind::Light(l) => o.line(2, format_args!("Light {:?}", l.kind)),
            ObjectKind::Particle(_) | ObjectKind::Text(_) | ObjectKind::Generic(_) => {}
        }
    }
}
