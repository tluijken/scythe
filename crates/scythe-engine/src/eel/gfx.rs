//! JSFX graphics. The `gfx_*` builtins queue drawing calls for a pixel canvas (the screen and
//! images 0..1023), drawn when a run ends or a call reads pixels back. The graphics state is
//! shared by an effect's machines, so images loaded in `@init` are there for `@gfx`. `@gfx`
//! itself runs on a [`View`]'s thread, as REAPER runs it beside the audio over the same
//! variables and memory. The screen keeps what was drawn while `gfx_clear` is negative, as
//! REAPER's framebuffer does.

mod bitmap;
mod menu;
mod raster;
mod text;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

pub use menu::{Menu, MenuItem};
pub use raster::{Bitmap, Canvas, Rgba};
pub use text::FontSpec;

use raster::{Cmd, Op, Paint};

use super::{Code, Machine, Node, Vars, lock};

/// What the UI hands `@gfx` for one run.
#[derive(Debug, Clone, Default)]
pub struct Input {
    /// The view's size and the mouse in points.
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
    /// Pixels per point of the screen the view is on; 0 counts as 1.
    pub scale: f64,
    /// Files dropped onto the view since the last run, for `gfx_getdropfile`.
    pub dropped: Vec<String>,
}

/// What one run of `@gfx` leaves.
pub struct Output {
    canvas: Arc<Mutex<Canvas>>,
    /// Screen pixels per point: the view's scale when the script asked for it with
    /// `gfx_ext_retina`, else 1.
    pub scale: f64,
    /// Sliders the run changed, by their index in the script, with their new values.
    pub sliders: Vec<(usize, f64)>,
}

impl Output {
    /// Draws what the run queued and returns the screen if it changed.
    pub fn render(&self) -> Option<Bitmap> {
        let mut canvas = lock(&self.canvas);
        canvas.flush();
        canvas.take_screen()
    }
}

/// What a [`View`]'s thread reports.
pub enum Event {
    /// A run ended.
    Frame(Output),
    /// The script waits in `gfx_showmenu` until [`View::choose`] answers.
    Menu(Menu),
}

/// `@gfx` running on a thread of its own, so the audio thread never waits for it and
/// `gfx_showmenu` waits for the user inside the script, as REAPER's does. The thread ends once
/// the view is dropped and its run is over.
pub struct View {
    input: Sender<Input>,
    choice: Sender<u32>,
    events: Receiver<Event>,
    busy: bool,
}

impl View {
    /// Runs `code` on `machine` (a [`Machine::share`] of the effect's) once per
    /// [`View::run`]; `sliders` are the slots of the script's sliders, reported when a run
    /// changes them.
    pub fn spawn(mut machine: Machine, code: Vec<Code>, sliders: Vec<usize>) -> Self {
        let (input, inputs) = channel::<Input>();
        let (choice, choices) = channel::<u32>();
        let (event, events) = channel();
        let menus = event.clone();
        machine.gfx.menu = Some(Box::new(move |menu| {
            if menus.send(Event::Menu(menu)).is_err() {
                return 0;
            }
            choices.recv().unwrap_or(0)
        }));
        let run = move || {
            while let Ok(input) = inputs.recv() {
                let before: Vec<f64> = sliders.iter().map(|&s| machine.vars.at(s)).collect();
                machine.gfx_begin(&input);
                for c in &code {
                    machine.run(c);
                }
                let mut out = machine.gfx_end();
                out.sliders = sliders
                    .iter()
                    .zip(before)
                    .enumerate()
                    .filter_map(|(i, (&s, old))| {
                        let now = machine.vars.at(s);
                        (now.to_bits() != old.to_bits()).then_some((i, now))
                    })
                    .collect();
                if event.send(Event::Frame(out)).is_err() {
                    break;
                }
            }
        };
        if let Err(e) = std::thread::Builder::new().name("jsfx-gfx".into()).spawn(run) {
            eprintln!("cannot start a JSFX view: {e}");
        }
        Self { input, choice, events, busy: false }
    }

    /// Whether a run has started and not yet ended.
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Starts a run unless one is going.
    pub fn run(&mut self, input: Input) {
        if !self.busy && self.input.send(input).is_ok() {
            self.busy = true;
        }
    }

    /// What the thread reported since last asked.
    pub fn poll(&mut self) -> Option<Event> {
        let e = self.events.try_recv().ok()?;
        if matches!(e, Event::Frame(_)) {
            self.busy = false;
        }
        Some(e)
    }

    /// Answers the menu the script waits on: the item chosen, or 0.
    pub fn choose(&mut self, choice: u32) {
        self.choice.send(choice).ok();
    }
}

/// Calls queued before they are drawn mid-run, to bound the queue.
const MAX_QUEUE: usize = 1 << 16;
/// `gfx_getsyscol`: the window background, as `r | g << 8 | b << 16`.
const SYSCOL: f64 = (0x26 | 0x26 << 8 | 0x2a << 16) as f64;
/// The largest table `gfx_transformblit` reads.
const MAX_DIVS: usize = 64;

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
    DeltaBlit,
    TransformBlit,
    SetImgDim,
    GetImgDim,
    GetChar,
    BlurTo,
    MulAddRect,
    ShowMenu,
    SysCol,
    DropFile,
    /// A ReaScript's window: `gfx_init`, `gfx_quit`, `gfx_update` and `gfx_dock`.
    Init,
    Quit,
    Update,
    Dock,
    /// Cursors and other images' extents, which this host does not have.
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
            "gfx_deltablit" => (DeltaBlit, ANY),
            "gfx_transformblit" => (TransformBlit, 8),
            "gfx_setimgdim" => (SetImgDim, 3),
            "gfx_getimgdim" => (GetImgDim, 3),
            "gfx_getchar" => (GetChar, ANY),
            "gfx_blurto" => (BlurTo, 2),
            "gfx_muladdrect" => (MulAddRect, ANY),
            "gfx_showmenu" => (ShowMenu, 1),
            "gfx_getsyscol" => (SysCol, ANY),
            "gfx_getdropfile" => (DropFile, ANY),
            "gfx_init" => (Init, ANY),
            "gfx_quit" => (Quit, ANY),
            "gfx_update" => (Update, ANY),
            "gfx_dock" => (Dock, ANY),
            "gfx_setcursor" | "gfx_setimgdim_ext" => (Nothing, ANY),
            _ => return None,
        })
    }
}

