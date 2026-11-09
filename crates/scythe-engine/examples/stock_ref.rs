//! Null tests of the dynamics emulations (ReaComp, ReaGate, ReaLimit, ReaXcomp) against
//! REAPER: writes test signals, lets REAPER add each effect to a project, set parameters one
//! case at a time (`TrackFX_SetParam`, or a state chunk), save the project and render it; then
//! bounces every saved project with scythe and prints the largest difference per case and per
//! plugin.
//!
//! cargo run --release -p scythe-engine --example stock_ref -- <work dir> <reaper> [name filter]
//!
//! REAPER runs with `-cfgfile <work dir>/reaper-cfg/reaper.ini`, which must be a configuration
//! without an audio device. Cases already rendered are reused; delete the work dir to redo them.

use std::f64::consts::PI;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use scythe_core::render_settings::RenderSettings;
use scythe_engine::MediaPool;
use scythe_engine::bounce::{BitDepth, Encoding, Finish, bounce_finished};

const SR: usize = 48000;

struct Rng(u64);

impl Rng {
    fn uniform(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
    }
}

fn write_wav(path: &Path, frames: &[[f64; 2]]) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SR as u32,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for f in frames {
        w.write_sample(f[0] as f32).unwrap();
        w.write_sample(f[1] as f32).unwrap();
    }
    w.finalize().unwrap();
}

/// A second of tones whose channels step in level at different times.
fn tones() -> Vec<[f64; 2]> {
    (0..SR)
        .map(|i| {
            let t = i as f64 / SR as f64;
            let l = if (0.25..0.6).contains(&t) { 0.5 } else { 0.05 };
            let r = if (0.4..0.8).contains(&t) { 0.3 } else { 0.03 };
            [l * (2.0 * PI * 440.0 * t).sin(), r * (2.0 * PI * 660.0 * t + 0.3).sin()]
        })
        .collect()
}

fn noise(rng: &mut Rng) -> Vec<[f64; 2]> {
    (0..SR)
        .map(|i| {
            let t = i as f64 / SR as f64;
            let a = if t < 0.2 {
                0.02
            } else if t < 0.5 {
                0.4
            } else if t < 0.7 {
                0.1
            } else {
                0.01
            };
            [a * rng.uniform(), a * rng.uniform()]
        })
        .collect()
}

/// Two seconds of kick, snare and hat hits in eighths, the channels weighted differently.
fn drums(rng: &mut Rng) -> Vec<[f64; 2]> {
    let mut x = vec![[0.0; 2]; 2 * SR];
    for k in 0..16 {
        let start = k * SR / 8;
        let vel = [1.0, 0.3, 0.6, 0.3, 0.9, 0.25, 0.5, 0.4][k % 8];
        let len = (2 * SR - start).min(SR / 3);
        let mut phase = 0.0;
        for n in 0..len {
            let t = n as f64 / SR as f64;
            let (l, r) = match k % 4 {
                0 => {
                    phase += 2.0 * PI * (50.0 + 80.0 * (-t * 40.0).exp()) / SR as f64;
                    let v = vel * 0.9 * phase.sin() * (-t * 12.0).exp();
                    (v, 0.2 * v)
                }
                2 => {
                    let v = vel
                        * (rng.uniform() * (-t * 25.0).exp() * 0.5
                            + 0.4 * (2.0 * PI * 190.0 * t).sin() * (-t * 30.0).exp());
                    (v, 0.5 * v)
                }
                _ if n < SR / 20 => {
                    let v = vel * rng.uniform() * (-t * 90.0).exp() * 0.25;
                    (0.3 * v, v)
                }
                _ => (0.0, 0.0),
            };
            x[start + n][0] += l;
            x[start + n][1] += r;
        }
    }
    x
}

fn dc_steps() -> Vec<[f64; 2]> {
    (0..SR).map(|i| if (SR * 3 / 10..SR * 7 / 10).contains(&i) { [0.5; 2] } else { [0.1; 2] }).collect()
}

