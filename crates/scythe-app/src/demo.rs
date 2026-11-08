//! Procedurally generated demo song matching the redesign mockup: six stems at 168 bpm,
//! bars 33–48, with verse/chorus/solo markers and a loop over the chorus.

use std::f32::consts::TAU;
use std::path::{Path, PathBuf};

use scythe_core::units::db_to_gain;
use scythe_core::{Color, FxSlot, Item, Marker, Project, Take, TempoMap, TimeRange};

const BPM: f64 = 168.0;
const FIRST_BAR: i64 = 33;
const BARS: usize = 16;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
    }
}

/// E minor-ish progression, one root per two bars, as MIDI notes.
const ROOTS: [u8; 8] = [28, 28, 24, 24, 31, 31, 26, 26];

fn hz(midi: f32) -> f32 {
    440.0 * 2f32.powf((midi - 69.0) / 12.0)
}

struct Synth {
    sr: f32,
    beat: f32,
    out: Vec<f32>,
}

impl Synth {
    fn new(sr: u32) -> Self {
        let beat = (60.0 / BPM) as f32;
        let len = (beat * 4.0 * BARS as f32 * sr as f32) as usize;
        Self { sr: sr as f32, beat, out: vec![0.0; len] }
    }

    fn at(&self, beats: f32) -> usize {
        (beats * self.beat * self.sr) as usize
    }

    fn add(&mut self, start_beat: f32, secs: f32, mut f: impl FnMut(f32) -> f32) {
        let start = self.at(start_beat);
        let n = (secs * self.sr) as usize;
        for i in 0..n {
            let Some(s) = self.out.get_mut(start + i) else { break };
            *s += f(i as f32 / self.sr);
        }
    }

    fn normalize(mut self, peak: f32) -> Vec<f32> {
        let max = self.out.iter().fold(0f32, |m, s| m.max(s.abs())).max(1e-6);
        for s in &mut self.out {
            *s *= peak / max;
        }
        self.out
    }
}

fn saw(phase: f32, harmonics: u32) -> f32 {
    (1..=harmonics).map(|k| (phase * TAU * k as f32).sin() / k as f32).sum::<f32>() * 0.6
}

fn drums(sr: u32) -> Vec<f32> {
    let mut s = Synth::new(sr);
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for bar in 0..BARS {
        for beat in 0..4 {
            let b = (bar * 4 + beat) as f32;
            if beat % 2 == 0 {
                s.add(b, 0.35, |t| {
                    let f = 45.0 + 90.0 * (-t * 30.0).exp();
                    (TAU * f * t).sin() * (-t * 9.0).exp()
                });
            } else {
                let mut noise = Rng(rng.0 ^ b as u64);
                s.add(b, 0.25, |t| (noise.next() * 0.7 + (TAU * 185.0 * t).sin() * 0.4) * (-t * 18.0).exp() * 0.7);
            }
            if bar % 4 == 3 && beat == 3 {
                s.add(b + 0.5, 0.25, |t| (TAU * (55.0 + 60.0 * (-t * 30.0).exp()) * t).sin() * (-t * 9.0).exp());
            }
            for half in [0.0, 0.5] {
                let mut prev = 0.0;
                let mut noise = Rng(rng.0.wrapping_add((b * 7.0 + half) as u64));
                s.add(b + half, 0.06, |t| {
                    let n = noise.next();
                    let hp = n - prev;
                    prev = n;
                    hp * (-t * 70.0).exp() * 0.25
                });
            }
        }
        rng.next();
    }
    s.normalize(0.8)
}

fn bass(sr: u32) -> Vec<f32> {
    let mut s = Synth::new(sr);
    for bar in 0..BARS {
        let root = ROOTS[(bar / 2) % ROOTS.len()] as f32;
        for eighth in 0..8 {
            let f = hz(root + if eighth == 6 { 7.0 } else { 0.0 });
            let mut phase = 0.0;
            let sr = s.sr;
            s.add((bar * 4) as f32 + eighth as f32 * 0.5, s.beat * 0.48, |t| {
                phase = (phase + f / sr) % 1.0;
                saw(phase, 10) * (-t * 4.0).exp()
            });
        }
    }
    s.normalize(0.7)
}

