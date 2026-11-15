//! ScyEQ-Pro in REAPER against Scythe: writes test projects that hold only ScyEQ-Pro instances,
//! and compares REAPER's render of each with Scythe's. `scripts/reaper-compare.sh` runs both
//! hosts in between; see docs/testing.md.
//!
//! cargo run --release -p scyeq-plugin --example reaper_projects -- write <dir>
//! cargo run --release -p scyeq-plugin --example reaper_projects -- compare <dir> [limit dBFS]

use std::fmt::Write as _;
use std::path::Path;

use base64::Engine as _;
use scyeq_dsp::filters::Shape;
use scyeq_dsp::params::{self, BAND_STRIDE, GLOBAL, band, global};

const RATE: u32 = 48_000;
const SECONDS: f64 = 2.0;
/// Off the 32-frame grid and the 512-frame block, so a host that does not pass the position
/// places them elsewhere.
const OFFSET_START: f64 = 1013.0 / RATE as f64;
const LIMIT_DB: f64 = -120.0;
/// Where automated projects are compared from. REAPER's first block of a render races its
/// parameter flush with the render thread, so it takes a parameter's stored value one run and its
/// envelope's the next; the filters settle from that within half a second.
const AUTOMATION_FROM: usize = RATE as usize / 2;

struct Case {
    name: String,
    state: Vec<u8>,
    keyed: bool,
    start: f64,
    envelope: Option<String>,
}

impl Case {
    fn new(name: impl Into<String>, values: &[f64]) -> Self {
        Self {
            name: name.into(),
            state: params::encode(values, &[], params::VERSION),
            keyed: false,
            start: 0.0,
            envelope: None,
        }
    }
}

fn set(v: &mut [f64], b: usize, offset: u32, value: f64) {
    v[b * BAND_STRIDE as usize + offset as usize] = value;
}

const SHAPES: [(Shape, &str, f64, f64, usize); 10] = [
    (Shape::Bell, "bell", 1000.0, 4.0, 0),
    (Shape::LowShelf, "low_shelf", 120.0, -3.0, 0),
    (Shape::HighShelf, "high_shelf", 8000.0, 2.5, 0),
    (Shape::LowCut, "low_cut", 40.0, 0.0, 0),
    (Shape::HighCut, "high_cut", 16000.0, 0.0, 0),
    (Shape::Notch, "notch", 3000.0, 0.0, 0),
    (Shape::BandPass, "band_pass", 700.0, 0.0, 4),
    (Shape::TiltShelf, "tilt_shelf", 2000.0, 1.5, 0),
    (Shape::FlatTilt, "flat_tilt", 500.0, -1.0, 0),
    (Shape::AllPass, "all_pass", 1500.0, 0.0, 1),
];

fn shape_band(v: &mut [f64], b: usize, (shape, _, freq, gain, placement): (Shape, &str, f64, f64, usize)) {
    set(v, b, band::USED, 1.0);
    set(v, b, band::SHAPE, shape.index() as f64);
    set(v, b, band::FREQ, freq);
    set(v, b, band::GAIN, gain);
    set(v, b, band::Q, 0.9);
    set(v, b, band::SLOPE, 3.0);
    set(v, b, band::STEREO, placement as f64);
}

/// A bell that follows its input (`mode` 1 dynamic, 2 spectral), keyed by the sidechain when
/// `external`.
fn dynamic_band(v: &mut [f64], b: usize, freq: f64, mode: f64, external: bool) {
    set(v, b, band::USED, 1.0);
    set(v, b, band::SHAPE, Shape::Bell.index() as f64);
    set(v, b, band::FREQ, freq);
    set(v, b, band::Q, 2.0);
    set(v, b, band::DYNAMIC, mode);
    set(v, b, band::RANGE, -9.0);
    set(v, b, band::THRESHOLD, -36.0);
    set(v, b, band::ATTACK, 2.0);
    set(v, b, band::RELEASE, 50.0);
    set(v, b, band::SIDECHAIN, if external { 1.0 } else { 0.0 });
}

/// Every shape, a dynamic, a spectral and a sidechain band, in phase mode `phase`.
fn everything(phase: usize) -> Vec<f64> {
    let mut v = params::defaults();
    for (b, shape) in SHAPES.into_iter().enumerate() {
        shape_band(&mut v, b, shape);
    }
    dynamic_band(&mut v, 10, 2000.0, 1.0, false);
    dynamic_band(&mut v, 11, 5000.0, 2.0, false);
    dynamic_band(&mut v, 12, 300.0, 1.0, true);
    v[(GLOBAL + global::PHASE) as usize] = phase as f64;
    v
}

