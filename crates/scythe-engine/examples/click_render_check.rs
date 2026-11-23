//! Renders a project's click track alone (every other track muted, the master playing it) and
//! checks the clicks it plays against a list of `time accent` lines (`click_tempo`'s
//! `CLICK_DUMP`): each listed click should sound once, on time, and nothing else should.
//!
//! cargo run --release -p scythe-engine --example click_render_check -- <project.RPP> <track> <clicks.txt> <start> <end>
use scythe_core::TimeRange;
use scythe_core::rpp::read_project;
use scythe_engine::MediaPool;
use scythe_engine::bounce::{BitDepth, bounce_to_wav};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("project");
    let track = args.next().expect("track");
    let list = args.next().expect("clicks list");
    let start: f64 = args.next().and_then(|s| s.parse().ok()).expect("start");
    let end: f64 = args.next().and_then(|s| s.parse().ok()).expect("end");
    let mut project = read_project(path.as_ref()).unwrap();
    for t in &mut project.tracks {
        t.muted = t.name != track;
    }
    let sr = 48_000u32;
    let wav = std::env::temp_dir().join(format!("click-render-{}.wav", std::process::id()));
    bounce_to_wav(&project, &MediaPool::default(), TimeRange::new(start, end), sr, BitDepth::Float32, &wav, |_| {})
        .expect("render");
    let audio = scythe_decode::decode(&wav, false).expect("read the render");
    let _ = std::fs::remove_file(&wav);
    let mono: Vec<f32> =
        (0..audio.channels[0].len()).map(|i| audio.channels.iter().map(|c| c[i]).sum::<f32>()).collect();

    // Onsets: the first sample above a tenth of the loudest click after 30 ms of near silence.
    let peak = mono.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    let quiet = (sr as f64 * 0.03) as usize;
    let mut heard = Vec::new();
    let mut silent = quiet;
    for (i, s) in mono.iter().enumerate() {
        if s.abs() > peak * 0.1 {
            if silent >= quiet {
                // Its pitch: zero crossings in the 10 ms after the onset.
                let seg = &mono[i..(i + sr as usize / 100).min(mono.len())];
                let crossings = seg.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
                heard.push((start + i as f64 / sr as f64, crossings));
            }
            silent = 0;
        } else {
            silent += 1;
        }
    }
    let wanted: Vec<(f64, bool)> = std::fs::read_to_string(&list)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            Some((w.next()?.parse().ok()?, w.next()? == "1"))
        })
        .filter(|&(t, _): &(f64, bool)| t >= start + 0.01 && t < end - 0.05)
        .collect();
    // The accent is the higher of the metronome's two sounds.
    let (lo, hi) = heard.iter().fold((usize::MAX, 0), |(lo, hi), h| (lo.min(h.1), hi.max(h.1)));
    let mut accents_off = 0;
    let mut worst: f64 = 0.0;
    let mut missing = 0;
    for &(w, accent) in &wanted {
        let nearest = heard.iter().min_by(|a, b| (a.0 - w).abs().total_cmp(&(b.0 - w).abs())).unwrap();
        let near = (nearest.0 - w).abs();
        if near <= 0.005 && (nearest.1 * 2 > lo + hi) != accent {
            accents_off += 1;
            if accents_off <= 5 {
                println!("  click at {w:.4}: accent {} where the recording has {accent}", !accent);
            }
        }
        if near > 0.005 {
            missing += 1;
            if missing <= 5 {
                println!("  no click heard at {w:.4} (nearest {:.1} ms)", near * 1000.0);
            }
        } else {
            worst = worst.max(near);
        }
    }
    let extra: Vec<f64> =
        heard.iter().map(|h| h.0).filter(|h| wanted.iter().all(|w| (w.0 - h).abs() > 0.005)).collect();
    for e in extra.iter().take(5) {
        println!("  click heard at {e:.4} that the recording does not have");
    }
    println!(
        "{start:.1}..{end:.1}: {} recorded clicks, {} heard, {missing} missing, {} extra, worst {:.2} ms, accents off {accents_off}",
        wanted.len(),
        heard.len(),
        extra.len(),
        worst * 1000.0
    );
}
