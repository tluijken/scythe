//! JSFX graphics: the `gfx_*` builtins of `@gfx` draw into a display list the UI paints. The
//! screen keeps what was drawn while `gfx_clear` is negative, as REAPER's framebuffer does;
//! images (`gfx_dest`) are display lists too, so blitting copies their shapes.

use std::collections::{HashMap, VecDeque};

use super::{Machine, Node, Vars};

/// Red, green, blue and alpha, 0..=1.
pub type Color = [f32; 4];
/// Left, top, right, bottom.
pub type Bounds = [f32; 4];

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        filled: bool,
    },
    Line {
        a: [f32; 2],
        b: [f32; 2],
    },
    Circle {
        x: f32,
        y: f32,
        r: f32,
        filled: bool,
    },
    /// Angles in radians, clockwise from straight up.
    Arc {
        x: f32,
        y: f32,
        r: f32,
        from: f32,
        to: f32,
    },
    RoundRect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        r: f32,
    },
    /// A filled convex polygon.
    Polygon(Vec<[f32; 2]>),
    /// Colours at the top-left, top-right, bottom-left and bottom-right corners.
    Gradient {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        corners: [Color; 4],
    },
    /// One line of text, `size` pixels high, from its top-left corner.
    Text {
        x: f32,
        y: f32,
        size: f32,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Draw {
    pub shape: Shape,
    pub color: Color,
    pub clip: Option<Bounds>,
}

/// What the UI hands `@gfx` for one run.
#[derive(Debug, Clone, Default)]
pub struct Input {
    pub w: f64,
    pub h: f64,
    pub mouse_x: f64,
    pub mouse_y: f64,
    /// 1 left, 2 right, 4 control, 8 shift, 16 alt, 64 middle button.
    pub mouse_cap: u32,
    /// Wheel movement since the last run, 120 per notch.
    pub wheel: f64,
    pub hwheel: f64,
    pub mouse_in: bool,
    /// Typed characters, for `gfx_getchar`.
    pub chars: Vec<u32>,
}

/// `gfx_texth` of the default font.
pub const DEFAULT_TEXTH: f32 = 8.0;
/// A display list longer than this loses its oldest shapes.
const MAX_DRAWS: usize = 100_000;
/// How far back an opaque rectangle looks for shapes it hides.
const HIDDEN_SCAN: usize = 1024;
/// `gfx_getsyscol`: the window background, as `r | g << 8 | b << 16`.
const SYSCOL: f64 = (0x26 | 0x26 << 8 | 0x2a << 16) as f64;

/// Advance of one character in a font `size` pixels high. Text is laid out on this fixed
/// grid, so `gfx_measurestr` and what is drawn agree whatever font the UI paints with.
pub fn char_width(size: f32) -> f32 {
    (size * 0.55).ceil().max(1.0)
}

/// Width and height of `text` (lines split at `\n`).
pub fn text_size(text: &str, size: f32) -> (f32, f32) {
    let lines = text.split('\n');
    let widest = lines.clone().map(|l| l.chars().count()).max().unwrap_or(0);
    (widest as f32 * char_width(size), lines.count() as f32 * size)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Call {
    Set,
    Rect,
    RectTo,
    Line,
    LineTo,
    Circle,
    Arc,
    RoundRect,
    Triangle,
    GradRect,
    DrawStr,
    DrawNumber,
    DrawChar,
    Printf,
    MeasureStr,
    MeasureChar,
    SetFont,
    GetFont,
    SetPixel,
    GetPixel,
    LoadImg,
    Blit,
    BlitExt,
    SetImgDim,
    GetImgDim,
    GetChar,
    BlurTo,
    SysCol,
    /// Menus, dropped files and drawing modes this host does not have.
    Nothing,
}

impl Call {
    pub(super) fn from_name(name: &str) -> Option<(Self, usize)> {
        use Call::*;
        const ANY: usize = usize::MAX;
        Some(match name {
            "gfx_set" => (Set, ANY),
            "gfx_rect" => (Rect, ANY),
            "gfx_rectto" => (RectTo, 2),
            "gfx_line" => (Line, ANY),
            "gfx_lineto" => (LineTo, ANY),
            "gfx_circle" => (Circle, ANY),
            "gfx_arc" => (Arc, ANY),
            "gfx_roundrect" => (RoundRect, ANY),
            "gfx_triangle" => (Triangle, ANY),
            "gfx_gradrect" => (GradRect, ANY),
            "gfx_drawstr" => (DrawStr, ANY),
            "gfx_drawnumber" => (DrawNumber, 2),
            "gfx_drawchar" => (DrawChar, 1),
            "gfx_printf" => (Printf, ANY),
            "gfx_measurestr" => (MeasureStr, 3),
            "gfx_measurechar" => (MeasureChar, 3),
            "gfx_setfont" => (SetFont, ANY),
            "gfx_getfont" => (GetFont, ANY),
            "gfx_setpixel" => (SetPixel, 3),
            "gfx_getpixel" => (GetPixel, 3),
            "gfx_loadimg" => (LoadImg, 2),
            "gfx_blit" => (Blit, ANY),
            "gfx_blitext" => (BlitExt, 3),
            "gfx_setimgdim" => (SetImgDim, 3),
            "gfx_getimgdim" => (GetImgDim, 3),
            "gfx_getchar" => (GetChar, ANY),
            "gfx_blurto" => (BlurTo, 2),
            "gfx_getsyscol" => (SysCol, ANY),
            "gfx_showmenu" | "gfx_getdropfile" | "gfx_setcursor" | "gfx_muladdrect" | "gfx_deltablit"
            | "gfx_transformblit" | "gfx_update" | "gfx_init" | "gfx_quit" | "gfx_dock" | "gfx_setimgdim_ext" => {
                (Nothing, ANY)
            }
            _ => return None,
        })
    }
}

struct Slots {
    r: usize,
    g: usize,
    b: usize,
    a: usize,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    clear: usize,
    texth: usize,
    mode: usize,
    dest: usize,
    mouse_x: usize,
    mouse_y: usize,
    mouse_cap: usize,
    mouse_wheel: usize,
    mouse_hwheel: usize,
}

#[derive(Default)]
struct Image {
    w: f64,
    h: f64,
    draws: Vec<Draw>,
}

/// The graphics state of a machine.
pub(super) struct Gfx {
    v: Slots,
    screen: Vec<Draw>,
    images: HashMap<i64, Image>,
    fonts: [f32; 16],
    font: usize,
    chars: VecDeque<u32>,
    mouse_in: bool,
}

impl Gfx {
    pub(super) fn new(vars: &mut Vars) -> Self {
        let mut s = |n: &str| vars.slot(n);
        let v = Slots {
            r: s("gfx_r"),
            g: s("gfx_g"),
            b: s("gfx_b"),
            a: s("gfx_a"),
            x: s("gfx_x"),
            y: s("gfx_y"),
            w: s("gfx_w"),
            h: s("gfx_h"),
            clear: s("gfx_clear"),
            texth: s("gfx_texth"),
            mode: s("gfx_mode"),
            dest: s("gfx_dest"),
            mouse_x: s("mouse_x"),
            mouse_y: s("mouse_y"),
            mouse_cap: s("mouse_cap"),
            mouse_wheel: s("mouse_wheel"),
            mouse_hwheel: s("mouse_hwheel"),
        };
        for (slot, value) in [(v.a, 1.0), (v.dest, -1.0), (v.texth, DEFAULT_TEXTH as f64)] {
            vars.values[slot] = value;
        }
        Self {
            v,
            screen: Vec::new(),
            images: HashMap::new(),
            fonts: [DEFAULT_TEXTH; 16],
            font: 0,
            chars: VecDeque::new(),
            mouse_in: false,
        }
    }
}

/// Strings end at a NUL, as C strings do.
fn c_text(s: &str) -> &str {
    s.split('\0').next().unwrap_or_default()
}

fn unpack(rgb: f64) -> Color {
    let c = rgb.max(0.0) as u32;
    [(c & 0xff) as f32 / 255.0, (c >> 8 & 0xff) as f32 / 255.0, (c >> 16 & 0xff) as f32 / 255.0, 1.0]
}

fn overlaps(a: Bounds, b: Bounds) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

fn intersect(a: Bounds, b: Bounds) -> Bounds {
    [a[0].max(b[0]), a[1].max(b[1]), a[2].min(b[2]), a[3].min(b[3])]
}

fn inside(a: Bounds, b: Bounds) -> bool {
    a[0] >= b[0] && a[1] >= b[1] && a[2] <= b[2] && a[3] <= b[3]
}

/// Where a draw can show: its shape's bounds within its clip.
fn visible(d: &Draw) -> Bounds {
    let b = d.shape.bounds();
    d.clip.map_or(b, |c| intersect(b, c))
}

fn rect_bounds(x: f32, y: f32, w: f32, h: f32) -> Bounds {
    [x, y, x + w, y + h]
}

impl Shape {
    pub fn bounds(&self) -> Bounds {
        match self {
            Shape::Rect { x, y, w, h, .. }
            | Shape::RoundRect { x, y, w, h, .. }
            | Shape::Gradient { x, y, w, h, .. } => rect_bounds(*x, *y, *w, *h),
            Shape::Line { a, b } => [a[0].min(b[0]), a[1].min(b[1]), a[0].max(b[0]) + 1.0, a[1].max(b[1]) + 1.0],
            Shape::Circle { x, y, r, .. } | Shape::Arc { x, y, r, .. } => [x - r, y - r, x + r + 1.0, y + r + 1.0],
            Shape::Polygon(points) => points.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| {
                [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0] + 1.0), b[3].max(p[1] + 1.0)]
            }),
            Shape::Text { x, y, size, text } => {
                let (w, h) = text_size(text, *size);
                rect_bounds(*x, *y, w, h)
            }
        }
    }

    /// The shape moved by `to(p)` and scaled by `(sx, sy)`, as a blit maps it.
    fn mapped(&self, to: impl Fn([f32; 2]) -> [f32; 2], sx: f32, sy: f32) -> Shape {
        let at = |x: f32, y: f32| to([x, y]);
        let r_scale = (sx + sy) / 2.0;
        match self {
            Shape::Rect { x, y, w, h, filled } => {
                let [x, y] = at(*x, *y);
                Shape::Rect { x, y, w: w * sx, h: h * sy, filled: *filled }
            }
            Shape::RoundRect { x, y, w, h, r } => {
                let [x, y] = at(*x, *y);
                Shape::RoundRect { x, y, w: w * sx, h: h * sy, r: r * r_scale }
            }
            Shape::Gradient { x, y, w, h, corners } => {
                let [x, y] = at(*x, *y);
                Shape::Gradient { x, y, w: w * sx, h: h * sy, corners: *corners }
            }
            Shape::Line { a, b } => Shape::Line { a: to(*a), b: to(*b) },
            Shape::Circle { x, y, r, filled } => {
                let [x, y] = at(*x, *y);
                Shape::Circle { x, y, r: r * r_scale, filled: *filled }
            }
            Shape::Arc { x, y, r, from, to: end } => {
                let [x, y] = at(*x, *y);
                Shape::Arc { x, y, r: r * r_scale, from: *from, to: *end }
            }
            Shape::Polygon(points) => Shape::Polygon(points.iter().map(|&p| to(p)).collect()),
            Shape::Text { x, y, size, text } => {
                let [x, y] = at(*x, *y);
                Shape::Text { x, y, size: size * sy, text: text.clone() }
            }
        }
    }
}

