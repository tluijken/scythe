//! JSFX, REAPER's script effects: the effect file (header, sliders, imports and code sections)
//! compiled with the EEL2 interpreter and run as an [`Effect`]. Effects are read from REAPER's
//! own `Effects` folders, as REAPER resolves the `<JS name` of a slot.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::clap_host::ParamInfo;
use crate::eel::{Code, Machine, Vars, compile};
use crate::fx::{Effect, ProcessContext};

#[derive(Debug, Clone, PartialEq)]
pub struct Slider {
    /// 1-based, as in `slider1`.
    pub index: usize,
    pub default: f64,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub name: String,
    /// Enum choices (`{a,b,c}`), shown instead of the value.
    pub choices: Vec<String>,
    pub hidden: bool,
    /// The variable the slider sets: `sliderN`, or a name given as `sliderN:name=…`.
    pub var: String,
    /// A file slider's folder under `Data` and its default file, until `Script::load` lists
    /// the folder into `files` (and their names into `choices`).
    pub file_dir: Option<(String, String)>,
    pub files: Vec<PathBuf>,
}

/// An effect file with its imports, split into sections.
#[derive(Debug, Clone, Default)]
pub struct Script {
    pub desc: String,
    pub sliders: Vec<Slider>,
    pub sections: HashMap<String, String>,
    /// Sections of imported files, run before the effect's own.
    pub imported: Vec<HashMap<String, String>>,
    /// Declared `in_pin`/`out_pin` lines (`in_pin:none` declares none); `None` when the
    /// effect declares nothing and gets a stereo pair.
    pub in_pins: Option<usize>,
    pub out_pins: Option<usize>,
    /// `Data` folders beside the effect folders, for file sliders and `file_open`.
    pub data_dirs: Vec<PathBuf>,
}

/// The files of `dir` in the data folders, sorted by name as REAPER lists them.
fn data_files(data_dirs: &[PathBuf], dir: &str) -> Vec<PathBuf> {
    let Some(folder) = data_dirs.iter().map(|d| d.join(dir)).find(|p| p.is_dir()) else { return Vec::new() };
    let mut files: Vec<PathBuf> = std::fs::read_dir(folder)
        .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect())
        .unwrap_or_default();
    files.sort_by_key(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase()));
    files
}

fn parse_slider(index: usize, spec: &str) -> Option<Slider> {
    // `default<min,max,step{a,b}>name`, optionally `var=default<…`; file sliders are
    // `/folder:default file:name`.
    let (var, spec) = match spec.split_once('=') {
        Some((v, rest)) if !v.contains('<') && v.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') => {
            (v.trim().to_ascii_lowercase(), rest)
        }
        _ => (format!("slider{index}"), spec),
    };
    if spec.starts_with('/') {
        let mut parts = spec.splitn(3, ':');
        let dir = parts.next()?.trim_start_matches('/').trim().to_owned();
        let default = parts.next()?.trim().to_owned();
        let name = parts.next().unwrap_or("").trim().to_owned();
        return Some(Slider {
            index,
            default: 0.0,
            min: 0.0,
            max: 0.0,
            step: 1.0,
            hidden: name.starts_with('-'),
            name: name.trim_start_matches('-').to_owned(),
            choices: Vec::new(),
            var,
            file_dir: Some((dir, default)),
            files: Vec::new(),
        });
    }
    let (default, rest) = match spec.find('<') {
        Some(at) => (spec[..at].trim(), &spec[at..]),
        None => {
            let (d, n) = spec.split_once(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))?;
            (d, n)
        }
    };
    let default: f64 = default.parse().ok()?;
    let (range, name) = match rest.strip_prefix('<').and_then(|r| r.split_once('>')) {
        Some((range, name)) => (range, name),
        None => ("", rest),
    };
    let (nums, choices) = match range.split_once('{') {
        Some((n, c)) => (n, c.trim_end_matches('}').split(',').map(|s| s.trim().to_owned()).collect()),
        None => (range, Vec::new()),
    };
    let mut it = nums.split(',').map(|v| v.trim().parse::<f64>().ok());
    let (min, max, step) =
        (it.next().flatten().unwrap_or(0.0), it.next().flatten().unwrap_or(1.0), it.next().flatten().unwrap_or(0.0));
    let name = name.trim().to_owned();
    Some(Slider {
        index,
        default,
        min,
        max,
        step,
        hidden: name.starts_with('-'),
        name: name.trim_start_matches('-').to_owned(),
        choices,
        var,
        file_dir: None,
        files: Vec::new(),
    })
}

