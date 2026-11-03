//! Text objects (spec 02 §7.7, spec 04 §18), drawn the way Wallpaper Engine draws them: as an
//! image layer whose main texture is the rasterised text, followed by the object's effects and
//! `colorBlendMode` pass like any image (`lumen_scene::TextObject::layer`).
//!
//! - Shaping, layout and rasterisation: `cosmic-text` (harfrust + swash). The font database
//!   starts empty: it only receives the font file the wallpaper names (through the VFS, so a
//!   `.ttf`/`.otf` inside the `.pkg`, the wallpaper folder or the WE `assets` folder), then, when
//!   that font is missing or lacks a glyph, the fallback fonts of spec 04 §18.2 / Q19. No system
//!   font scan.
//! - The glyph texture is one R8 coverage texture per object. The WE `font` shader multiplies it
//!   by `g_Color4` = (colour × brightness, alpha), so colour and alpha changes are uniform writes.
//! - The texture is regenerated only when the displayed string or the point size changes
//!   (checked every frame without allocating); nothing else happens per frame.
//!
//! Geometry (Lumen, parity rule 5: WE behaviour known from WE-authored data, see the crate
//! README): one em is `pointsize · 300/72` layer pixels and a line is `ascender − descender`
//! (hhea) high, which reproduces the `size` the WE editor stores for every text of the installed
//! wallpapers. The text block is anchored on the object's origin by its horizontal and vertical
//! alignment (left: its left edge; centre: the middle of the lines from the first ascender to
//! the last baseline), measured against WE renders; `padding` surrounds it in the texture to give
//! effects room without moving it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, Wrap, fontdb,
};
use lumen_assets::Vfs;
use lumen_render::dynbuf::UniformSlot;
use lumen_render::math::{HAlign, VAlign};
use lumen_render::texture::{MipLevel, TexFlags, TexelFormat, TextureUpload};
use lumen_render::{GpuContext, PassId, RenderScene, SceneBuilder, TextureId};
use lumen_scene::{MainTextureFormat, ObjectIndex, ObjectKind, Project, SettingId, Settings};
use serde_json::Value as Json;

use crate::texture::{LoadedTexture, TextureCache};

/// Layer pixels per point: Wallpaper Engine lays text out at 300 dpi.
///
/// Evidence: for every text object of the installed wallpapers (Atami, Quicksand, Anurati
/// fonts, 32 and 96 pt), the `size` saved by the WE editor is exactly
/// `(ascender − descender) · pointsize · 300/72 + 2 · padding` in height.
pub const PIXELS_PER_POINT: f32 = 300.0 / 72.0;

/// Largest glyph texture side (well under every adapter's 2D limit).
const MAX_TEXTURE_SIDE: u32 = 8192;

/// Width reserved for texts that can change (script or property binding): `width · factor +
/// em`. A changed string re-renders into the same texture as long as it fits; WE resizes the
/// layer instead, but the text stays anchored the same way, so the result is identical. A
/// longer string is drawn smaller to fit (see `fit`). Example: a clock script turning
/// "12:34" into "-03:42-" needs 1.5× (wallpaper 3372348904).
const HEADROOM_FACTOR: f32 = 1.5;
const HEADROOM_EM: f32 = 1.0;

/// Fallback fonts of spec 04 §18.2 (Linux: DejaVu Sans, then Liberation Sans, at their usual
/// locations) and Q19 (Windows: Segoe UI, then Arial). One file per family: the first found.
#[cfg(not(windows))]
const FALLBACK_FILES: &[&[&str]] = &[
    &[
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf",
    ],
    &[
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/truetype/liberation2/LiberationSans-Regular.ttf",
        "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
        "/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf",
        "/usr/share/fonts/liberation-sans-fonts/LiberationSans-Regular.ttf",
        "/usr/share/fonts/TTF/LiberationSans-Regular.ttf",
    ],
];
#[cfg(windows)]
const FALLBACK_FILES: &[&[&str]] = &[&["segoeui.ttf"], &["arial.ttf"]];
/// Wallpaper Engine's bundled sans-serif (VFS path, `assets/fonts`), after the system ones.
const WE_FALLBACK: &str = "fonts/NotoSans-Regular.ttf";

#[cfg(windows)]
fn windows_fonts_dir() -> PathBuf {
    std::env::var_os("WINDIR")
        .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from)
        .join("Fonts")
}