impl Machine {
    /// Prepares a run of `@gfx`: the view's size, the mouse and keys, and the screen cleared
    /// to `gfx_clear` unless it is negative.
    pub fn gfx_begin(&mut self, input: &Input) {
        let v = &self.gfx.v;
        let values = &mut self.vars.values;
        values[v.w] = input.w;
        values[v.h] = input.h;
        values[v.mouse_x] = input.mouse_x;
        values[v.mouse_y] = input.mouse_y;
        values[v.mouse_cap] = input.mouse_cap as f64;
        values[v.mouse_wheel] += input.wheel;
        values[v.mouse_hwheel] += input.hwheel;
        values[v.dest] = -1.0;
        values[v.texth] = self.gfx.fonts[self.gfx.font] as f64;
        let clear = values[v.clear];
        self.gfx.mouse_in = input.mouse_in;
        self.gfx.chars.extend(&input.chars);
        if clear >= 0.0 {
            self.gfx.screen.clear();
            let shape = Shape::Rect { x: 0.0, y: 0.0, w: input.w as f32, h: input.h as f32, filled: true };
            self.gfx.screen.push(Draw { shape, color: unpack(clear), clip: None });
        }
    }

    /// What the screen shows after the run.
    pub fn gfx_end(&mut self) -> Vec<Draw> {
        if self.vars.values[self.gfx.v.clear] >= 0.0 {
            std::mem::take(&mut self.gfx.screen)
        } else {
            self.gfx.screen.clone()
        }
    }