/// Every test input, by name, with its length in seconds.
fn signals() -> Vec<(&'static str, Vec<[f64; 2]>)> {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    let main = [tones(), noise(&mut rng), drums(&mut rng), dc_steps()].concat();
    let key = [drums(&mut rng), tones(), noise(&mut rng), dc_steps()].concat();
    let ramp: Vec<[f64; 2]> =
        (0..5 * SR).map(|i| [10f64.powf((-48.0 + 48.0 * i as f64 / (5 * SR) as f64) / 20.0); 2]).collect();
    let mut lim = vec![[0.1; 2]; 5 * SR];
    lim[SR * 3 / 10][0] = 1.0;
    for f in &mut lim[SR * 6 / 10..SR * 6 / 10 + 10] {
        *f = [0.8; 2];
    }
    for f in &mut lim[SR * 13 / 10..SR * 15 / 10] {
        *f = [0.5; 2];
    }
    for (i, f) in lim.iter_mut().enumerate().take(5 * SR / 2).skip(2 * SR) {
        *f = [0.1 + 0.6 * (2.0 * PI * 1000.0 * i as f64 / SR as f64).sin(); 2];
    }
    for (i, f) in lim.iter_mut().enumerate().take(7 * SR / 2).skip(3 * SR) {
        *f = [0.7 * (2.0 * PI * 11025.0 * i as f64 / SR as f64 + 0.4).sin(); 2];
    }
    let mut bursts = vec![[0.1; 2]; 12 * SR];
    for (k, ms) in [5, 10, 20, 50, 100, 200, 400].into_iter().enumerate() {
        let start = ((0.5 + k as f64 * 1.6) * SR as f64) as usize;
        for f in &mut bursts[start..start + ms * SR / 1000] {
            *f = [0.5; 2];
        }
    }
    let mut steps = Vec::new();
    for base in [0.2, 0.26, 0.3, 0.4] {
        let mut s = vec![[base; 2]; SR];
        for f in &mut s[SR / 2..SR / 2 + SR * 3 / 10] {
            *f = [0.5; 2];
        }
        steps.extend(s);
    }
    for base in [0.2, 0.3] {
        let mut s = vec![[base; 2]; SR];
        s[SR / 2] = [0.9; 2];
        steps.extend(s);
    }
    let mut imp = vec![[0.0; 2]; SR];
    imp[SR / 10] = [0.5; 2];
    vec![("main", main), ("key", key), ("ramp", ramp), ("lim", lim), ("bursts", bursts), ("steps", steps), ("imp", imp)]
}

fn template(dir: &Path, name: &str, seconds: usize, sidechain: bool) -> String {
    let item = |file: &str| {
        format!(
            "    <ITEM\n      POSITION 0\n      LENGTH {seconds}\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n      <SOURCE WAVE\n        FILE \"{}\"\n      >\n    >\n",
            dir.join(format!("in_{file}.wav")).display()
        )
    };
    let mut t = format!(
        "<REAPER_PROJECT 0.1 \"7.73/linux-x86_64\" 1762500000\n  SAMPLERATE {SR} 1 0\n  SELECTION 0 {seconds}\n  RENDER_FILE \"{}\"\n  RENDER_PATTERN \"\"\n  RENDER_FMT 0 2 {SR}\n  RENDER_1X 0\n  RENDER_RANGE 1 0 0 0 1000\n  RENDER_RESAMPLE 3 0 1\n  RENDER_ADDTOPROJ 0\n  RENDER_STEMS 0\n  RENDER_DITHER 0\n  <RENDER_CFG\n    ZXZhdyAAAQ==\n  >\n  MASTER_VOLUME 1 0 -1 -1 1\n",
        dir.display()
    );
    if sidechain {
        t += &format!("  <TRACK\n    NAME key\n    MAINSEND 0 0\n{}  >\n", item("key"));
    }
    t += "  <TRACK\n    NAME main\n    NCHAN 4\n";
    if sidechain {
        t += "    AUXRECV 0 0 1 0 0 0 0 0 2 -1:U 0 -1 ''\n";
    }
    t += &item(name);
    t += "  >\n>\n";
    t
}

