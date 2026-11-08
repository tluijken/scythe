//! JSFX graphics. The `gfx_*` builtins of `@gfx` queue drawing calls for a pixel canvas
//! (the screen and images 0..1023), which the UI thread draws once it has let go of the
//! effect, so the audio thread waits on `@gfx` only for the script itself. The screen keeps
//! what was drawn while `gfx_clear` is negative, as REAPER's framebuffer does.

mod bitmap;
mod menu;
mod raster;
mod text;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub use menu::{Menu, MenuItem};
pub use raster::{Bitmap, Canvas, Rgba};
pub use text::FontSpec;

use raster::{Cmd, Op, Paint};

use super::{Machine, Node, Vars};

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
    /// What the user chose from the menu the last run asked for (0 for nothing), when this
    /// run repeats it.
    pub menu_choice: Option<u32>,
}

/// What one run of `@gfx` leaves.
pub struct Output {
    canvas: Arc<Mutex<Canvas>>,
    /// A menu the run asked for. `gfx_showmenu` waits for the user in REAPER; here the run
    /// stops at the call, its drawing is dropped, and the UI runs it again with the same
    /// input and [`Input::menu_choice`] set, which that call then returns.
    pub menu: Option<Menu>,
}

impl Output {
    /// Draws what the run queued and returns the screen if it changed.
    pub fn render(&self) -> Option<Bitmap> {
        let mut canvas = self.canvas.lock().unwrap_or_else(|e| e.into_inner());
        canvas.flush();
        canvas.take_screen()
    }
}

/// Calls queued before they are drawn mid-run, to bound the queue.
const MAX_QUEUE: usize = 1 << 16;
/// `gfx_getsyscol`: the window background, as `r | g << 8 | b << 16`.
const SYSCOL: f64 = (0x26 | 0x26 << 8 | 0x2a << 16) as f64;

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
    MulAddRect,
    ShowMenu,
    SysCol,
    /// Dropped files, cursors, windows and the blits through coordinate tables, which this
    /// host does not have.
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
            "gfx_muladdrect" => (MulAddRect, ANY),
            "gfx_showmenu" => (ShowMenu, 1),
            "gfx_getsyscol" => (SysCol, ANY),
            "gfx_getdropfile" | "gfx_setcursor" | "gfx_deltablit" | "gfx_transformblit" | "gfx_update" | "gfx_init"
            | "gfx_quit" | "gfx_dock" | "gfx_setimgdim_ext" => (Nothing, ANY),
            _ => return None,
        })
    }
}

struct Slots {
    r: usize,
    g: usize,
    b: usize,
    a: usize,
    a2: usize,
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

/// The graphics state of a machine.
pub(super) struct Gfx {
    v: Slots,
    canvas: Arc<Mutex<Canvas>>,
    queue: Vec<Cmd>,
    /// The size of each image, as the queued calls leave it.
    sizes: HashMap<i64, (usize, usize)>,
    /// Font 0 and the configurable fonts 1..=16.
    fonts: Vec<FontSpec>,
    font: usize,
    chars: VecDeque<u32>,
    mouse_in: bool,
    menu_choice: Option<u32>,
    menu: Option<Menu>,
    /// The run stopped at `gfx_showmenu`.
    halted: bool,
    /// Where `gfx_loadimg` looks for a file named without a folder.
    image_dirs: Vec<PathBuf>,
}

impl Gfx {
    pub(super) fn new(vars: &mut Vars) -> Self {
        let mut s = |n: &str| vars.slot(n);
        let v = Slots {
            r: s("gfx_r"),
            g: s("gfx_g"),
            b: s("gfx_b"),
            a: s("gfx_a"),
            a2: s("gfx_a2"),
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
        for (slot, value) in [(v.a, 1.0), (v.a2, 1.0), (v.dest, -1.0)] {
            vars.values[slot] = value;
        }
        Self {
            v,
            canvas: Default::default(),
            queue: Vec::new(),
            sizes: HashMap::new(),
            fonts: vec![FontSpec::bitmap(); 17],
            font: 0,
            chars: VecDeque::new(),
            mouse_in: false,
            menu_choice: None,
            menu: None,
            halted: false,
            image_dirs: Vec::new(),
        }
    }

    /// Whether the running code should stop: `gfx_showmenu` is waiting for the user.
    pub(super) fn halted(&self) -> bool {
        self.halted
    }
}

/// Strings end at a NUL, as C strings do.
fn c_text(s: &str) -> &str {
    s.split('\0').next().unwrap_or_default()
}

fn unpack(rgb: f64) -> [f32; 3] {
    let c = rgb.max(0.0) as u32;
    [(c & 0xff) as f32 / 255.0, (c >> 8 & 0xff) as f32 / 255.0, (c >> 16 & 0xff) as f32 / 255.0]
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
        values[v.texth] = self.gfx.fonts[self.gfx.font].size.round() as f64;
        let clear = values[v.clear];
        self.gfx.mouse_in = input.mouse_in;
        self.gfx.chars.extend(&input.chars);
        self.gfx.menu_choice = input.menu_choice;
        self.gfx.menu = None;
        self.gfx.halted = false;
        let (w, h) = (input.w.max(0.0) as usize, input.h.max(0.0) as usize);
        self.push(Cmd { dest: -1, paint: Paint::default(), clip: None, op: Op::Resize(w, h) });
        if clear >= 0.0 {
            let paint = Paint { rgb: unpack(clear), ..Paint::default() };
            self.push(Cmd { dest: -1, paint, clip: None, op: Op::Clear });
        }
    }

