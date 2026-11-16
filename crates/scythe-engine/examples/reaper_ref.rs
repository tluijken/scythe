//! Null test against REAPER: writes small projects that isolate one behaviour (pan law, fade
//! shapes, sections, gain staging), lets REAPER render them with `-renderproject`, bounces the
//! same projects with scythe and prints both side by side.
//!
//! cargo run -p scythe-engine --example reaper_ref -- <work dir> [path to reaper]
//!
//! Without a REAPER binary only the scythe side is rendered. REAPER runs with `-cfgfile`
//! pointing into the work dir, so the user's own configuration is never touched.

use std::path::{Path, PathBuf};

use scythe_core::TimeRange;
use scythe_core::rpp::project_from_rpp;
use scythe_engine::MediaPool;
use scythe_engine::bounce::{BitDepth, bounce_to_wav};

const SR: u32 = 48000;
const PROBES: &[f64] = &[0.1, 0.25, 0.5, 0.75, 0.9];

struct Case {
    name: String,
    project: String,
}

fn write_wav(path: &Path, channels: u16, f: impl Fn(usize, u16) -> f32, frames: usize) {
    let spec =
        hound::WavSpec { channels, sample_rate: SR, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    for i in 0..frames {
        for c in 0..channels {
            w.write_sample(f(i, c)).unwrap();
        }
    }
    w.finalize().unwrap();
}

fn project(dir: &Path, name: &str, header: &str, tracks: &str) -> Case {
    let out = dir.join(format!("{name}.reaper.wav"));
    let project = format!(
        r#"<REAPER_PROJECT 0.1 "7.73/linux-x86_64" 1762500000
  SAMPLERATE {SR} 0 0
  {header}
  SELECTION 0 1
  RENDER_FILE "{}"
  RENDER_PATTERN ""
  RENDER_FMT 0 2 {SR}
  RENDER_1X 0
  RENDER_RANGE 2 0 0 0 1000
  RENDER_RESAMPLE 3 0 1
  RENDER_ADDTOPROJ 0
  RENDER_STEMS 0
  RENDER_DITHER 0
  <RENDER_CFG
    ZXZhdyAAAQ==
  >
  MASTER_VOLUME 1 0 -1 -1 1
{tracks}>
"#,
        out.display()
    );
    Case { name: name.to_owned(), project }
}

fn track(volpan: &str, item: &str) -> String {
    format!("  <TRACK\n    NAME t\n    VOLPAN {volpan}\n    <ITEM\n{item}    >\n  >\n")
}

fn item(file: &str, extra: &str) -> String {
    format!(
        "      POSITION 0\n      LENGTH 1\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n{extra}      <SOURCE WAVE\n        FILE \"{file}\"\n      >\n"
    )
}

fn cases(dir: &Path) -> Vec<Case> {
    let dc_stereo = dir.join("dc_stereo.wav");
    let dc_mono = dir.join("dc_mono.wav");
    let ramp = dir.join("ramp.wav");
    write_wav(&dc_stereo, 2, |_, _| 0.5, SR as usize * 2);
    write_wav(&dc_mono, 1, |_, _| 0.5, SR as usize * 2);
    write_wav(&ramp, 1, |i, _| i as f32 / SR as f32, SR as usize);
    let (st, mo, ra) = (dc_stereo.display().to_string(), dc_mono.display().to_string(), ramp.display().to_string());
    let default_law = "PANLAW 1\n  PANLAWFLAGS 3\n  PANMODE 3";

    let mut out = Vec::new();
    for pan in ["-1", "-0.5", "0", "0.5", "1"] {
        out.push(project(
            dir,
            &format!("stereo_pan_{pan}"),
            default_law,
            &track(&format!("1 {pan} -1 -1 1"), &item(&st, "")),
        ));
        out.push(project(
            dir,
            &format!("mono_pan_{pan}"),
            default_law,
            &track(&format!("1 {pan} -1 -1 1"), &item(&mo, "")),
        ));
    }
    for law in ["1", "0.841", "0.707", "0.5", "1.414", "2"] {
        for flags in ["0", "2", "3"] {
            let header = format!("PANLAW {law}\n  PANLAWFLAGS {flags}\n  PANMODE 3");
            for pan in ["0", "0.25", "0.5", "0.75"] {
                out.push(project(
                    dir,
                    &format!("law{law}_f{flags}_pan_{pan}"),
                    &header,
                    &track(&format!("1 {pan} -1 -1 1"), &item(&st, "")),
                ));
            }
        }
    }
    for shape in 0..=6 {
        let fade = format!("{shape} 1 0 {shape} 0 0 0");
        let it = item(&st, "").replace("FADEIN 1 0 0 1 0 0 0", &format!("FADEIN {fade}"));
        out.push(project(dir, &format!("fadein_shape_{shape}"), default_law, &track("1 0 -1 -1 1", &it)));
    }
    for curve in ["-1", "-0.5", "0.5", "1"] {
        for shape in 0..=6 {
            let it = item(&st, "").replace("FADEIN 1 0 0 1 0 0 0", &format!("FADEIN {shape} 1 0 {shape} 0 {curve} 0"));
            out.push(project(
                dir,
                &format!("fadein_shape_{shape}_curve_{curve}"),
                default_law,
                &track("1 0 -1 -1 1", &it),
            ));
        }
    }
    let it = item(&st, "").replace("FADEOUT 1 0 0 1 0 0 0", "FADEOUT 1 1 0 1 0 0 0");
    out.push(project(dir, "fadeout_shape_1", default_law, &track("1 0 -1 -1 1", &it)));
    let section = format!(
        "      POSITION 0\n      LENGTH 1\n      FADEIN 1 0 0 1 0 0 0\n      FADEOUT 1 0 0 1 0 0 0\n      <SOURCE SECTION\n        LENGTH 1\n        MODE 2\n        STARTPOS 0\n        OVERLAP 0.01\n        <SOURCE WAVE\n          FILE \"{ra}\"\n        >\n      >\n"
    );
    out.push(project(dir, "reverse_ramp", default_law, &track("1 -1 -1 -1 1", &section)));
    out.push(project(dir, "forward_ramp", default_law, &track("1 -1 -1 -1 1", &item(&ra, ""))));
    let env = |name: &str, extra: &str, pts: &str| {
        format!(
            "    <{name}\n      ACT 1 -1\n      VIS 1 1 1\n      LANEHEIGHT 0 0\n      ARM 0\n      DEFSHAPE 0 -1 -1\n{extra}{pts}    >\n"
        )
    };
    let with_env =
        |volpan: &str, env: String| track(volpan, &item(&st, "")).replacen("    <ITEM", &format!("{env}    <ITEM"), 1);
    for v in ["0.25", "0.5", "1", "2"] {
        let pts = format!("      PT 0 {v} 0\n");
        out.push(project(
            dir,
            &format!("env_vol_amp_{v}"),
            default_law,
            &with_env("1 0 -1 -1 1", env("VOLENV2", "", &pts)),
        ));
        out.push(project(
            dir,
            &format!("env_vol_fader_{v}"),
            default_law,
            &with_env("1 0 -1 -1 1", env("VOLENV2", "      VOLTYPE 1\n", &pts)),
        ));
    }
    for v in ["0.1", "0.3", "0.6", "0.8"] {
        let pts = format!("      PT 0 {v} 0\n");
        out.push(project(
            dir,
            &format!("env_vol_fader_{v}"),
            default_law,
            &with_env("1 0 -1 -1 1", env("VOLENV2", "      VOLTYPE 1\n", &pts)),
        ));
    }
    out.push(project(
        dir,
        "env_vol_half_fader_half",
        default_law,
        &with_env("0.5 0 -1 -1 1", env("VOLENV2", "", "      PT 0 0.5 0\n")),
    ));
    out.push(project(
        dir,
        "env_vol_inactive",
        default_law,
        &with_env("1 0 -1 -1 1", env("VOLENV2", "", "      PT 0 0.5 0\n").replace("ACT 1 -1", "ACT 0 -1")),
    ));
    for shape in 0..=5 {
        let pts = format!("      PT 0 0 {shape}\n      PT 1 1 0\n");
        out.push(project(
            dir,
            &format!("env_shape_{shape}"),
            default_law,
            &with_env("1 0 -1 -1 1", env("VOLENV2", "", &pts)),
        ));
    }
    for tension in ["-0.5", "0.5"] {
        let pts = format!("      PT 0 0 5 0 0 0 {tension}\n      PT 1 1 0\n");
        out.push(project(
            dir,
            &format!("env_bezier_{tension}"),
            default_law,
            &with_env("1 0 -1 -1 1", env("VOLENV2", "", &pts)),
        ));
    }
    out.push(project(
        dir,
        "env_pan_0.5",
        default_law,
        &with_env("1 0 -1 -1 1", env("PANENV2", "", "      PT 0 0.5 0\n")),
    ));
    out.push(project(
        dir,
        "env_pan_0.5_track_pan_-0.5",
        default_law,
        &with_env("1 -0.5 -1 -1 1", env("PANENV2", "", "      PT 0 0.5 0\n")),
    ));
    out.push(project(dir, "env_mute_0", default_law, &with_env("1 0 -1 -1 1", env("MUTEENV", "", "      PT 0 0 1\n"))));
    out.push(project(dir, "env_mute_1", default_law, &with_env("1 0 -1 -1 1", env("MUTEENV", "", "      PT 0 1 1\n"))));
    out.push(project(
        dir,
        "env_vol_pre_fx",
        default_law,
        &with_env("1 0 -1 -1 1", env("VOLENV", "", "      PT 0 0.5 0\n")),
    ));
    out.push(project(dir, "env_trim", default_law, &with_env("1 0 -1 -1 1", env("VOLENV3", "", "      PT 0 0.5 0\n"))));
    out.push(project(
        dir,
        "take_pan_0.5",
        default_law,
        &track("1 0 -1 -1 1", &item(&st, "      TAKEVOLPAN 0.5 1 -1\n")),
    ));
    out.push(project(
        dir,
        "item_vol_half_take_vol_half",
        default_law,
        &track("1 0 -1 -1 1", &item(&st, "      VOLPAN 0.5 0 1 -1\n      TAKEVOLPAN 0 0.5 -1\n")),
    ));

    let quarter = dir.join("dc_quarter.wav");
    write_wav(&quarter, 2, |_, _| 0.25, SR as usize * 2);
    let qu = quarter.display().to_string();
    let lane_item = |file: &str, ypos: &str| item(file, &format!("      YPOS {ypos}\n"));
    let two_items = |extra: &str, a: &str, b: &str| {
        format!("  <TRACK\n    NAME t\n    VOLPAN 1 0 -1 -1 1\n{extra}    <ITEM\n{a}    >\n    <ITEM\n{b}    >\n  >\n")
    };
    let lanes = |flags: &str| format!("    FIXEDLANES {flags} 0 0 0 0\n");
    let cases = [
        ("overlap_no_lanes", String::new(), item(&st, ""), item(&qu, "")),
        ("lanes9_top_and_second", lanes("9"), lane_item(&st, "0 0.5 2"), lane_item(&qu, "0.5 0.5 2")),
        (
            "lanes9_second_only",
            lanes("9"),
            lane_item(&qu, "0.5 0.5 2"),
            lane_item(&qu, "0.5 0.5 2").replace("POSITION 0", "POSITION 5"),
        ),
        ("lanes1_top_and_second", lanes("1"), lane_item(&st, "0 0.5 2"), lane_item(&qu, "0.5 0.5 2")),
        ("lanes0_top_and_second", lanes("0"), lane_item(&st, "0 0.5 2"), lane_item(&qu, "0.5 0.5 2")),
        ("lanes8_top_and_second", lanes("8"), lane_item(&st, "0 0.5 2"), lane_item(&qu, "0.5 0.5 2")),
        (
            "lanes9_solo_second",
            format!("{}    LANESOLO 2 0 0 0\n", lanes("9")),
            lane_item(&st, "0 0.5 2"),
            lane_item(&qu, "0.5 0.5 2"),
        ),
        ("ypos_without_fixedlanes", String::new(), lane_item(&st, "0 0.5 2"), lane_item(&qu, "0.5 0.5 2")),
    ];
    for (name, extra, a, b) in cases {
        out.push(project(dir, name, default_law, &two_items(&extra, &a, &b)));
    }
    out
}

fn read(path: &Path) -> Option<Vec<(f32, f32)>> {
    let mut r = hound::WavReader::open(path).ok()?;
    let spec = r.spec();
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().map(Result::unwrap).collect(),
        hound::SampleFormat::Int => {
            let scale = (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>().map(|s| s.unwrap() as f32 / scale).collect()
        }
    };
    Some(samples.chunks(spec.channels as usize).map(|c| (c[0], *c.get(1).unwrap_or(&c[0]))).collect())
}

fn probe(frames: &[(f32, f32)]) -> String {
    PROBES
        .iter()
        .map(|t| {
            let (l, r) = frames.get((t * SR as f64) as usize).copied().unwrap_or((f32::NAN, f32::NAN));
            format!("{l:>7.4}/{r:<7.4}")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("usage: reaper_ref <work dir> [reaper]"));
    let reaper = args.next();
    std::fs::create_dir_all(&dir).unwrap();
    let cfg = dir.join("reaper-cfg").join("reaper.ini");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();

    for case in cases(&dir) {
        let rpp = dir.join(format!("{}.RPP", case.name));
        std::fs::write(&rpp, &case.project).unwrap();
        let reaper_out = dir.join(format!("{}.reaper.wav", case.name));
        if let Some(reaper) = &reaper
            && !reaper_out.exists()
        {
            let status = scythe_engine::process::command(reaper)
                .args(["-cfgfile", &cfg.display().to_string(), "-nosplash", "-renderproject"])
                .arg(&rpp)
                .status();
            if let Err(e) = status {
                eprintln!("{}: failed to run REAPER: {e}", case.name);
            }
        }

        let mut project = project_from_rpp(&case.project).unwrap();
        project.path = Some(rpp.clone());
        let pool = MediaPool::default();
        for t in &project.tracks {
            for it in &t.items {
                if let Some(f) = it.take().and_then(|t| t.source.file()) {
                    pool.load(f, SR).unwrap();
                }
            }
        }
        let ours = dir.join(format!("{}.scythe.wav", case.name));
        bounce_to_wav(&project, &pool, TimeRange::new(0.0, 1.0), SR, BitDepth::Float32, &ours, |_| {}).unwrap();

        let mine = read(&ours).unwrap();
        match read(&reaper_out) {
            Some(theirs) => {
                let diff = mine
                    .iter()
                    .zip(&theirs)
                    .map(|(a, b)| (a.0 - b.0).abs().max((a.1 - b.1).abs()))
                    .fold(0.0f32, f32::max);
                let verdict = if diff < 1e-3 { "match" } else { "DIFFERS" };
                println!("{:<36} {verdict:<8} max |diff| {diff:.4}", case.name);
                if diff >= 1e-3 {
                    println!("    scythe {}", probe(&mine));
                    println!("    reaper {}", probe(&theirs));
                }
            }
            None => println!("{:<36} scythe {}", case.name, probe(&mine)),
        }
    }
}