    fn gvar(&self, slot: fn(&Slots) -> usize) -> f64 {
        self.vars.values[slot(&self.gfx.v)]
    }

    fn set_gvar(&mut self, slot: fn(&Slots) -> usize, value: f64) {
        self.vars.values[slot(&self.gfx.v)] = value;
    }

    fn gfx_color(&self) -> Color {
        let c = |s: fn(&Slots) -> usize| self.gvar(s).clamp(0.0, 1.0) as f32;
        [c(|v| v.r), c(|v| v.g), c(|v| v.b), c(|v| v.a)]
    }

    fn dest(&self) -> i64 {
        self.gvar(|v| v.dest).round() as i64
    }

    fn surface_size(&self, id: i64) -> (f64, f64) {
        match self.gfx.images.get(&id) {
            Some(img) if id >= 0 => (img.w, img.h),
            _ => (self.gvar(|v| v.w), self.gvar(|v| v.h)),
        }
    }

    fn surface(&mut self, id: i64) -> &mut Vec<Draw> {
        if id < 0 { &mut self.gfx.screen } else { &mut self.gfx.images.entry(id).or_default().draws }
    }

    fn draw(&mut self, shape: Shape, clip: Option<Bounds>) {
        let color = self.gfx_color();
        self.draw_colored(shape, color, clip);
    }