/// Notes for the instruments as (start tick, length, key, velocity) at 960 ticks per quarter
/// and 120 bpm: a single long note, and a grid of chords, extremes of pitch and velocity, short
/// and overlapping notes.
type Notes = Vec<(u32, u32, u8, u8)>;

fn note_grids() -> Vec<(&'static str, Notes, Vec<(u32, &'static str)>)> {
    vec![
        ("note", vec![(192, 1920, 69, 100)], vec![]),
        ("bend", vec![(192, 1920, 69, 100)], vec![(960, "e0 00 60"), (1500, "e0 7f 20"), (1800, "e0 00 40")]),
        (
            "notes",
            vec![
                (192, 1920, 69, 100),
                (2400, 480, 48, 127),
                (2400, 480, 60, 64),
                (3360, 240, 84, 90),
                (3700, 900, 96, 30),
                (4800, 960, 36, 110),
                (5000, 300, 108, 100),
                (5200, 700, 69, 100),
                (6000, 1500, 57, 1),
                (7700, 100, 72, 80),
            ],
            vec![],
        ),
    ]
}

/// A five-second project with one MIDI item holding `notes`.
fn midi_template(dir: &Path, notes: &[(u32, u32, u8, u8)], raw: &[(u32, &str)]) -> String {
    let mut events: Vec<(u32, String)> = raw.iter().map(|&(t, m)| (t, m.to_owned())).collect();
    for &(start, len, key, vel) in notes {
        events.push((start, format!("90 {key:02x} {vel:02x}")));
        events.push((start + len, format!("80 {key:02x} 00")));
    }
    events.sort_by_key(|e| (e.0, e.1.starts_with("90")));
    let mut body = String::new();
    let mut at = 0;
    for (tick, msg) in events {
        let _ = writeln!(body, "        E {} {msg}", tick - at);
        at = tick;
    }
    let _ = writeln!(body, "        E {} b0 7b 00", 9600 - at);
    let mut t = template(dir, "imp", 5, false);
    let item = t.find("    <ITEM").unwrap();
    t.truncate(item);
    t += &format!(
        "    <ITEM\n      POSITION 0\n      LENGTH 5\n      <SOURCE MIDI\n        HASDATA 1 960 QN\n{body}      >\n    >\n  >\n>\n"
    );
    t
}

/// A JSFX that plays each MIDI message it receives as one sample (status, data 1 and data 2
/// packed into 24 bits), one per frame in order, so MIDI effects can be compared as audio.
const MIDI_PROBE: &str = r#"desc:Scythe MIDI probe (one message per sample as audio)
@init
n = 0;
@block
k = 0;
while (k < n - rd) ( q[k*2] = 0; q[k*2+1] = q[(rd+k)*2+1]; k += 1; );
n = n - rd; rd = 0; s = 0;
while (midirecv(ofs, m1, m2, m3)) ( q[n*2] = ofs; q[n*2+1] = m1*65536 + m2*256 + m3; n += 1; );
@sample
rd < n && q[rd*2] <= s ? ( spl0 = q[rd*2+1] / 16777216; spl1 = (q[rd*2] + 1) / 1024; rd += 1; ) : ( spl0 = 0; spl1 = 0; );
s += 1;
"#;

