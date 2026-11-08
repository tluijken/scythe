//! Measures ReaDelay in REAPER: renders an impulse through one tap with given settings and
//! prints where the echoes land and how loud they are.
//!
//! cargo run -p scythe-engine --example readelay_ref -- <work dir> <path to reaper>

use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;

const SR: u32 = 48000;

/// Tap fields after the leading word: enabled, time, musical, feedback, lowpass, hipass,
/// resolution, width, volume, pan.
fn chunk(wet: f32, dry: f32, tap: [f32; 10]) -> Vec<u8> {
    let mut d = Vec::new();
    for v in [0u32, 0, 1, 44, 2, 0] {
        d.extend(v.to_le_bytes());
    }
    d.extend(wet.to_le_bytes());
    d.extend(dry.to_le_bytes());
    d.extend(0f32.to_le_bytes());
    for v in tap {
        d.extend(v.to_le_bytes());
    }
    d
}

fn vst_lines(chunk: &[u8]) -> Vec<String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let mut h = Vec::new();
    h.extend(1919247468u32.to_le_bytes());
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
    vec![b64.encode(&h), b64.encode(chunk), b64.encode(b"\0\x10\0\0\0")]
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
    let reaper = args.next().expect("reaper");
    let impulse = dir.join("dl_impulse.wav");
    let spec =
        hound::WavSpec { channels: 1, sample_rate: SR, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(&impulse, spec).unwrap();
    for i in 0..SR as usize * 3 {
        w.write_sample(if i == 0 { 1.0f32 } else { 0.0 }).unwrap();
    }
    w.finalize().unwrap();
    let cfg = dir.join("reaper-cfg").join("reaper.ini");
    // Defaults: enabled, time 0, musical 2/256, feedback -inf, lowpass 20k, hipass 0, 24 bit,
    // width 1, volume 1, pan centre.
    let base = [1.0, 0.0, 2.0 / 256.0, 0.0, 1.0, 0.0, 1.0, 1.0, 1.0, 0.5];
    let mut cases: Vec<(String, f32, f32, [f32; 10])> = vec![("default".into(), 0.5, 1.0, base)];
    let mut t = base;
    t[1] = 0.025;
    t[2] = 0.0;
    cases.push(("time250ms".into(), 1.0, 0.0, t));
    let mut t2 = t;
    t2[3] = 0.5;
    cases.push(("time250ms_fb0.5".into(), 1.0, 0.0, t2));
    let mut m = base;
    m[2] = 4.0 / 256.0;
    cases.push(("musical4".into(), 1.0, 0.0, m));
    let mut lp = t;
    lp[4] = 0.05;
    cases.push(("lp1000".into(), 1.0, 0.0, lp));
    let mut hp = t;
    hp[5] = 0.05;
    cases.push(("hp1000".into(), 1.0, 0.0, hp));
    let mut pan = t;
    pan[9] = 0.0;
    cases.push(("pan_left".into(), 1.0, 0.0, pan));
    for (name, wet, dry, tap) in cases {
        let out = dir.join(format!("dl_{name}.reaper.wav"));
        let lines = vst_lines(&chunk(wet, dry, tap)).iter().map(|l| format!("        {l}\n")).collect::<String>();
        let rpp = format!(
            "<REAPER_PROJECT 0.1 \"7.73/linux-x86_64\" 1762500000\n  SAMPLERATE {SR} 0 0\n  TEMPO 120 4 4\n  SELECTION 0 3\n  RENDER_FILE \"{out}\"\n  RENDER_PATTERN \"\"\n  RENDER_FMT 0 2 {SR}\n  RENDER_1X 0\n  RENDER_RANGE 2 0 0 0 1000\n  RENDER_RESAMPLE 3 0 1\n  RENDER_ADDTOPROJ 0\n  RENDER_STEMS 0\n  RENDER_DITHER 0\n  <RENDER_CFG\n    ZXZhdyAAAQ==\n  >\n  <TRACK\n    VOLPAN 1 0 -1 -1 1\n    <FXCHAIN\n      SHOW 0\n      LASTSEL 0\n      DOCKED 0\n      BYPASS 0 0 0\n      <VST \"VST: ReaDelay (Cockos)\" readelay.vst.so 0 \"\" 1919247468<56535472656C79726561646C61790000> \"\"\n{lines}      >\n      FLOATPOS 0 0 0 0\n      WAK 0 0\n    >\n    <ITEM\n      POSITION 0\n      LENGTH 3\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n      <SOURCE WAVE\n        FILE \"{imp}\"\n      >\n    >\n  >\n>\n",
            out = out.display(),
            imp = impulse.display(),
        );
        let rpp_path = dir.join(format!("dl_{name}.RPP"));
        std::fs::write(&rpp_path, rpp).unwrap();
        if !out.exists() {
            let _ = Command::new(&reaper)
                .args(["-cfgfile", &cfg.display().to_string(), "-nosplash", "-renderproject"])
                .arg(&rpp_path)
                .status();
        }
        let Some(s) = read_left(&out) else {
            println!("{name}: not rendered");
            continue;
        };
        let right = {
            let mut r = hound::WavReader::open(&out).unwrap();
            let v: Vec<f32> = r.samples::<f32>().map(Result::unwrap).collect();
            v.chunks(2).map(|c| c[1]).collect::<Vec<f32>>()
        };
        let peaks: Vec<String> = s
            .iter()
            .enumerate()
            .filter(|(_, v)| v.abs() > 1e-3)
            .take(6)
            .map(|(i, v)| format!("{i}:{v:.4}/{:.4}", right[i]))
            .collect();
        let energy: f32 = s.iter().map(|v| v * v).sum();
        println!("{name:<16} {} | energy {energy:.4}", peaks.join(" "));
        let mut ours = scythe_engine::delay::ReaDelayCompat::new(
            scythe_engine::delay::ReaDelaySettings::parse(&chunk(wet, dry, tap)).unwrap(),
            SR,
        );
        let n = s.len();
        let (mut l, mut r) = (vec![0.0f32; n], vec![0.0f32; n]);
        l[0] = 1.0;
        r[0] = 1.0;
        for (a, b) in l.chunks_mut(512).zip(r.chunks_mut(512)) {
            use scythe_engine::fx::Effect;
            ours.process(
                a,
                b,
                &scythe_engine::fx::ProcessContext {
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
        let diff = l.iter().zip(&s).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        println!("{:<16} scythe max |diff| {diff:.5}", "");
    }
}