fn cases() -> Vec<Case> {
    let mut cases = vec![Case::new("defaults", &params::defaults())];
    for shape in SHAPES {
        let mut v = params::defaults();
        shape_band(&mut v, 0, shape);
        cases.push(Case::new(format!("shape_{}", shape.1), &v));
    }
    let mut v = params::defaults();
    dynamic_band(&mut v, 0, 2000.0, 1.0, false);
    cases.push(Case::new("dynamic", &v));
    let mut v = params::defaults();
    dynamic_band(&mut v, 0, 5000.0, 2.0, false);
    cases.push(Case::new("spectral", &v));
    let mut v = params::defaults();
    dynamic_band(&mut v, 0, 300.0, 1.0, true);
    cases.push(Case { keyed: true, ..Case::new("sidechain", &v) });
    for (phase, name) in ["zero_latency", "natural", "linear"].into_iter().enumerate() {
        cases.push(Case { keyed: true, ..Case::new(format!("phase_{name}"), &everything(phase)) });
        cases.push(Case {
            keyed: true,
            start: OFFSET_START,
            ..Case::new(format!("phase_{name}_offset"), &everything(phase))
        });
    }
    let mut v = params::defaults();
    shape_band(&mut v, 0, SHAPES[0]);
    // As REAPER writes them: `<index>:<CLAP id>`, the parameter's own range and units. Band 1's
    // parameters have the same index and id. Each starts at the stored value, which keeps what
    // REAPER's first block does from reaching far (see `AUTOMATION_FROM`).
    let gain = envelope("3:3 -30 30 0", &[(0.0, 4.0, 0), (0.7, -12.0, 1), (1.1, 9.0, 0), (1.9, -3.0, 0)]);
    cases.push(Case { envelope: Some(gain), ..Case::new("automated_gain", &v) });
    let frequency =
        envelope("2:2 10 30000 1000", &[(0.0, 1000.0, 0), (0.6, 200.0, 0), (1.0, 5000.0, 1), (1.5, 800.0, 0)]);
    cases.push(Case { envelope: Some(frequency), ..Case::new("automated_frequency", &v) });
    // A ramp changes the gain every block while a dynamic band follows the noise.
    dynamic_band(&mut v, 1, 2000.0, 1.0, false);
    let gain = envelope("3:3 -30 30 0", &[(0.0, 4.0, 0), (0.8, -6.0, 0), (1.4, 6.0, 0), (1.9, 0.0, 0)]);
    cases.push(Case { envelope: Some(gain), ..Case::new("automated_dynamic", &v) });
    // A dynamic band switched off for about a block now and then, square: its detector starts
    // afresh each time it comes back.
    let mut v = params::defaults();
    dynamic_band(&mut v, 0, 2000.0, 1.0, false);
    let toggle = envelope("8:8 0 2 1", &[(0.0, 1.0, 1), (0.8, 0.0, 1), (0.81, 1.0, 1), (1.4, 0.0, 1), (1.6, 1.0, 1)]);
    cases.push(Case { envelope: Some(toggle), ..Case::new("automated_dynamic_toggle", &v) });
    cases
}

/// A `PARMENV` chunk with `points` (time, value, shape: 0 linear, 1 square).
fn envelope(header: &str, points: &[(f64, f64, u32)]) -> String {
    let mut e = format!(
        "<PARMENV {header}\n  EGUID {{8B6E2D41-6C1F-4F57-9E4B-2E1D3B6A9C01}}\n  ACT 1 -1\n  VIS 1 1 1\n  LANEHEIGHT 0 0\n  ARM 0\n  DEFSHAPE 0 -1 -1\n"
    );
    for (time, value, shape) in points {
        let _ = writeln!(e, "  PT {time} {value} {shape}");
    }
    e.push_str(">\n");
    e
}

/// Two seconds of stereo noise whose level steps every `period` frames, so dynamic bands move.
fn write_noise(path: &Path, seed: u64, period: usize) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    let mut state = seed;
    for i in 0..(RATE as f64 * SECONDS) as usize {
        let level = if (i / period) % 2 == 1 { 0.05 } else { 0.8 };
        for _ in 0..2 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            w.write_sample(((state >> 33) as f32 / (1u64 << 31) as f32 - 0.5) * level).unwrap();
        }
    }
    w.finalize().unwrap();
}

fn indent(text: &str, by: &str) -> String {
    text.lines().fold(String::new(), |mut s, l| {
        let _ = writeln!(s, "{by}{l}");
        s
    })
}