/// First existing file of each fallback family, in order.
fn fallback_paths() -> impl Iterator<Item = PathBuf> {
    #[cfg(windows)]
    let dir = windows_fonts_dir();
    FALLBACK_FILES.iter().filter_map(move |family| {
        family
            .iter()
            .map(|f| {
                #[cfg(windows)]
                {
                    dir.join(f)
                }
                #[cfg(not(windows))]
                {
                    PathBuf::from(f)
                }
            })
            .find(|p| p.is_file())
    })
}

/// `systemfont_<name>` → an installed file of that name (Windows only, Q19). Linux has no
/// stable family → file mapping without a font scan: the fallback is used.
fn system_font_path(name: &str) -> Option<PathBuf> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'))
    {
        return None;
    }
    #[cfg(windows)]
    {
        let dir = windows_fonts_dir();
        let stem = name.to_ascii_lowercase().replace(' ', "");
        ["ttf", "otf", "ttc"]
            .iter()
            .map(|ext| dir.join(format!("{stem}.{ext}")))
            .find(|p| p.is_file())
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// One loaded font face and what layout needs from it.
#[derive(Clone, Debug)]
struct Face {
    family: String,
    weight: fontdb::Weight,
    style: fontdb::Style,
    stretch: fontdb::Stretch,
    /// Ascender per em (positive).
    ascent: f32,
    /// Descender per em (positive).
    descent: f32,
}

impl Face {
    fn attrs(&self) -> Attrs<'_> {
        Attrs::new()
            .family(Family::Name(&self.family))
            .weight(self.weight)
            .style(self.style)
            .stretch(self.stretch)
    }
}

/// Font database, shaper and glyph cache of one scene. Created with the first text object.
pub struct Fonts {
    system: FontSystem,
    swash: SwashCache,
    /// VFS path → face (`None`: unreadable, logged once).
    files: HashMap<String, Option<Face>>,
    /// Fallback faces, loaded on first need; the first one replaces a missing font.
    fallback: Option<Vec<Face>>,
}

impl Default for Fonts {
    fn default() -> Self {
        Self::new()
    }
}

impl Fonts {
    /// Empty database: nothing is read until a text asks for a font.
    pub fn new() -> Self {
        Self {
            system: FontSystem::new_with_locale_and_db("en-US".into(), fontdb::Database::new()),
            swash: SwashCache::new(),
            files: HashMap::new(),
            fallback: None,
        }
    }

    /// Number of faces in the database.
    pub fn faces(&self) -> usize {
        self.system.db().len()
    }

    fn load(&mut self, bytes: Vec<u8>) -> Option<Face> {
        let ids = self
            .system
            .db_mut()
            .load_font_source(fontdb::Source::Binary(Arc::new(bytes)));
        let id = *ids.first()?;
        let info = self.system.db().face(id)?;
        let family = info.families.first()?.0.clone();
        let (weight, style, stretch) = (info.weight, info.style, info.stretch);
        let font = self.system.get_font(id, weight)?;
        let m = font.metrics();
        let upem = f32::from(m.units_per_em.max(1));
        Some(Face {
            family,
            weight,
            style,
            stretch,
            ascent: m.ascent / upem,
            descent: -m.descent / upem,
        })
    }

    fn load_file(&mut self, path: &std::path::Path) -> Option<Face> {
        let bytes = std::fs::read(path).ok()?;
        let face = self.load(bytes);
        if face.is_none() {
            tracing::warn!("font '{}' cannot be parsed", path.display());
        }
        face
    }

    /// Loads the fallback faces once (spec 04 §18.2, Q19): DejaVu Sans and Liberation Sans
    /// (Segoe UI and Arial on Windows) when installed, then WE's Noto Sans when a VFS is given.
    /// The first one replaces a missing font; all of them provide glyphs a font lacks.
    fn fallbacks(&mut self, vfs: Option<&Vfs>) -> &[Face] {
        if self.fallback.is_none() {
            let mut faces = Vec::new();
            for path in fallback_paths() {
                if let Some(face) = self.load_file(&path) {
                    faces.push(face);
                }
            }
            if let Some(Ok(bytes)) = vfs.map(|v| v.read(WE_FALLBACK))
                && let Some(face) = self.load(bytes.into_owned())
            {
                faces.push(face);
            }
            if faces.is_empty() {
                tracing::warn!("no fallback font found (DejaVu Sans, Liberation Sans, Noto Sans)");
            } else {
                tracing::debug!(
                    "text fallback fonts: {:?}",
                    faces.iter().map(|f| f.family.as_str()).collect::<Vec<_>>()
                );
            }
            self.fallback = Some(faces);
        }
        self.fallback.as_deref().unwrap_or_default()
    }