    /// Hands what the run drew to the canvas, to be drawn there.
    pub fn gfx_end(&mut self) -> Output {
        if std::mem::take(&mut self.gfx.halted) {
            // The run is repeated once the user has chosen; only what sizes the surfaces stays.
            self.gfx.queue.retain(|c| matches!(c.op, Op::Resize(..) | Op::SetImgDim(..) | Op::SetImage(_)));
        }
        self.gfx.menu_choice = None;
        self.send_queue();
        Output { canvas: self.gfx.canvas.clone(), menu: self.gfx.menu.take() }
    }

    /// Sets the folders `gfx_loadimg` looks in, the effect's own first.
    pub fn set_image_dirs(&mut self, dirs: Vec<PathBuf>) {
        self.gfx.image_dirs = dirs;
    }

    fn send_queue(&mut self) {
        let queue = std::mem::take(&mut self.gfx.queue);
        self.gfx.canvas.lock().unwrap_or_else(|e| e.into_inner()).queue.extend(queue);
    }

    /// Draws everything queued so far, for calls that read pixels back.
    fn draw_now(&mut self) -> std::sync::MutexGuard<'_, Canvas> {
        self.send_queue();
        let mut canvas = self.gfx.canvas.lock().unwrap_or_else(|e| e.into_inner());
        canvas.flush();
        canvas
    }

    fn push(&mut self, cmd: Cmd) {
        self.gfx.queue.push(cmd);
        if self.gfx.queue.len() >= MAX_QUEUE {
            drop(self.draw_now());
        }
    }

    fn gvar(&self, slot: fn(&Slots) -> usize) -> f64 {
        self.vars.values[slot(&self.gfx.v)]
    }

    fn set_gvar(&mut self, slot: fn(&Slots) -> usize, value: f64) {
        self.vars.values[slot(&self.gfx.v)] = value;
    }

    fn paint(&self) -> Paint {
        let c = |s: fn(&Slots) -> usize| self.gvar(s).clamp(0.0, 1.0) as f32;
        Paint {
            rgb: [c(|v| v.r), c(|v| v.g), c(|v| v.b)],
            a: self.gvar(|v| v.a) as f32,
            mode: self.gvar(|v| v.mode).max(0.0) as u32,
            a2: c(|v| v.a2),
        }
    }

    fn dest(&self) -> i64 {
        (self.gvar(|v| v.dest).round() as i64).max(-1)
    }