fn project(dir: &Path, case: &Case) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(&case.state);
    let state: String =
        b64.as_bytes().chunks(128).map(|l| format!("          {}\n", String::from_utf8_lossy(l))).collect();
    let envelope = case.envelope.as_deref().map(|e| indent(e, "      ")).unwrap_or_default();
    let item = |file: &str| {
        format!(
            "    <ITEM\n      POSITION 0\n      LENGTH {SECONDS}\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n      <SOURCE WAVE\n        FILE \"{}\"\n      >\n    >\n",
            dir.join(file).display()
        )
    };
    let receive = if case.keyed { "    AUXRECV 1 0 1 0 0 0 0 0 2 -1:U 4177951 -1 \"\"\n" } else { "" };
    let key = if case.keyed {
        format!("  <TRACK\n    NAME key\n    VOLPAN 1 0 -1 -1 1\n    MAINSEND 0 0\n{}  >\n", item("key.wav"))
    } else {
        String::new()
    };
    format!(
        "<REAPER_PROJECT 0.1 \"7.73/linux-x86_64\" 1762500000
  SAMPLERATE {RATE} 1 0
  SELECTION {start} {SECONDS}
  RENDER_FILE \"{out}\"
  RENDER_PATTERN \"\"
  RENDER_FMT 0 2 {RATE}
  RENDER_1X 0
  RENDER_RANGE 2 0 0 0 1000
  RENDER_RESAMPLE 3 0 1
  RENDER_ADDTOPROJ 0
  RENDER_STEMS 0
  RENDER_DITHER 0
  <RENDER_CFG
    ZXZhdyAAAQ==
  >
  <TRACK
    NAME main
    VOLPAN 1 0 -1 -1 1
    NCHAN 4
{receive}    <FXCHAIN
      SHOW 0
      LASTSEL 0
      DOCKED 0
      BYPASS 0 0 0
      <CLAP \"CLAP: ScyEQ-Pro (Scythe)\" scythe.eq \"\"
        CFG 4 0 0 \"\"
        <STATE
{state}        >
      >
      FLOATPOS 0 0 0 0
      FXID {{5C1E0B7A-3F2D-4E8B-9A61-0D4C2B7E8F13}}
{envelope}      WAK 0 0
    >
{item}  >
{key}>
",
        start = case.start,
        out = dir.join(format!("{}.reaper.wav", case.name)).display(),
        item = item("main.wav"),
    )
}

fn write(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    write_noise(&dir.join("main.wav"), 7, 6000);
    write_noise(&dir.join("key.wav"), 11, 4000);
    for case in cases() {
        std::fs::write(dir.join(format!("{}.RPP", case.name)), project(dir, &case)).unwrap();
        println!("{}", case.name);
    }
}

fn read(path: &Path) -> Result<(Vec<f32>, u16), String> {
    let mut r = hound::WavReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let spec = r.spec();
    if spec.sample_format != hound::SampleFormat::Float || spec.bits_per_sample != 32 {
        return Err(format!("{}: not 32-bit float", path.display()));
    }
    let samples = r.samples::<f32>().collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    Ok((samples, spec.channels))
}

fn db(v: f64) -> f64 {
    20.0 * v.max(1e-30).log10()
}

/// The largest difference between `a` and `b` (interleaved, `channels` wide) in dBFS, and the
/// frame where it is.
fn peak_difference(a: &[f32], b: &[f32], channels: u16) -> (f64, usize) {
    let (peak, at) = a
        .iter()
        .zip(b)
        .enumerate()
        .map(|(i, (a, b))| ((*a as f64 - *b as f64).abs(), i / channels as usize))
        .fold((0.0, 0), |m, d| if d.0 > m.0 || d.0.is_nan() { d } else { m });
    (if peak.is_nan() { f64::INFINITY } else { db(peak) }, at)
}

/// How far REAPER's render of `case` is from Scythe's, and the frame where it is furthest.
fn compare_case(dir: &Path, case: &Case) -> Result<(f64, usize), String> {
    let (reaper, rc) = read(&dir.join(format!("{}.reaper.wav", case.name)))?;
    let (scythe, sc) = read(&dir.join(format!("{}.scythe.wav", case.name)))?;
    if rc != sc || reaper.len() != scythe.len() {
        return Err(format!(
            "REAPER wrote {} frames of {rc} channels, Scythe {} of {sc}",
            reaper.len() / rc as usize,
            scythe.len() / sc as usize
        ));
    }
    let (dry, _) = read(&dir.join("main.wav"))?;
    let skip = (case.start * RATE as f64).round() as usize * 2;
    // Two hosts that both left the EQ out would agree too.
    if case.name != "defaults" && peak_difference(&scythe, &dry[skip..], sc).0 < -60.0 {
        return Err("Scythe's render is the dry signal".into());
    }
    let from = if case.envelope.is_some() { AUTOMATION_FROM * rc as usize } else { 0 };
    let (peak, at) = peak_difference(&reaper[from..], &scythe[from..], rc);
    Ok((peak, at + from / rc as usize))
}

fn compare(dir: &Path, limit: f64) -> bool {
    let cases = cases();
    let mut passed = 0;
    for case in &cases {
        match compare_case(dir, case) {
            Ok((peak, _)) if peak == db(0.0) => {
                passed += 1;
                println!("{:<26} exact", case.name);
            }
            Ok((peak, _)) if peak <= limit => {
                passed += 1;
                println!("{:<26} {peak:>7.1} dBFS", case.name);
            }
            Ok((peak, at)) => println!("{:<26} {peak:>7.1} dBFS  FAIL, largest at frame {at}", case.name),
            Err(e) => println!("{:<26} FAIL: {e}", case.name),
        }
    }
    println!("{passed}/{} within {limit} dBFS", cases.len());
    passed == cases.len()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "usage: reaper_projects write <dir> | compare <dir> [limit dBFS]";
    match (args.first().map(String::as_str), args.get(1)) {
        (Some("write"), Some(dir)) => write(Path::new(dir)),
        (Some("compare"), Some(dir)) => {
            let limit = args.get(2).map_or(LIMIT_DB, |v| v.parse().expect("limit in dBFS"));
            if !compare(Path::new(dir), limit) {
                std::process::exit(1);
            }
        }
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    }
}
