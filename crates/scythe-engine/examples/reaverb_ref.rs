//! Measures how ReaVerb's stored header values scale the dry and wet (impulse response) paths:
//! an impulse goes through ReaVerb loading a delta IR, and the output is read at the dry and the
//! delayed wet position.
//!
//! cargo run -p scythe-engine --example reaverb_ref -- <work dir> <path to reaper>

use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;

const SR: u32 = 48000;
const N: usize = 4800;
const IR_DELAY: usize = 1000;

fn write_mono(path: &Path, f: impl Fn(usize) -> f32, frames: usize) {
    let spec =
        hound::WavSpec { channels: 1, sample_rate: SR, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..frames {
        w.write_sample(f(i)).unwrap();
    }
    w.finalize().unwrap();
}

fn chunk(header: [f32; 10], ir: &Path) -> Vec<u8> {
    let mut d: Vec<u8> = header.iter().flat_map(|v| v.to_le_bytes()).collect();
    let path = format!("{}\0", ir.display());
    d.extend(b"FILELDR\0");
    d.extend(((4 + path.len()) as u32).to_le_bytes());
    d.extend(0x3cu32.to_le_bytes());
    d.extend(path.as_bytes());
    d
}

fn vst_lines(chunk: &[u8]) -> Vec<String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut h = Vec::new();
    h.extend(1919252066u32.to_le_bytes());
    h.extend(0xfeed5eeeu32.to_le_bytes());
    h.extend(2u32.to_le_bytes());
    h.extend(1u64.to_le_bytes());
    h.extend(2u64.to_le_bytes());
    h.extend(2u32.to_le_bytes());
    h.extend(1u64.to_le_bytes());
    h.extend(2u64.to_le_bytes());
    h.extend((chunk.len() as u32).to_le_bytes());
    h.extend(1u32.to_le_bytes());
    h.extend(0x100000u32.to_le_bytes());
    let data = b64.encode(chunk);
    let mut lines = vec![b64.encode(&h)];
    lines.extend(data.as_bytes().chunks(128).map(|c| String::from_utf8_lossy(c).into_owned()));
    lines.push(b64.encode(b"\0\x10\0\0\0"));
    lines
}

fn read_left(path: &Path) -> Option<Vec<f32>> {
    let mut r = hound::WavReader::open(path).ok()?;
    let ch = r.spec().channels as usize;
    let s: Vec<f32> = r.samples::<f32>().map(Result::unwrap).collect();
    Some(s.chunks(ch).map(|c| c[0]).collect())
}