#[derive(Clone, Copy)]
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
    retina: usize,
    mouse_x: usize,
    mouse_y: usize,
    mouse_cap: usize,
    mouse_wheel: usize,
    mouse_hwheel: usize,
}

/// The graphics state of an effect, shared by its machines.
struct Gfx {
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
    /// Files dropped onto the view and not yet cleared with `gfx_getdropfile(-1)`.
    dropped: Vec<String>,
    /// The scale of the run going on.
    scale: f64,
    /// Where `gfx_loadimg` looks for a file named without a folder.
    image_dirs: Vec<PathBuf>,
    /// The window a ReaScript opened with `gfx_init`; effects have none.
    window: Option<Window>,
}

/// A ReaScript's graphics window as the script left it.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub title: String,
    pub w: u32,
    pub h: u32,
    /// False once `gfx_quit` closed it or the user did; `gfx_getchar` then returns -1.
    pub open: bool,
}

/// A machine's way to the shared graphics state, and how its `gfx_showmenu` reaches the user.
pub(super) struct Link {
    state: Arc<Mutex<Gfx>>,
    v: Slots,
    /// Shows a menu and waits for the choice; without it `gfx_showmenu` gives 0.
    menu: Option<Box<dyn FnMut(Menu) -> u32 + Send>>,
}

