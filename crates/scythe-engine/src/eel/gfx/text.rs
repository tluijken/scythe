//! Text for `@gfx`: the face `gfx_setfont` names, found through fontconfig and drawn with
//! `ab_glyph`. Measuring and drawing share one layout, so `gfx_measurestr` matches the ink.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use ab_glyph::{Font, FontArc, FontVec, GlyphId, PxScale, ScaleFont, point};

/// The face font 0 and fonts named without a face use.
const DEFAULT_FACE: &str = "sans-serif";
/// Glyph images kept between frames before the cache starts over.
const MAX_GLYPHS: usize = 8192;
/// Fonts tried when fontconfig is not there.
const FALLBACK_FILES: [&str; 4] = [
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
];

/// A font as `gfx_setfont` sets it: face name, height in pixels and flag characters
/// packed as a multi-character constant (`'bi'`).
#[derive(Debug, Clone, PartialEq)]
pub struct FontSpec {
    pub face: String,
    pub size: f32,
    pub flags: u32,
}

/// Which way text runs.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Turn {
    None,
    /// `'y'`: read from the bottom up.
    Up,
    /// `'z'`: read from the top down.
    Down,
}

impl FontSpec {
    fn has(&self, flag: u8) -> bool {
        self.flags.to_le_bytes().iter().any(|&b| b.eq_ignore_ascii_case(&flag))
    }

    fn turn(&self) -> Turn {
        if self.has(b'z') {
            Turn::Down
        } else if self.has(b'y') {
            Turn::Up
        } else {
            Turn::None
        }
    }

    /// The fontconfig pattern of the face with its weight and slant.
    fn pattern(&self) -> String {
        let face = if self.face.trim().is_empty() { DEFAULT_FACE } else { self.face.trim() };
        let mut p: String =
            face.chars().flat_map(|c| if "-:,\\".contains(c) { vec!['\\', c] } else { vec![c] }).collect();
        if self.has(b'b') {
            p.push_str(":bold");
        }
        if self.has(b'i') {
            p.push_str(":italic");
        }
        p
    }
}

struct GlyphImage {
    left: i32,
    top: i32,
    w: usize,
    coverage: Vec<u8>,
}

#[derive(Default)]
struct Fonts {
    loaded: Vec<FontArc>,
    by_pattern: HashMap<String, Option<usize>>,
    by_char: HashMap<char, Option<usize>>,
    glyphs: HashMap<(usize, u32, u16), Arc<GlyphImage>>,
}

fn fonts() -> std::sync::MutexGuard<'static, Fonts> {
    static FONTS: OnceLock<Mutex<Fonts>> = OnceLock::new();
    FONTS.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// The file and face index fontconfig picks for `pattern`.
fn fc_match(pattern: &str) -> Option<(PathBuf, u32)> {
    let out = Command::new("fc-match").args(["-f", "%{file}\n%{index}", pattern]).output().ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let (file, index) = text.split_once('\n').unwrap_or((&text, "0"));
    (!file.is_empty()).then(|| (PathBuf::from(file), index.trim().parse().unwrap_or(0)))
}

fn read_font(path: &std::path::Path, index: u32) -> Option<FontArc> {
    let data = std::fs::read(path).ok()?;
    FontVec::try_from_vec_and_index(data, index).ok().map(FontArc::new)
}

impl Fonts {
    fn add(&mut self, font: Option<FontArc>) -> Option<usize> {
        font.map(|f| {
            self.loaded.push(f);
            self.loaded.len() - 1
        })
    }

    fn face(&mut self, spec: &FontSpec) -> Option<usize> {
        let pattern = spec.pattern();
        if let Some(&found) = self.by_pattern.get(&pattern) {
            return found;
        }
        let font = fc_match(&pattern)
            .and_then(|(path, index)| read_font(&path, index))
            .or_else(|| FALLBACK_FILES.iter().find_map(|f| read_font(std::path::Path::new(f), 0)));
        let id = self.add(font);
        self.by_pattern.insert(pattern, id);
        id
    }

    /// A font that has `c`, for characters the chosen face lacks.
    fn for_char(&mut self, c: char) -> Option<usize> {
        if let Some(&found) = self.by_char.get(&c) {
            return found;
        }
        let font = fc_match(&format!(":charset={:x}", c as u32))
            .and_then(|(path, index)| read_font(&path, index))
            .filter(|f| f.glyph_id(c).0 != 0);
        let id = self.add(font);
        self.by_char.insert(c, id);
        id
    }