fn compare_real_ir(dir: &Path, reaper: &str, cfg: &Path, ir: &Path, name: &str) {
    use scythe_engine::fx::{Effect, ProcessContext};
    use scythe_engine::reaverb::{ReaVerbCompat, ReaVerbSettings};
    let noise = dir.join("rv_noise.wav");
    let len = SR as usize;
    write_mono(&noise, |i| ((i as u64).wrapping_mul(2654435761) % 1000) as f32 / 1000.0 - 0.5, len);
    let header = [-9.0, 0.251_188_64, 1.0, 0.0, 1.0, 1.0, 0.5, 1.0, 1.0, 2.0];
    let data = chunk(header, ir);
    let out = dir.join(format!("rv_real_{name}.reaper.wav"));
    let lines = vst_lines(&data).iter().map(|l| format!("        {l}\n")).collect::<String>();
    let rpp = format!(
        "<REAPER_PROJECT 0.1 \"7.73/linux-x86_64\" 1762500000\n  SAMPLERATE {SR} 0 0\n  SELECTION 0 1\n  RENDER_FILE \"{out}\"\n  RENDER_PATTERN \"\"\n  RENDER_FMT 0 2 {SR}\n  RENDER_1X 0\n  RENDER_RANGE 2 0 0 0 1000\n  RENDER_RESAMPLE 3 0 1\n  RENDER_ADDTOPROJ 0\n  RENDER_STEMS 0\n  RENDER_DITHER 0\n  <RENDER_CFG\n    ZXZhdyAAAQ==\n  >\n  <TRACK\n    VOLPAN 1 0 -1 -1 1\n    <FXCHAIN\n      SHOW 0\n      LASTSEL 0\n      DOCKED 0\n      BYPASS 0 0 0\n      <VST \"VST: ReaVerb (Cockos)\" reaverb.vst.so 0 \"\" 1919252066<56535472657662726561766572620000> \"\"\n{lines}      >\n      FLOATPOS 0 0 0 0\n      WAK 0 0\n    >\n    <ITEM\n      POSITION 0\n      LENGTH 1\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n      <SOURCE WAVE\n        FILE \"{src}\"\n      >\n    >\n  >\n>\n",
        out = out.display(),
        src = noise.display(),
    );
    let rpp_path = dir.join(format!("rv_real_{name}.RPP"));
    std::fs::write(&rpp_path, rpp).unwrap();
    if !out.exists() {
        let _ = Command::new(reaper)
            .args(["-cfgfile", &cfg.display().to_string(), "-nosplash", "-renderproject"])
            .arg(&rpp_path)
            .status();
    }
    let theirs = read_left(&out).unwrap();
    let mut fx = ReaVerbCompat::new(ReaVerbSettings::parse(&data).unwrap(), SR, None).unwrap();
    let mut l: Vec<f32> =
        (0..len).map(|i| ((i as u64).wrapping_mul(2654435761) % 1000) as f32 / 1000.0 - 0.5).collect();
    let mut r = l.clone();
    for (a, b) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
        fx.process(
            a,
            b,
            &ProcessContext {
                midi: None,
                midi_from: 0,
                sample_rate: SR,
                position: 0,
                playing: true,
                notes: None,
                live: &[],
                live_channel: None,
                aux: None,
                bpm: 120.0,
            },
        );
    }
    let diff = l.iter().zip(&theirs).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    let peak = theirs.iter().map(|v| v.abs()).fold(0.0f32, f32::max);
    println!("real IR {name}: max |diff| {diff:.6} (signal peak {peak:.3})");
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("work dir"));
    let reaper = args.next().expect("reaper");
    let impulse = dir.join("rv_impulse.wav");
    let ir = dir.join("rv_ir.wav");
    write_mono(&impulse, |i| if i == 0 { 1.0 } else { 0.0 }, N);
    write_mono(&ir, |i| if i == IR_DELAY { 0.5 } else { 0.0 }, 2000);
    let cfg = dir.join("reaper-cfg").join("reaper.ini");
    for (name, ir) in std::env::var("RV_REAL_IRS").unwrap_or_default().split(':').filter(|s| !s.is_empty()).enumerate()
    {
        compare_real_ir(&dir, &reaper, &cfg, Path::new(ir), &name.to_string());
    }
    let base = [-9.0, 0.251_188_64, 1.0, 0.0, 1.0, 1.0, 0.5, 1.0, 1.0, 2.0];
    let mut cases: Vec<(String, [f32; 10])> = vec![("default".into(), base)];
    for (i, v) in [
        (0usize, 0.0f32),
        (0, -20.0),
        (1, 1.0),
        (1, 0.5),
        (2, 0.5),
        (3, 1.0),
        (4, 0.5),
        (5, 0.5),
        (6, 1.0),
        (7, 0.5),
        (8, 0.5),
        (9, 1.0),
    ] {
        let mut h = base;
        h[i] = v;
        cases.push((format!("f{i}={v}"), h));
    }
    println!("{:<14} {:>10} {:>10} {:>10}", "case", "dry@0", "wet@1000", "max other");
    for (name, header) in cases {
        let out = dir.join(format!("rv_{name}.reaper.wav"));
        let lines = vst_lines(&chunk(header, &ir)).iter().map(|l| format!("        {l}\n")).collect::<String>();
        let rpp = format!(
            "<REAPER_PROJECT 0.1 \"7.73/linux-x86_64\" 1762500000\n  SAMPLERATE {SR} 0 0\n  SELECTION 0 0.1\n  RENDER_FILE \"{out}\"\n  RENDER_PATTERN \"\"\n  RENDER_FMT 0 2 {SR}\n  RENDER_1X 0\n  RENDER_RANGE 2 0 0 0 1000\n  RENDER_RESAMPLE 3 0 1\n  RENDER_ADDTOPROJ 0\n  RENDER_STEMS 0\n  RENDER_DITHER 0\n  <RENDER_CFG\n    ZXZhdyAAAQ==\n  >\n  <TRACK\n    VOLPAN 1 0 -1 -1 1\n    <FXCHAIN\n      SHOW 0\n      LASTSEL 0\n      DOCKED 0\n      BYPASS 0 0 0\n      <VST \"VST: ReaVerb (Cockos)\" reaverb.vst.so 0 \"\" 1919252066<56535472657662726561766572620000> \"\"\n{lines}      >\n      FLOATPOS 0 0 0 0\n      WAK 0 0\n    >\n    <ITEM\n      POSITION 0\n      LENGTH 0.1\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n      <SOURCE WAVE\n        FILE \"{imp}\"\n      >\n    >\n  >\n>\n",
            out = out.display(),
            imp = impulse.display(),
        );
        let rpp_path = dir.join(format!("rv_{name}.RPP"));
        std::fs::write(&rpp_path, rpp).unwrap();
        if !out.exists() {
            let _ = Command::new(&reaper)
                .args(["-cfgfile", &cfg.display().to_string(), "-nosplash", "-renderproject"])
                .arg(&rpp_path)
                .status();
        }
        match read_left(&out) {
            Some(s) => {
                let other = s
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| *i != 0 && *i != IR_DELAY)
                    .map(|(_, v)| v.abs())
                    .fold(0.0f32, f32::max);
                let peak_at = s
                    .iter()
                    .enumerate()
                    .skip(1)
                    .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                println!(
                    "{name:<14} {:>10.5} {:>10.5} {:>10.5} (largest after 0 at {peak_at})",
                    s[0], s[IR_DELAY], other
                );
            }
            None => println!("{name:<14} not rendered"),
        }
    }
}