/// Every pitch class from C4 and the extremes, two channels, a controller, the pitch wheel
/// and a program change, for the MIDI probe.
fn probe_events() -> (Notes, Vec<(u32, &'static str)>) {
    let mut notes: Notes = (0..12).map(|k| (100 + k * 100, 50, 60 + k as u8, 100)).collect();
    notes.extend([(1500, 50, 127, 100), (1600, 50, 0, 100)]);
    (notes, vec![(400, "b0 07 50"), (500, "e0 00 50"), (1010, "91 40 30"), (1110, "81 40 00"), (1200, "c0 05 00")])
}

const RUNNER: &str = r#"local dir = os.getenv("WDIR")
local log = io.open(dir .. "/run.log", "a")
local function set_vst_data(tr, b64)
  local _, c = reaper.GetTrackStateChunk(tr, "", false)
  local lines = {}
  for l in (c .. "\n"):gmatch("([^\n]*)\n") do lines[#lines + 1] = l end
  local vst
  for i, l in ipairs(lines) do if l:match("^%s*<VST") then vst = i end end
  lines[vst + 2] = b64
  reaper.SetTrackStateChunk(tr, table.concat(lines, "\n"), false)
end
for line in io.lines(os.getenv("LIST")) do
  local name, fxname, tpl, spec = line:match("^([^;]+);([^;]+);([^;]+);?(.*)$")
  if name then
    local ok, err = pcall(function()
      reaper.Main_openProject("noprompt:" .. dir .. "/tpl_" .. tpl .. ".RPP")
      local tr = reaper.GetTrack(0, reaper.CountTracks(0) - 1)
      -- Probe templates already end in the MIDI probe; the effect goes in front of it.
      local fx = reaper.TrackFX_AddByName(tr, fxname, false, tpl:match("^probe") and -1000 or -1)
      for item in spec:gmatch("%S+") do
        local k, v = item:match("^([^=]+)=(.*)$")
        if k == "chunk" then set_vst_data(tr, v)
        else reaper.TrackFX_SetParam(tr, fx, tonumber(k:sub(2)), tonumber(v)) end
      end
      reaper.GetSetProjectInfo_String(0, "RENDER_FILE", dir, true)
      reaper.GetSetProjectInfo_String(0, "RENDER_PATTERN", name .. ".reaper", true)
      reaper.Main_SaveProjectEx(0, dir .. "/" .. name .. ".RPP", 0)
      reaper.Main_OnCommand(42230, 0)
    end)
    log:write((ok and "ok " or "FAIL ") .. name .. " " .. tostring(err or "") .. "\n")
    log:flush()
  end
end
log:close()
reaper.Main_openProject("noprompt:" .. dir .. "/tpl_imp.RPP")
reaper.Main_OnCommand(40004, 0)
"#;

/// ReaLimit's state: version 3, flags, threshold and ceiling in dB, two words, a double and the
/// release as `10^(-dB per second / 20)`.
fn realimit_chunk(flags: u32, threshold: f64, words: [u32; 2]) -> String {
    use base64::Engine as _;
    let mut b = Vec::new();
    b.extend(3u32.to_le_bytes());
    b.extend(flags.to_le_bytes());
    b.extend(threshold.to_le_bytes());
    b.extend(0f64.to_le_bytes());
    b.extend(words[0].to_le_bytes());
    b.extend(words[1].to_le_bytes());
    b.extend(0f64.to_le_bytes());
    b.extend(10f64.powf(-15.0 / 20.0).to_le_bytes());
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn reaxcomp_chunk(output: f64) -> String {
    use base64::Engine as _;
    let mut b = Vec::new();
    b.extend(56u32.to_le_bytes());
    b.extend(4u32.to_le_bytes());
    for top in [200.0f64, 1000.0, 5000.0, 24000.0] {
        for v in [top, 1.0, 0.1, 2.0, 0.0] {
            b.extend(v.to_le_bytes());
        }
        for v in [15i32, 150, 5, 17] {
            b.extend(v.to_le_bytes());
        }
    }
    b.extend(1u32.to_le_bytes());
    b.extend(1u32.to_le_bytes());
    for v in [output, 0.0, 0.0] {
        b.extend(v.to_le_bytes());
    }
    b.extend(2u32.to_le_bytes());
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// `name;effect;template;p<index>=<raw value> …` lines for the Lua runner.
fn cases() -> Vec<String> {
    let mut c = Vec::new();
    let mut add = |name: &str, fx: &str, tpl: &str, spec: &str| c.push(format!("{name};{fx};{tpl};{spec}"));
    // ReaComp: one parameter at a time around a -12 dB threshold, then static curves on a ramp.
    let t = "p0=0.2512";
    for (name, spec) in [
        ("base", ""),
        ("ratio2", "p1=0.010101"),
        ("ratio8", "p1=0.070707"),
        ("ratio_inf", "p1=1"),
        ("ratio_98.5", "p1=0.985"),
        ("attack0", "p2=0"),
        ("attack0.5", "p2=0.001"),
        ("attack10", "p2=0.02"),
        ("attack50", "p2=0.1"),
        ("release0", "p3=0"),
        ("release10", "p3=0.002"),
        ("release500", "p3=0.1"),
        ("rms0", "p13=0"),
        ("rms1", "p13=0.01"),
        ("rms20", "p13=0.2"),
        ("rms100", "p13=1"),
        ("knee6", "p14=0.25"),
        ("knee12", "p14=0.5"),
        ("precomp5", "p4=0.02"),
        ("precomp20", "p4=0.08"),
        ("lowpass2k", "p6=0.1"),
        ("hipass500", "p7=0.025"),
        ("hipass2k", "p7=0.1"),
        ("dry", "p10=0.5"),
        ("wet", "p11=0.5"),
        ("automakeup", "p15=1"),
        ("autorelease10", "p16=1 p3=0.002"),
        ("autorelease500", "p16=1 p3=0.1"),
        ("autorelease_inf", "p16=1 p3=0.002 p1=1"),
        ("legacy0.1", "p14=0.5 p17=0.1"),
        ("legacy0.3", "p14=0.5 p17=0.3"),
        ("legacy0.6", "p14=0.5 p17=0.6"),
        ("legacy0.8", "p14=0.5 p17=0.8"),
        ("legacy1", "p14=0.5 p17=1"),
        ("preview", "p12=1 p6=0.1"),
    ] {
        add(&format!("comp_{name}"), "ReaComp", "main", &format!("{t} {spec}"));
    }
    for k in [0, 1, 2, 3, 8] {
        add(&format!("comp_sidechain{k}"), "ReaComp", "sc", &format!("{t} p8={:.9}", k as f64 / 1084.0));
    }
    for (knee, ratio) in [(0.0, 0.0303), (0.25, 0.0101), (0.5, 0.0303), (1.0, 1.0)] {
        for legacy in [0.0, 0.25] {
            let spec = format!("p0=0.2512 p2=0 p3=0 p13=0 p14={knee} p1={ratio} p17={legacy}");
            add(&format!("comp_curve_k{knee}_r{ratio}_l{legacy}"), "ReaComp", "ramp", &spec);
        }
    }
    add("comp_curve_makeup", "ReaComp", "ramp", "p0=0.0631 p2=0 p3=0 p13=0 p15=1");
    // ReaGate.
    for (name, spec) in [
        ("base", ""),
        ("attack0", "p1=0"),
        ("attack20", "p1=0.04"),
        ("release0", "p2=0"),
        ("release1s", "p2=0.2"),
        ("preopen5", "p3=0.02"),
        ("preopen20", "p3=0.08"),
        ("hold50", "p4=0.05"),
        ("lowpass2k", "p5=0.1"),
        ("hipass500", "p6=0.025"),
        ("dry", "p9=0.5"),
        ("wet", "p10=0.5"),
        ("hysteresis-6", "p12=0.5"),
        ("hysteresis-12", "p12=0.25"),
        ("hysteresis+3.5", "p12=1.5"),
        ("rms1", "p14=0.01"),
        ("rms20", "p14=0.2"),
        ("invert_legacy", "p18=1 p9=0.5"),
        ("invert", "p18=2 p9=0.25 p10=0.5"),
        ("preview", "p13=1 p5=0.1"),
        ("combo", "p0=0.05 p1=0.1 p2=0.1 p4=0.1 p3=0.04 p12=0.3 p14=0.1 p5=0.3 p6=0.005"),
    ] {
        add(&format!("gate_{name}"), "ReaGate", "main", &format!("p0=0.2512 {spec}"));
    }
    add("gate_sidechain", "ReaGate", "sc", "p0=0.2512 p7=0.00184502");
    // ReaLimit: threshold, ceiling and release, then isolated peaks, bursts and limited steps.
    for (name, spec) in [
        ("base", "p0=0.666667"),
        ("t-6", "p0=0.75"),
        ("t-24", "p0=0.5"),
        ("ceil-3", "p0=0.666667 p1=0.875"),
        ("ceil-12", "p0=0.666667 p1=0.5"),
        ("rel32", "p0=0.666667 p2=0.05"),
        ("rel6", "p0=0.666667 p2=1"),
        ("rel_inf", "p0=0.666667 p2=0"),
    ] {
        add(&format!("limit_{name}"), "ReaLimit", "main", spec);
    }
    for (name, spec) in [("base", "p0=0.666667"), ("rel6", "p0=0.666667 p2=1"), ("ceil-6", "p0=0.666667 p1=0.75")] {
        add(&format!("limit_peaks_{name}"), "ReaLimit", "lim", spec);
    }
    for r in [1.0, 0.354813, 0.05] {
        add(&format!("limit_bursts_{r}"), "ReaLimit", "bursts", &format!("p0=0.666667 p2={r}"));
    }
    add("limit_steps", "ReaLimit", "steps", "p0=0.666667");
    add("limit_words", "ReaLimit", "lim", &format!("chunk={}", realimit_chunk(0, -12.0, [3, 0])));
    // ReaXcomp: crossovers on an impulse, then every band compressing at -20 dB.
    let all = |p: u32, v: &str| (0..4).map(|b| format!("p{}={v}", b * 12 + p)).collect::<Vec<_>>().join(" ");
    add("xcomp_impulse", "ReaXcomp", "imp", "");
    for b in 0..4 {
        let others = (0..4).filter(|&o| o != b).map(|o| format!("p{}=0", o * 12 + 1)).collect::<Vec<_>>().join(" ");
        add(&format!("xcomp_band{}", b + 1), "ReaXcomp", "imp", &others);
    }
    let t = all(2, "0.025");
    for (name, spec) in [
        ("base", String::new()),
        ("no_makeup", all(8, "0")),
        ("ratio4", all(3, "0.174")),
        ("ratio100", all(3, "1")),
        ("expand", all(3, "-0.2")),
        ("knee", all(4, "0.25")),
        ("attack1", all(5, "0.004")),
        ("release100", all(6, "0.05")),
        ("rms0", all(7, "0")),
        ("autorelease", all(9, "1")),
        ("feedback", all(10, "1")),
        ("feedback_gain", format!("{} p1=0.5", all(10, "1"))),
        ("gains", "p1=0.5 p13=0.15".into()),
        ("band2_off", "p23=0 p13=0.5".into()),
        ("frequencies", "p0=0.3 p12=0.6".into()),
        (
            "mix",
            "p0=0.1 p2=0.05 p4=0.5 p5=0.01 p6=0.02 p7=0.1 p9=1 p12=0.35 p14=0.04 p16=0.2 p21=1 p24=0.8 p26=0.03 p27=0.4 p38=0.06 p43=0 p46=1".into(),
        ),
    ] {
        add(&format!("xcomp_{name}"), "ReaXcomp", "main", &format!("{t} {spec}"));
    }
    add("xcomp_output", "ReaXcomp", "main", &format!("chunk={}", reaxcomp_chunk(0.5)));
    // ReaVerbate: one parameter at a time on the impulse and on the program material.
    for (name, spec) in [
        ("base", ""),
        ("room0", "p2=0"),
        ("room1", "p2=1"),
        ("room0.73", "p2=0.73"),
        ("damp0", "p3=0"),
        ("damp1", "p3=1"),
        ("width0", "p4=0"),
        ("width0.3", "p4=0.3"),
        ("delay", "p5=0.12345"),
        ("lowpass", "p6=0.23"),
        ("highpass", "p7=0.05"),
        ("band", "p6=0.4 p7=0.01"),
        ("wet", "p0=1.7 p1=0.3"),
        ("combo", "p0=0.8 p1=0.6 p2=0.9 p3=0.2 p4=0.65 p5=0.031 p6=0.6 p7=0.004"),
    ] {
        add(&format!("verbate_{name}"), "ReaVerbate", "main", spec);
        add(&format!("verbate_imp_{name}"), "ReaVerbate", "imp", spec);
    }
    // ReaSynth: each parameter on one long note, then on the note grid.
    for (name, spec) in [
        ("base", ""),
        ("attack0", "p0=0 p1=0"),
        ("attack_long", "p0=0.8"),
        ("release", "p1=0.03"),
        ("decay", "p9=0.5 p6=0.01"),
        ("decay_to_0", "p9=0 p6=0.003"),
        ("sustain_up", "p9=0.7 p6=0.004"),
        ("square", "p2=1"),
        ("square_mix", "p2=0.6"),
        ("pulse", "p2=1 p10=0.3"),
        ("saw", "p3=1"),
        ("saw_mix", "p3=0.4"),
        ("triangle", "p4=1"),
        ("extra_sine", "p7=0.6"),
        ("extra_tuned", "p7=1 p8=0.7"),
        ("mix", "p2=0.5 p3=0.5 p4=0.5 p7=0.3 p8=0.45"),
        ("overmix", "p2=0.8 p3=0.9 p4=1"),
        ("detune", "p11=0.6"),
        ("volume", "p5=0.9"),
        ("legacy", "p12=1 p3=1"),
    ] {
        add(&format!("synth_{name}"), "ReaSynth", "note", spec);
        add(&format!("synth_grid_{name}"), "ReaSynth", "notes", spec);
    }
    add("synth_bend", "ReaSynth", "bend", "");
    // ReaControlMIDI: what reaches the MIDI probe after it.
    for (name, spec) in [
        ("base", ""),
        ("transpose", "p9=0.6"),
        ("transpose_max", "p9=1"),
        ("snap", "p10=1"),
        ("snap_d", "p10=1 p11=0.2"),
        ("snap_down", "p10=1 p11=0.2 p9=0.3"),
        ("program_all", "p13=1 p2=0.3"),
        ("program_ch3", "p13=1 p2=0.3 p0=0.1 p1=0.6 p8=0.2"),
        ("controls_all", "p14=1 p3=0.5 p4=0.3 p5=0.7"),
        ("controls_ch2", "p14=1 p3=0.5 p4=0.3 p5=0.7 p8=0.1"),
        ("controls_unset", "p14=1 p8=0.1"),
        ("both", "p13=1 p14=1 p8=0.1 p4=0.2"),
    ] {
        add(&format!("controlmidi_{name}"), "ReaControlMIDI", "probe", spec);
    }
    c
}

fn read(path: &Path) -> Option<Vec<(f32, f32)>> {
    let mut r = hound::WavReader::open(path).ok()?;
    let ch = r.spec().channels as usize;
    let s: Vec<f32> = r.samples::<f32>().map(Result::unwrap).collect();
    Some(s.chunks(ch).map(|c| (c[0], *c.get(1).unwrap_or(&c[0]))).collect())
}

/// Scythe's render of a project REAPER saved and rendered, against REAPER's.
fn null(rpp: &Path) -> Result<f32, String> {
    let project = scythe_core::rpp::read_project(rpp).map_err(|e| e.to_string())?;
    let settings = RenderSettings::from_project(&project);
    let selection = scythe_core::render_settings::RenderSelection::saved(&project);
    let jobs = settings.jobs(&project, &selection, 0).map_err(|e| format!("{e:?}"))?;
    let job = jobs.into_iter().next().ok_or("nothing to render")?;
    let (range, theirs_path) = (job.range, job.path);
    let theirs = read(&theirs_path).ok_or("no REAPER render")?;
    let pool = MediaPool::with_resample(settings.resample);
    for t in &project.tracks {
        for take in t.items.iter().flat_map(|i| &i.takes) {
            if let Some(f) = take.source.file() {
                let _ = pool.load(&scythe_core::rpp::resolve_media_path(&project, f), SR as u32);
            }
        }
    }
    pool.prepare_project(&project);
    let ours_path = theirs_path.with_extension("scythe.wav");
    let finish = Finish { dither: false, noise_shape: false, ..Finish::from_settings(&settings) };
    bounce_finished(&project, &pool, range, SR as u32, 2, Encoding::wav(BitDepth::Float32), &ours_path, finish, |_| {})
        .map_err(|e| e.to_string())?;
    let ours = read(&ours_path).ok_or("no scythe render")?;
    Ok(ours.iter().zip(&theirs).map(|(a, b)| (a.0 - b.0).abs().max((a.1 - b.1).abs())).fold(0.0, f32::max))
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: stock_ref <work dir> <reaper> [filter]"));
    let reaper = args.next().expect("usage: stock_ref <work dir> <reaper> [filter]");
    let filter = args.next().unwrap_or_default();
    let dir = std::fs::canonicalize(&dir).expect("work dir");
    let mut lengths = Vec::new();
    for (name, frames) in signals() {
        let path = dir.join(format!("in_{name}.wav"));
        if !path.exists() {
            write_wav(&path, &frames);
        }
        lengths.push((name, frames.len().div_ceil(SR)));
    }
    for &(name, secs) in &lengths {
        std::fs::write(dir.join(format!("tpl_{name}.RPP")), template(&dir, name, secs, false)).unwrap();
    }
    std::fs::write(dir.join("tpl_sc.RPP"), template(&dir, "main", 5, true)).unwrap();
    for (name, notes, raw) in note_grids() {
        std::fs::write(dir.join(format!("tpl_{name}.RPP")), midi_template(&dir, &notes, &raw)).unwrap();
    }
    let effects = dir.join("reaper-cfg").join("Effects");
    std::fs::create_dir_all(&effects).unwrap();
    std::fs::write(effects.join("scythe_midi_probe.jsfx"), MIDI_PROBE).unwrap();
    // SAFETY: set before any thread starts.
    unsafe { std::env::set_var("SCYTHE_JSFX_PATH", &effects) };
    let (notes, raw) = probe_events();
    let probe = midi_template(&dir, &notes, &raw)
        .replace("    <ITEM", "    <FXCHAIN\n      <JS scythe_midi_probe.jsfx \"\"\n      >\n    >\n    <ITEM");
    std::fs::write(dir.join("tpl_probe.RPP"), probe).unwrap();
    std::fs::write(dir.join("run.lua"), RUNNER).unwrap();

    let cases: Vec<String> = cases().into_iter().filter(|c| c.contains(&filter)).collect();
    let todo: Vec<&String> = cases
        .iter()
        .filter(|c| {
            let name = c.split(';').next().unwrap();
            !dir.join(format!("{name}.reaper.wav")).exists()
        })
        .collect();
    let cfg = dir.join("reaper-cfg").join("reaper.ini");
    for batch in todo.chunks(30) {
        let list = dir.join("cases.txt");
        std::fs::write(
            &list,
            batch.iter().fold(String::new(), |mut s, c| {
                let _ = writeln!(s, "{c}");
                s
            }),
        )
        .unwrap();
        let _ = Command::new(&reaper)
            .args(["-cfgfile", &cfg.display().to_string(), "-nosplash", "-new"])
            .arg(dir.join("run.lua"))
            .env("WDIR", &dir)
            .env("LIST", &list)
            .status();
    }

    let mut worst: Vec<(String, usize, f32, String)> = Vec::new();
    for case in &cases {
        let name = case.split(';').next().unwrap();
        let plugin = case.split(';').nth(1).unwrap().to_owned();
        let result = null(&dir.join(format!("{name}.RPP")));
        match &result {
            Ok(d) => println!("{name:<36} {d:.2e}"),
            Err(e) => println!("{name:<36} {e}"),
        }
        let d = result.unwrap_or(f32::INFINITY);
        match worst.iter_mut().find(|w| w.0 == plugin) {
            Some(w) => {
                w.1 += 1;
                if d > w.2 {
                    (w.2, w.3) = (d, name.to_owned());
                }
            }
            None => worst.push((plugin, 1, d, name.to_owned())),
        }
    }
    println!();
    for (plugin, n, d, name) in worst {
        println!("{plugin:<10} {n:>3} cases, largest difference {d:.2e} ({name})");
    }
}