fn guitar(sr: u32, detune: f32, seed: u64) -> Vec<f32> {
    let mut s = Synth::new(sr);
    let mut rng = Rng(seed);
    for bar in 0..BARS {
        let root = ROOTS[(bar / 2) % ROOTS.len()] as f32 + 12.0;
        let chorus = bar >= 8;
        let hits: &[f32] = if chorus { &[0.0, 2.0] } else { &[0.0, 0.5, 1.0, 1.5, 2.0, 2.5, 3.0, 3.5] };
        let len = if chorus { s.beat * 1.95 } else { s.beat * 0.4 };
        for &h in hits {
            let freqs = [hz(root) * detune, hz(root + 7.0) * detune, hz(root + 12.0) * detune];
            let mut phases = [rng.next().abs(), rng.next().abs(), rng.next().abs()];
            let sr = s.sr;
            let decay = if chorus { 1.2 } else { 9.0 };
            s.add((bar * 4) as f32 + h, len, |t| {
                let mut v = 0.0;
                for (p, f) in phases.iter_mut().zip(freqs) {
                    *p = (*p + f / sr) % 1.0;
                    v += saw(*p, 14);
                }
                (v * 2.5).tanh() * (-t * decay).exp().max(0.15)
            });
        }
    }
    s.normalize(0.6)
}

fn lead(sr: u32) -> Vec<f32> {
    let mut s = Synth::new(sr);
    let melody: [(f32, f32, u8); 10] = [
        (32.0, 1.0, 64),
        (33.0, 1.0, 67),
        (34.0, 2.0, 71),
        (36.0, 1.5, 69),
        (37.5, 2.5, 67),
        (40.0, 1.0, 66),
        (41.0, 1.0, 67),
        (42.0, 2.0, 71),
        (46.0, 4.0, 76),
        (52.0, 6.0, 74),
    ];
    for (start, beats, note) in melody {
        let f = hz(note as f32);
        let mut phase = 0.0;
        let sr = s.sr;
        let len = beats * s.beat;
        s.add(start, len, |t| {
            let vib = 1.0 + 0.006 * (TAU * 5.5 * t).sin() * (t * 2.0).min(1.0);
            phase = (phase + f * vib / sr) % 1.0;
            let env = (t * 40.0).min(1.0) * (1.0 - (t / len).powi(4));
            ((phase * TAU).sin() + 0.3 * (phase * TAU * 2.0).sin() + 0.15 * (phase * TAU * 3.0).sin()) * env
        });
    }
    s.normalize(0.6)
}

fn pad(sr: u32) -> Vec<f32> {
    let mut s = Synth::new(sr);
    for chord in 0..BARS / 2 {
        let root = ROOTS[chord % ROOTS.len()] as f32 + 36.0;
        let notes = [root, root + 3.0, root + 7.0, root + 10.0];
        let len = s.beat * 8.0;
        let sr = s.sr;
        let mut phases = [0.0f32; 4];
        s.add((chord * 8) as f32, len, |t| {
            let env = (t / 0.4).min(1.0) * (1.0 - (t / len).powi(6));
            let vowel = 0.6 + 0.4 * (TAU * 0.7 * t).sin();
            let mut v = 0.0;
            for (p, n) in phases.iter_mut().zip(notes) {
                *p = (*p + hz(n) / sr) % 1.0;
                v += (*p * TAU).sin() + 0.4 * vowel * (*p * TAU * 3.0).sin();
            }
            v * env
        });
    }
    s.normalize(0.55)
}

fn write_wav(path: &Path, sr: u32, samples: &[f32]) -> Result<(), hound::Error> {
    let spec =
        hound::WavSpec { channels: 1, sample_rate: sr, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec)?;
    for &s in samples {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)?;
    }
    w.finalize()
}

/// Scythe's own data directory (`$XDG_DATA_HOME/scythe`).
pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("scythe")
}

fn demo_dir() -> PathBuf {
    data_dir().join("demo")
}

