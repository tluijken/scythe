//! The pixels `@gfx` draws into, and the drawing itself, as LICE does it: pixel `(x, y)` is
//! the unit square at `(x, y)`, lines and circles run through pixel centres at whole
//! coordinates, and every pixel drawn blends in with the alpha and mode of the moment.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, PI, TAU};

use super::text::{self, FontSpec};

/// Red, green, blue and alpha, 0..=255, not premultiplied.
pub type Rgba = [u8; 4];

/// `gfx_mode` bits.
pub const ADDITIVE: u32 = 1;
pub const NO_SOURCE_ALPHA: u32 = 2;
pub const NO_FILTER: u32 = 4;

/// Images are at most this wide and high, as in REAPER.
pub const MAX_DIM: usize = 8192;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bitmap {
    pub w: usize,
    pub h: usize,
    pub px: Vec<Rgba>,
}

impl Bitmap {
    pub fn new(w: usize, h: usize) -> Self {
        let (w, h) = (w.min(MAX_DIM), h.min(MAX_DIM));
        Self { w, h, px: vec![[0; 4]; w * h] }
    }

    pub fn get(&self, x: i64, y: i64) -> Option<Rgba> {
        (x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h)
            .then(|| self.px[y as usize * self.w + x as usize])
    }

    /// The bitmap at a new size, keeping what overlaps.
    fn resized(&self, w: usize, h: usize) -> Self {
        let mut out = Self::new(w, h);
        for y in 0..out.h.min(self.h) {
            let n = out.w.min(self.w);
            out.px[y * out.w..y * out.w + n].copy_from_slice(&self.px[y * self.w..y * self.w + n]);
        }
        out
    }
}

/// Reads a PNG or JPEG file.
pub fn load_image(path: &std::path::Path) -> Option<Bitmap> {
    let img = image::open(path).ok()?.to_rgba8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    if w > MAX_DIM || h > MAX_DIM {
        return None;
    }
    let px = img.pixels().map(|p| p.0).collect();
    Some(Bitmap { w, h, px })
}

/// The colour and blending one drawing call uses.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Paint {
    pub rgb: [f32; 3],
    /// `gfx_a`; negative with the additive mode subtracts.
    pub a: f32,
    pub mode: u32,
    /// `gfx_a2`, the alpha written to the destination's own alpha channel.
    pub a2: f32,
}

impl Default for Paint {
    fn default() -> Self {
        Self { rgb: [1.0; 3], a: 1.0, mode: 0, a2: 1.0 }
    }
}

/// One drawing call, played onto its destination (`-1` the screen) later.
#[derive(Debug, Clone)]
pub struct Cmd {
    pub dest: i64,
    pub paint: Paint,
    /// Left, top, right, bottom (exclusive).
    pub clip: Option<[i64; 4]>,
    pub op: Op,
}

#[derive(Debug, Clone)]
pub enum Op {
    /// The screen at the view's size, keeping what overlaps.
    Resize(usize, usize),
    /// The whole destination in the paint's colour, opaque.
    Clear,
    Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        filled: bool,
    },
    Line {
        a: [f64; 2],
        b: [f64; 2],
        aa: bool,
    },
    Circle {
        x: f64,
        y: f64,
        r: f64,
        filled: bool,
        aa: bool,
    },
    /// Angles in radians, clockwise from straight up.
    Arc {
        x: f64,
        y: f64,
        r: f64,
        from: f64,
        to: f64,
        aa: bool,
    },
    RoundRect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        r: f64,
        aa: bool,
    },
    /// A filled convex polygon.
    Polygon(Vec<[f64; 2]>),
    /// Red, green, blue and alpha at the top-left corner, and their change per pixel right
    /// and down.
    Gradient {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        start: [f32; 4],
        dx: [f32; 4],
        dy: [f32; 4],
    },
    Pixel {
        x: f64,
        y: f64,
    },
    /// Text with its box's top-left corner at `(x, y)`: a line, or any text in font 0.
    Text {
        x: f64,
        y: f64,
        font: FontSpec,
        text: String,
    },
    /// `from` of image `src` (left, top, width, height) scaled onto `to`, turned `angle`
    /// radians clockwise about `to`'s centre moved by `pivot`.
    Blit {
        src: i64,
        from: [f64; 4],
        to: [f64; 4],
        angle: f64,
        pivot: [f64; 2],
    },
    /// An image cleared to transparent at a new size; one already that size keeps its pixels.
    SetImgDim(usize, usize),
    SetImage(Bitmap),
    /// Left, top, right, bottom.
    Blur([f64; 4]),
    MulAdd {
        rect: [f64; 4],
        mul: [f32; 4],
        add: [f32; 4],
    },
}