    /// Face of a text object's `font` (spec 04 §18.2): the VFS file, else the fallback.
    fn face(&mut self, vfs: &Vfs, font: &str) -> Option<Face> {
        if let Some(name) = font.strip_prefix("systemfont_") {
            if let Some(face) = self.files.get(font) {
                if face.is_some() {
                    return face.clone();
                }
            } else {
                let face = system_font_path(name).and_then(|p| self.load_file(&p));
                self.files.insert(font.to_owned(), face.clone());
                if face.is_some() {
                    return face;
                }
            }
        } else if !font.is_empty() {
            let face = match self.files.get(font) {
                Some(face) => face.clone(),
                None => {
                    let face = match vfs.read(font) {
                        Ok(bytes) => {
                            let face = self.load(bytes.into_owned());
                            if face.is_none() {
                                tracing::warn!("font '{font}' cannot be parsed, fallback used");
                            }
                            face
                        }
                        Err(e) => {
                            tracing::warn!("font '{font}': {e}; fallback used");
                            None
                        }
                    };
                    self.files.insert(font.to_owned(), face.clone());
                    face
                }
            };
            if face.is_some() {
                return face;
            }
        }
        self.fallbacks(Some(vfs)).first().cloned()
    }
}

/// Layout settings of one text object (fixed at load; the point size is read live).
#[derive(Clone, Debug)]
struct Style {
    face: Face,
    h: HAlign,
    v: VAlign,
    /// `padding` in pixels on each side.
    padding: (u32, u32),
    /// `maxwidth` when `limitwidth` (layer pixels): lines wrap there.
    max_width: Option<f32>,
    /// `maxrows` when `limitrows`.
    max_rows: Option<usize>,
    /// `limituseellipsis`: a truncated last row ends with "…".
    ellipsis: bool,
}

/// A shaped string.
struct Shaped {
    buffer: Buffer,
    /// Widest line.
    width: f32,
    /// Number of lines.
    lines: usize,
    /// Line height, ascender and descender in pixels.
    line: f32,
    ascent: f32,
    descent: f32,
}

impl Shaped {
    /// Height of the block the vertical alignment uses: from the top of the first line to the
    /// last baseline (the last descender hangs below). Measured on Wallpaper Engine renders: a
    /// centred single line has its baseline `ascender / 2` below the origin (3 fonts, 2
    /// wallpapers).
    fn aligned_height(&self) -> f32 {
        self.lines.saturating_sub(1) as f32 * self.line + self.ascent
    }

    /// Box height needed around the anchor for this vertical alignment.
    fn needed_height(&self, v: VAlign) -> f32 {
        match v {
            VAlign::Top | VAlign::Bottom => self.aligned_height() + self.descent,
            VAlign::Center => self.aligned_height() + 2.0 * self.descent,
        }
    }
}

fn align_h(a: HAlign, extent: f32) -> f32 {
    match a {
        HAlign::Left => 0.0,
        HAlign::Center => extent * 0.5,
        HAlign::Right => extent,
    }
}

fn align_v(a: VAlign, extent: f32) -> f32 {
    match a {
        VAlign::Top => 0.0,
        VAlign::Center => extent * 0.5,
        VAlign::Bottom => extent,
    }
}

/// Where a text's anchor point (the point placed on the object's origin) lies in its texture
/// of `box + 2·padding`: on the box edge or centre given by the alignment, the bottom anchor
/// leaving room for the last descender. Padding is outside the box on every side, so it never
/// moves the text (Wallpaper Engine: a left-aligned text starts exactly at its origin).
fn anchor_in_texture(style: &Style, box_size: (u32, u32), descent: f32) -> (f32, f32) {
    let (bw, bh) = (box_size.0 as f32, box_size.1 as f32);
    let x = style.padding.0 as f32 + align_h(style.h, bw);
    let y = style.padding.1 as f32
        + match style.v {
            VAlign::Top => 0.0,
            VAlign::Center => bh * 0.5,
            VAlign::Bottom => bh - descent,
        };
    (x, y)
}

/// Offset to add to the origin (layer pixels before scale, x right, y up) so that the anchor
/// point lands on it, given that the image quad itself is anchored by its edge or centre.
pub fn anchor_offset(h: HAlign, v: VAlign, size: (u32, u32), anchor: (f32, f32)) -> (f32, f32) {
    let qx = align_h(h, size.0 as f32);
    let qy = align_v(v, size.1 as f32);
    (qx - anchor.0, anchor.1 - qy)
}