    fn glyph(&mut self, font: usize, size: f32, id: GlyphId) -> Arc<GlyphImage> {
        let key = (font, size.to_bits(), id.0);
        if let Some(g) = self.glyphs.get(&key) {
            return g.clone();
        }
        if self.glyphs.len() >= MAX_GLYPHS {
            self.glyphs.clear();
        }
        let f = &self.loaded[font];
        let glyph = id.with_scale_and_position(f.as_scaled(PxScale::from(size)).scale(), point(0.0, 0.0));
        let image = match f.outline_glyph(glyph) {
            Some(outlined) => {
                let b = outlined.px_bounds();
                let (w, h) = (b.width() as usize, b.height() as usize);
                let mut coverage = vec![0u8; w * h];
                outlined.draw(|x, y, c| {
                    if let Some(p) = coverage.get_mut(y as usize * w + x as usize) {
                        *p = (c.clamp(0.0, 1.0) * 255.0).round() as u8;
                    }
                });
                GlyphImage { left: b.min.x as i32, top: b.min.y as i32, w, coverage }
            }
            None => GlyphImage { left: 0, top: 0, w: 0, coverage: Vec::new() },
        };
        let image = Arc::new(image);
        self.glyphs.insert(key, image.clone());
        image
    }
}

struct Laid {
    /// Font, glyph and pen position of each character.
    glyphs: Vec<(usize, GlyphId, f32)>,
    width: f32,
    ascent: f32,
}

fn layout(fonts: &mut Fonts, spec: &FontSpec, text: &str) -> Option<Laid> {
    let main = fonts.face(spec)?;
    let scale = PxScale::from(spec.size.max(1.0));
    let mut glyphs = Vec::new();
    let mut pen = 0.0f32;
    let mut last: Option<(usize, GlyphId)> = None;
    for c in text.chars().filter(|c| !c.is_control()) {
        let mut font = main;
        let mut id = fonts.loaded[main].glyph_id(c);
        if id.0 == 0
            && let Some(other) = fonts.for_char(c)
        {
            (font, id) = (other, fonts.loaded[other].glyph_id(c));
        }
        let scaled = fonts.loaded[font].as_scaled(scale);
        if let Some((f, prev)) = last
            && f == font
        {
            pen += scaled.kern(prev, id);
        }
        glyphs.push((font, id, pen));
        pen += scaled.h_advance(id);
        last = Some((font, id));
    }
    let ascent = fonts.loaded[main].as_scaled(scale).ascent();
    Some(Laid { glyphs, width: pen, ascent })
}

/// Width and height of one line of `text`, turned as the font turns it.
pub fn measure(spec: &FontSpec, text: &str) -> (f64, f64) {
    let size = spec.size.max(1.0).round() as f64;
    let width = match layout(&mut fonts(), spec, text) {
        Some(laid) => laid.width.round() as f64,
        None => (text.chars().count() as f32 * spec.size * 0.55).round() as f64,
    };
    match spec.turn() {
        Turn::None => (width, size),
        _ => (size, width),
    }
}

/// The pixels one line of `text` covers, relative to the top-left of its measured box, and
/// how much of each (0..=255).
pub fn rasterize(spec: &FontSpec, text: &str) -> Vec<(i32, i32, u8)> {
    let mut fonts = fonts();
    let Some(laid) = layout(&mut fonts, spec, text) else { return Vec::new() };
    let size = spec.size.max(1.0);
    let baseline = laid.ascent.round() as i32;
    let mut out = Vec::new();
    for &(font, id, x) in &laid.glyphs {
        let g = fonts.glyph(font, size, id);
        let (left, top) = (x.round() as i32 + g.left, baseline + g.top);
        for (k, &c) in g.coverage.iter().enumerate() {
            if c > 0 {
                out.push((left + (k % g.w) as i32, top + (k / g.w) as i32, c));
            }
        }
    }
    if spec.has(b'u') {
        let y = baseline + (size / 12.0).ceil() as i32;
        out.extend((0..laid.width.round() as i32).map(|x| (x, y, 255)));
    }
    let (w, h) = (laid.width.round() as i32, size.round() as i32);
    match spec.turn() {
        Turn::None => {}
        Turn::Down => out.iter_mut().for_each(|p| (p.0, p.1) = (h - 1 - p.1, p.0)),
        Turn::Up => out.iter_mut().for_each(|p| (p.0, p.1) = (p.1, w - 1 - p.0)),
    }
    out
}