/// The screen and images of one view, and the calls not yet drawn onto them.
#[derive(Debug, Default)]
pub struct Canvas {
    pub screen: Bitmap,
    images: HashMap<i64, Bitmap>,
    pub(super) queue: Vec<Cmd>,
    /// Whether the screen changed since it was last taken.
    changed: bool,
}

impl Canvas {
    /// Draws every queued call.
    pub fn flush(&mut self) {
        for cmd in std::mem::take(&mut self.queue) {
            self.exec(cmd);
        }
    }

    /// The screen's pixels, opaque, when they changed since last taken.
    pub fn take_screen(&mut self) -> Option<Bitmap> {
        std::mem::take(&mut self.changed).then(|| {
            let mut out = self.screen.clone();
            for p in &mut out.px {
                p[3] = 255;
            }
            out
        })
    }

    pub fn pixel(&self, dest: i64, x: i64, y: i64) -> Option<Rgba> {
        self.bitmap(dest)?.get(x, y)
    }

    fn bitmap(&self, id: i64) -> Option<&Bitmap> {
        if id < 0 { Some(&self.screen) } else { self.images.get(&id) }
    }

    fn exec(&mut self, cmd: Cmd) {
        let Cmd { dest, paint, clip, op } = cmd;
        match op {
            Op::Resize(w, h) => {
                if (w, h) != (self.screen.w, self.screen.h) {
                    self.screen = self.screen.resized(w, h);
                    self.changed = true;
                }
                return;
            }
            Op::SetImgDim(w, h) => {
                if dest >= 0 {
                    let img = self.images.entry(dest).or_default();
                    if (img.w, img.h) != (w, h) {
                        *img = Bitmap::new(w, h);
                    }
                }
                return;
            }
            Op::SetImage(bmp) => {
                if dest >= 0 {
                    self.images.insert(dest, bmp);
                }
                return;
            }
            _ => {}
        }
        // A surface copied onto itself reads what it held before.
        let source = match &op {
            Op::Blit { src, .. } => {
                let src = (*src).max(-1);
                match self.bitmap(src) {
                    Some(b) => Some(b.clone()),
                    None => return,
                }
            }
            _ => None,
        };
        let target = if dest < 0 {
            self.changed = true;
            &mut self.screen
        } else {
            match self.images.get_mut(&dest) {
                Some(b) => b,
                None => return,
            }
        };
        let mut pen = Pen::new(target, clip, paint);
        match op {
            Op::Clear => {
                let c = paint.rgb.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
                pen.bmp.px.fill([c[0], c[1], c[2], 255]);
            }
            Op::Rect { x, y, w, h, filled } => pen.rect(x, y, w, h, filled),
            Op::Line { a, b, aa } => pen.line(a, b, aa),
            Op::Circle { x, y, r, filled, aa } => pen.circle(x, y, r, filled, aa),
            Op::Arc { x, y, r, from, to, aa } => pen.arc(x, y, r, from, to, aa),
            Op::RoundRect { x, y, w, h, r, aa } => pen.round_rect(x, y, w, h, r, aa),
            Op::Polygon(points) => pen.polygon(&points),
            Op::Gradient { x, y, w, h, start, dx, dy } => pen.gradient(x, y, w, h, start, dx, dy),
            Op::Pixel { x, y } => pen.plot(x.floor() as i64, y.floor() as i64, 1.0),
            Op::Text { x, y, font, text } => {
                let (x0, y0) = (x.round() as i64, y.round() as i64);
                for (i, j, cov) in text::rasterize(&font, &text) {
                    pen.plot(x0 + i as i64, y0 + j as i64, cov as f32 / 255.0);
                }
            }
            Op::Blit { from, to, angle, pivot, .. } => {
                if let Some(src) = source {
                    pen.blit(&src, from, to, angle, pivot);
                }
            }
            Op::Blur(r) => pen.blur(r),
            Op::MulAdd { rect, mul, add } => pen.mul_add(rect, mul, add),
            Op::Resize(..) | Op::SetImgDim(..) | Op::SetImage(_) => {}
        }
    }
}