fn split_sections(text: &str) -> (Vec<&str>, HashMap<String, String>) {
    let mut header = Vec::new();
    let mut sections: HashMap<String, String> = HashMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if let Some(name) = trimmed.strip_prefix('@') {
            let name = name.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
            sections.entry(name.clone()).or_default();
            current = Some(name);
            continue;
        }
        match &current {
            Some(name) => {
                let s = sections.get_mut(name).unwrap();
                s.push_str(line);
                s.push('\n');
            }
            None => header.push(line),
        }
    }
    (header, sections)
}

impl Script {
    /// Reads an effect and the files it imports (looked up next to it, then in `roots`).
    pub fn load(path: &Path, roots: &[PathBuf]) -> Result<Self, String> {
        let text = read_text(path)?;
        let (header, sections) = split_sections(&text);
        let mut script = Script { sections, ..Default::default() };
        for line in header {
            let line = line.trim();
            if let Some(d) = line.strip_prefix("desc:") {
                if script.desc.is_empty() {
                    script.desc = d.trim().to_owned();
                }
            } else if let Some(rest) = line.strip_prefix("slider") {
                let Some((num, spec)) = rest.split_once(':') else { continue };
                if let Ok(index) = num.parse::<usize>()
                    && let Some(s) = parse_slider(index, spec)
                {
                    script.sliders.push(s);
                }
            } else if let Some(pin) = line.strip_prefix("in_pin:") {
                let n = script.in_pins.get_or_insert(0);
                *n += (!pin.trim().eq_ignore_ascii_case("none")) as usize;
            } else if let Some(pin) = line.strip_prefix("out_pin:") {
                let n = script.out_pins.get_or_insert(0);
                *n += (!pin.trim().eq_ignore_ascii_case("none")) as usize;
            } else if let Some(file) = line.strip_prefix("import") {
                let file = file.trim();
                let found = path
                    .parent()
                    .map(|d| d.join(file))
                    .filter(|p| p.is_file())
                    .or_else(|| roots.iter().map(|r| r.join(file)).find(|p| p.is_file()))
                    .ok_or_else(|| format!("import {file} not found"))?;
                let (_, sections) = split_sections(&read_text(&found)?);
                script.imported.push(sections);
            }
        }
        script.data_dirs = roots.iter().filter_map(|r| Some(r.parent()?.join("Data"))).filter(|d| d.is_dir()).collect();
        for slider in &mut script.sliders {
            let Some((dir, default)) = &slider.file_dir else { continue };
            slider.files = data_files(&script.data_dirs, dir);
            slider.choices =
                slider.files.iter().map(|p| p.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
            slider.default = slider.choices.iter().position(|c| c == default).unwrap_or(0) as f64;
            slider.max = slider.files.len().saturating_sub(1) as f64;
        }
        Ok(script)
    }
}

fn read_text(path: &Path) -> Result<String, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    preprocess(&String::from_utf8_lossy(&bytes)).map_err(|e| format!("{}: preprocessor: {e}", path.display()))
}