    fn surface_size(&self, id: i64) -> (f64, f64) {
        if id < 0 {
            (self.gvar(|v| v.w), self.gvar(|v| v.h))
        } else {
            self.gfx.sizes.get(&id).map_or((0.0, 0.0), |&(w, h)| (w as f64, h as f64))
        }
    }

    fn draw(&mut self, op: Op) {
        self.draw_clipped(op, None);
    }

    fn draw_clipped(&mut self, op: Op, clip: Option<[i64; 4]>) {
        let cmd = Cmd { dest: self.dest(), paint: self.paint(), clip, op };
        self.push(cmd);
    }

    fn current_font(&self) -> &FontSpec {
        &self.gfx.fonts[self.gfx.font]
    }

    /// Width and height of `text`, its lines split at `\n`.
    fn text_size(&self, text: &str) -> (f64, f64) {
        let font = self.current_font();
        if font.bitmap {
            return bitmap::measure(text);
        }
        text.split('\n').map(|l| text::measure(font, l)).fold((0.0, 0.0), |(w, h), (lw, lh)| (w.max(lw), h + lh))
    }

    /// Draws `text` at `(x, y)` a line at a time, leaving `gfx_x`/`gfx_y` after its end.
    fn draw_text(&mut self, text: &str, x: f64, y: f64, clip: Option<[i64; 4]>) {
        let text = c_text(text);
        let font = self.current_font().clone();
        if font.bitmap {
            let (dx, dy) = bitmap::advance(text);
            self.draw_clipped(Op::Text { x, y, font, text: text.to_owned() }, clip);
            self.set_gvar(|v| v.x, x + dx);
            self.set_gvar(|v| v.y, y + dy);
            return;
        }
        let mut last = (x, y);
        let mut ly = y;
        for line in text.split('\n') {
            let (w, h) = text::measure(&font, line);
            if !line.is_empty() {
                self.draw_clipped(Op::Text { x, y: ly, font: font.clone(), text: line.to_owned() }, clip);
            }
            last = (x + w, ly);
            ly += h;
        }
        self.set_gvar(|v| v.x, last.0);
        self.set_gvar(|v| v.y, last.1);
    }

    fn text_at_cursor(&mut self, text: &str) {
        let (x, y) = (self.gvar(|v| v.x), self.gvar(|v| v.y));
        self.draw_text(text, x, y, None);
    }

    /// `from` of `src` onto `to` (left, top, width, height) of the destination.
    fn blit(&mut self, src: i64, from: [f64; 4], to: [f64; 4], angle: f64, pivot: [f64; 2]) {
        self.draw(Op::Blit { src: src.max(-1), from, to, angle, pivot });
    }

    fn load_image(&mut self, id: i64, name: &str) -> f64 {
        let name = std::path::Path::new(name);
        let path = if name.is_absolute() {
            Some(name.to_path_buf())
        } else {
            self.gfx.image_dirs.iter().chain(&self.data_dirs).map(|d| d.join(name)).find(|p| p.is_file())
        };
        let Some(bmp) = path.and_then(|p| raster::load_image(&p)) else { return -1.0 };
        self.gfx.sizes.insert(id, (bmp.w, bmp.h));
        self.push(Cmd { dest: id, paint: Paint::default(), clip: None, op: Op::SetImage(bmp) });
        id as f64
    }