/// Drawing onto a bitmap within a clip rectangle.
struct Pen<'a> {
    bmp: &'a mut Bitmap,
    /// Left, top, right, bottom (exclusive), within the bitmap.
    clip: [i64; 4],
    paint: Paint,
}

impl<'a> Pen<'a> {
    fn new(bmp: &'a mut Bitmap, clip: Option<[i64; 4]>, paint: Paint) -> Self {
        let full = [0, 0, bmp.w as i64, bmp.h as i64];
        let clip = clip.map_or(full, |c| [c[0].max(0), c[1].max(0), c[2].min(full[2]), c[3].min(full[3])]);
        Self { bmp, clip, paint }
    }

    /// The whole pixels from `x0` to `x1` (exclusive) the clip lets through.
    fn columns(&self, x0: f64, x1: f64) -> std::ops::Range<i64> {
        (x0.floor() as i64).max(self.clip[0])..(x1.floor() as i64).min(self.clip[2])
    }

    fn rows(&self, y0: f64, y1: f64) -> std::ops::Range<i64> {
        (y0.floor() as i64).max(self.clip[1])..(y1.floor() as i64).min(self.clip[3])
    }

    /// Blends `rgb` (0..=1) into pixel `(x, y)` with alpha `a`.
    fn blend(&mut self, x: i64, y: i64, rgb: [f32; 3], a: f32) {
        if x < self.clip[0] || y < self.clip[1] || x >= self.clip[2] || y >= self.clip[3] || a == 0.0 {
            return;
        }
        let d = &mut self.bmp.px[y as usize * self.bmp.w + x as usize];
        let src = [rgb[0], rgb[1], rgb[2], self.paint.a2].map(|v| v * 255.0);
        if self.paint.mode & ADDITIVE != 0 {
            for c in 0..4 {
                d[c] = (d[c] as f32 + src[c] * a).round().clamp(0.0, 255.0) as u8;
            }
        } else {
            let a = a.clamp(0.0, 1.0);
            for c in 0..4 {
                d[c] = (d[c] as f32 + (src[c].clamp(0.0, 255.0) - d[c] as f32) * a).round().clamp(0.0, 255.0) as u8;
            }
        }
    }

    /// The paint's colour at `coverage` of a pixel.
    fn plot(&mut self, x: i64, y: i64, coverage: f32) {
        let (rgb, a) = (self.paint.rgb, self.paint.a);
        self.blend(x, y, rgb, a * coverage);
    }