/// Patches the f32 at word `index` of a state chunk.
fn with_f32(mut state: Vec<u8>, index: usize, value: f32) -> Vec<u8> {
    state[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
    state
}

/// Built-in effects for the demo tracks, set up for each role; names without a built-in
/// equivalent are left out.
fn fx(names: &[(&str, bool)], irs: &DemoIrs) -> Vec<FxSlot> {
    use scythe_engine::builtin::{Builtin, eq_state, reverb_state};
    let db = |v: f32| 10f32.powf(v / 20.0);
    names
        .iter()
        .filter_map(|&(name, enabled)| {
            let slot = match name {
                "EQ" if irs.guitar_eq => Builtin::Eq.slot_with(&eq_state(&[
                    (4, 90.0, 1.0, 0.8),
                    (8, 2500.0, 1.4, 1.2),
                    (3, 6500.0, 1.0, 0.8),
                ])),
                "EQ" => Builtin::Eq.slot_with(&eq_state(&[
                    (0, 80.0, 1.26, 0.8),
                    (8, 400.0, 0.8, 1.5),
                    (1, 8000.0, 1.26, 0.8),
                ])),
                "Noise gate" => Builtin::Gate.slot_with(&with_f32(Builtin::Gate.default_state(), 2, db(-55.0))),
                "Compressor" | "Bus compressor" => {
                    let s = with_f32(Builtin::Compressor.default_state(), 2, db(-18.0));
                    Builtin::Compressor.slot_with(&with_f32(s, 3, 2.0 / 99.0))
                }
                "Delay" => Builtin::Delay.slot_with(&with_f32(Builtin::Delay.default_state(), 12, 0.35)),
                "Cab IR" => Builtin::Reverb.slot_with(&reverb_state(Some(&irs.cab), 1.0, 0.0)),
                "Plate reverb" => Builtin::Reverb.slot_with(&reverb_state(Some(&irs.plate), 0.2, 1.0)),
                _ => return None,
            };
            Some(FxSlot { enabled, ..slot })
        })
        .collect()
}

struct DemoIrs {
    cab: PathBuf,
    plate: PathBuf,
    guitar_eq: bool,
}

/// A speaker-cabinet-like impulse response (a short, low-passed burst) and a plate-like one
/// (two seconds of decaying noise, different per channel).
fn write_irs(dir: &std::path::Path, sample_rate: u32) -> Result<(PathBuf, PathBuf), hound::Error> {
    let cab = dir.join("demo-cab-ir.wav");
    let plate = dir.join("demo-plate-ir.wav");
    let sr = sample_rate as f32;
    let noise = |i: u64, seed: u64| {
        (((i ^ seed).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    };
    if !cab.exists() {
        let n = (sr * 0.03) as usize;
        let mut lp = 0.0f32;
        let samples: Vec<f32> = (0..n)
            .map(|i| {
                lp += 0.25 * (noise(i as u64, 1) * (-(i as f32) / (sr * 0.004)).exp() - lp);
                lp * 1.6
            })
            .collect();
        write_wav(&cab, sample_rate, &samples)?;
    }
    if !plate.exists() {
        let spec =
            hound::WavSpec { channels: 2, sample_rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&plate, spec)?;
        let n = (sr * 2.0) as usize;
        for i in 0..n {
            let env = (-(i as f32) / (sr * 0.45)).exp() * 0.25;
            for seed in [7u64, 13] {
                w.write_sample((noise(i as u64, seed) * env * 32767.0) as i16)?;
            }
        }
        w.finalize()?;
    }
    Ok((cab, plate))
}

pub fn build(sample_rate: u32) -> Result<Project, hound::Error> {
    let dir = demo_dir();
    std::fs::create_dir_all(&dir)?;
    let tempo = TempoMap::constant(BPM, 4, 4);
    let bar = |b: f64| tempo.bar_start(FIRST_BAR) + (b - FIRST_BAR as f64) * tempo.bar_seconds();

    struct Def {
        name: &'static str,
        file: &'static str,
        color: u32,
        db: f64,
        pan: f64,
        render: fn(u32) -> Vec<f32>,
        items: &'static [(f64, f64, &'static str)],
        fx: &'static [(&'static str, bool)],
    }
    let defs = [
        Def {
            name: "Drums",
            file: "drums.wav",
            color: 0x66d9ef,
            db: -1.2,
            pan: 0.0,
            render: drums,
            items: &[(33.0, 16.0, "Drums_take3")],
            fx: &[("Transient shaper", true), ("Bus compressor", true), ("EQ", true)],
        },
        Def {
            name: "Bass",
            file: "bass.wav",
            color: 0xa6e22e,
            db: -3.4,
            pan: 0.0,
            render: bass,
            items: &[(33.0, 8.0, "Bass_DI"), (41.0, 8.0, "Bass_DI_chorus")],
            fx: &[("Amp sim", true), ("Compressor", true)],
        },
        Def {
            name: "Guitar L · 7-str",
            file: "guitar_l.wav",
            color: 0xe6db74,
            db: -2.8,
            pan: -0.8,
            render: |sr| guitar(sr, 1.0, 7),
            items: &[(33.0, 8.0, "Rhythm_L_v2"), (41.0, 8.0, "Rhythm_L_chorus")],
            fx: &[("Noise gate", true), ("Amp sim", true), ("Cab IR", true), ("EQ", true), ("Compressor", false)],
        },
        Def {
            name: "Guitar R · 7-str",
            file: "guitar_r.wav",
            color: 0xfd971f,
            db: -2.8,
            pan: 0.8,
            render: |sr| guitar(sr, 1.003, 11),
            items: &[(33.0, 8.0, "Rhythm_R_v2"), (41.0, 8.0, "Rhythm_R_chorus")],
            fx: &[("Noise gate", true), ("Amp sim", true), ("Cab IR", true), ("EQ", true), ("Compressor", true)],
        },
        Def {
            name: "Lead",
            file: "lead.wav",
            color: 0xf92672,
            db: -4.5,
            pan: 0.1,
            render: lead,
            items: &[(41.0, 4.0, "Lead_solo_t2"), (45.5, 3.5, "Lead_outro")],
            fx: &[("Overdrive", true), ("Delay", true), ("Plate reverb", true), ("EQ", true)],
        },
        Def {
            name: "Vocals",
            file: "vocals.wav",
            color: 0xae81ff,
            db: -1.9,
            pan: 0.0,
            render: pad,
            items: &[(34.0, 6.0, "Vox_verse2"), (41.0, 7.0, "Vox_chorus")],
            fx: &[
                ("Noise gate", true),
                ("EQ", true),
                ("Compressor", true),
                ("De-esser", true),
                ("Plate reverb", true),
                ("Limiter", true),
            ],
        },
    ];

    let (cab_ir, plate_ir) = write_irs(&dir, sample_rate)?;
    let mut p = Project { tempo: tempo.clone(), ..Project::default() };
    for d in &defs {
        let path = dir.join(d.file);
        if !path.exists() {
            write_wav(&path, sample_rate, &(d.render)(sample_rate))?;
        }
        let id = p.add_track(d.name);
        let t = p.track_mut(id).unwrap();
        t.color = Some(Color::from_hex(d.color));
        t.volume = db_to_gain(d.db);
        t.pan = d.pan;
        let irs = DemoIrs { cab: cab_ir.clone(), plate: plate_ir.clone(), guitar_eq: d.name.starts_with("Guitar") };
        t.fx = fx(d.fx, &irs);
        for &(start, len, name) in d.items {
            let mut take = Take::from_file(path.clone());
            take.name = name.to_owned();
            take.source_offset = bar(start) - bar(FIRST_BAR as f64);
            t.items.push(Item::new(bar(start), len * tempo.bar_seconds(), take));
        }
    }
    p.tracks[2].armed = true;
    // Armed for show; monitoring would route the microphone to the speakers on first launch.
    p.tracks[2].monitor = false;
    // The pad swells into the chorus.
    let pad = p.tracks.len() - 1;
    let level = p.tracks[pad].volume;
    p.tracks[pad].volume = 1.0;
    let env = p.tracks[pad].envelope_mut(scythe_core::EnvelopeKind::Volume);
    env.points = vec![
        scythe_core::EnvPoint::new(bar(33.0), level * 0.35),
        scythe_core::EnvPoint {
            shape: scythe_core::PointShape::SlowStartEnd,
            ..scythe_core::EnvPoint::new(bar(39.0), level * 0.35)
        },
        scythe_core::EnvPoint::new(bar(41.0), level),
    ];
    p.tracks[4].muted = true;
    p.markers = vec![
        Marker {
            index: 1,
            position: bar(33.0),
            name: "Verse 2".into(),
            color: Some(Color::from_hex(0x66d9ef)),
            region_end: None,
            lines: Default::default(),
        },
        Marker {
            index: 2,
            position: bar(41.0),
            name: "Chorus".into(),
            color: Some(Color::from_hex(0xfd971f)),
            region_end: None,
            lines: Default::default(),
        },
        Marker {
            index: 3,
            position: bar(45.0),
            name: "Solo".into(),
            color: Some(Color::from_hex(0xf92672)),
            region_end: None,
            lines: Default::default(),
        },
    ];
    p.time_selection = Some(TimeRange::new(bar(41.0), bar(45.0)));
    p.loop_enabled = true;
    p.cursor = bar(37.0) + tempo.beat_seconds();
    Ok(p)
}