/// Shapes and lays out `text` at `em` pixels per em: lines split at line breaks, wrapped at
/// `max_width`, cut to `max_rows` (with an ellipsis if asked).
fn shape(fonts: &mut Fonts, style: &Style, text: &str, em: f32) -> Shaped {
    let em = em.max(1.0);
    let line = (em * (style.face.ascent + style.face.descent)).max(1.0);
    let attrs = style.face.attrs();
    let fs = &mut fonts.system;
    let mut buffer = Buffer::new_empty(Metrics::new(em, line));
    buffer.set_wrap(
        fs,
        if style.max_width.is_some() {
            Wrap::WordOrGlyph
        } else {
            Wrap::None
        },
    );
    buffer.set_size(fs, style.max_width, None);
    buffer.set_text(fs, text, &attrs, Shaping::Advanced, None);
    if let Some(max_rows) = style.max_rows
        && buffer.layout_runs().count() > max_rows.max(1)
    {
        let cut = truncate_rows(fonts, style, &buffer, max_rows.max(1), em, line);
        buffer.set_text(&mut fonts.system, &cut, &attrs, Shaping::Advanced, None);
    }
    let (width, lines) = buffer
        .layout_runs()
        .fold((0.0f32, 0usize), |(w, n), r| (w.max(r.line_w), n + 1));
    Shaped {
        buffer,
        width,
        lines: lines.max(1),
        line,
        ascent: em * style.face.ascent,
        descent: em * style.face.descent,
    }
}

/// [`shape`], then, if a glyph is missing and the fallback faces are not loaded yet, loads them
/// (once per scene) and shapes again: the shaper takes missing glyphs from them.
fn shape_with_fallback(
    fonts: &mut Fonts,
    style: &Style,
    text: &str,
    em: f32,
    vfs: Option<&Vfs>,
) -> Shaped {
    let shaped = shape(fonts, style, text, em);
    let missing = shaped
        .buffer
        .layout_runs()
        .any(|r| r.glyphs.iter().any(|g| g.glyph_id == 0));
    if missing && fonts.fallback.is_none() && !fonts.fallbacks(vfs).is_empty() {
        return shape(fonts, style, text, em);
    }
    shaped
}

/// Width of one unwrapped line.
fn line_width(fonts: &mut Fonts, style: &Style, text: &str, em: f32, line: f32) -> f32 {
    let mut b = Buffer::new_empty(Metrics::new(em, line));
    b.set_text(
        &mut fonts.system,
        text,
        &style.face.attrs(),
        Shaping::Advanced,
        None,
    );
    b.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
}

/// The first `max_rows` laid-out rows as text, the last one ending with "…" (shortened until it
/// fits `max_width`) when `ellipsis` is set.
fn truncate_rows(
    fonts: &mut Fonts,
    style: &Style,
    buffer: &Buffer,
    max_rows: usize,
    em: f32,
    line: f32,
) -> String {
    let mut rows: Vec<String> = buffer
        .layout_runs()
        .take(max_rows)
        .map(|r| {
            let start = r.glyphs.iter().map(|g| g.start).min().unwrap_or(0);
            let end = r.glyphs.iter().map(|g| g.end).max().unwrap_or(0);
            r.text.get(start..end).unwrap_or_default().to_owned()
        })
        .collect();
    if style.ellipsis
        && let Some(last) = rows.last_mut()
    {
        let limit = style.max_width.unwrap_or(f32::INFINITY);
        loop {
            let candidate = format!("{}…", last.trim_end());
            if last.is_empty() || line_width(fonts, style, &candidate, em, line) <= limit {
                *last = candidate;
                break;
            }
            last.pop();
        }
    }
    rows.join("\n")
}