impl Link {
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
            retina: s("gfx_ext_retina"),
            mouse_x: s("mouse_x"),
            mouse_y: s("mouse_y"),
            mouse_cap: s("mouse_cap"),
            mouse_wheel: s("mouse_wheel"),
            mouse_hwheel: s("mouse_hwheel"),
        };
        for (slot, value) in [(v.a, 1.0), (v.a2, 1.0), (v.dest, -1.0)] {
            vars.put(slot, value);
        }
        let state = Gfx {
            v,
            canvas: Default::default(),
            queue: Vec::new(),
            sizes: HashMap::new(),
            fonts: vec![FontSpec::bitmap(); 17],
            font: 0,
            chars: VecDeque::new(),
            mouse_in: false,
            dropped: Vec::new(),
            scale: 1.0,
            image_dirs: Vec::new(),
            window: None,
        };
        Self { state: Arc::new(Mutex::new(state)), v, menu: None }
    }

    pub(super) fn share(&self) -> Self {
        Self { state: self.state.clone(), v: self.v, menu: None }
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

/// A colour channel as REAPER takes it: truncated to a byte.
fn channel_byte(v: f64) -> f32 {
    ((v * 255.0) as i64).clamp(0, 255) as f32 / 255.0
}

/// What a call reads besides its arguments: its string and memory, fetched before the
/// graphics state is locked.
#[derive(Default)]
struct Reads {
    text: String,
    table: Vec<f64>,
    dirs: Vec<PathBuf>,
}

/// What a call writes back once the graphics state is let go: values to its arguments, and a
/// string.
#[derive(Default)]
struct Writes {
    args: Vec<(usize, f64)>,
    string: Option<(f64, String)>,
}

impl Machine {
    /// Prepares a run of `@gfx`: the view's size, the mouse and keys, and the screen cleared
    /// to `gfx_clear` unless it is negative.
    pub fn gfx_begin(&mut self, input: &Input) {
        let state = self.gfx.state.clone();
        let mut g = lock(&state);
        let v = g.v;
        let vars = &self.vars;
        let view_scale = if input.scale > 0.0 { input.scale } else { 1.0 };
        // A script that sets `gfx_ext_retina` draws at the screen's own pixels.
        let scale = if vars.at(v.retina) > 0.0 {
            vars.put(v.retina, view_scale);
            view_scale
        } else {
            1.0
        };
        let (w, h) = ((input.w * scale).floor(), (input.h * scale).floor());
        vars.put(v.w, w);
        vars.put(v.h, h);
        vars.put(v.mouse_x, (input.mouse_x * scale).floor());
        vars.put(v.mouse_y, (input.mouse_y * scale).floor());
        vars.put(v.mouse_cap, input.mouse_cap as f64);
        vars.put(v.mouse_wheel, vars.at(v.mouse_wheel) + input.wheel);
        vars.put(v.mouse_hwheel, vars.at(v.mouse_hwheel) + input.hwheel);
        vars.put(v.dest, -1.0);
        vars.put(v.texth, g.fonts[g.font].size.round() as f64);
        g.mouse_in = input.mouse_in;
        g.scale = scale;
        g.chars.extend(&input.chars);
        g.dropped.extend(input.dropped.iter().cloned());
        let clear = vars.at(v.clear);
        g.push(Cmd {
            dest: -1,
            paint: Paint::default(),
            clip: None,
            op: Op::Resize(w.max(0.0) as usize, h.max(0.0) as usize),
        });
        if clear >= 0.0 {
            let paint = Paint { rgb: unpack(clear), ..Paint::default() };
            g.push(Cmd { dest: -1, paint, clip: None, op: Op::Clear });
        }
    }

    /// Hands what the run drew to the canvas, to be drawn there.
    pub fn gfx_end(&mut self) -> Output {
        let mut g = lock(&self.gfx.state);
        g.send_queue();
        Output { canvas: g.canvas.clone(), scale: g.scale, sliders: Vec::new() }
    }

    /// The window a ReaScript opened with `gfx_init`.
    pub fn gfx_window(&self) -> Option<Window> {
        lock(&self.gfx.state).window.clone()
    }

    /// The user closed the script's window.
    pub fn close_gfx_window(&mut self) {
        if let Some(w) = &mut lock(&self.gfx.state).window {
            w.open = false;
        }
    }

    /// Sets the folders `gfx_loadimg` looks in, the effect's own first.
    pub fn set_image_dirs(&mut self, dirs: Vec<PathBuf>) {
        lock(&self.gfx.state).image_dirs = dirs;
    }

    pub(super) fn gfx_call(&mut self, call: Call, args: &[Node]) -> f64 {
        use Call::*;
        let n = args.len();
        let mut a = [0.0f64; 16];
        // Outputs (`gfx_measurestr`'s width and height, …) are read too, which does no harm;
        // `gfx_printf` reads its arguments as it formats them.
        if call != Printf {
            for (slot, node) in a.iter_mut().zip(args) {
                *slot = self.eval(node);
            }
        }
        let mut reads = Reads::default();
        match call {
            DrawStr | MeasureStr | ShowMenu | Init => reads.text = c_text(&self.string(a[0])).to_owned(),
            SetFont | LoadImg if n >= 2 => reads.text = c_text(&self.string(a[1])).to_owned(),
            Printf => reads.text = self.format(args),
            LoadImg => {}
            BlitExt => reads.table = (0..10).map(|i| self.read(a[1] + i as f64)).collect(),
            TransformBlit => {
                let (dw, dh) = (a[5].clamp(2.0, MAX_DIVS as f64) as usize, a[6].clamp(2.0, MAX_DIVS as f64) as usize);
                reads.table = (0..2 * dw * dh).map(|i| self.read(a[7] + i as f64)).collect();
            }
            _ => {}
        }
        if call == ShowMenu {
            return self.show_menu(&reads.text);
        }
        let mut writes = Writes::default();
        let state = self.gfx.state.clone();
        if call == LoadImg {
            reads.dirs = self.data_dirs.clone();
        }
        let r = lock(&state).call(call, &self.vars, &a, n.min(16), &reads, &mut writes);
        for (i, v) in writes.args {
            if let Some(node) = args.get(i) {
                self.store(node, v);
            }
        }
        if let Some((h, s)) = writes.string {
            self.set_string(h, s);
        }
        r
    }

    /// `gfx_showmenu`: waits for the user's choice, with the graphics state let go so the
    /// audio thread's calls do not wait on the menu.
    fn show_menu(&mut self, text: &str) -> f64 {
        let items = menu::parse(text);
        if items.is_empty() {
            return 0.0;
        }
        let v = self.gfx.v;
        let menu = Menu { items, x: self.vars.at(v.x), y: self.vars.at(v.y) };
        self.gfx.menu.as_mut().map_or(0.0, |show| show(menu) as f64)
    }
}

impl Gfx {
    fn send_queue(&mut self) {
        let queue = std::mem::take(&mut self.queue);
        lock(&self.canvas).queue.extend(queue);
    }

    /// Draws everything queued so far, for calls that read pixels back.
    fn draw_now(&mut self) -> std::sync::MutexGuard<'_, Canvas> {
        self.send_queue();
        let mut canvas = lock(&self.canvas);
        canvas.flush();
        canvas
    }

    fn push(&mut self, cmd: Cmd) {
        self.queue.push(cmd);
        if self.queue.len() >= MAX_QUEUE {
            drop(self.draw_now());
        }
    }

    fn paint(&self, vars: &Vars) -> Paint {
        let v = self.v;
        Paint {
            rgb: [v.r, v.g, v.b].map(|s| channel_byte(vars.at(s))),
            a: vars.at(v.a) as f32,
            mode: vars.at(v.mode).max(0.0) as u32,
            a2: channel_byte(vars.at(v.a2)),
        }
    }

    fn dest(&self, vars: &Vars) -> i64 {
        (vars.at(self.v.dest).round() as i64).max(-1)
    }

    fn surface_size(&self, vars: &Vars, id: i64) -> (f64, f64) {
        if id < 0 {
            (vars.at(self.v.w), vars.at(self.v.h))
        } else {
            self.sizes.get(&id).map_or((0.0, 0.0), |&(w, h)| (w as f64, h as f64))
        }
    }

    fn draw(&mut self, vars: &Vars, op: Op) {
        self.draw_clipped(vars, op, None);
    }

    fn draw_clipped(&mut self, vars: &Vars, op: Op, clip: Option<[i64; 4]>) {
        let cmd = Cmd { dest: self.dest(vars), paint: self.paint(vars), clip, op };
        self.push(cmd);
    }

    fn current_font(&self) -> &FontSpec {
        &self.fonts[self.font]
    }

    /// Width and height of `text`, its lines split at `\n`.
    fn text_size(&self, text: &str) -> (f64, f64) {
        let font = self.current_font();
        if font.bitmap {
            return bitmap::measure(text);
        }
        text.split('\n').map(|l| text::measure(font, l)).fold((0.0, 0.0), |(w, h), (lw, lh)| (w.max(lw), h + lh))
    }

    /// `gfx_drawstr`, `gfx_printf` and the rest: `text` from `gfx_x`/`gfx_y`, or placed in the
    /// box to `(right, bottom)` by `flags` (1 centred, 2 right, 4 middle, 8 bottom, 256 not
    /// clipped to the box). Font 0 aligns the text as a block and, clipped to a box, draws
    /// into a window on the box (its corner moved onto the destination, as LICE's sub-bitmaps
    /// are) and leaves `gfx_x`/`gfx_y` measured from that corner; other fonts align each line
    /// in the box below the lines before it and leave `gfx_x` after the last line, measured
    /// from the start whatever the alignment.
    fn draw_text(&mut self, vars: &Vars, text: &str, boxed: Option<(u32, f64, f64)>) {
        let v = self.v;
        let (x0, y0) = (vars.at(v.x).floor(), vars.at(v.y).floor());
        let font = self.current_font().clone();
        let (flags, right, bottom) = boxed.map_or((0, 0.0, 0.0), |(f, r, b)| (f, r.floor(), b.floor()));
        let (bw, bh) = (right - x0, bottom - y0);
        let clipped = boxed.is_some() && flags & 256 == 0;
        let empty = clipped && (bw <= 0.0 || bh <= 0.0);
        let offset = |flags: u32, center: u32, far: u32, room: f64, size: f64| {
            if flags & center != 0 {
                ((room - size) / 2.0).floor()
            } else if flags & far != 0 {
                room - size
            } else {
                0.0
            }
        };
        if font.bitmap {
            if empty {
                vars.put(v.x, 0.0);
                return;
            }
            let (w, h) = bitmap::measure(text);
            let (dx, dy) = match boxed {
                Some(_) => (offset(flags, 1, 2, bw, w), offset(flags, 4, 8, bh, h)),
                None => (0.0, 0.0),
            };
            let (adv_x, adv_y) = bitmap::advance(text);
            let (ox, oy, clip) = if clipped {
                let (ox, oy) = (x0.max(0.0), y0.max(0.0));
                (ox, oy, Some([ox, oy, ox + bw, oy + bh].map(|c| c as i64)))
            } else {
                (x0, y0, None)
            };
            self.draw_clipped(vars, Op::Text { x: ox + dx, y: oy + dy, font, text: text.to_owned() }, clip);
            let (bx, by) = if clipped { (0.0, 0.0) } else { (x0, y0) };
            vars.put(v.x, bx + dx + adv_x);
            vars.put(v.y, by + dy + adv_y);
            return;
        }
        let lines: Vec<&str> = text.split('\n').collect();
        let clip = clipped.then(|| [x0, y0, right, bottom].map(|c| c as i64));
        let mut last_w = 0.0;
        let mut top = y0;
        for (i, line) in lines.iter().enumerate() {
            let (w, lh) = text::measure(&font, line);
            if i > 0 {
                top += lh;
            }
            last_w = w;
            if empty || line.is_empty() {
                continue;
            }
            let (dx, dy) = match boxed {
                Some(_) => (offset(flags, 1, 2, bw, w), offset(flags, 4, 8, bottom - top, lh)),
                None => (0.0, 0.0),
            };
            self.draw_clipped(
                vars,
                Op::Text { x: x0 + dx, y: top + dy, font: font.clone(), text: (*line).to_owned() },
                clip,
            );
        }
        vars.put(v.x, x0 + last_w);
        vars.put(v.y, if empty { y0 } else { top });
    }

    fn load_image(&mut self, data_dirs: &[PathBuf], id: i64, name: &str) -> f64 {
        let name = std::path::Path::new(name);
        let path = if name.is_absolute() {
            Some(name.to_path_buf())
        } else {
            self.image_dirs.iter().chain(data_dirs).map(|d| d.join(name)).find(|p| p.is_file())
        };
        let Some(bmp) = path.and_then(|p| raster::load_image(&p)) else { return -1.0 };
        self.sizes.insert(id, (bmp.w, bmp.h));
        self.push(Cmd { dest: id, paint: Paint::default(), clip: None, op: Op::SetImage(bmp) });
        id as f64
    }

    fn call(&mut self, call: Call, vars: &Vars, a: &[f64; 16], n: usize, reads: &Reads, out: &mut Writes) -> f64 {
        use Call::*;
        let get = |i: usize, default: f64| if i < n { a[i] } else { default };
        let v = self.v;
        let (gx, gy) = (vars.at(v.x), vars.at(v.y));
        match call {
            Set => {
                let (g, b) = if n >= 3 { (a[1], a[2]) } else { (get(0, 0.0), get(0, 0.0)) };
                for (slot, value) in [v.r, v.g, v.b, v.a].into_iter().zip([get(0, 0.0), g, b, get(3, 1.0)]) {
                    vars.put(slot, value);
                }
                for (i, slot) in [(4, v.mode), (5, v.dest), (6, v.a2)] {
                    if n > i {
                        vars.put(slot, a[i]);
                    }
                }
                0.0
            }
            Rect => {
                self.draw(vars, Op::Rect { x: a[0], y: a[1], w: a[2], h: a[3], filled: get(4, 1.0) != 0.0 });
                0.0
            }
            RectTo => {
                let (x0, x1) = (gx.min(a[0]), gx.max(a[0]));
                let (y0, y1) = (gy.min(a[1]), gy.max(a[1]));
                self.draw(vars, Op::Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0, filled: true });
                vars.put(v.x, a[0]);
                vars.put(v.y, a[1]);
                a[0]
            }
            Line => {
                self.draw(vars, Op::Line { a: [a[0], a[1]], b: [a[2], a[3]], aa: get(4, 1.0) != 0.0 });
                0.0
            }
            LineTo => {
                self.draw(vars, Op::Line { a: [gx, gy], b: [a[0], a[1]], aa: get(2, 1.0) != 0.0 });
                vars.put(v.x, a[0]);
                vars.put(v.y, a[1]);
                a[0]
            }
            Circle => {
                let (filled, aa) = (get(3, 0.0) != 0.0, get(4, 1.0) != 0.0);
                self.draw(vars, Op::Circle { x: a[0], y: a[1], r: a[2], filled, aa });
                0.0
            }
            Arc => {
                self.draw(vars, Op::Arc { x: a[0], y: a[1], r: a[2], from: a[3], to: a[4], aa: get(5, 1.0) != 0.0 });
                0.0
            }
            RoundRect => {
                self.draw(vars, Op::RoundRect { x: a[0], y: a[1], w: a[2], h: a[3], r: a[4], aa: get(5, 1.0) != 0.0 });
                0.0
            }
            Triangle => {
                let points: Vec<[f64; 2]> = (0..n / 2).map(|i| [a[2 * i], a[2 * i + 1]]).collect();
                if points.len() >= 3 {
                    self.draw(vars, Op::Polygon(points));
                }
                0.0
            }
            GradRect => {
                let part = |from: usize| std::array::from_fn(|c| get(from + c, 0.0) as f32);
                let (start, dx, dy) = (part(4), part(8), part(12));
                self.draw(vars, Op::Gradient { x: a[0], y: a[1], w: a[2], h: a[3], start, dx, dy });
                0.0
            }
            DrawStr => {
                let boxed = (n >= 4).then(|| (a[1] as i64 as u32, a[2], a[3]));
                self.draw_text(vars, &reads.text, boxed);
                0.0
            }
            DrawNumber => {
                self.draw_text(vars, &format!("{:.*}", a[1].clamp(0.0, 16.0) as usize, a[0]), None);
                0.0
            }
            DrawChar => {
                let c = char::from_u32(a[0].max(0.0) as u32).unwrap_or(' ');
                self.draw_text(vars, &c.to_string(), None);
                a[0]
            }
            Printf => {
                self.draw_text(vars, &reads.text, None);
                reads.text.len() as f64
            }
            MeasureStr | MeasureChar => {
                let text = match call {
                    MeasureStr => reads.text.clone(),
                    _ => char::from_u32(a[0].max(0.0) as u32).unwrap_or(' ').to_string(),
                };
                // REAPER leaves the outputs as they were for an empty string.
                if text.is_empty() {
                    return 0.0;
                }
                let (w, h) = self.text_size(&text);
                out.args.extend([(1, w), (2, h)]);
                w
            }
            SetFont => {
                // Font 0 is the default font and cannot be changed.
                let idx = (get(0, 0.0).max(0.0) as usize).min(16);
                if idx > 0 && n >= 2 {
                    let font = &mut self.fonts[idx];
                    font.face = reads.text.clone();
                    font.bitmap = false;
                    if n >= 3 && a[2] > 0.0 {
                        font.size = a[2] as f32;
                    }
                    font.flags = if n >= 4 { a[3].max(0.0) as u32 } else { 0 };
                }
                self.font = idx;
                vars.put(v.texth, self.fonts[idx].size.round() as f64);
                1.0
            }
            GetFont => {
                if n >= 1 {
                    out.string = Some((a[0], self.current_font().face.clone()));
                }
                self.font as f64
            }
            SetPixel => {
                let mut paint = self.paint(vars);
                paint.rgb = [a[0], a[1], a[2]].map(channel_byte);
                self.push(Cmd { dest: self.dest(vars), paint, clip: None, op: Op::Pixel { x: gx, y: gy } });
                0.0
            }
            GetPixel => {
                let dest = self.dest(vars);
                let p = self.draw_now().pixel(dest, gx.floor() as i64, gy.floor() as i64).unwrap_or([0; 4]);
                out.args.extend((0..n.min(3)).map(|i| (i, p[i] as f64 / 255.0)));
                p[0] as f64 / 255.0
            }
            LoadImg => {
                let id = a[0].round() as i64;
                if (0..1024).contains(&id) { self.load_image(&reads.dirs, id, &reads.text) } else { -1.0 }
            }
            SetImgDim => {
                let id = a[0].round() as i64;
                if !(0..1024).contains(&id) {
                    return 0.0;
                }
                let dim = |v: f64| (v.max(0.0) as usize).min(raster::MAX_DIM);
                let (w, h) = (dim(a[1]), dim(a[2]));
                self.sizes.insert(id, (w, h));
                self.push(Cmd { dest: id, paint: Paint::default(), clip: None, op: Op::SetImgDim(w, h) });
                1.0
            }
            GetImgDim => {
                let (w, h) = self.surface_size(vars, a[0].round() as i64);
                out.args.extend([(1, w), (2, h)]);
                w
            }
            Blit => {
                let src = get(0, 0.0).round() as i64;
                let (sw, sh) = self.surface_size(vars, src);
                let scale = get(1, 1.0);
                let (fx, fy, fw, fh) = (get(3, 0.0), get(4, 0.0), get(5, sw), get(6, sh));
                let (tx, ty) = (get(7, gx), get(8, gy));
                let (tw, th) = (get(9, fw * scale), get(10, fh * scale));
                let op = Op::Blit {
                    src: src.max(-1),
                    from: [fx, fy, fw, fh],
                    to: [tx, ty, tw, th],
                    angle: get(2, 0.0),
                    pivot: [get(11, 0.0), get(12, 0.0)],
                };
                self.draw(vars, op);
                src as f64
            }
            BlitExt => {
                let p = &reads.table;
                let op = Op::Blit {
                    src: (a[0].round() as i64).max(-1),
                    from: [p[0], p[1], p[2], p[3]],
                    to: [p[4], p[5], p[6], p[7]],
                    angle: a[2],
                    pivot: [p[8], p[9]],
                };
                self.draw(vars, op);
                a[0]
            }
            DeltaBlit => {
                let src = get(0, 0.0).round() as i64;
                let d: [f64; 14] = std::array::from_fn(|i| get(i + 1, 0.0));
                let op = Op::DeltaBlit {
                    src: src.max(-1),
                    from: [d[0], d[1], d[2], d[3]],
                    to: [d[4], d[5], d[6], d[7]].map(|c| c as i64),
                    deltas: [d[8], d[9], d[10], d[11], d[12], d[13]],
                    clip_source: get(15, 1.0) != 0.0,
                };
                self.draw(vars, op);
                src as f64
            }
            TransformBlit => {
                let src = a[0].round() as i64;
                let (dw, dh) = (a[5].clamp(2.0, MAX_DIVS as f64) as usize, a[6].clamp(2.0, MAX_DIVS as f64) as usize);
                let op = Op::TransformBlit {
                    src: src.max(-1),
                    to: [a[1], a[2], a[3], a[4]].map(|c| c as i64),
                    divs: (dw, dh),
                    table: reads.table.clone(),
                };
                self.draw(vars, op);
                src as f64
            }
            GetChar if self.window.as_ref().is_some_and(|w| !w.open) => -1.0,
            GetChar => match get(0, 0.0) as i64 {
                65536 => (1 | 4 | if self.mouse_in { 2 | 8 } else { 0 }) as f64,
                0 => self.chars.pop_front().map_or(0.0, |c| c as f64),
                _ => 0.0,
            },
            BlurTo => {
                self.draw(vars, Op::Blur([gx, gy, a[0], a[1]]));
                vars.put(v.x, a[0]);
                vars.put(v.y, a[1]);
                a[0]
            }
            MulAddRect => {
                let mul = [get(4, 0.0), get(5, 0.0), get(6, 0.0), get(7, 1.0)].map(|v| v as f32);
                let add = [get(8, 0.0), get(9, 0.0), get(10, 0.0), get(11, 0.0)].map(|v| v as f32);
                self.draw(vars, Op::MulAdd { rect: [a[0], a[1], a[2], a[3]], mul, add });
                0.0
            }
            DropFile => {
                let idx = get(0, 0.0);
                if idx < 0.0 {
                    self.dropped.clear();
                    return 0.0;
                }
                match self.dropped.get(idx as usize) {
                    Some(name) => {
                        if n >= 2 {
                            out.string = Some((a[1], name.clone()));
                        }
                        1.0
                    }
                    None => 0.0,
                }
            }
            ShowMenu => 0.0,
            SysCol => SYSCOL,
            Init => {
                let (w, h) = (get(1, 0.0).max(0.0) as u32, get(2, 0.0).max(0.0) as u32);
                let window = self.window.get_or_insert(Window { title: String::new(), w: 640, h: 400, open: true });
                // An empty title on an open window only resizes it.
                if !reads.text.is_empty() || !window.open {
                    window.title = reads.text.clone();
                }
                if w > 0 && h > 0 {
                    (window.w, window.h) = (w, h);
                }
                window.open = true;
                let (w, h) = (window.w as usize, window.h as usize);
                vars.put(v.w, w as f64);
                vars.put(v.h, h as f64);
                self.push(Cmd { dest: -1, paint: Paint::default(), clip: None, op: Op::Resize(w, h) });
                1.0
            }
            Quit => {
                if let Some(w) = &mut self.window {
                    w.open = false;
                }
                0.0
            }
            Update | Dock | Nothing => 0.0,
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
        m.vars.put(clear, 0x204000 as f64);
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
    fn showmenu_waits_inside_the_run_for_the_choice() {
        let (m, code) = machine(
            "runs += 1; gfx_x = 5; gfx_y = 6; gfx_set(1); gfx_rect(0, 0, 4, 4); \
             mouse_cap == 2 ? (choice = gfx_showmenu(\"a|>sub|b|<c\"); after = 1;);",
        );
        let shared = m.share();
        let mut gfx_view = View::spawn(m.share(), vec![code], Vec::new());
        gfx_view.run(Input { mouse_cap: 2, ..view() });
        let next = |view: &mut View| {
            let started = std::time::Instant::now();
            loop {
                if let Some(e) = view.poll() {
                    return e;
                }
                assert!(started.elapsed().as_secs() < 10, "the view answers");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        };
        let Event::Menu(menu) = next(&mut gfx_view) else { panic!("a menu is asked for") };
        assert_eq!((menu.x, menu.y, menu.items.len()), (5.0, 6.0, 2));
        assert_eq!((shared.vars.get("runs"), shared.vars.get("after")), (Some(1.0), Some(0.0)), "the run waits");
        assert!(gfx_view.busy());
        gfx_view.choose(2);
        let Event::Frame(out) = next(&mut gfx_view) else { panic!("the run ends") };
        assert_eq!(
            [shared.vars.get("choice"), shared.vars.get("after"), shared.vars.get("runs")],
            [Some(2.0), Some(1.0), Some(1.0)]
        );
        assert_eq!(out.render().map(|s| rgb(&s, 1, 1)), Some([255; 3]), "with what was drawn before the menu");
        assert!(!gfx_view.busy());
    }

    #[test]
    fn showmenu_without_a_view_gives_0() {
        let (m, _) = frame("x = gfx_showmenu(\"a|b\") + 5;");
        assert_eq!(m.vars.get("x"), Some(5.0));
    }

    /// `gfx_x`/`gfx_y` after `gfx_drawstr("abcdef", flags, right, bottom)` and the ink's
    /// bounds, measured in REAPER with font 0 (48 x 8): `(start, flags, right, bottom, after,
    /// ink left, top, right, bottom)`.
    type Boxed = ((f64, f64), u32, f64, f64, (f64, f64), [i64; 4]);
    const BOXED: [Boxed; 11] = [
        ((10.0, 20.0), 0, 100.0, 50.0, (48.0, 0.0), [10, 20, 56, 26]),
        ((10.0, 20.0), 1, 100.0, 50.0, (69.0, 0.0), [31, 20, 77, 26]),
        ((10.0, 20.0), 5, 100.0, 50.0, (69.0, 11.0), [31, 31, 77, 37]),
        ((10.0, 20.0), 10, 100.0, 50.0, (90.0, 22.0), [52, 42, 98, 48]),
        ((10.0, 20.0), 257, 100.0, 50.0, (79.0, 20.0), [31, 20, 77, 26]),
        ((10.7, 20.7), 5, 100.0, 80.0, (69.0, 26.0), [31, 46, 77, 52]),
        ((-20.0, -6.0), 5, 100.0, 80.0, (84.0, 39.0), [36, 39, 82, 45]),
        ((-20.0, -6.0), 0, 100.0, 50.0, (48.0, 0.0), [0, 0, 46, 6]),
        ((10.0, 20.0), 1, 99.0, 50.0, (68.0, 0.0), [30, 20, 76, 26]),
        ((10.0, 20.0), 4, 100.0, 23.0, (48.0, -3.0), [10, 20, 54, 22]),
        ((10.0, 20.0), 1, 47.0, 50.0, (42.0, 0.0), [-1; 4]),
    ];

    #[test]
    fn font_0_in_a_box_draws_and_leaves_gfx_x_as_reaper_does() {
        for ((x, y), flags, r, b, after, ink) in BOXED {
            let (mut m, code) = machine(&format!(
                "gfx_setimgdim(0, 120, 100); gfx_dest = 0; gfx_set(1); gfx_x = {x}; gfx_y = {y}; \
                 gfx_drawstr(\"abcdef\", {flags}, {r}, {b}); ax = gfx_x; ay = gfx_y; gfx_dest = -1; \
                 gfx_blit(0, 1, 0, 0, 0, 120, 100, 0, 0, 120, 100);"
            ));
            let s = run(&mut m, &code, &Input { w: 120.0, h: 100.0, ..Default::default() }).1.unwrap();
            let case = format!("from ({x}, {y}) with flags {flags} to ({r}, {b})");
            assert_eq!((m.vars.get("ax").unwrap(), m.vars.get("ay").unwrap()), after, "{case}");
            let lit: Vec<(i64, i64)> = (0..100)
                .flat_map(|y| (0..120).map(move |x| (x, y)))
                .filter(|&(x, y)| rgb(&s, x, y) != [0; 3])
                .collect();
            let bounds = [
                lit.iter().map(|p| p.0).min().unwrap(),
                lit.iter().map(|p| p.1).min().unwrap(),
                lit.iter().map(|p| p.0).max().unwrap(),
                lit.iter().map(|p| p.1).max().unwrap(),
            ];
            // Scythe's font 0 glyphs are its own, so their ink may start or end a column apart, and
            // where the box cuts a glyph the ink is not compared.
            assert!(
                ink[0] < 0 || bounds.iter().zip(ink).all(|(a, b)| a.abs_diff(b) <= 1),
                "{case}: ink {bounds:?}, REAPER's {ink:?}"
            );
        }
    }

    #[test]
    fn font_0_lines_align_as_a_block_and_an_empty_box_zeroes_gfx_x() {
        let (m, _) = frame(
            "gfx_x = 10; gfx_y = 20; gfx_drawstr(\"abcdef\\nab\", 1, 100, 50); a = gfx_x; b = gfx_y; \
             gfx_x = 10; gfx_y = 20.5; gfx_drawstr(\"abcdef\", 0, 100, 20); c = gfx_x; d = gfx_y; \
             gfx_x = 10.7; gfx_y = 20.7; gfx_drawstr(\"ab\\ncdef\"); e = gfx_x; f = gfx_y;",
        );
        let var = |n: &str| m.vars.get(n).unwrap();
        assert_eq!([var("a"), var("b")], [37.0, 8.0], "the second line starts where the first does");
        assert_eq!([var("c"), var("d")], [0.0, 20.5]);
        assert_eq!([var("e"), var("f")], [42.0, 28.0], "measured from the whole pixel");
    }

    #[test]
    fn other_fonts_leave_gfx_x_after_the_last_line_whatever_the_box() {
        let (m, _) = frame(
            "gfx_setfont(1, \"Sans\", 20); gfx_measurestr(\"cdef\", w, h); \
             gfx_x = 10.7; gfx_y = 20; gfx_drawstr(\"ab\\ncdef\", 10, 100, 80); a = gfx_x; b = gfx_y; \
             gfx_x = 280; gfx_y = 190; gfx_drawstr(\"ab\\ncdef\", 0, 100, 50); c = gfx_x; d = gfx_y;",
        );
        let var = |n: &str| m.vars.get(n).unwrap();
        let w = var("w");
        assert_eq!([var("a"), var("b")], [10.0 + w, 40.0]);
        assert_eq!([var("c"), var("d")], [280.0 + w, 190.0], "an empty box leaves gfx_y");
    }

    /// An 8 x 8 image whose pixel `(x, y)` holds about `(30x, 30y, 0)`, with the values REAPER
    /// gave the probes (whose `xx * 30 / 255` it reads as `xx * (30 / 255)`), and what one call made of it in a 14 x 14 view.
    fn coords(call: &str) -> Bitmap {
        frame(&format!(
            "gfx_setimgdim(1, 8, 8); gfx_dest = 1; yy = 0; loop(8, xx = 0; loop(8, gfx_x = xx; gfx_y = yy; \
             gfx_setpixel(xx * (30 / 255), yy * (30 / 255), 0); xx += 1); yy += 1); \
             gfx_dest = -1; gfx_set(0); gfx_rect(0, 0, 14, 14); gfx_set(1); {call}"
        ))
        .1
    }

    fn row(s: &Bitmap, y: i64) -> Vec<[u8; 3]> {
        (0..14).map(|x| rgb(s, x, y)).collect()
    }

    #[test]
    fn deltablit_steps_as_lice_does() {
        let s = coords("gfx_mode = 0; gfx_deltablit(1, 0, 0, 8, 8, 0, 0, 14, 14, 0.3, 0, 0, 0.3, 0, 0);");
        let reds: Vec<u8> = row(&s, 0).iter().map(|p| p[0]).collect();
        assert_eq!(reds, [0, 8, 17, 26, 35, 44, 53, 62, 71, 80, 88, 98, 107, 116], "16.16 steps, 16-bit weights");
        assert_eq!(rgb(&s, 1, 1), [8, 8, 0]);
        let s = coords("gfx_mode = 0; gfx_deltablit(1, 0.5, 0.25, 8, 8, 2, 3, 10, 10, 1, 0, 0, 1, 0, 0);");
        assert_eq!((rgb(&s, 2, 3), rgb(&s, 9, 3), rgb(&s, 2, 10)), ([15, 7, 0], [210, 7, 0], [15, 210, 0]));
        assert_eq!(rgb(&s, 10, 3), [0; 3], "ten pixels wide");
        let s = coords("gfx_mode = 4; gfx_deltablit(1, 2, 2, 3, 3, 0, 0, 14, 14, 1, 0, 0, 1, 0, 0);");
        assert_eq!((rgb(&s, 0, 0), rgb(&s, 1, 0), rgb(&s, 0, 1)), ([120, 120, 0], [0; 3], [0; 3]), "read from (4, 4)");
        let s = coords("gfx_mode = 4; gfx_deltablit(1, 0, 0, 4, 4, 0, 0, 14, 14, 1, 0, 0, 1, 0, 0, 0);");
        assert_eq!(rgb(&s, 7, 7), [210, 210, 0], "the whole image without the source rectangle");
        let s = coords("gfx_mode = 4; gfx_deltablit(1, 0, 0, 8, 8, 0, 0, 14, 14, 0.5, 0, 0, 0.5, 0.1, 0.05);");
        assert_eq!((rgb(&s, 11, 1), rgb(&s, 12, 1), rgb(&s, 11, 2)), ([179, 30, 0], [210, 30, 0], [210, 60, 0]));
    }

    #[test]
    fn transformblit_runs_a_delta_blit_per_cell() {
        let s = coords(
            "300[0] = 0; 300[1] = 0; 300[2] = 8; 300[3] = 0; 300[4] = 0; 300[5] = 8; 300[6] = 8; 300[7] = 8; \
             gfx_mode = 4; gfx_transformblit(1, 0, 0, 14, 14, 2, 2, 300);",
        );
        let reds: Vec<u8> = row(&s, 0).iter().map(|p| p[0]).collect();
        assert_eq!(reds, [0, 0, 30, 30, 60, 60, 89, 89, 120, 150, 150, 179, 179, 210]);
        let s = coords(
            "300[0] = 1; 300[1] = 1; 300[2] = 5; 300[3] = 2; 300[4] = 2; 300[5] = 6; 300[6] = 7; 300[7] = 7; \
             gfx_mode = 0; gfx_transformblit(1, 0, 0, 14, 14, 2, 2, 300);",
        );
        let reds: Vec<u8> = row(&s, 0).iter().map(|p| p[0]).collect();
        assert_eq!(reds, [30, 38, 47, 55, 64, 72, 80, 88, 97, 106, 115, 124, 132, 141]);
        assert_eq!(rgb(&s, 13, 13), [194, 196, 0]);
    }

    #[test]
    fn retina_scripts_draw_at_the_screens_pixels() {
        let (mut m, code) = machine("w = gfx_w; r = gfx_ext_retina; mx = mouse_x;");
        let input = Input { scale: 2.0, mouse_x: 10.0, ..view() };
        let retina = m.vars.slot("gfx_ext_retina");
        m.vars.put(retina, 1.0);
        let (out, screen) = run(&mut m, &code, &input);
        assert_eq!((out.scale, screen.map(|s| s.w)), (2.0, Some(128)));
        assert_eq!([m.vars.get("w"), m.vars.get("r"), m.vars.get("mx")], [Some(128.0), Some(2.0), Some(20.0)]);
        let (mut m, code) = machine("w = gfx_w; r = gfx_ext_retina;");
        let (out, _) = run(&mut m, &code, &input);
        assert_eq!((out.scale, m.vars.get("w"), m.vars.get("r")), (1.0, Some(64.0), Some(0.0)));
    }

    #[test]
    fn dropped_files_are_listed_until_cleared() {
        let (mut m, code) = machine(
            "n = 0; while (gfx_getdropfile(n, #f)) (n += 1); gfx_getdropfile(0, #first); \
             mouse_cap == 1 ? gfx_getdropfile(-1);",
        );
        let input = Input { dropped: vec!["/a.wav".into(), "/b c.wav".into()], ..view() };
        run(&mut m, &code, &input);
        let first = m.vars.named["#first"] as f64;
        assert_eq!((m.string(first), m.vars.get("n")), ("/a.wav".to_owned(), Some(2.0)));
        run(&mut m, &code, &Input { mouse_cap: 1, ..view() });
        run(&mut m, &code, &view());
        assert_eq!(m.vars.get("n"), Some(0.0), "gfx_getdropfile(-1) clears the list");
    }

    #[test]
    fn outline_shadow_and_inverted_fonts() {
        let draw = |flags: &str| {
            frame(&format!(
                "gfx_set(0.5); gfx_rect(0, 0, 64, 48); gfx_setfont(1, \"Sans\", 16, {flags}); gfx_set(1, 0, 0); \
                 gfx_x = 4; gfx_y = 4; gfx_drawstr(\"Ab\"); gfx_measurestr(\"Ab\", w, h);"
            ))
        };
        let (m, plain) = draw("0");
        let (w, h) = (m.vars.get("w").unwrap() as i64, m.vars.get("h").unwrap() as i64);
        let colours = |s: &Bitmap| {
            let mut seen: Vec<[u8; 3]> =
                (0..48).flat_map(|y| (0..64).map(move |x| (x, y))).map(|(x, y)| rgb(s, x, y)).collect();
            seen.sort();
            seen.dedup();
            seen
        };
        if colours(&plain).len() < 3 {
            // No font on this system.
            return;
        }
        let (_, outline) = draw("'o'");
        assert_eq!(colours(&outline), [[127, 127, 127], [255, 0, 0], [255, 255, 255]], "all or nothing, white around");
        let (_, shadow) = draw("'s'");
        let white: Vec<(i64, i64)> = (0..48)
            .flat_map(|y| (0..64).map(move |x| (x, y)))
            .filter(|&(x, y)| rgb(&shadow, x, y) == [255; 3])
            .collect();
        assert!(white.iter().all(|&(x, y)| [(x - 1, y), (x, y - 1), (x - 1, y - 1)].iter().any(|&(a, b)| rgb(
            &shadow, a, b
        ) == [
            255, 0, 0
        ])));
        let (_, mono_outline) = draw("'om'");
        assert_eq!(colours(&mono_outline), [[127, 127, 127], [255, 0, 0]], "one colour with 'm'");
        let (_, inverted) = draw("'v'");
        assert_eq!(
            (rgb(&inverted, 4, 4), rgb(&inverted, 4 + w - 1, 4 + h - 1)),
            ([255, 0, 0], [255, 0, 0]),
            "the box filled"
        );
        assert_eq!((rgb(&inverted, 4 + w, 4), rgb(&inverted, 4, 4 + h)), ([127; 3], [127; 3]), "and no more");
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