    fn draw_colored(&mut self, shape: Shape, color: Color, clip: Option<Bounds>) {
        let dest = self.dest();
        let (w, h) = self.surface_size(dest);
        let opaque = clip.is_none()
            && color[3] >= 1.0
            && self.gvar(|v| v.mode) == 0.0
            && matches!(shape, Shape::Rect { filled: true, .. });
        let list = self.surface(dest);
        if opaque {
            // What an opaque rectangle hides is dropped: everything when it covers the
            // surface, else what was drawn just before inside it (a pixel set again).
            let r = shape.bounds();
            if r[0] <= 0.0 && r[1] <= 0.0 && r[2] as f64 >= w && r[3] as f64 >= h {
                list.clear();
            }
            let from = list.len().saturating_sub(HIDDEN_SCAN);
            let mut kept = from;
            for i in from..list.len() {
                if !inside(visible(&list[i]), r) {
                    list.swap(kept, i);
                    kept += 1;
                }
            }
            list.truncate(kept);
        }
        if list.len() >= MAX_DRAWS {
            list.drain(..MAX_DRAWS / 4);
        }
        list.push(Draw { shape, color, clip });
    }

    /// Draws `text` at `(x, y)`, one shape per line, leaving `gfx_x`/`gfx_y` after its end.
    fn draw_text(&mut self, text: &str, x: f64, y: f64, clip: Option<Bounds>) {
        let text = c_text(text);
        let size = self.gfx.fonts[self.gfx.font];
        let mut last = (x, y);
        for (i, line) in text.split('\n').enumerate() {
            let ly = y + i as f64 * size as f64;
            if !line.is_empty() {
                self.draw(Shape::Text { x: x as f32, y: ly as f32, size, text: line.to_owned() }, clip);
            }
            last = (x + text_size(line, size).0 as f64, ly);
        }
        self.set_gvar(|v| v.x, last.0);
        self.set_gvar(|v| v.y, last.1);
    }

