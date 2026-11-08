//! Measures ReaEQ band types in REAPER against scythe's ReaEQ-compatible EQ: renders an impulse
//! through a one-band EQ per case and prints the magnitude response at a few frequencies.
//!
//! cargo run -p scythe-engine --example reaeq_ref -- <work dir> [path to reaper]

use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;
use scythe_engine::fx::{Effect, ProcessContext};
use scythe_engine::reaeq::ReaEqCompat;

const SR: u32 = 48000;
const N: usize = 16384;
const FREQS: &[f64] = &[50.0, 200.0, 500.0, 1000.0, 2000.0, 5000.0, 15000.0];

fn eq_chunk(code: u32, freq: f64, gain: f64, bw: f64, output: f64) -> Vec<u8> {
    let mut d = Vec::new();
    d.extend(33u32.to_le_bytes());
    d.extend(1u32.to_le_bytes());
    d.extend(code.to_le_bytes());
    d.extend(1u32.to_le_bytes());
    d.extend(freq.to_le_bytes());
    d.extend(gain.to_le_bytes());
    d.extend(bw.to_le_bytes());
    d.push(1);
    for v in [1u32, 1] {
        d.extend(v.to_le_bytes());
    }
    d.extend(output.to_le_bytes());
    for v in [0u32, 644, 1424, 2] {
        d.extend(v.to_le_bytes());
    }
    d
}

fn vst_lines(chunk: &[u8]) -> Vec<String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut h = Vec::new();
    h.extend(1919247729u32.to_le_bytes());
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
    lines.push(b64.encode(b"\0Program 1\0\x10\0\0\0"));
    lines
}

fn magnitude(ir: &[f32], freq: f64) -> f64 {
    let (mut re, mut im) = (0.0, 0.0);
    for (i, &s) in ir.iter().enumerate() {
        let w = std::f64::consts::TAU * freq * i as f64 / SR as f64;
        re += s as f64 * w.cos();
        im -= s as f64 * w.sin();
    }
    20.0 * (re * re + im * im).sqrt().max(1e-12).log10()
}

fn read_left(path: &Path) -> Option<Vec<f32>> {
    let mut r = hound::WavReader::open(path).ok()?;
    let ch = r.spec().channels as usize;
    let s: Vec<f32> = r.samples::<f32>().map(Result::unwrap).collect();
    Some(s.chunks(ch).map(|c| c[0]).collect())
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("work dir"));
    let reaper = args.next();
    std::fs::create_dir_all(&dir).unwrap();
    let impulse = dir.join("impulse.wav");
    let spec =
        hound::WavSpec { channels: 1, sample_rate: SR, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(&impulse, spec).unwrap();
    for i in 0..N {
        w.write_sample(if i == 0 { 1.0f32 } else { 0.0 }).unwrap();
    }
    w.finalize().unwrap();
    let cfg = dir.join("reaper-cfg").join("reaper.ini");

    println!("{:<28} {}", "case (dB at Hz)", FREQS.iter().map(|f| format!("{f:>8.0}")).collect::<String>());
    let mut cases: Vec<(String, u32, f64, f64, f64)> =
        (0..=10).map(|c| (format!("type{c}"), c, 1000.0, 4.0, 1.0)).collect();
    for (code, bw) in
        [(0, 0.5), (0, 2.0), (1, 2.0), (2, 0.5), (2, 2.0), (3, 0.8), (3, 2.0), (4, 0.8), (6, 0.3), (7, 2.0), (8, 2.0)]
    {
        cases.push((format!("type{code}_bw{bw}"), code, 1000.0, 4.0, bw));
    }
    cases.push(("type0_bw1_cut".into(), 0, 1000.0, 0.25, 1.0));
    for (db, bw) in [
        (12.0, 0.3),
        (12.0, 0.8),
        (12.0, 1.5),
        (12.0, 3.0),
        (6.0, 0.8),
        (6.0, 2.0),
        (-11.2, 0.8),
        (16.6, 0.8),
        (3.0, 0.8),
    ] {
        let g: f64 = 10f64.powf(db / 20.0);
        cases.push((format!("shelf_{db}dB_bw{bw}"), 0, 1000.0, g, bw));
    }
    cases.push(("type8_bw0.3".into(), 8, 1000.0, 4.0, 0.3));
    cases.push(("type4_bw2_cut".into(), 4, 1000.0, 0.25, 2.0));
    cases.push(("type3_bw0.8_cut".into(), 3, 1000.0, 0.25, 0.8));
    for (name, code, freq, gain, bw) in cases {
        let output = if name == "type5" { 0.5 } else { 1.0 };
        let chunk = eq_chunk(code, freq, gain, bw, output);
        let out = dir.join(format!("eq_{name}.reaper.wav"));
        let lines = vst_lines(&chunk).iter().map(|l| format!("        {l}\n")).collect::<String>();
        let rpp = format!(
            "<REAPER_PROJECT 0.1 \"7.73/linux-x86_64\" 1762500000\n  SAMPLERATE {SR} 0 0\n  SELECTION 0 {len}\n  RENDER_FILE \"{out}\"\n  RENDER_PATTERN \"\"\n  RENDER_FMT 0 2 {SR}\n  RENDER_1X 0\n  RENDER_RANGE 2 0 0 0 1000\n  RENDER_RESAMPLE 3 0 1\n  RENDER_ADDTOPROJ 0\n  RENDER_STEMS 0\n  RENDER_DITHER 0\n  <RENDER_CFG\n    ZXZhdyAAAQ==\n  >\n  <TRACK\n    VOLPAN 1 0 -1 -1 1\n    <FXCHAIN\n      SHOW 0\n      LASTSEL 0\n      DOCKED 0\n      BYPASS 0 0 0\n      <VST \"VST: ReaEQ (Cockos)\" reaeq.vst.so 0 \"\" 1919247729<56535472656571726561657100000000> \"\"\n{lines}      >\n      FLOATPOS 0 0 0 0\n      WAK 0 0\n    >\n    <ITEM\n      POSITION 0\n      LENGTH {len}\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n      <SOURCE WAVE\n        FILE \"{imp}\"\n      >\n    >\n  >\n>\n",
            out = out.display(),
            imp = impulse.display(),
            len = N as f64 / SR as f64,
        );
        let rpp_path = dir.join(format!("eq_{name}.RPP"));
        std::fs::write(&rpp_path, rpp).unwrap();
        if let Some(r) = &reaper
            && !out.exists()
        {
            let _ = Command::new(r)
                .args(["-cfgfile", &cfg.display().to_string(), "-nosplash", "-renderproject"])
                .arg(&rpp_path)
                .status();
        }
        let mut eq = ReaEqCompat::from_chunk(&chunk, SR).unwrap();
        let mut l = vec![0.0f32; N];
        l[0] = 1.0;
        let mut r = l.clone();
        for (cl, cr) in l.chunks_mut(1024).zip(r.chunks_mut(1024)) {
            eq.process(
                cl,
                cr,
                &ProcessContext {
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
        let row = |ir: &[f32]| FREQS.iter().map(|&f| format!("{:>8.2}", magnitude(ir, f))).collect::<String>();
        println!("{:<28} {}", format!("{name} scythe"), row(&l));
        if let Some(ir) = read_left(&out) {
            println!("{:<28} {}", format!("{name} reaper"), row(&ir));
        }
    }
}
