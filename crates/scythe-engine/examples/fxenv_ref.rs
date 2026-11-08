//! Compares FX parameter envelopes with REAPER: renders a project whose ReaVerb wet gain is
//! automated (and one with a bypass envelope) in REAPER and in scythe.
//!
//! cargo run -p scythe-engine --example fxenv_ref -- <work dir> <reaper> <impulse response wav>

use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;
use scythe_core::TimeRange;
use scythe_core::rpp::project_from_rpp;
use scythe_engine::MediaPool;
use scythe_engine::bounce::{BitDepth, bounce_to_wav};

const SR: u32 = 48000;

fn reaverb_lines(ir: &Path) -> String {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut d: Vec<u8> =
        [-9.0f32, 0.5, 1.0, 0.0, 1.0, 1.0, 0.5, 1.0, 1.0, 2.0].iter().flat_map(|v| v.to_le_bytes()).collect();
    let path = format!("{}\0", ir.display());
    d.extend(b"FILELDR\0");
    d.extend(((4 + path.len()) as u32).to_le_bytes());
    d.extend(0x3cu32.to_le_bytes());
    d.extend(path.as_bytes());
    let mut h = Vec::new();
    h.extend(1919252066u32.to_le_bytes());
    h.extend(0xfeed5eeeu32.to_le_bytes());
    h.extend(2u32.to_le_bytes());
    h.extend(1u64.to_le_bytes());
    h.extend(2u64.to_le_bytes());
    h.extend(2u32.to_le_bytes());
    h.extend(1u64.to_le_bytes());
    h.extend(2u64.to_le_bytes());
    h.extend((d.len() as u32).to_le_bytes());
    h.extend(1u32.to_le_bytes());
    h.extend(0x100000u32.to_le_bytes());
    [b64.encode(&h), b64.encode(&d), b64.encode(b"\0\x10\0\0\0")].iter().map(|l| format!("        {l}\n")).collect()
}

fn read(path: &Path) -> Vec<f32> {
    let mut r = hound::WavReader::open(path).unwrap();
    let spec = r.spec();
    let s: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(Result::unwrap).collect(),
        hound::SampleFormat::Int => r.samples::<i32>().map(|v| v.unwrap() as f32 / 8_388_608.0).collect(),
    };
    s.chunks(spec.channels as usize).map(|c| c[0]).collect()
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("work dir"));
    let reaper = args.next().expect("reaper");
    let ir = PathBuf::from(args.next().expect("impulse response"));
    let noise = dir.join("fxenv_noise.wav");
    let spec =
        hound::WavSpec { channels: 1, sample_rate: SR, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(&noise, spec).unwrap();
    for i in 0..SR as usize * 2 {
        w.write_sample(((i as u64).wrapping_mul(2654435761) % 1000) as f32 / 1000.0 - 0.5).unwrap();
    }
    w.finalize().unwrap();
    let cfg = dir.join("reaper-cfg").join("reaper.ini");
    let cases = [
        (
            "wet_ramp",
            "      <PARMENV 0:_Wet 0 2 1 \"Wet / ReaVerb\"\n        ACT 1 -1\n        VIS 1 1 1\n        LANEHEIGHT 0 0\n        ARM 1\n        DEFSHAPE 0 -1 -1\n        PT 0 0 0\n        PT 1.5 1 0\n      >\n",
        ),
        (
            "bypass_step",
            "      <PARMENV 8:bypass 0 1 0.5 \"Bypass / ReaVerb\"\n        ACT 1 -1\n        VIS 1 1 1\n        LANEHEIGHT 0 0\n        ARM 1\n        DEFSHAPE 1 -1 -1\n        PT 0 1 1\n        PT 1 0 1\n      >\n",
        ),
    ];
    for (name, env) in cases {
        let out = dir.join(format!("fxenv_{name}.reaper.wav"));
        let rpp = format!(
            "<REAPER_PROJECT 0.1 \"7.73/linux-x86_64\" 1762500000\n  SAMPLERATE {SR} 0 0\n  SELECTION 0 2\n  RENDER_FILE \"{out}\"\n  RENDER_PATTERN \"\"\n  RENDER_FMT 0 2 {SR}\n  RENDER_1X 0\n  RENDER_RANGE 2 0 0 0 1000\n  RENDER_RESAMPLE 3 0 1\n  RENDER_ADDTOPROJ 0\n  RENDER_STEMS 0\n  RENDER_DITHER 0\n  <RENDER_CFG\n    ZXZhdyAAAQ==\n  >\n  <TRACK\n    VOLPAN 1 0 -1 -1 1\n    <FXCHAIN\n      SHOW 0\n      LASTSEL 0\n      DOCKED 0\n      BYPASS 0 0 0\n      <VST \"VST: ReaVerb (Cockos)\" reaverb.vst.so 0 \"\" 1919252066<56535472657662726561766572620000> \"\"\n{lines}      >\n      FLOATPOS 0 0 0 0\n{env}      WAK 0 0\n    >\n    <ITEM\n      POSITION 0\n      LENGTH 2\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n      <SOURCE WAVE\n        FILE \"{src}\"\n      >\n    >\n  >\n>\n",
            out = out.display(),
            lines = reaverb_lines(&ir),
            src = noise.display(),
        );
        let rpp_path = dir.join(format!("fxenv_{name}.RPP"));
        std::fs::write(&rpp_path, &rpp).unwrap();
        if !out.exists() {
            let _ = Command::new(&reaper)
                .args(["-cfgfile", &cfg.display().to_string(), "-nosplash", "-renderproject"])
                .arg(&rpp_path)
                .status();
        }
        let mut project = project_from_rpp(&rpp).unwrap();
        project.path = Some(rpp_path.clone());
        let pool = MediaPool::default();
        pool.load(&noise, SR).unwrap();
        let ours = dir.join(format!("fxenv_{name}.scythe.wav"));
        bounce_to_wav(&project, &pool, TimeRange::new(0.0, 2.0), SR, BitDepth::Float32, &ours, |_| {}).unwrap();
        let (a, b) = (read(&out), read(&ours));
        let rms = |v: &[f32]| (v.iter().map(|s| s * s).sum::<f32>() / v.len().max(1) as f32).sqrt();
        let diff: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x - y).collect();
        println!(
            "{name}: signal rms {:.4}, difference rms {:.5} ({:.1} dB below), max {:.4}",
            rms(&a),
            rms(&diff),
            20.0 * (rms(&a) / rms(&diff).max(1e-9)).log10(),
            diff.iter().fold(0.0f32, |m, v| m.max(v.abs()))
        );
    }
}