    fn rect(&mut self, x: f64, y: f64, w: f64, h: f64, filled: bool) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        if filled {
            for py in self.rows(y, y + h) {
                for px in self.columns(x, x + w) {
                    self.plot(px, py, 1.0);
                }
            }
            return;
        }
        let (l, t) = (x.floor() as i64, y.floor() as i64);
        let (r, b) = ((x + w).floor() as i64 - 1, (y + h).floor() as i64 - 1);
        for px in l..=r {
            self.plot(px, t, 1.0);
            if b != t {
                self.plot(px, b, 1.0);
            }
        }
        for py in t + 1..b {
            self.plot(l, py, 1.0);
            if r != l {
                self.plot(r, py, 1.0);
            }
        }
    }

    /// A one-pixel line; antialiased lines spread each step over the two nearest pixels.
    fn line(&mut self, a: [f64; 2], b: [f64; 2], aa: bool) {
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let steep = dy.abs() > dx.abs();
        // Walk along the longer axis; `(u, v)` is (x, y) or, for steep lines, (y, x).
        let (mut a, mut b) = if steep { ([a[1], a[0]], [b[1], b[0]]) } else { (a, b) };
        if a[0] > b[0] {
            std::mem::swap(&mut a, &mut b);
        }
        let grad = if b[0] != a[0] { (b[1] - a[1]) / (b[0] - a[0]) } else { 0.0 };
        let (u0, u1) = (a[0].round() as i64, b[0].round() as i64);
        // Lines far outside the destination are walked only where they could show.
        let limit = (self.bmp.w.max(self.bmp.h) as i64) + 2;
        let (u0, u1) = (u0.max(-limit), u1.min(2 * limit));
        for u in u0..=u1 {
            let v = a[1] + grad * (u as f64 - a[0]);
            let mut put = |v: i64, cov: f32| {
                let (x, y) = if steep { (v, u) } else { (u, v) };
                self.plot(x, y, cov);
            };
            if aa {
                let f = v.floor();
                let t = (v - f) as f32;
                put(f as i64, 1.0 - t);
                if t > 0.0 {
                    put(f as i64 + 1, t);
                }
            } else {
                put(v.round() as i64, 1.0);
            }
        }
    }

    /// Pixels whose centres lie within one pixel of the circle `(x, y, r)`, with how much
    /// of each the ring (or, `filled`, the disc) covers.
    fn ring(&mut self, x: f64, y: f64, r: f64, filled: bool, aa: bool, mut keep: impl FnMut(f64, f64) -> bool) {
        if r < 0.0 {
            return;
        }
        for py in self.rows(y - r - 1.0, y + r + 2.0) {
            let dy = py as f64 - y;
            let outer = ((r + 1.0).powi(2) - dy * dy).max(0.0).sqrt();
            let inner = if filled { 0.0 } else { ((r - 1.0).max(0.0).powi(2) - dy * dy).max(0.0).sqrt() };
            let spans = [(x - outer, x - inner + 1.0), (x + inner, x + outer + 1.0)];
            let left = self.columns(spans[0].0, spans[0].1);
            let right = self.columns(spans[1].0, spans[1].1);
            let right = right.start.max(left.end)..right.end;
            for px in left.chain(right) {
                let dx = px as f64 - x;
                let d = (dx * dx + dy * dy).sqrt();
                let cov = match (filled, aa) {
                    (true, true) => (r + 0.5 - d).clamp(0.0, 1.0),
                    (true, false) => ((d < r + 0.5) as u8) as f64,
                    (false, true) => (1.0 - (d - r).abs()).max(0.0),
                    (false, false) => (((d - r).abs() < 0.5) as u8) as f64,
                };
                if cov > 0.0 && keep(dx, dy) {
                    self.plot(px, py, cov as f32);
                }
            }
        }
    }

    fn circle(&mut self, x: f64, y: f64, r: f64, filled: bool, aa: bool) {
        self.ring(x, y, r, filled, aa, |_, _| true);
    }

    fn arc(&mut self, x: f64, y: f64, r: f64, from: f64, to: f64, aa: bool) {
        let (from, to) = if from <= to { (from, to) } else { (to, from) };
        let span = to - from;
        self.ring(x, y, r, false, aa, |dx, dy| span >= TAU || (dx.atan2(-dy) - from).rem_euclid(TAU) <= span);
    }

    fn round_rect(&mut self, x: f64, y: f64, w: f64, h: f64, r: f64, aa: bool) {
        let (r0, b0) = (x + w - 1.0, y + h - 1.0);
        let r = r.min(w / 2.0).min(h / 2.0).max(0.0);
        if r < 1.0 {
            self.rect(x, y, w, h, false);
            return;
        }
        self.line([x + r, y], [r0 - r, y], false);
        self.line([x + r, b0], [r0 - r, b0], false);
        self.line([x, y + r], [x, b0 - r], false);
        self.line([r0, y + r], [r0, b0 - r], false);
        let corners =
            [(x + r, y + r, -FRAC_PI_2), (r0 - r, y + r, 0.0), (r0 - r, b0 - r, FRAC_PI_2), (x + r, b0 - r, PI)];
        for (cx, cy, from) in corners {
            self.arc(cx, cy, r, from, from + FRAC_PI_2, aa);
        }
    }

    /// Fills the pixels whose centres lie inside or on the edge of a convex polygon.
    fn polygon(&mut self, points: &[[f64; 2]]) {
        let (top, bottom) = points.iter().fold((f64::MAX, f64::MIN), |(t, b), p| (t.min(p[1]), b.max(p[1])));
        for py in self.rows(top.ceil(), bottom.floor() + 1.0) {
            let y = py as f64;
            let (mut l, mut r) = (f64::MAX, f64::MIN);
            for (i, p) in points.iter().enumerate() {
                let q = points[(i + 1) % points.len()];
                if (p[1] - y) * (q[1] - y) > 0.0 {
                    continue;
                }
                if p[1] == q[1] {
                    (l, r) = (l.min(p[0]).min(q[0]), r.max(p[0]).max(q[0]));
                } else {
                    let x = p[0] + (q[0] - p[0]) * (y - p[1]) / (q[1] - p[1]);
                    (l, r) = (l.min(x), r.max(x));
                }
            }
            if l <= r {
                for px in self.columns(l.ceil(), r.floor() + 1.0) {
                    self.plot(px, py, 1.0);
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn gradient(&mut self, x: f64, y: f64, w: f64, h: f64, start: [f32; 4], dx: [f32; 4], dy: [f32; 4]) {
        let (x0, y0) = (x.floor() as i64, y.floor() as i64);
        for py in self.rows(y, y + h) {
            for px in self.columns(x, x + w) {
                let (i, j) = ((px - x0) as f32, (py - y0) as f32);
                let c: [f32; 4] = std::array::from_fn(|k| (start[k] + dx[k] * i + dy[k] * j).clamp(0.0, 1.0));
                self.blend(px, py, [c[0], c[1], c[2]], c[3]);
            }
        }
    }

    fn blit(&mut self, src: &Bitmap, from: [f64; 4], to: [f64; 4], angle: f64, pivot: [f64; 2]) {
        let [fx, fy, fw, fh] = from;
        let [tx, ty, tw, th] = to;
        if fw <= 0.0 || fh <= 0.0 || tw == 0.0 || th == 0.0 {
            return;
        }
        // Where the source may be read: its rectangle within the image.
        let (sl, st) = (fx.max(0.0), fy.max(0.0));
        let (sr, sb) = ((fx + fw).min(src.w as f64), (fy + fh).min(src.h as f64));
        if sl >= sr || st >= sb {
            return;
        }
        let (sx, sy) = (fw / tw, fh / th);
        let (cx, cy) = (tx + tw / 2.0 + pivot[0], ty + th / 2.0 + pivot[1]);
        let (sin, cos) = (-angle).sin_cos();
        let mode = self.paint.mode;
        let exact = angle == 0.0 && sx == 1.0 && sy == 1.0 && fx.fract() == 0.0 && tx.fract() == 0.0;
        let nearest = mode & NO_FILTER != 0 || exact;
        let (dl, dr) = (tx.min(tx + tw), tx.max(tx + tw));
        let (dt, db) = (ty.min(ty + th), ty.max(ty + th));
        let alpha = self.paint.a;
        for py in self.rows(dt, db) {
            for px in self.columns(dl, dr) {
                let (mut x, mut y) = (px as f64 + 0.5, py as f64 + 0.5);
                if angle != 0.0 {
                    let (rx, ry) = (x - cx, y - cy);
                    (x, y) = (cx + rx * cos - ry * sin, cy + rx * sin + ry * cos);
                }
                let (u, v) = (fx + (x - tx) * sx, fy + (y - ty) * sy);
                if u < sl || v < st || u >= sr || v >= sb {
                    continue;
                }
                let s = if nearest {
                    src.px[v as usize * src.w + u as usize].map(f32::from)
                } else {
                    bilinear(src, u, v, [sl, st, sr, sb])
                };
                let a = if mode & NO_SOURCE_ALPHA != 0 { alpha } else { alpha * s[3] / 255.0 };
                self.blend(px, py, [s[0] / 255.0, s[1] / 255.0, s[2] / 255.0], a);
            }
        }
    }

    fn blur(&mut self, [x0, y0, x1, y1]: [f64; 4]) {
        let (l, r) = (x0.min(x1), x0.max(x1));
        let (t, b) = (y0.min(y1), y0.max(y1));
        let (cols, rows) = (self.columns(l, r), self.rows(t, b));
        if cols.is_empty() || rows.is_empty() {
            return;
        }
        let w = self.bmp.w as i64;
        let before = self.bmp.px.clone();
        let at = |x: i64, y: i64| before[(y * w + x) as usize];
        for py in rows.clone() {
            for px in cols.clone() {
                let near = [
                    (px, py),
                    ((px - 1).max(cols.start), py),
                    ((px + 1).min(cols.end - 1), py),
                    (px, (py - 1).max(rows.start)),
                    (px, (py + 1).min(rows.end - 1)),
                ];
                let mut sum = [0u32; 4];
                for (x, y) in near {
                    for (s, v) in sum.iter_mut().zip(at(x, y)) {
                        *s += v as u32;
                    }
                }
                self.bmp.px[(py * w + px) as usize] = sum.map(|s| ((s + 2) / 5) as u8);
            }
        }
    }

    fn mul_add(&mut self, [x, y, w, h]: [f64; 4], mul: [f32; 4], add: [f32; 4]) {
        let width = self.bmp.w as i64;
        for py in self.rows(y, y + h) {
            for px in self.columns(x, x + w) {
                let d = &mut self.bmp.px[(py * width + px) as usize];
                for c in 0..4 {
                    d[c] = (d[c] as f32 * mul[c] + add[c] * 255.0).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
    }
}

/// The source's colour at `(u, v)`, interpolated between the four nearest pixel centres
/// within `bounds` (left, top, right, bottom).
fn bilinear(src: &Bitmap, u: f64, v: f64, bounds: [f64; 4]) -> [f32; 4] {
    let (l, t) = (bounds[0].floor() as i64, bounds[1].floor() as i64);
    let (r, b) = (bounds[2].ceil() as i64 - 1, bounds[3].ceil() as i64 - 1);
    let (x, y) = (u - 0.5, v - 0.5);
    let (x0, y0) = (x.floor(), y.floor());
    let (fx, fy) = ((x - x0) as f32, (y - y0) as f32);
    let px = |x: i64, y: i64| src.px[(y.clamp(t, b) as usize) * src.w + x.clamp(l, r) as usize].map(f32::from);
    let (x0, y0) = (x0 as i64, y0 as i64);
    let (a, b2, c, d) = (px(x0, y0), px(x0 + 1, y0), px(x0, y0 + 1), px(x0 + 1, y0 + 1));
    std::array::from_fn(|k| {
        let top = a[k] + (b2[k] - a[k]) * fx;
        let bottom = c[k] + (d[k] - c[k]) * fx;
        top + (bottom - top) * fy
    })
}