/// Draws the shaped lines into an R8 coverage image of `size`, the text block placed on the
/// `anchor` point by its alignment. Overlapping glyphs keep the highest coverage.
fn rasterize(
    fonts: &mut Fonts,
    shaped: &Shaped,
    style: &Style,
    anchor: (f32, f32),
    size: (u32, u32),
) -> Vec<u8> {
    let (w, h) = (size.0 as usize, size.1 as usize);
    let mut px = vec![0u8; w * h];
    let bx = (anchor.0 - align_h(style.h, shaped.width)).round();
    let by = (anchor.1 - align_v(style.v, shaped.aligned_height())).round();
    for run in shaped.buffer.layout_runs() {
        let x = bx + align_h(style.h, shaped.width - run.line_w);
        let y = by + run.line_y;
        for g in run.glyphs {
            let p = g.physical((x, y), 1.0);
            let Some(img) = fonts.swash.get_image(&mut fonts.system, p.cache_key) else {
                continue;
            };
            let (stride, channel) = match img.content {
                SwashContent::Mask => (1usize, 0usize),
                SwashContent::Color => (4, 3),
                SwashContent::SubpixelMask => (4, 1),
            };
            let pl = img.placement;
            let x0 = i64::from(p.x) + i64::from(pl.left);
            let y0 = i64::from(p.y) - i64::from(pl.top);
            for gy in 0..pl.height as i64 {
                let ty = y0 + gy;
                if ty < 0 || ty >= h as i64 {
                    continue;
                }
                let row = ty as usize * w;
                for gx in 0..pl.width as i64 {
                    let tx = x0 + gx;
                    if tx < 0 || tx >= w as i64 {
                        continue;
                    }
                    let src = (gy as usize * pl.width as usize + gx as usize) * stride + channel;
                    let Some(&c) = img.data.get(src) else {
                        continue;
                    };
                    let d = &mut px[row + tx as usize];
                    *d = (*d).max(c);
                }
            }
        }
    }
    px
}

/// Reads a plain JSON field of the object, or the `value` of a `{ "value": … }` form.
fn raw_field<'a>(object: Option<&'a Json>, key: &str) -> Option<&'a Json> {
    let v = object?.get(key)?;
    match v {
        Json::Object(o) => o.get("value"),
        v => Some(v),
    }
}

/// A text object being set up (creation order), finished by [`TextRuntime::attach`].
pub struct TextRuntime {
    /// Object `id`.
    pub object_id: i32,
    text: SettingId,
    point_size: SettingId,
    color: SettingId,
    alpha: SettingId,
    brightness: SettingId,
    /// Empty content displays a space (spec 04 §18.2).
    scripted: bool,
    style: Style,
    box_size: (u32, u32),
    /// Anchor point in the texture (fixed: a changed string stays anchored the same way).
    anchor: (f32, f32),
    texture_size: (u32, u32),
    texture: TextureId,
    color_slot: Option<UniformSlot>,
    /// String currently in the texture.
    shown: String,
    /// Point size currently in the texture.
    shown_points: f32,
    /// Content set through [`TextRuntime::set_text`] (scripts); replaces the setting.
    forced: Option<String>,
    /// Texture regenerations after load, and their total time.
    pub regenerations: u64,
    pub regeneration_time: Duration,
}

/// What the scene builder needs to know about a prepared text.
pub struct PreparedText {
    pub runtime: TextRuntime,
    pub texture: Rc<LoadedTexture>,
    /// Added to the object's origin (layer pixels × scale, x right, y up) so that the text's
    /// anchor point, not the padded quad's edge, lands on it.
    pub offset: (f32, f32),
}

fn source_text(settings: &Settings, setting: SettingId, scripted: bool) -> &str {
    let s = settings.value(setting).text();
    if s.is_empty() && scripted { " " } else { s }
}