    pub(super) fn gfx_call(&mut self, call: Call, args: &[Node]) -> f64 {
        use Call::*;
        let n = args.len();
        let mut a = [0.0f64; 16];
        // Outputs (`gfx_measurestr`'s width and height, …) are read too, which does no harm.
        for (slot, node) in a.iter_mut().zip(args) {
            *slot = self.eval(node);
        }
        let get = |i: usize, default: f64| if n > i { a[i] } else { default };
        let (gx, gy) = (self.gvar(|v| v.x), self.gvar(|v| v.y));
        match call {
            Set => {
                let (g, b) = if n >= 3 { (a[1], a[2]) } else { (a[0], a[0]) };
                let slots = [self.gfx.v.r, self.gfx.v.g, self.gfx.v.b, self.gfx.v.a];
                for (slot, value) in slots.into_iter().zip([a[0], g, b, get(3, 1.0)]) {
                    self.vars.values[slot] = value;
                }
                if n >= 5 {
                    self.set_gvar(|v| v.mode, a[4]);
                }
                if n >= 6 {
                    self.set_gvar(|v| v.dest, a[5]);
                }
                if n >= 7 {
                    self.set_gvar(|v| v.a2, a[6]);
                }
                0.0
            }
            Rect => {
                self.draw(Op::Rect { x: a[0], y: a[1], w: a[2], h: a[3], filled: get(4, 1.0) != 0.0 });
                0.0
            }
            RectTo => {
                let (x0, x1) = (gx.min(a[0]), gx.max(a[0]));
                let (y0, y1) = (gy.min(a[1]), gy.max(a[1]));
                self.draw(Op::Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0, filled: true });
                self.set_gvar(|v| v.x, a[0]);
                self.set_gvar(|v| v.y, a[1]);
                a[0]
            }
            Line => {
                self.draw(Op::Line { a: [a[0], a[1]], b: [a[2], a[3]], aa: get(4, 1.0) != 0.0 });
                0.0
            }
            LineTo => {
                self.draw(Op::Line { a: [gx, gy], b: [a[0], a[1]], aa: get(2, 1.0) != 0.0 });
                self.set_gvar(|v| v.x, a[0]);
                self.set_gvar(|v| v.y, a[1]);
                a[0]
            }
            Circle => {
                let (filled, aa) = (get(3, 0.0) != 0.0, get(4, 1.0) != 0.0);
                self.draw(Op::Circle { x: a[0], y: a[1], r: a[2], filled, aa });
                0.0
            }
            Arc => {
                self.draw(Op::Arc { x: a[0], y: a[1], r: a[2], from: a[3], to: a[4], aa: get(5, 1.0) != 0.0 });
                0.0
            }
            RoundRect => {
                self.draw(Op::RoundRect { x: a[0], y: a[1], w: a[2], h: a[3], r: a[4], aa: get(5, 1.0) != 0.0 });
                0.0
            }
            Triangle => {
                let points: Vec<[f64; 2]> = (0..n.min(16) / 2).map(|i| [a[2 * i], a[2 * i + 1]]).collect();
                if points.len() >= 3 {
                    self.draw(Op::Polygon(points));
                }
                0.0
            }
            GradRect => {
                let part = |from: usize| std::array::from_fn(|c| a[from + c] as f32);
                let (start, dx, dy) = (part(4), part(8), part(12));
                self.draw(Op::Gradient { x: a[0], y: a[1], w: a[2], h: a[3], start, dx, dy });
                0.0
            }
            DrawStr => {
                let text = c_text(&self.string(a[0])).to_owned();
                let (w, h) = self.text_size(&text);
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
                        clip = Some([gx, gy, right, bottom].map(|v| v.floor() as i64));
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
                // REAPER leaves the outputs as they were for an empty string.
                if text.is_empty() {
                    return 0.0;
                }
                let (w, h) = self.text_size(&text);
                self.store(&args[1], w);
                self.store(&args[2], h);
                w
            }
            SetFont => {
                // Font 0 is the default font and cannot be changed.
                let idx = (a[0].max(0.0) as usize).min(16);
                if idx > 0 && n >= 2 {
                    let face = c_text(&self.string(a[1])).to_owned();
                    let font = &mut self.gfx.fonts[idx];
                    font.face = face;
                    font.bitmap = false;
                    if n >= 3 && a[2] > 0.0 {
                        font.size = a[2] as f32;
                    }
                    font.flags = if n >= 4 { a[3].max(0.0) as u32 } else { 0 };
                }
                self.gfx.font = idx;
                self.set_gvar(|v| v.texth, self.gfx.fonts[idx].size.round() as f64);
                1.0
            }
            GetFont => {
                if n >= 1 {
                    let face = self.current_font().face.clone();
                    self.set_string(a[0], face);
                }
                self.gfx.font as f64
            }
            SetPixel => {
                let mut paint = self.paint();
                paint.rgb = [a[0], a[1], a[2]].map(|c| c.clamp(0.0, 1.0) as f32);
                self.push(Cmd { dest: self.dest(), paint, clip: None, op: Op::Pixel { x: gx, y: gy } });
                0.0
            }
            GetPixel => {
                let dest = self.dest();
                let p = self.draw_now().pixel(dest, gx.floor() as i64, gy.floor() as i64).unwrap_or([0; 4]);
                for (arg, c) in args.iter().zip(p) {
                    self.store(arg, c as f64 / 255.0);
                }
                p[0] as f64 / 255.0
            }
            LoadImg => {
                let id = a[0].round() as i64;
                let name = c_text(&self.string(a[1])).to_owned();
                if (0..1024).contains(&id) { self.load_image(id, &name) } else { -1.0 }
            }
            SetImgDim => {
                let id = a[0].round() as i64;
                if !(0..1024).contains(&id) {
                    return 0.0;
                }
                let dim = |v: f64| (v.max(0.0) as usize).min(raster::MAX_DIM);
                let (w, h) = (dim(a[1]), dim(a[2]));
                self.gfx.sizes.insert(id, (w, h));
                self.push(Cmd { dest: id, paint: Paint::default(), clip: None, op: Op::SetImgDim(w, h) });
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
                let scale = get(1, 1.0);
                let (fx, fy, fw, fh) = (get(3, 0.0), get(4, 0.0), get(5, sw), get(6, sh));
                let (tx, ty) = (get(7, gx), get(8, gy));
                let (tw, th) = (get(9, fw * scale), get(10, fh * scale));
                self.blit(src, [fx, fy, fw, fh], [tx, ty, tw, th], get(2, 0.0), [get(11, 0.0), get(12, 0.0)]);
                src as f64
            }
            BlitExt => {
                let p: [f64; 10] = std::array::from_fn(|i| self.read(a[1] + i as f64));
                self.blit(a[0].round() as i64, [p[0], p[1], p[2], p[3]], [p[4], p[5], p[6], p[7]], a[2], [p[8], p[9]]);
                a[0]
            }
            GetChar => match a[0] as i64 {
                65536 => (1 | 4 | if self.gfx.mouse_in { 2 | 8 } else { 0 }) as f64,
                0 => self.gfx.chars.pop_front().map_or(0.0, |c| c as f64),
                _ => 0.0,
            },
            BlurTo => {
                self.draw(Op::Blur([gx, gy, a[0], a[1]]));
                self.set_gvar(|v| v.x, a[0]);
                self.set_gvar(|v| v.y, a[1]);
                a[0]
            }
            MulAddRect => {
                let mul = [a[4], a[5], a[6], get(7, 1.0)].map(|v| v as f32);
                let add = [a[8], a[9], a[10], a[11]].map(|v| v as f32);
                self.draw(Op::MulAdd { rect: [a[0], a[1], a[2], a[3]], mul, add });
                0.0
            }
            ShowMenu => {
                if let Some(choice) = self.gfx.menu_choice.take() {
                    return choice as f64;
                }
                let items = menu::parse(c_text(&self.string(a[0])));
                if !items.is_empty() {
                    self.gfx.menu = Some(Menu { items, x: gx, y: gy });
                    self.gfx.halted = true;
                }
                0.0
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

    fn machine(src: &str) -> (Machine, crate::eel::Code) {
        let mut vars = Vars::default();
        let code = compile(src, &mut vars, &mut HashMap::new()).unwrap();
        (Machine::new(vars), code)
    }

    fn run(m: &mut Machine, code: &crate::eel::Code, input: &Input) -> (Output, Option<Bitmap>) {
        m.gfx_begin(input);
        m.run(code);
        let out = m.gfx_end();
        let screen = out.render();
        (out, screen)
    }

    fn frame(src: &str) -> (Machine, Bitmap) {
        let (mut m, code) = machine(src);
        let (_, screen) = run(&mut m, &code, &view());
        (m, screen.expect("a first frame shows"))
    }

    fn view() -> Input {
        Input { w: 64.0, h: 48.0, ..Default::default() }
    }

    fn rgb(b: &Bitmap, x: i64, y: i64) -> [u8; 3] {
        let p = b.get(x, y).unwrap();
        [p[0], p[1], p[2]]
    }

    fn near(a: [u8; 3], b: [u8; 3]) -> bool {
        a.iter().zip(b).all(|(&a, b)| a.abs_diff(b) <= 1)
    }

    #[test]
    fn rects_blend_with_alpha_and_add_in_additive_mode() {
        let (mut m, code) = machine(
            "gfx_set(1, 0, 0, 0.5); gfx_rect(0, 0, 10, 10); \
             gfx_set(0.6, 0, 0, 0.5, 1); gfx_rect(20, 0, 10, 10); gfx_rect(20, 0, 10, 10); \
             gfx_set(0, 1, 0, 1, 0); gfx_rect(40, 0, 5, 5, 0);",
        );
        let clear = m.vars.slot("gfx_clear");
        m.vars.values[clear] = 0x204000 as f64;
        let s = run(&mut m, &code, &view()).1.unwrap();
        assert_eq!(rgb(&s, 30, 30), [0x00, 0x40, 0x20], "gfx_clear is r | g << 8 | b << 16");
        assert!(near(rgb(&s, 5, 5), [128, 32, 16]), "half of red over the clear: {:?}", rgb(&s, 5, 5));
        assert_eq!(rgb(&s, 10, 5), [0x00, 0x40, 0x20], "the rect ends before x + w");
        assert!(near(rgb(&s, 25, 5), [154, 64, 32]), "added twice: {:?}", rgb(&s, 25, 5));
        assert_eq!((rgb(&s, 40, 2), rgb(&s, 44, 4)), ([0, 255, 0], [0, 255, 0]), "an outline");
        assert_eq!(rgb(&s, 42, 2), [0x00, 0x40, 0x20], "with nothing inside");
    }

    #[test]
    fn lines_and_circles_through_pixel_centres() {
        let (_, s) = frame(
            "gfx_set(1); gfx_line(0, 5, 19, 5); gfx_line(0, 20, 10, 25); \
             gfx_circle(40, 20, 5, 1); gfx_circle(40, 40, 5, 0);",
        );
        assert_eq!((rgb(&s, 10, 5), rgb(&s, 10, 4), rgb(&s, 20, 5)), ([255; 3], [0; 3], [0; 3]));
        assert_eq!(rgb(&s, 2, 21), [255; 3], "the diagonal crosses (2, 21) exactly");
        assert!(near(rgb(&s, 1, 20), [128; 3]) && near(rgb(&s, 1, 21), [128; 3]), "and halfway at x = 1");
        assert_eq!((rgb(&s, 40, 20), rgb(&s, 44, 20), rgb(&s, 47, 20)), ([255; 3], [255; 3], [0; 3]));
        assert_eq!((rgb(&s, 45, 40), rgb(&s, 40, 40)), ([255; 3], [0; 3]), "an outline is a ring");
    }

    #[test]
    fn getpixel_reads_what_was_drawn() {
        let (m, _) = frame("gfx_set(0.2, 0.4, 1); gfx_rect(3, 3, 2, 2); gfx_x = gfx_y = 4; gfx_getpixel(r, g, b);");
        let v = |n: &str| (m.vars.get(n).unwrap() * 255.0).round();
        assert_eq!([v("r"), v("g"), v("b")], [51.0, 102.0, 255.0]);
    }

    #[test]
    fn blits_turn_clockwise_about_the_centre() {
        let src = "gfx_setimgdim(0, 4, 4); gfx_dest = 0; gfx_set(1, 0, 0); gfx_x = gfx_y = 0; gfx_setpixel(1, 0, 0); \
                   gfx_dest = -1; gfx_a = 1; gfx_mode = MODE; gfx_blit(0, 1, $pi / 2, 0, 0, 4, 4, 10, 10, 4, 4); \
                   gfx_blit(0, 2, 0, 0, 0, 4, 4, 30, 10, 8, 8); gfx_getimgdim(0, w, h);";
        for mode in ["4", "0"] {
            let (m, s) = frame(&src.replace("MODE", mode));
            assert_eq!((m.vars.get("w"), m.vars.get("h")), (Some(4.0), Some(4.0)));
            assert_eq!(rgb(&s, 13, 10), [255, 0, 0], "the top-left corner turns to the top-right (mode {mode})");
            assert_eq!((rgb(&s, 10, 10), rgb(&s, 10, 13)), ([0; 3], [0; 3]));
            if mode == "4" {
                assert_eq!((rgb(&s, 31, 11), rgb(&s, 32, 11)), ([255, 0, 0], [0; 3]), "scaled up two times");
            }
        }
    }

    #[test]
    fn font_0_is_eight_pixel_cells() {
        let (m, s) = frame(
            "th = gfx_texth; gfx_setfont(1, \"Sans\", 20); gfx_setfont(0); th0 = gfx_texth; \
             w = h = -1; gfx_measurestr(\"Hello\", w, h); ew = eh = -1; gfx_measurestr(\"\", ew, eh); \
             gfx_measurechar(233, cw, ch); gfx_set(1); gfx_x = 2; gfx_y = 3; gfx_drawstr(\"I\\nI\"); \
             x_after = gfx_x; y_after = gfx_y;",
        );
        let var = |n: &str| m.vars.get(n).unwrap();
        assert_eq!((var("th"), var("th0")), (8.0, 8.0));
        assert_eq!((var("w"), var("h"), var("ew"), var("eh")), (40.0, 8.0, -1.0, -1.0));
        assert_eq!((var("cw"), var("ch")), (16.0, 8.0), "a byte a cell");
        assert_eq!((var("x_after"), var("y_after")), (10.0, 11.0));
        let lit: Vec<(i64, i64)> =
            (0..20).flat_map(|y| (0..20).map(move |x| (x, y))).filter(|&(x, y)| rgb(&s, x, y) != [0; 3]).collect();
        assert!(!lit.is_empty() && lit.iter().all(|&(x, y)| (2..10).contains(&x) && (3..19).contains(&y)), "{lit:?}");
        assert!(lit.iter().any(|&(_, y)| y >= 11), "the second line is drawn a cell lower");
    }

    #[test]
    fn texth_is_0_until_the_first_view() {
        let (mut m, code) = machine("th = gfx_texth;");
        m.run(&code);
        assert_eq!(m.vars.get("th"), Some(0.0));
    }

    #[test]
    fn measures_text_as_it_draws_it() {
        let (m, s) = frame(
            "gfx_setfont(1, \"Sans\", 20); gfx_measurestr(\"Hello\", w, h); th = gfx_texth; \
             gfx_set(1); gfx_x = 10; gfx_y = 10; gfx_drawstr(\"Hello\"); x_after = gfx_x;",
        );
        let var = |n: &str| m.vars.get(n).unwrap();
        let (w, h) = (var("w"), var("h"));
        assert_eq!((h, var("th"), var("x_after")), (20.0, 20.0, 10.0 + w));
        let lit: Vec<(i64, i64)> = (0..s.h as i64)
            .flat_map(|y| (0..s.w as i64).map(move |x| (x, y)))
            .filter(|&(x, y)| rgb(&s, x, y) != [0; 3])
            .collect();
        if lit.is_empty() {
            // No font on this system: nothing is drawn, but the measures still hold.
            return;
        }
        let l = lit.iter().map(|p| p.0).min().unwrap() as f64;
        let r = lit.iter().map(|p| p.0).max().unwrap() as f64 + 1.0;
        let t = lit.iter().map(|p| p.1).min().unwrap() as f64;
        let b = lit.iter().map(|p| p.1).max().unwrap() as f64 + 1.0;
        assert!(l >= 9.0 && r <= 10.0 + w + 1.0 && t >= 10.0 && b <= 10.0 + h, "ink {l}..{r} x {t}..{b} in {w} x {h}");
        assert!(r - l >= w * 0.8, "the ink spans the measured width: {} of {w}", r - l);
    }

    #[test]
    fn loads_png_images() {
        let dir = std::env::temp_dir().join(format!("scythe-gfx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut img = image::RgbaImage::new(3, 2);
        img.put_pixel(1, 0, image::Rgba([10, 200, 30, 255]));
        img.put_pixel(2, 1, image::Rgba([250, 0, 0, 0]));
        img.save(dir.join("t.png")).unwrap();
        let (mut m, code) = machine(
            "id = gfx_loadimg(3, \"t.png\"); missing = gfx_loadimg(4, \"none.png\"); gfx_getimgdim(3, w, h); \
             gfx_set(1, 1, 1); gfx_rect(0, 0, 10, 10); gfx_x = gfx_y = 0; gfx_blit(3, 1, 0);",
        );
        m.set_image_dirs(vec![dir.clone()]);
        let (_, s) = run(&mut m, &code, &view());
        let s = s.unwrap();
        std::fs::remove_dir_all(&dir).ok();
        let var = |n: &str| m.vars.get(n).unwrap();
        assert_eq!([var("id"), var("missing"), var("w"), var("h")], [3.0, -1.0, 3.0, 2.0]);
        assert_eq!(rgb(&s, 1, 0), [10, 200, 30]);
        assert_eq!(rgb(&s, 2, 1), [255; 3], "a transparent pixel leaves what is under it");
    }

    #[test]
    fn showmenu_stops_the_run_and_returns_the_choice_when_repeated() {
        let (mut m, code) = machine(
            "gfx_x = 5; gfx_y = 6; gfx_set(1); gfx_rect(0, 0, 4, 4); \
             mouse_cap == 2 ? (choice = gfx_showmenu(\"a|>sub|b|<c\"); after = 1;); runs += 1;",
        );
        let click = Input { mouse_cap: 2, ..view() };
        let (out, screen) = run(&mut m, &code, &click);
        let menu = out.menu.expect("a menu is asked for");
        assert_eq!((menu.x, menu.y, menu.items.len()), (5.0, 6.0, 2));
        assert_eq!((m.vars.get("after"), m.vars.get("runs")), (Some(0.0), Some(0.0)), "the run stops at the menu");
        assert_eq!(screen.map(|s| rgb(&s, 1, 1)), Some([0; 3]), "and what it drew is dropped");
        let (out, screen) = run(&mut m, &code, &Input { menu_choice: Some(2), ..click });
        assert!(out.menu.is_none());
        assert_eq!([m.vars.get("choice"), m.vars.get("after"), m.vars.get("runs")], [Some(2.0), Some(1.0), Some(1.0)]);
        assert_eq!(screen.map(|s| rgb(&s, 1, 1)), Some([255; 3]));
    }

    #[test]
    fn keeps_the_screen_without_gfx_clear_and_scrolls_by_blitting() {
        let (mut m, code) = machine(
            "gfx_clear = -1; gfx_blit(-1, 1, 0, 1, 0, 9, 10, 0, 0, 9, 10); gfx_x = 9; gfx_y = 0; gfx_setpixel(1, 1, 1);",
        );
        let input = Input { w: 10.0, h: 10.0, ..Default::default() };
        let mut screen = None;
        for _ in 0..3 {
            screen = run(&mut m, &code, &input).1.or(screen);
        }
        let s = screen.unwrap();
        let lit: Vec<i64> = (0..10).filter(|&x| rgb(&s, x, 0) == [255; 3]).collect();
        assert_eq!(lit, [7, 8, 9], "each frame's pixel moves one to the left");
    }

    #[test]
    fn mouse_and_size_reach_the_script() {
        let input = Input { mouse_x: 12.0, mouse_y: 3.0, mouse_cap: 1, wheel: 120.0, w: 200.0, ..view() };
        let (mut m, code) = machine("a = gfx_w; b = mouse_cap & 1 ? mouse_x : -1; c = mouse_wheel;");
        run(&mut m, &code, &input);
        assert_eq!([m.vars.get("a"), m.vars.get("b"), m.vars.get("c")], [Some(200.0), Some(12.0), Some(120.0)]);
    }
}
