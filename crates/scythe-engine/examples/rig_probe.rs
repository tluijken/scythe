//! Writes the projects that check a guitar rig against REAPER: the rig as a container, the
//! same effects in series on the track, an older NAM + ReaVerb chain and that chain converted
//! to a rig. REAPER renders each (`<name>.reaper.wav`); `null_check` compares Scythe with
//! them, and `rig_probe.lua` lists what REAPER loaded.
//!
//! cargo run -p scythe-engine --example rig_probe -- <work dir>

use std::path::{Path, PathBuf};

use scythe_core::{FxSlot, Item, Project, Take};
use scythe_engine::builtin::{Builtin, reverb_state};
use scythe_engine::fx::nam_slot;
use scythe_engine::rig::{self, AmpSettings, CabSettings, EqSettings, GateSettings, RigSettings};

const SR: u32 = 48000;

fn write_mono(path: &Path, samples: &[f32]) {
    let spec =
        hound::WavSpec { channels: 1, sample_rate: SR, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for &s in samples {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
}

/// Plucked notes over a quiet hiss, so the gate has something to close on.
fn guitar(seconds: f64) -> Vec<f32> {
    let n = (seconds * SR as f64) as usize;
    let mut x = 0x2545_f491u32;
    let mut noise = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        x as f32 / u32::MAX as f32 - 0.5
    };
    (0..n)
        .map(|i| {
            let t = i as f64 / SR as f64;
            let note = (t * 2.0).floor();
            let since = t - note * 0.5;
            let f = [110.0, 146.8, 196.0, 82.4][note as usize % 4];
            let pluck = if since < 0.35 {
                ((f * std::f64::consts::TAU * t).sin() * 0.6 + (f * 2.0 * std::f64::consts::TAU * t).sin() * 0.2)
                    * (-since * 9.0).exp()
            } else {
                0.0
            };
            pluck as f32 + noise() * 0.004
        })
        .collect()
}

/// A cabinet-like response: a few early taps and a decaying, darkening tail.
fn cab(seed: u32, len: usize) -> Vec<f32> {
    let mut x = seed;
    let mut prev = 0.0f32;
    (0..len)
        .map(|i| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let n = x as f32 / u32::MAX as f32 - 0.5;
            prev = prev * 0.7 + n * 0.3;
            let early = match i {
                0 => 0.6,
                7 => -0.35,
                23 => 0.2,
                _ => 0.0,
            };
            early + prev * (-(i as f32) / 600.0).exp() * 0.5
        })
        .collect()
}

fn project(dir: &Path, name: &str, source: &Path, fx: Vec<FxSlot>) -> PathBuf {
    let mut p = Project::default();
    let t = p.add_track("Guitar");
    let track = p.track_mut(t).unwrap();
    track.items.push(Item::new(0.0, 2.0, Take::from_file(source.to_path_buf())));
    track.fx = fx;
    let text = scythe_core::rpp::project_to_rpp(&p);
    let out = dir.join(format!("{name}.reaper.wav"));
    let render = format!(
        "  SAMPLERATE {SR} 1 0\n  RENDER_FILE \"{}\"\n  RENDER_PATTERN \"\"\n  RENDER_FMT 0 2 {SR}\n  RENDER_1X 0\n  \
         RENDER_RANGE 1 0 0 0 1000\n  RENDER_RESAMPLE 3 0 1\n  RENDER_ADDTOPROJ 0\n  RENDER_STEMS 0\n  \
         RENDER_DITHER 0\n  <RENDER_CFG\n    ZXZhdyAAAQ==\n  >\n",
        out.display()
    );
    let text: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("SAMPLERATE ") && !l.trim_start().starts_with("RENDER_"))
        .collect::<Vec<_>>()
        .join("\n");
    let (head, rest) = text.split_once('\n').unwrap();
    let path = dir.join(format!("{name}.RPP"));
    std::fs::write(&path, format!("{head}\n{render}{rest}\n")).unwrap();
    path
}

fn main() {
    let dir = PathBuf::from(std::env::args().nth(1).expect("work dir"));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("guitar.wav");
    write_mono(&source, &guitar(2.0));
    let (cab_a, cab_b) = (dir.join("cab_a.wav"), dir.join("cab_b.wav"));
    write_mono(&cab_a, &cab(0x1234_5678, 4800));
    write_mono(&cab_b, &cab(0x0bad_cafe, 3000));

    let settings = RigSettings {
        gate: GateSettings { enabled: true, threshold_db: -40.0 },
        amp: AmpSettings { enabled: true, model: None, input_db: 0.0, output_db: 0.0 },
        cab: CabSettings {
            ir: Some(cab_a.display().to_string()),
            ir2: Some(cab_b.display().to_string()),
            blend: 0.3,
            level_db: -2.0,
            ..CabSettings::default()
        },
        eq: EqSettings {
            low_cut: Some(90.0),
            high_cut: Some(7000.0),
            bass_db: 2.0,
            mid_db: -1.5,
            ..Default::default()
        },
        output_db: 1.5,
    };
    let rig = rig::new_rig(&settings);
    project(&dir, "rig", &source, vec![rig.clone()]);
    project(&dir, "series", &source, rig.container_children());

    let old = vec![nam_slot(None), Builtin::Reverb.slot_with(&reverb_state(Some(&cab_a), 1.0, 0.0))];
    project(&dir, "old", &source, old.clone());
    let mut converted = old;
    rig::convert(&mut converted, 0).expect("a NAM slot to convert");
    project(&dir, "converted", &source, converted);

    let mut bypassed = settings.clone();
    bypassed.cab.enabled = false;
    bypassed.gate.enabled = false;
    project(&dir, "bypassed", &source, vec![rig::new_rig(&bypassed)]);
    println!("wrote rig, series, old, converted and bypassed projects to {}", dir.display());

    // With a model and an impulse response: a project to try the rig window on, a rig on one
    // track and an older NAM + ReaVerb chain on another.
    let args: Vec<String> = std::env::args().skip(2).collect();
    if let [amp, ir, old_amp, ..] = &args[..] {
        let demo = RigSettings {
            amp: AmpSettings { model: Some(amp.clone()), input_db: 2.0, output_db: -3.0, ..Default::default() },
            cab: CabSettings { ir: Some(ir.clone()), ..CabSettings::default() },
            gate: GateSettings { enabled: true, threshold_db: -56.0 },
            ..RigSettings::default()
        };
        let mut p = Project::default();
        let t = p.add_track("Guitar");
        let track = p.track_mut(t).unwrap();
        track.items.push(Item::new(0.0, 2.0, Take::from_file(source.clone())));
        track.fx = vec![Builtin::Compressor.slot(), rig::new_rig(&demo)];
        let t = p.add_track("Old chain");
        p.track_mut(t).unwrap().fx = vec![
            nam_slot(Some(Path::new(old_amp))),
            Builtin::Reverb.slot_with(&reverb_state(Some(Path::new(ir)), 1.0, 0.0)),
        ];
        std::fs::write(dir.join("demo.RPP"), scythe_core::rpp::project_to_rpp(&p)).unwrap();
        println!("wrote demo.RPP");
    }
}