/// Lays out and rasterises a text object, uploads its glyph texture and registers it in the
/// texture cache under the name its layer material samples. `Ok(None)`: nothing to draw
/// (static empty text, spec 04 §18.2).
#[allow(clippy::too_many_arguments)]
pub fn prepare(
    b: &mut SceneBuilder<'_>,
    project: &Project,
    vfs: &Vfs,
    fonts: &mut Fonts,
    textures: &mut TextureCache,
    idx: ObjectIndex,
    raw: Option<&Json>,
) -> Result<Option<PreparedText>, String> {
    let scene = project.scene.as_ref().ok_or("not a scene project")?;
    let obj = scene.object(idx);
    let ObjectKind::Text(t) = &obj.kind else {
        return Err("not a text".into());
    };
    let settings = &project.settings;
    let meta = settings.meta(t.text);
    let scripted = meta.script.is_some();
    let changes = scripted || meta.property.is_some();
    let content = source_text(settings, t.text, scripted);
    if content.is_empty() && !changes {
        return Ok(None);
    }
    let Some(lumen_scene::TextureRef::File(name)) = project.main_texture(idx) else {
        return Err("text layer without glyph texture slot".into());
    };
    let name = project.strings.resolve(name).to_owned();
    let font = project.strings.resolve(t.font);
    let face = fonts
        .face(vfs, font)
        .ok_or_else(|| format!("font '{font}' unavailable and no fallback font found"))?;
    let h = if t.horizontal_align.left {
        HAlign::Left
    } else if t.horizontal_align.right {
        HAlign::Right
    } else {
        HAlign::Center
    };
    let v = if t.vertical_align.top {
        VAlign::Top
    } else if t.vertical_align.bottom {
        VAlign::Bottom
    } else {
        VAlign::Center
    };
    let flag = |k: &str| raw_field(raw, k).and_then(Json::as_bool).unwrap_or(false);
    let max_width = flag("limitwidth")
        .then(|| raw_field(raw, "maxwidth").and_then(Json::as_f64))
        .flatten()
        .map(|w| w as f32)
        .filter(|w| *w >= 1.0);
    let max_rows = flag("limitrows")
        .then(|| raw_field(raw, "maxrows").and_then(Json::as_u64))
        .flatten()
        .map(|r| r.max(1) as usize);
    let pad = |p: f32| p.clamp(0.0, 1024.0) as u32;
    let style = Style {
        face,
        h,
        v,
        padding: (pad(t.padding.x), pad(t.padding.y)),
        max_width,
        max_rows,
        ellipsis: flag("limituseellipsis"),
    };
    let points = settings.value(t.point_size).float();
    let em = points * PIXELS_PER_POINT;
    let shaped = shape_with_fallback(fonts, &style, content, em, Some(vfs));

    // Box: the text, at least the editor's size (`size` minus padding), plus room for changes.
    let mut bw = shaped.width.max(t.size.x - 2.0 * style.padding.0 as f32);
    let mut bh = shaped
        .needed_height(style.v)
        .max(t.size.y - 2.0 * style.padding.1 as f32);
    if let Some(mw) = style.max_width {
        // Lines never get wider than `maxwidth`.
        bw = shaped.width.max(mw);
    } else if changes {
        bw = bw * HEADROOM_FACTOR + HEADROOM_EM * em;
    }
    if let Some(rows) = style.max_rows {
        let extra = shaped.needed_height(style.v) - shaped.aligned_height();
        bh = bh.max((rows - 1) as f32 * shaped.line + shaped.ascent + extra);
    }
    let limit = |side: f32, pad: u32| {
        (((side - 0.01).ceil()).max(1.0) as u32)
            .min(MAX_TEXTURE_SIDE.saturating_sub(2 * pad).max(1))
    };
    let box_size = (limit(bw, style.padding.0), limit(bh, style.padding.1));
    let size = (
        box_size.0 + 2 * style.padding.0,
        box_size.1 + 2 * style.padding.1,
    );
    let anchor = anchor_in_texture(&style, box_size, shaped.descent);
    let shaped = fit(fonts, &style, content, em, shaped, box_size);
    let pixels = rasterize(fonts, &shaped, &style, anchor, size);
    let flags = TexFlags(TexFlags::CLAMP_UVS);
    let id = b
        .add_texture(&TextureUpload {
            label: &name,
            format: TexelFormat::R8,
            levels: &[MipLevel {
                width: size.0,
                height: size.1,
                data: &pixels,
            }],
            content_size: None,
            flags,
        })
        .map_err(|e| e.to_string())?;
    textures.uploaded_bytes += pixels.len() as u64;
    let texture = textures.insert(
        &name,
        LoadedTexture {
            ids: vec![id],
            storage: size,
            real: size,
            flags,
            format: TexelFormat::R8,
            main_format: MainTextureFormat::R8,
            animation: None,
            spritesheet: Default::default(),
            tex_json: None,
        },
    );
    tracing::debug!(
        "text object {}: {:?} in '{}' at {points} pt, texture {}x{}",
        obj.id,
        content,
        style.face.family,
        size.0,
        size.1
    );
    Ok(Some(PreparedText {
        offset: anchor_offset(style.h, style.v, size, anchor),
        runtime: TextRuntime {
            object_id: obj.id,
            text: t.text,
            point_size: t.point_size,
            color: t.color,
            alpha: t.alpha,
            brightness: t.brightness,
            scripted,
            style,
            box_size,
            anchor,
            texture_size: size,
            texture: id,
            color_slot: None,
            shown: content.to_owned(),
            shown_points: points,
            forced: None,
            regenerations: 0,
            regeneration_time: Duration::ZERO,
        },
        texture,
    }))
}

/// Shrinks the text uniformly when it overflows the texture box (a changed string longer than
/// the room reserved at load; WE would grow the layer). Rare, logged at debug level.
fn fit(
    fonts: &mut Fonts,
    style: &Style,
    text: &str,
    em: f32,
    shaped: Shaped,
    box_size: (u32, u32),
) -> Shaped {
    let (bw, bh) = (box_size.0 as f32, box_size.1 as f32);
    let needed = shaped.needed_height(style.v);
    if shaped.width <= bw + 0.5 && needed <= bh + 0.5 {
        return shaped;
    }
    let f = (bw / shaped.width.max(1.0))
        .min(bh / needed.max(1.0))
        .min(1.0);
    tracing::debug!("text {text:?} overflows its {bw}x{bh} box: drawn at {f:.2}x");
    shape(fonts, style, text, em * f)
}