    fn text_at_cursor(&mut self, text: &str) {
        let (x, y) = (self.gvar(|v| v.x), self.gvar(|v| v.y));
        self.draw_text(text, x, y, None);
    }

    /// Copies the shapes of `src` within `from` into `to` on the current destination. A
    /// surface copied onto itself loses what lay under `to`.
    fn blit(&mut self, src: i64, from: Bounds, to: Bounds) {
        let (fw, fh) = (from[2] - from[0], from[3] - from[1]);
        if fw <= 0.0 || fh <= 0.0 || to[2] <= to[0] || to[3] <= to[1] {
            return;
        }
        let (sx, sy) = ((to[2] - to[0]) / fw, (to[3] - to[1]) / fh);
        let map = |p: [f32; 2]| [to[0] + (p[0] - from[0]) * sx, to[1] + (p[1] - from[1]) * sy];
        let src = if src < 0 { -1 } else { src };
        let copied: Vec<Draw> = self
            .surface(src)
            .iter()
            .filter(|d| overlaps(visible(d), from))
            .map(|d| {
                let clip = d.clip.map_or(to, |c| {
                    let (a, b) = (map([c[0], c[1]]), map([c[2], c[3]]));
                    intersect([a[0], a[1], b[0], b[1]], to)
                });
                Draw { shape: d.shape.mapped(map, sx, sy), color: d.color, clip: Some(clip) }
            })
            .collect();
        let dest = self.dest();
        let same = dest.max(-1) == src;
        let list = self.surface(dest);
        // What stays of the surface goes after the copy (they do not overlap), so shapes
        // drawn next over the uncovered strip find what they hide among the last ones.
        let kept = if same {
            std::mem::take(list).into_iter().filter(|d| !overlaps(visible(d), to)).collect()
        } else {
            Vec::new()
        };
        let list = self.surface(dest);
        list.extend(copied);
        list.extend(kept);
        if list.len() > MAX_DRAWS {
            let extra = list.len() - MAX_DRAWS;
            list.drain(..extra);
        }
    }

    /// The colour last drawn at `(x, y)` by a filled rectangle or gradient, or black.
    fn pixel(&mut self, x: f32, y: f32) -> Color {
        let p = [x, y, x + 1.0, y + 1.0];
        let dest = self.dest();
        self.surface(dest)
            .iter()
            .rev()
            .find(|d| {
                matches!(d.shape, Shape::Rect { filled: true, .. } | Shape::Gradient { .. })
                    && overlaps(d.shape.bounds(), p)
                    && d.clip.is_none_or(|c| overlaps(c, p))
            })
            .map_or([0.0, 0.0, 0.0, 1.0], |d| match &d.shape {
                Shape::Gradient { corners, .. } => corners[0],
                _ => d.color,
            })
    }