/// EEL2's preprocessor: code between `<?` and `?>` runs once when the file is read, and what
/// it prints takes its place. Variables and functions carry over from one block to the next.
fn preprocess(text: &str) -> Result<String, String> {
    if !text.contains("<?") {
        return Ok(text.to_owned());
    }
    let mut machine = Machine::new(Vars::default());
    let mut functions = HashMap::new();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<?") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find("?>").ok_or("unterminated <?")?;
        let code = compile(&after[..end], &mut machine.vars, &mut functions)?;
        machine.output.clear();
        machine.run(&code);
        out.push_str(&machine.output);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Folders effects are looked up in: `$SCYTHE_JSFX_PATH`, REAPER's user folder, then the
/// `InstallData/Effects` of an installed REAPER (found through `reaper` on the `PATH`).
pub fn search_paths() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        std::env::var_os("SCYTHE_JSFX_PATH").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(c) = config {
        dirs.push(c.join("REAPER").join("Effects"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let bin = dir.join("reaper");
            // The binary sits next to `InstallData`, or under `bin/` of a prefix with `opt/REAPER`.
            if let Ok(real) = std::fs::canonicalize(&bin)
                && let Some(dir) = real.parent()
            {
                dirs.push(dir.join("InstallData/Effects"));
                if let Some(prefix) = dir.parent() {
                    dirs.push(prefix.join("opt/REAPER/InstallData/Effects"));
                }
            }
        }
    }
    dirs.extend(["/opt/REAPER/InstallData/Effects", "/usr/share/REAPER/Effects"].map(PathBuf::from));
    dirs.retain(|d| d.is_dir());
    dirs.dedup();
    dirs
}

pub fn find(name: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    roots.iter().map(|r| r.join(name)).find(|p| p.is_file())
}

/// REAPER renders in blocks of this many frames, aligned to the project start; `@block` runs
/// once per block and MIDI offsets count from its start.
const BLOCK_FRAMES: i64 = 512;

/// `(offset, length)` of the REAPER blocks covering `n` frames from `position`.
fn blocks(position: i64, n: usize) -> impl Iterator<Item = (usize, usize)> {
    let mut at = 0;
    std::iter::from_fn(move || {
        (at < n).then(|| {
            let frame = position + at as i64;
            let len = ((BLOCK_FRAMES - frame.rem_euclid(BLOCK_FRAMES)) as usize).min(n - at);
            at += len;
            (at - len, len)
        })
    })
}

/// A running JSFX.
pub struct JsfxEffect {
    pub script: Script,
    machine: Machine,
    init: Vec<Code>,
    slider: Vec<Code>,
    block: Vec<Code>,
    sample: Vec<Code>,
    slider_vars: Vec<usize>,
    values: Vec<f64>,
    sample_rate: u32,
    srate: usize,
    samplesblock: usize,
    play_state: usize,
    tempo: usize,
    pdc_delay: usize,
    num_ch: usize,
    ext_noinit: usize,
    was_playing: bool,
    /// Whether the script reads or writes MIDI; others leave the chain's MIDI alone.
    uses_midi: bool,
    midi_in: Vec<(usize, [u8; 3])>,
    /// What the effect passed on since the chain last took it.
    midi_sent: Vec<(usize, [u8; 3])>,
    /// Whether it ran since; a bypassed effect lets the chain's MIDI go past it.
    midi_ran: bool,
}

impl JsfxEffect {
    /// Compiles `script` and runs `@init` and `@slider` with `values` (one per slider, `None`
    /// for the default).
    pub fn new(script: Script, values: &[Option<f64>], sample_rate: u32) -> Result<Self, String> {
        let mut vars = Vars::default();
        let mut functions = HashMap::new();
        let mut sections: HashMap<&str, Vec<Code>> = HashMap::new();
        let all: Vec<&HashMap<String, String>> = script.imported.iter().chain([&script.sections]).collect();
        for name in ["init", "slider", "block", "sample"] {
            for secs in &all {
                if let Some(src) = secs.get(name) {
                    let code = compile(src, &mut vars, &mut functions).map_err(|e| format!("@{name}: {e}"))?;
                    sections.entry(name).or_default().push(code);
                }
            }
        }
        let slider_vars = script.sliders.iter().map(|s| vars.slot(&s.var)).collect();
        let values = script
            .sliders
            .iter()
            .enumerate()
            .map(|(i, s)| values.get(i).copied().flatten().unwrap_or(s.default))
            .collect();
        let slot = |vars: &mut Vars, n: &str| vars.slot(n);
        let (srate, samplesblock, play_state, tempo, pdc_delay, num_ch) = (
            slot(&mut vars, "srate"),
            slot(&mut vars, "samplesblock"),
            slot(&mut vars, "play_state"),
            slot(&mut vars, "tempo"),
            slot(&mut vars, "pdc_delay"),
            slot(&mut vars, "num_ch"),
        );
        let uses_midi = all.iter().flat_map(|secs| secs.values()).any(|src| {
            let src = src.to_ascii_lowercase();
            src.contains("midirecv") || src.contains("midisend")
        });
        let mut take = |n: &str| sections.remove(n).unwrap_or_default();
        let mut fx = Self {
            init: take("init"),
            slider: take("slider"),
            block: take("block"),
            sample: take("sample"),
            machine: Machine::new(vars),
            script,
            slider_vars,
            values,
            sample_rate,
            srate,
            samplesblock,
            play_state,
            tempo,
            pdc_delay,
            num_ch,
            ext_noinit: 0,
            was_playing: false,
            uses_midi,
            midi_in: Vec::new(),
            midi_sent: Vec::new(),
            midi_ran: false,
        };
        fx.ext_noinit = fx.machine.vars.slot("ext_noinit");
        fx.machine.data_dirs = fx.script.data_dirs.clone();
        fx.machine.file_sliders = fx
            .script
            .sliders
            .iter()
            .zip(&fx.slider_vars)
            .filter(|(s, _)| s.file_dir.is_some())
            .map(|(s, &slot)| (slot, s.files.clone()))
            .collect();
        fx.initialise();
        // Effects often set `pdc_delay` in `@block`; one block up front makes their latency
        // known before the first is rendered, as REAPER has it.
        fx.machine.vars.values[fx.samplesblock] = BLOCK_FRAMES as f64;
        for code in &fx.block {
            fx.machine.run(code);
        }
        Ok(fx)
    }

    /// `@init` then `@slider`, as REAPER runs them on load and at the start of playback.
    fn initialise(&mut self) {
        let v = &mut self.machine.vars.values;
        v[self.srate] = self.sample_rate as f64;
        v[self.num_ch] = 2.0;
        v[self.tempo] = 120.0;
        for (&slot, &value) in self.slider_vars.iter().zip(&self.values) {
            v[slot] = value;
        }
        for code in &self.init {
            self.machine.run(code);
        }
        self.run_slider();
    }

    fn run_slider(&mut self) {
        for (&slot, &value) in self.slider_vars.iter().zip(&self.values) {
            self.machine.vars.values[slot] = value;
        }
        for code in &self.slider {
            self.machine.run(code);
        }
    }

    fn process_block(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        // REAPER runs `@init` again when playback starts, unless the effect sets ext_noinit.
        if ctx.playing && !self.was_playing && self.machine.vars.values[self.ext_noinit] == 0.0 {
            self.initialise();
        }
        self.was_playing = ctx.playing;
        self.receive_midi(left.len(), ctx);
        let v = &mut self.machine.vars.values;
        v[self.samplesblock] = left.len() as f64;
        v[self.play_state] = if ctx.playing { 1.0 } else { 0.0 };
        v[self.tempo] = ctx.bpm;
        for code in &self.block {
            self.machine.run(code);
        }
        if self.sample.is_empty() {
            self.pass_midi(left.len(), ctx);
            return;
        }
        let (s0, s1) = (self.machine.spl_slots[0], self.machine.spl_slots[1]);
        let (s2, s3) = (self.machine.spl_slots[2], self.machine.spl_slots[3]);
        // A sidechain arrives as channels 3/4 (`spl2`, `spl3`).
        self.machine.vars.values[self.num_ch] = if ctx.aux.is_some() { 4.0 } else { 2.0 };
        for (i, (l, r)) in left.iter_mut().zip(right.iter_mut()).enumerate() {
            self.machine.vars.values[s0] = *l as f64;
            self.machine.vars.values[s1] = *r as f64;
            if let Some((a, b)) = ctx.aux {
                self.machine.vars.values[s2] = a[i] as f64;
                self.machine.vars.values[s3] = b[i] as f64;
            }
            for code in &self.sample {
                self.machine.run(code);
            }
            *l = self.machine.vars.values[s0] as f32;
            *r = self.machine.vars.values[s1] as f32;
        }
        self.pass_midi(left.len(), ctx);
    }

    fn process_pins_block(&mut self, bufs: &mut [Vec<f32>], range: std::ops::Range<usize>, ctx: &ProcessContext) {
        let (ins, outs) = self.pins();
        if ins <= 2 && outs <= 2 {
            if let [l, r, ..] = bufs {
                self.process_block(&mut l[range.clone()], &mut r[range], ctx);
            }
            return;
        }
        if ctx.playing && !self.was_playing && self.machine.vars.values[self.ext_noinit] == 0.0 {
            self.initialise();
        }
        self.was_playing = ctx.playing;
        self.receive_midi(range.len(), ctx);
        let channels = ins.max(outs).min(bufs.len()).min(64);
        let v = &mut self.machine.vars.values;
        v[self.samplesblock] = range.len() as f64;
        v[self.play_state] = if ctx.playing { 1.0 } else { 0.0 };
        v[self.tempo] = ctx.bpm;
        v[self.num_ch] = channels as f64;
        for code in &self.block {
            self.machine.run(code);
        }
        if self.sample.is_empty() {
            self.pass_midi(range.len(), ctx);
            return;
        }
        let n = range.len();
        for i in range {
            for (c, buf) in bufs.iter().enumerate().take(channels) {
                let slot = self.machine.spl_slots[c];
                self.machine.vars.values[slot] = if c < ins { buf[i] as f64 } else { 0.0 };
            }
            for code in &self.sample {
                self.machine.run(code);
            }
            for (c, buf) in bufs.iter_mut().enumerate().take(outs.min(channels)) {
                buf[i] = self.machine.vars.values[self.machine.spl_slots[c]] as f32;
            }
        }
        self.pass_midi(n, ctx);
    }

    fn receive_midi(&mut self, n: usize, ctx: &ProcessContext) {
        if self.uses_midi {
            ctx.block_midi(n, &mut self.midi_in);
            self.machine.set_midi(self.midi_in.iter().map(|&(at, m)| (at as f64, m)));
        }
    }

    fn pass_midi(&mut self, n: usize, ctx: &ProcessContext) {
        if self.uses_midi {
            self.machine.drain_midi(n, ctx.midi_from, &mut self.midi_sent);
            self.midi_ran = true;
        }
    }

    fn visible(&self) -> impl Iterator<Item = (usize, &Slider)> {
        self.script.sliders.iter().enumerate().filter(|(_, s)| !s.hidden)
    }
}

impl Effect for JsfxEffect {
    /// A script that reads or sends MIDI takes the track's notes, as an instrument does.
    fn is_instrument(&self) -> bool {
        self.uses_midi
    }

    fn process(&mut self, left: &mut [f32], right: &mut [f32], ctx: &ProcessContext) {
        for (at, len) in blocks(ctx.position, left.len()) {
            let sub = ProcessContext {
                position: ctx.position + at as i64,
                live: if at == 0 { ctx.live } else { &[] },
                aux: ctx.aux.map(|(a, b)| (&a[at..at + len], &b[at..at + len])),
                midi_from: ctx.midi_from + at,
                ..*ctx
            };
            self.process_block(&mut left[at..at + len], &mut right[at..at + len], &sub);
        }
    }

    fn process_pins(&mut self, bufs: &mut [Vec<f32>], range: std::ops::Range<usize>, ctx: &ProcessContext) {
        for (at, len) in blocks(ctx.position, range.len()) {
            let sub = ProcessContext {
                position: ctx.position + at as i64,
                live: if at == 0 { ctx.live } else { &[] },
                midi_from: ctx.midi_from + at,
                ..*ctx
            };
            self.process_pins_block(bufs, range.start + at..range.start + at + len, &sub);
        }
    }

    fn pins(&self) -> (usize, usize) {
        (self.script.in_pins.unwrap_or(2), self.script.out_pins.unwrap_or(2))
    }

    fn take_midi(&mut self, out: &mut Vec<(usize, [u8; 3])>) -> bool {
        if !std::mem::take(&mut self.midi_ran) {
            return false;
        }
        out.append(&mut self.midi_sent);
        true
    }

    fn reset(&mut self) {
        self.initialise();
    }

    fn latency(&mut self) -> usize {
        self.machine.vars.values[self.pdc_delay].max(0.0) as usize
    }

    fn params(&mut self) -> Vec<ParamInfo> {
        self.visible()
            .map(|(i, s)| ParamInfo {
                id: i as u32,
                name: s.name.clone(),
                min: s.min,
                max: if s.max > s.min { s.max } else { s.min + 1.0 },
                default: s.default,
                log: false,
            })
            .collect()
    }

    fn param(&mut self, id: u32) -> Option<(f64, String)> {
        let s = self.script.sliders.get(id as usize)?;
        let v = *self.values.get(id as usize)?;
        let text = match s.choices.get(v.round().max(0.0) as usize) {
            Some(c) if !s.choices.is_empty() => c.clone(),
            _ => format!("{v:.2}"),
        };
        Some((v, text))
    }

    fn set_param(&mut self, id: u32, value: f64) {
        if let Some(v) = self.values.get_mut(id as usize) {
            *v = value;
            self.run_slider();
        }
    }

    /// REAPER's slider order, values spread linearly over each slider's range.
    fn set_normalized(&mut self, index: usize, value: f64) -> bool {
        let Some(s) = self.script.sliders.get(index) else { return false };
        let v = s.min + value.clamp(0.0, 1.0) * (s.max - s.min);
        if self.values[index] != v {
            self.values[index] = v;
            self.run_slider();
        }
        true
    }

    /// The slider values line REAPER stores: 64 fields, `-` where there is no slider.
    fn save_nodes(&mut self) -> Option<Vec<scythe_core::rpp::Node>> {
        let mut fields = vec!["-".to_owned(); 64];
        for (s, v) in self.script.sliders.iter().zip(&self.values) {
            if let Some(f) = s.index.checked_sub(1).and_then(|i| fields.get_mut(i)) {
                // REAPER stores a file slider as the chosen file's name.
                *f = match s.file_dir {
                    Some(_) => scythe_core::rpp::quote_always(
                        s.choices.get(v.round().max(0.0) as usize).map_or("none", String::as_str),
                    ),
                    None => scythe_core::rpp::fmt_num(*v),
                };
            }
        }
        Some(vec![scythe_core::rpp::Node::Line(fields.join(" "))])
    }
}

/// Slider values stored in a `<JS` slot, by slider number (`sliderN` is at `N - 1`), `-` for
/// unset ones; returned in the script's slider order. File sliders hold a file name, which
/// REAPER reads as the first file when it is not in the folder.
pub fn parse_values(line: &str, sliders: &[Slider]) -> Vec<Option<f64>> {
    let stored = scythe_core::rpp::tokenize(line);
    sliders
        .iter()
        .map(|s| {
            let token = stored.get(s.index.checked_sub(1)?)?;
            match s.file_dir {
                Some(_) if token == "-" => None,
                Some(_) => Some(s.choices.iter().position(|c| c == token).unwrap_or(0) as f64),
                None => token.parse().ok(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_sliders_store_file_names() {
        let mut s = parse_slider(1, "/amp_models:none:Model").unwrap();
        assert_eq!(s.file_dir, Some(("amp_models".to_owned(), "none".to_owned())));
        s.choices = vec!["a.wav".to_owned(), "b c.wav".to_owned()];
        let gain = parse_slider(2, "0<-12,12,1>Gain").unwrap();
        let sliders = [s, gain];
        assert_eq!(parse_values("\"b c.wav\" -3", &sliders), [Some(1.0), Some(-3.0)]);
        assert_eq!(parse_values("5 -", &sliders), [Some(0.0), None], "a number is no file name");
    }

    #[test]
    fn parses_slider_declarations() {
        let s = parse_slider(2, "5<0,9,1{Blown,Four,Eight}>Ratio").unwrap();
        assert_eq!(
            (s.default, s.min, s.max, s.step, s.choices.len(), s.var.as_str()),
            (5.0, 0.0, 9.0, 1.0, 3, "slider2")
        );
        let s = parse_slider(1, "gain_db=0<-24,24,0.1>-Gain (dB)").unwrap();
        assert_eq!((s.var.as_str(), s.hidden, s.name.as_str()), ("gain_db", true, "Gain (dB)"));
    }

    #[test]
    fn runs_a_script_per_sample() {
        let text =
            "desc:test\nslider1:-6<-24,0,0.1>Gain\n@slider\ng = 10^(slider1/20);\n@sample\nspl0 *= g;\nspl1 = -spl1;\n";
        let (header, sections) = split_sections(text);
        assert_eq!(header.len(), 2);
        let script =
            Script { sliders: vec![parse_slider(1, "-6<-24,0,0.1>Gain").unwrap()], sections, ..Default::default() };
        let mut fx = JsfxEffect::new(script, &[Some(-20.0)], 48000).unwrap();
        let (mut l, mut r) = (vec![1.0f32; 4], vec![0.5f32; 4]);
        let ctx = ProcessContext {
            midi: None,
            midi_from: 0,
            sample_rate: 48000,
            position: 0,
            playing: true,
            notes: None,
            live: &[],
            live_channel: None,
            aux: None,
            bpm: 120.0,
        };
        fx.process(&mut l, &mut r, &ctx);
        assert!((l[0] - 0.1).abs() < 1e-6 && r[3] == -0.5);
    }

    #[test]
    fn preprocessor_writes_code() {
        let text = preprocess("a = 1;\n<? function f(n) ( printf(\"x%d = %d;\", n, n * 2) ); ?>\n<? loop(i = 0; 2, f(i); i += 1) ?>\nb = 2;").unwrap();
        assert_eq!(text, "a = 1;\n\nx0 = 0;x1 = 2;\nb = 2;");
    }
}