impl TextRuntime {
    /// Binds the glyph pass (first pass of the layer) once the image passes exist.
    pub fn attach(&mut self, b: &mut SceneBuilder<'_>, glyph_pass: PassId, settings: &Settings) {
        self.color_slot = b.uniform(glyph_pass, "g_Color4");
        if let Some(slot) = &self.color_slot {
            b.shadow_mut().write_floats(slot, &self.color4(settings));
        }
    }

    /// `g_Color4` of the glyph pass: (colour × brightness, alpha).
    fn color4(&self, settings: &Settings) -> [f32; 4] {
        let c = settings.value(self.color).vec3() * settings.value(self.brightness).float();
        [c.x, c.y, c.z, settings.value(self.alpha).float()]
    }

    /// Replaces the content (SceneScript `thisLayer.text`). Applied by the next update, only if
    /// the string differs from the one shown. No allocation once the buffer is large enough.
    pub fn set_text(&mut self, text: &str) {
        match &mut self.forced {
            Some(s) if s == text => {}
            Some(s) => {
                s.clear();
                s.push_str(text);
            }
            None => self.forced = Some(text.to_owned()),
        }
    }

    /// The string currently displayed.
    pub fn shown(&self) -> &str {
        &self.shown
    }

    /// `g_Color4` of the glyph pass, owned by this runtime (the layer's neutral colour must not
    /// be written over it).
    pub fn color_slot(&self) -> Option<&UniformSlot> {
        self.color_slot.as_ref()
    }

    /// Glyph texture size (box + padding).
    pub fn texture_size(&self) -> (u32, u32) {
        self.texture_size
    }

    /// Per frame: colour uniform (written only when it changed) and regeneration when the string
    /// or the point size changed. No allocation in the steady state.
    pub fn update(
        &mut self,
        ctx: &GpuContext,
        scene: &mut RenderScene,
        fonts: &mut Fonts,
        settings: &Settings,
    ) {
        if let Some(slot) = &self.color_slot {
            scene
                .shadow_mut()
                .write_floats(slot, &self.color4(settings));
        }
        let points = settings.value(self.point_size).float();
        let text = match &self.forced {
            Some(s) if s.is_empty() && self.scripted => " ",
            Some(s) => s.as_str(),
            None => source_text(settings, self.text, self.scripted),
        };
        if text == self.shown && points == self.shown_points {
            return;
        }
        let t0 = Instant::now();
        let em = points * PIXELS_PER_POINT;
        let shaped = shape_with_fallback(fonts, &self.style, text, em, None);
        let shaped = fit(fonts, &self.style, text, em, shaped, self.box_size);
        let pixels = rasterize(fonts, &shaped, &self.style, self.anchor, self.texture_size);
        if let Some(tex) = scene.texture(self.texture) {
            tex.write_level0(ctx, &pixels);
        }
        scene.mark_dirty();
        self.shown.clear();
        self.shown.push_str(text);
        self.shown_points = points;
        self.regenerations += 1;
        self.regeneration_time += t0.elapsed();
    }
}