    pub(super) fn gfx_call(&mut self, call: Call, args: &[Node]) -> f64 {
        use Call::*;
        let n = args.len();
        let mut a = [0.0f64; 16];
        // Outputs (`gfx_measurestr`'s width and height, …) are read too, which does no harm.
        for (slot, node) in a.iter_mut().zip(args) {
            *slot = self.eval(node);
        }
        let f = |i: usize| a[i] as f32;
        let (gx, gy) = (self.gvar(|v| v.x), self.gvar(|v| v.y));
        match call {
            Set => {
                let (g, b) = if n >= 3 { (a[1], a[2]) } else { (a[0], a[0]) };
                let values = [(0, a[0]), (1, g), (2, b), (3, if n >= 4 { a[3] } else { 1.0 })];
                let slots = [self.gfx.v.r, self.gfx.v.g, self.gfx.v.b, self.gfx.v.a];
                for (i, value) in values {
                    self.vars.values[slots[i]] = value;
                }
                if n >= 5 {
                    self.set_gvar(|v| v.mode, a[4]);
                }
                if n >= 6 {
                    self.set_gvar(|v| v.dest, a[5]);
                }
                0.0
            }
            Rect => {
                let filled = n < 5 || a[4] != 0.0;
                self.draw(Shape::Rect { x: f(0), y: f(1), w: f(2), h: f(3), filled }, None);
                0.0
            }
            RectTo => {
                let (x0, x1) = (gx.min(a[0]), gx.max(a[0]));
                let (y0, y1) = (gy.min(a[1]), gy.max(a[1]));
                let shape =
                    Shape::Rect { x: x0 as f32, y: y0 as f32, w: (x1 - x0) as f32, h: (y1 - y0) as f32, filled: true };
                self.draw(shape, None);
                self.set_gvar(|v| v.x, a[0]);
                self.set_gvar(|v| v.y, a[1]);
                a[0]
            }
            Line => {
                self.draw(Shape::Line { a: [f(0), f(1)], b: [f(2), f(3)] }, None);
                0.0
            }
            LineTo => {
                self.draw(Shape::Line { a: [gx as f32, gy as f32], b: [f(0), f(1)] }, None);
                self.set_gvar(|v| v.x, a[0]);
                self.set_gvar(|v| v.y, a[1]);
                a[0]
            }
            Circle => {
                self.draw(Shape::Circle { x: f(0), y: f(1), r: f(2), filled: n >= 4 && a[3] != 0.0 }, None);
                0.0
            }
            Arc => {
                self.draw(Shape::Arc { x: f(0), y: f(1), r: f(2), from: f(3), to: f(4) }, None);
                0.0
            }
            RoundRect => {
                self.draw(Shape::RoundRect { x: f(0), y: f(1), w: f(2), h: f(3), r: f(4) }, None);
                0.0
            }
            Triangle => {
                let points: Vec<[f32; 2]> = (0..n.min(16) / 2).map(|i| [f(2 * i), f(2 * i + 1)]).collect();
                if points.len() >= 3 {
                    self.draw(Shape::Polygon(points), None);
                }
                0.0
            }
            GradRect => {
                let (w, h) = (a[2], a[3]);
                let at = |dx: f64, dy: f64| -> Color {
                    std::array::from_fn(|c| (a[4 + c] + a[8 + c] * dx + a[12 + c] * dy).clamp(0.0, 1.0) as f32)
                };
                let corners = [at(0.0, 0.0), at(w, 0.0), at(0.0, h), at(w, h)];
                self.draw_colored(Shape::Gradient { x: f(0), y: f(1), w: f(2), h: f(3), corners }, corners[0], None);
                0.0
            }
            DrawStr => {
                let text = c_text(&self.string(a[0])).to_owned();
                let size = self.gfx.fonts[self.gfx.font];
                let (w, h) = text_size(&text, size);
                let (w, h) = (w as f64, h as f64);
                let (flags, right, bottom) = (a[1] as i64, a[2], a[3]);
                let (mut x, mut y) = (gx, gy);
                let mut clip = None;
                if n >= 4 {
                    if flags & 1 != 0 {
                        x = gx + (right - gx - w) / 2.0;
                    } else if flags & 2 != 0 {
                        x = right - w;
                    }
                    if flags & 4 != 0 {
                        y = gy + (bottom - gy - h) / 2.0;
                    } else if flags & 8 != 0 {
                        y = bottom - h;
                    }
                    if flags & 256 == 0 {
                        clip = Some([gx as f32, gy as f32, right as f32, bottom as f32]);
                    }
                }
                self.draw_text(&text, x, y, clip);
                0.0
            }
            DrawNumber => {
                self.text_at_cursor(&format!("{:.*}", a[1].clamp(0.0, 16.0) as usize, a[0]));
                0.0
            }
            DrawChar => {
                let c = char::from_u32(a[0].max(0.0) as u32).unwrap_or(' ');
                self.text_at_cursor(&c.to_string());
                a[0]
            }
            Printf => {
                let text = self.format(args);
                self.text_at_cursor(&text);
                text.len() as f64
            }
            MeasureStr | MeasureChar => {
                let text = match call {
                    MeasureStr => c_text(&self.string(a[0])).to_owned(),
                    _ => char::from_u32(a[0].max(0.0) as u32).unwrap_or(' ').to_string(),
                };
                let (w, h) = text_size(&text, self.gfx.fonts[self.gfx.font]);
                self.store(&args[1], w as f64);
                self.store(&args[2], h as f64);
                w as f64
            }
            SetFont => {
                let idx = (a[0].max(0.0) as usize).min(15);
                if n >= 3 && a[2] > 0.0 {
                    self.gfx.fonts[idx] = a[2] as f32;
                } else if idx == 0 {
                    self.gfx.fonts[0] = DEFAULT_TEXTH;
                }
                self.gfx.font = idx;
                self.set_gvar(|v| v.texth, self.gfx.fonts[idx] as f64);
                1.0
            }
            GetFont => self.gfx.font as f64,
            SetPixel => {
                let color = [f(0), f(1), f(2), self.gvar(|v| v.a) as f32].map(|c| c.clamp(0.0, 1.0));
                let shape = Shape::Rect { x: gx.floor() as f32, y: gy.floor() as f32, w: 1.0, h: 1.0, filled: true };
                self.draw_colored(shape, color, None);
                0.0
            }
            GetPixel => {
                let c = self.pixel(gx.floor() as f32, gy.floor() as f32);
                for i in 0..3 {
                    self.store(&args[i], c[i] as f64);
                }
                c[0] as f64
            }
            LoadImg => -1.0,
            SetImgDim => {
                let id = a[0].round() as i64;
                if id >= 0 {
                    self.gfx.images.insert(id, Image { w: a[1].max(0.0), h: a[2].max(0.0), draws: Vec::new() });
                }
                1.0
            }
            GetImgDim => {
                let (w, h) = self.surface_size(a[0].round() as i64);
                self.store(&args[1], w);
                self.store(&args[2], h);
                w
            }
            Blit => {
                let src = a[0].round() as i64;
                let (sw, sh) = self.surface_size(src);
                let scale = if n >= 2 { a[1] } else { 1.0 };
                let get = |i: usize, default: f64| if n > i { a[i] } else { default };
                let (fx, fy, fw, fh) = (get(3, 0.0), get(4, 0.0), get(5, sw), get(6, sh));
                let (tx, ty) = (get(7, gx), get(8, gy));
                let (tw, th) = (get(9, fw * scale), get(10, fh * scale));
                let b = |x: f64, y: f64, w: f64, h: f64| rect_bounds(x as f32, y as f32, w as f32, h as f32);
                self.blit(src, b(fx, fy, fw, fh), b(tx, ty, tw, th));
                src as f64
            }
            BlitExt => {
                let p: [f32; 8] = std::array::from_fn(|i| self.read(a[1] + i as f64) as f32);
                self.blit(
                    a[0].round() as i64,
                    rect_bounds(p[0], p[1], p[2], p[3]),
                    rect_bounds(p[4], p[5], p[6], p[7]),
                );
                a[0]
            }
            GetChar => match a[0] as i64 {
                65536 => (1 | 4 | if self.gfx.mouse_in { 2 | 8 } else { 0 }) as f64,
                0 => self.gfx.chars.pop_front().map_or(0.0, |c| c as f64),
                _ => 0.0,
            },
            BlurTo => {
                self.set_gvar(|v| v.x, a[0]);
                self.set_gvar(|v| v.y, a[1]);
                a[0]
            }
            SysCol => SYSCOL,
            Nothing => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eel::compile;

    fn frame(src: &str, input: &Input) -> (Machine, Vec<Draw>) {
        let mut vars = Vars::default();
        let code = compile(src, &mut vars, &mut HashMap::new()).unwrap();
        let mut m = Machine::new(vars);
        m.gfx_begin(input);
        m.run(&code);
        let draws = m.gfx_end();
        (m, draws)
    }

    fn view() -> Input {
        Input { w: 200.0, h: 100.0, ..Default::default() }
    }

    #[test]
    fn draws_rects_and_text() {
        let (m, draws) = frame(
            "gfx_set(1, 0.5, 0); gfx_rect(10, 20, 30, 40); gfx_a = 0.5; gfx_x = 5; gfx_y = 6; gfx_drawstr(\"hi\");",
            &view(),
        );
        assert_eq!(draws.len(), 3, "the clear, the rect and the text");
        assert_eq!(draws[0].color, [0.0, 0.0, 0.0, 1.0], "gfx_clear 0 is black");
        assert_eq!(draws[1].shape, Shape::Rect { x: 10.0, y: 20.0, w: 30.0, h: 40.0, filled: true });
        assert_eq!(draws[1].color, [1.0, 0.5, 0.0, 1.0]);
        let text = Shape::Text { x: 5.0, y: 6.0, size: DEFAULT_TEXTH, text: "hi".into() };
        assert_eq!((&draws[2].shape, draws[2].color[3]), (&text, 0.5));
        assert_eq!(m.vars.get("gfx_x"), Some(5.0 + 2.0 * char_width(DEFAULT_TEXTH) as f64), "the cursor moves on");
    }

    #[test]
    fn measures_text_as_it_draws_it() {
        let (m, draws) = frame(
            "gfx_setfont(1, \"Arial\", 16); gfx_measurestr(\"abcd\", w, h); gfx_measurechar($'x', cw, ch); \
             gfx_x = 0; gfx_y = 0; gfx_drawstr(\"abcd\", 1 | 4, 100, 50); th = gfx_texth;",
            &view(),
        );
        let var = |n: &str| m.vars.get(n).unwrap();
        assert_eq!((var("w"), var("h"), var("th")), (4.0 * var("cw"), 16.0, 16.0));
        assert_eq!(var("cw") as f32, char_width(16.0));
        let Shape::Text { x, y, .. } = draws[1].shape else { panic!("{:?}", draws[1]) };
        assert_eq!((x as f64, y as f64), ((100.0 - var("w")) / 2.0, (50.0 - 16.0) / 2.0), "centred in its box");
        assert_eq!(draws[1].clip, Some([0.0, 0.0, 100.0, 50.0]));
    }

    #[test]
    fn keeps_the_screen_without_gfx_clear_and_scrolls_by_blitting() {
        let (mut vars, mut fns) = (Vars::default(), HashMap::new());
        let init = compile("gfx_clear = -1;", &mut vars, &mut fns).unwrap();
        let src = "gfx_blit(-1, 1, 0, 1, 0, 9, 10, 0, 0, 9, 10); gfx_x = 9; gfx_y = 0; gfx_setpixel(1, 1, 1);";
        let code = compile(src, &mut vars, &mut fns).unwrap();
        let mut m = Machine::new(vars);
        m.run(&init);
        let input = Input { w: 10.0, h: 10.0, ..Default::default() };
        let mut draws = Vec::new();
        for _ in 0..3 {
            m.gfx_begin(&input);
            m.run(&code);
            draws = m.gfx_end();
        }
        let xs: Vec<f32> = draws.iter().map(|d| d.shape.bounds()[0]).collect();
        assert_eq!(xs, [7.0, 8.0, 9.0], "each frame's pixel moves one to the left");
    }

    #[test]
    fn mouse_and_size_reach_the_script() {
        let input = Input { mouse_x: 12.0, mouse_y: 3.0, mouse_cap: 1, wheel: 120.0, ..view() };
        let (m, _) = frame("a = gfx_w; b = mouse_cap & 1 ? mouse_x : -1; c = mouse_wheel;", &input);
        assert_eq!([m.vars.get("a"), m.vars.get("b"), m.vars.get("c")], [Some(200.0), Some(12.0), Some(120.0)]);
    }
}