/// Lays out and rasterises `text` with a font file, without GPU: `(width, height, coverage)`.
/// Used by tests and benchmarks.
pub fn rasterize_standalone(
    fonts: &mut Fonts,
    font: &[u8],
    text: &str,
    points: f32,
    padding: u32,
) -> Option<((u32, u32), Vec<u8>)> {
    let face = fonts.load(font.to_vec())?;
    let style = Style {
        face,
        h: HAlign::Left,
        v: VAlign::Center,
        padding: (padding, padding),
        max_width: None,
        max_rows: None,
        ellipsis: false,
    };
    let shaped = shape(fonts, &style, text, points * PIXELS_PER_POINT);
    let box_size = (
        shaped.width.ceil().max(1.0) as u32,
        shaped.needed_height(style.v).ceil().max(1.0) as u32,
    );
    let size = (box_size.0 + 2 * padding, box_size.1 + 2 * padding);
    let anchor = anchor_in_texture(&style, box_size, shaped.descent);
    let px = rasterize(fonts, &shaped, &style, anchor, size);
    Some((size, px))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    fn style(h: HAlign, v: VAlign, pad: u32) -> Style {
        Style {
            face: Face {
                family: String::new(),
                weight: fontdb::Weight::NORMAL,
                style: fontdb::Style::Normal,
                stretch: fontdb::Stretch::Normal,
                ascent: 0.8,
                descent: 0.2,
            },
            h,
            v,
            padding: (pad, pad),
            max_width: None,
            max_rows: None,
            ellipsis: false,
        }
    }

    #[test]
    fn anchor_is_the_text_edge_not_the_padded_quad_edge() {
        let size = (164, 124); // box 100×60, padding 32
        let left = style(HAlign::Left, VAlign::Center, 32);
        let a = anchor_in_texture(&left, (100, 60), 10.0);
        assert_eq!(a, (32.0, 62.0));
        // The quad is anchored by its left edge: move it left by the padding.
        assert_eq!(
            anchor_offset(HAlign::Left, VAlign::Center, size, a),
            (-32.0, 0.0)
        );
        let right = style(HAlign::Right, VAlign::Top, 32);
        let a = anchor_in_texture(&right, (100, 60), 10.0);
        assert_eq!(a, (132.0, 32.0));
        // Right: move right by the padding; top: up by the padding (y up).
        assert_eq!(
            anchor_offset(HAlign::Right, VAlign::Top, size, a),
            (32.0, 32.0)
        );
        let bottom = style(HAlign::Center, VAlign::Bottom, 32);
        let a = anchor_in_texture(&bottom, (100, 60), 10.0);
        // The last baseline sits on the origin, the descender below it.
        assert_eq!(a, (82.0, 82.0));
        assert_eq!(
            anchor_offset(HAlign::Center, VAlign::Bottom, size, a),
            (0.0, -42.0)
        );
        let centre = style(HAlign::Center, VAlign::Center, 32);
        let a = anchor_in_texture(&centre, (100, 60), 10.0);
        assert_eq!(
            anchor_offset(HAlign::Center, VAlign::Center, size, a),
            (0.0, 0.0)
        );
    }

    /// First fallback font installed on this machine, if any.
    fn system_font() -> Option<Vec<u8>> {
        fallback_paths().find_map(|p| std::fs::read(p).ok())
    }

    #[test]
    fn rasterises_and_measures_generation_cost() {
        let Some(font) = system_font() else {
            eprintln!("skipped: no DejaVu Sans / Liberation Sans installed");
            return;
        };
        let mut fonts = Fonts::new();
        assert_eq!(fonts.faces(), 0, "the font database starts empty");
        let ((w, h), px) = rasterize_standalone(&mut fonts, &font, "12:34", 32.0, 8).unwrap();
        assert_eq!(fonts.faces(), 1);
        assert_eq!(px.len(), (w * h) as usize);
        // 32 pt = 133 px per em: five glyphs are a few hundred pixels wide.
        assert!((200..600).contains(&w), "{w}x{h}");
        assert!(
            px.iter().filter(|&&c| c > 200).count() > 1000,
            "solid glyph cores"
        );
        // The padding stays empty.
        assert!(px[..(8 * w) as usize].iter().all(|&c| c == 0));
        let t0 = Instant::now();
        let n = 20;
        for i in 0..n {
            let s = format!("{:02}:{:02}", i % 24, (i * 7) % 60);
            rasterize_standalone(&mut fonts, &font, &s, 32.0, 8).unwrap();
        }
        eprintln!(
            "layout + rasterisation of a 32 pt clock: {:?} per string",
            t0.elapsed() / n
        );
    }

    #[test]
    fn rows_and_ellipsis() {
        let Some(font) = system_font() else {
            eprintln!("skipped: no system font");
            return;
        };
        let mut fonts = Fonts::new();
        let face = fonts.load(font).unwrap();
        let mut s = style(HAlign::Left, VAlign::Top, 0);
        s.face = face;
        s.max_width = Some(300.0);
        let shaped = shape(&mut fonts, &s, "a long title that wraps over lines", 40.0);
        assert!(shaped.lines > 1 && shaped.width <= 300.5);
        s.max_rows = Some(1);
        s.ellipsis = true;
        let shaped = shape(&mut fonts, &s, "a long title that wraps over lines", 40.0);
        assert_eq!(shaped.lines, 1);
        assert!(shaped.width <= 300.5);
        let text: String = shaped
            .buffer
            .layout_runs()
            .map(|r| r.text.to_owned())
            .collect();
        assert!(text.ends_with('…'), "{text:?}");
    }
}
