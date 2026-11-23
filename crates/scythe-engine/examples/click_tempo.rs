//! Turns a project's recorded click into a tempo map and click sources: finds every click in the
//! items of the track named `track` (its time, and from its pitch whether it starts a bar),
//! fits tempo markers to them (constant stretches and linear ramps, REAPER's gradual tempo),
//! time signatures from the accents, then puts a click source over each stretch of clicks on
//! that track and mutes the recorded items. The project's timebase becomes time, so the audio
//! stays where it is. Prints how far each generated click lands from the recorded one.
//!
//! cargo run --release -p scythe-engine --example click_tempo -- <in.RPP> <track> <out.RPP>
use scythe_core::envelope::{EnvPoint, Envelope, EnvelopeKind, PointShape};
use scythe_core::model::{Item, Source};
use scythe_core::rpp::{Chunk, read_project, resolve_media_path, write_project};
use scythe_core::tempo::{TempoMap, TempoPoint};
use scythe_core::timebase::Timebase;

/// A generated beat may land this far from the recorded click: well under what a musician hears,
/// and wide enough that the onsets' jitter (±0.3 ms in an mp3) does not read as tempo changes.
const TOLERANCE: f64 = 0.003;

#[derive(Debug, Clone, Copy)]
struct Click {
    t: f64,
    accent: bool,
}

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().expect("project");
    let track_name = args.next().expect("track name");
    let output = args.next().expect("output project");
    let mut project = read_project(input.as_ref()).unwrap();
    let ti = project.tracks.iter().position(|t| t.name == track_name).expect("no such track");

    let mut clicks = Vec::new();
    for item in project.tracks[ti].items.iter().filter(|i| !i.muted) {
        let Some(take) = item.take() else { continue };
        let Some(file) = take.source.file() else { continue };
        let file = resolve_media_path(&project, file);
        let audio = scythe_decode::decode(&file, false).expect("decode");
        clicks.extend(item_clicks(item, take, &audio));
    }
    clicks.sort_by(|a, b| a.t.total_cmp(&b.t));
    clicks.dedup_by(|b, a| b.t - a.t < 0.03);
    // Some clicks accent two beats in a row (Iconoclasm's "one" and the beat after it): the
    // first starts the bar, the second is an accent a click source has no sound for.
    let accented: Vec<bool> = clicks.iter().map(|c| c.accent).collect();
    let prev_t: Vec<f64> = clicks.iter().map(|c| c.t).collect();
    for (k, c) in clicks.iter_mut().enumerate().skip(1) {
        if accented[k - 1] && c.t - prev_t[k - 1] < 1.0 {
            c.accent = false;
        }
    }
    let runs = split_runs(&clicks);
    // `CLICK_DUMP=file` writes every click's time and accent, to check the map elsewhere.
    if let Ok(dump) = std::env::var("CLICK_DUMP") {
        let text: String = runs.iter().flatten().map(|c| format!("{:.6} {}\n", c.t, u8::from(c.accent))).collect();
        std::fs::write(dump, text).expect("write the click dump");
    }
    println!("{} clicks in {} stretches", clicks.len(), runs.len());

    let mut points: Vec<TempoPoint> = Vec::new();
    let mut runs_done: Vec<&Vec<Click>> = Vec::new();
    // Where each stretch's first bar starts: its first click, or the bar line before a pickup.
    let mut bar_starts: Vec<f64> = Vec::new();
    let debug_at: Option<f64> = std::env::var("CLICK_DEBUG_AT").ok().and_then(|v| v.parse().ok());
    for run in &runs {
        let segs = fit(run);
        if let Some(at) = debug_at.filter(|&at| run[0].t <= at && at <= run.last().unwrap().t) {
            for s in segs.iter().filter(|s| (s.start - at).abs() < 8.0) {
                let err = segment(run, s.start, s.from, s.to, s.linear, None).0;
                println!(
                    "  seg {}..{} {:.4}..{:.4} {} {:.3}->{:.3} (click {:.4}..{:.4}) free-err {:.2} ms",
                    s.from,
                    s.to,
                    s.start,
                    s.end,
                    if s.linear { "ramp" } else { "const" },
                    s.b0,
                    s.b1,
                    run[s.from].t,
                    run[s.to].t,
                    err * 1000.0
                );
            }
        }
        // Where the previous stretch's last bar ends (the project start before the first), so
        // the bars before this stretch start after it.
        let after = match runs_done.last() {
            None => 0.0,
            Some(prev) => {
                let map = TempoMap { points: points.clone(), ..project.tempo.clone() };
                let tl = map.timeline();
                tl.bar_start(tl.bar_beat(prev.last().unwrap().t + 1e-6).bar + 1)
            }
        };
        let mine = run_points(run, &segs, after);
        bar_starts.push(mine[0].time);
        // The silence up to this stretch is whole one-beat bars at about 120 bpm, so every
        // signature change lands on a bar line: no bar is cut short, which REAPER only keeps
        // as it is when a marker allows a partial measure.
        let span = mine[0].time - after;
        if span > 0.05 {
            let beats = (span / 0.5).round().max(1.0);
            points.push(TempoPoint { time_sig: Some((1, 4)), ..TempoPoint::new(after, 60.0 * beats / span) });
        }
        points.extend(mine);
        runs_done.push(run);
    }
    project.tempo = TempoMap { points, ..project.tempo.clone() };
    project.timebase = Timebase::Time;

    // Check every recorded click against the beats the map now gives.
    let timeline = project.tempo.timeline();
    let mut worst_all: f64 = 0.0;
    for run in &runs {
        let (t0, t1) = (run[0].t - 0.05, run.last().unwrap().t + 0.05);
        let mut beats = Vec::new();
        timeline.for_each_beat(t0, t1, |t, down| beats.push((t, down)));
        let mut worst: f64 = 0.0;
        let mut accents_wrong = 0;
        let mut notes = Vec::new();
        for (k, c) in run.iter().enumerate() {
            let (bt, down) =
                beats.iter().min_by(|a, b| (a.0 - c.t).abs().total_cmp(&(b.0 - c.t).abs())).copied().unwrap();
            worst = worst.max((bt - c.t).abs());
            if down != c.accent {
                accents_wrong += 1;
                notes.push(format!("accent {} at {:.3} (click {k}, recorded {})", down, c.t, c.accent));
            }
        }
        for (bt, _) in &beats {
            if run.iter().all(|c| (c.t - bt).abs() > 0.005) {
                notes.push(format!("extra beat at {bt:.3}"));
            }
        }
        for w in beats.windows(2) {
            if w[1].0 - w[0].0 < 0.03 {
                notes.push(format!("double beat at {:.4} and {:.4}", w[0].0, w[1].0));
            }
        }
        worst_all = worst_all.max(worst);
        let markers = project.tempo.points.iter().filter(|p| p.time >= t0 && p.time < t1).count();
        println!(
            "{:8.2}..{:8.2}  {:4} clicks, {:4} beats, {:3} markers, worst {:5.2} ms, accents off: {}",
            run[0].t,
            run.last().unwrap().t,
            run.len(),
            beats.len(),
            markers,
            worst * 1000.0,
            accents_wrong
        );
        for note in notes.iter().take(6) {
            println!("      {note}");
        }
    }
    println!("worst of all: {:.2} ms", worst_all * 1000.0);

    // A signature change off the previous signature's bar lines cuts that bar short.
    let mut partial = 0;
    for (k, p) in project.tempo.points.iter().enumerate().filter(|(_, p)| p.time_sig.is_some()).skip(1) {
        // The whole tempo, with only the signatures before this one.
        let mut points = project.tempo.points.clone();
        for q in &mut points[k..] {
            q.time_sig = None;
        }
        let before = TempoMap { points, ..project.tempo.clone() };
        let bb = before.timeline().bar_beat(p.time + 1e-7);
        if bb.beat != 1 || bb.fraction > 1e-4 {
            partial += 1;
            if partial <= 5 {
                println!("  short bar before {:.4}: beat {} + {:.4}", p.time, bb.beat, bb.fraction);
            }
        }
    }
    println!("bars cut short: {partial}");

    // A click source over each stretch, on the click track; the recorded items are muted.
    let template = project.tracks[ti].items.iter().find_map(|i| i.take().cloned()).expect("a take to copy");
    for item in &mut project.tracks[ti].items {
        item.muted = true;
    }
    // A click source counts its bars from its own start and accents its first click (REAPER
    // 7.65, and Scythe after it), so each starts on its stretch's first bar line. Beats of that
    // bar before the first recorded click (a pickup) are muted with a take mute envelope.
    for (run, &start) in runs.iter().zip(&bar_starts) {
        let last = run.last().unwrap().t;
        let tail = if run.len() > 1 { (last - run[run.len() - 2].t) * 0.75 } else { 0.3 };
        let mut take = template.clone();
        take.name = "Click source".into();
        take.source = Source::Raw(click_source());
        take.source_offset = 0.0;
        take.playrate = 1.0;
        take.stretch_markers.clear();
        take.envelopes.clear();
        if start < run[0].t - 1e-6 {
            let mut mute = Envelope::new(EnvelopeKind::Mute);
            let square = |t: f64, v: f64| EnvPoint { shape: PointShape::Square, ..EnvPoint::new(t, v) };
            mute.points = vec![square(0.0, 0.0), square(run[0].t - start - 0.01, 1.0)];
            take.envelopes.push(mute);
        }
        let mut item = Item::new(start, last + tail - start, take);
        item.fade_in.length = 0.0;
        item.fade_out.length = 0.0;
        item.looped = true;
        project.tracks[ti].items.push(item);
    }
    project.tracks[ti].items.sort_by(|a, b| a.position.total_cmp(&b.position));
    write_project(&project, output.as_ref()).expect("write");
    println!("wrote {output}");
}

/// The click source chunk as REAPER 7.65 writes it, following the tempo map.
fn click_source() -> Chunk {
    let mut c = Chunk::new("SOURCE", &["CLICK"]);
    for line in [
        "AUTO 1 0",
        "BPM 120",
        "BPI 4 4",
        "VOL 0.49867 0.248672",
        "BEATLEN 4",
        "FREQ 1760 880 1",
        "SAMPLES \"\" \"\" \"\" \"\"",
        "SPLIGNORE 0 0",
        "SPLDEF 2 660 \"\" 0 \"\"",
        "SPLDEF 3 440 \"\" 0 \"\"",
        "PATTERN 0 169",
        "MULT 1",
    ] {
        c.line(line.split_whitespace());
    }
    c
}

/// The clicks an item plays: onsets of the source as the item reads it, kept when their pitch
/// is one of the item's click sounds; the rarest sound is the accent on each bar's first beat.
fn item_clicks(item: &Item, take: &scythe_core::model::Take, audio: &scythe_decode::Audio) -> Vec<Click> {
    let sr = audio.sample_rate as f64;
    let src_len = audio.channels[0].len();
    let frames = (item.length * sr) as usize;
    let offset = take.source_pos(0.0) * sr;
    let mono: Vec<f32> = (0..frames)
        .map(|i| {
            let mut pos = offset + i as f64 * take.playrate;
            if item.looped {
                pos = pos.rem_euclid(src_len as f64);
            }
            let p = pos as usize;
            if p >= src_len {
                return 0.0;
            }
            audio.channels.iter().map(|c| c[p]).sum::<f32>() / audio.channels.len() as f32
        })
        .collect();
    let onsets = onsets(&mono, sr);
    if onsets.is_empty() {
        return Vec::new();
    }
    // Sounds: pitches within 100 Hz of each other, from the most common down.
    let mut hist: Vec<(f64, usize)> = Vec::new();
    for o in &onsets {
        let bin = (o.2 / 25.0).round() * 25.0;
        match hist.iter_mut().find(|h| h.0 == bin) {
            Some(h) => h.1 += 1,
            None => hist.push((bin, 1)),
        }
    }
    hist.sort_by_key(|h| std::cmp::Reverse(h.1));
    let mut sounds: Vec<(f64, usize)> = Vec::new();
    for (f, n) in hist {
        match sounds.iter_mut().find(|s| (s.0 - f).abs() < 100.0) {
            Some(s) => s.1 += n,
            None => sounds.push((f, n)),
        }
    }
    let min = (onsets.len() as f64 * 0.03).max(2.0) as usize;
    sounds.retain(|s| s.1 >= min);
    let sound_of = |f: f64| sounds.iter().position(|s| (s.0 - f).abs() < 100.0);
    let accent = (sounds.len() > 1).then(|| sounds.iter().enumerate().min_by_key(|s| s.1.1).unwrap().0);
    onsets
        .iter()
        .filter_map(|o| {
            let s = sound_of(o.2)?;
            Some(Click { t: item.position + o.0, accent: Some(s) == accent })
        })
        .collect()
}

/// Stretches of clicks with no silence: a gap over 1.2 s and twice the beat before it, or 2.5 s.
fn split_runs(clicks: &[Click]) -> Vec<Vec<Click>> {
    let mut runs: Vec<Vec<Click>> = Vec::new();
    for c in clicks {
        let new = match runs.last() {
            None => true,
            Some(run) => {
                let gap = c.t - run.last().unwrap().t;
                let beat = if run.len() > 1 { run[run.len() - 1].t - run[run.len() - 2].t } else { gap };
                gap > 2.5 || (gap > beat * 2.05 && gap > 1.2)
            }
        };
        if new {
            runs.push(Vec::new());
        }
        runs.last_mut().unwrap().push(*c);
    }
    runs.retain(|r| r.len() >= 2);
    runs
}

/// Beats `from..=to` at one tempo, or ramping linearly in time from `b0` to `b1` (REAPER's
/// gradual tempo, which ends at the next marker's tempo). It starts at `start`, where the
/// segment before it put beat `from`, and ends at `end`, where it puts beat `to`.
#[derive(Debug, Clone, Copy)]
struct Seg {
    from: usize,
    to: usize,
    linear: bool,
    start: f64,
    end: f64,
    b0: f64,
    b1: f64,
}

/// Quarter notes after `dt` seconds of a tempo ramping linearly from `b0` to `b1` over `d`.
fn qn(dt: f64, b0: f64, b1: f64, d: f64) -> f64 {
    (b0 * dt + (b1 - b0) * dt * dt / (2.0 * d)) / 60.0
}

/// The segment from beat `from` at `start` to beat `to`, and the worst timing error of its
/// clicks. Without `forced` it lands on `to`'s click exactly: a constant tempo, or a ramp with
/// the start tempo that fits best. After a ramp the start tempo is `forced` (the ramp ends on
/// it): a constant tempo then keeps it, and a ramp starts from it and lands on `to`'s click.
fn segment(run: &[Click], start: f64, from: usize, to: usize, linear: bool, forced: Option<f64>) -> (f64, Seg) {
    let d = run[to].t - start;
    let n = (to - from) as f64;
    let sum = 120.0 * n / d;
    let (b0, b1, end) = match (linear, forced) {
        (false, None) => (sum / 2.0, sum / 2.0, run[to].t),
        (false, Some(f)) => (f, f, start + n * 60.0 / f),
        (true, Some(f)) => (f, sum - f, run[to].t),
        (true, None) => {
            // qn(t) = b0 (t/60 - t²/(60 d)) + sum t²/(120 d): least squares in b0.
            let (mut num, mut den) = (0.0, 0.0);
            for (k, click) in run.iter().enumerate().take(to).skip(from + 1) {
                let t = click.t - start;
                let a = t / 60.0 - t * t / (60.0 * d);
                num += a * ((k - from) as f64 - sum * t * t / (120.0 * d));
                den += a * a;
            }
            let b0 = if den > 0.0 { num / den } else { sum / 2.0 };
            (b0, sum - b0, run[to].t)
        }
    };
    let seg = Seg { from, to, linear, start, end, b0, b1 };
    if !(20.0..=960.0).contains(&b0) || !(20.0..=960.0).contains(&b1) {
        return (f64::INFINITY, seg);
    }
    let len = end - start;
    let worst = (from + 1..=to)
        .map(|k| {
            let t = run[k].t - start;
            let bpm = b0 + (b1 - b0) * (t / len).min(1.0);
            ((qn(t, b0, b1, len) - (k - from) as f64) * 60.0 / bpm).abs()
        })
        .fold(0.0, f64::max);
    (worst, seg)
}

/// Splits a stretch into the longest constant or ramping segments that keep every click within
/// the tolerance; a ramp is taken only when it reaches clearly further (over four beats) than a
/// constant tempo.
fn fit(run: &[Click]) -> Vec<Seg> {
    let n = run.len();
    let mut segs: Vec<Seg> = Vec::new();
    let (mut i, mut start, mut forced) = (0, run[0].t, None);
    while i + 1 < n {
        let longest = |linear: bool| {
            let mut best = None;
            let mut misses = 0;
            for j in i + 1..n {
                let (err, seg) = segment(run, start, i, j, linear, forced);
                if err <= TOLERANCE {
                    best = Some(seg);
                    misses = 0;
                } else {
                    misses += 1;
                    if misses > 4 {
                        break;
                    }
                }
            }
            best
        };
        // A ramp ends on the next segment's tempo, so it is taken only where the clicks after
        // it go on from that tempo (a constant tempo, or a ramp with clicks inside it to fit);
        // else it is cut shorter, or a constant tempo is taken.
        let goes_on = |s: &Seg| {
            s.to + 1 >= n
                || (s.to + 1..(s.to + 4).min(n)).any(|j| segment(run, s.end, s.to, j, false, Some(s.b1)).0 <= TOLERANCE)
                || (s.to + 3..(s.to + 6).min(n)).any(|j| segment(run, s.end, s.to, j, true, Some(s.b1)).0 <= TOLERANCE)
        };
        let (c, r) = (longest(false), longest(true));
        let c_to = c.map_or(i, |c| c.to);
        let ramp = r.filter(|r| r.to > c_to + 4 || c.is_none()).and_then(|r| {
            (i + 1..=r.to).rev().filter(|&to| to > c_to + 4 || c.is_none()).find_map(|to| {
                let (err, s) = segment(run, start, i, to, true, forced);
                (err <= TOLERANCE && (goes_on(&s) || to == i + 1)).then_some(s)
            })
        });
        // Nothing fits on from a ramp's end tempo: a one-beat ramp lands on the next click
        // exactly; failing that the tempo jumps, and that click is off by the difference.
        let seg = ramp.or(c).unwrap_or_else(|| {
            let (err, s) = segment(run, start, i, i + 1, true, forced);
            if err.is_finite() { s } else { segment(run, start, i, i + 1, false, None).1 }
        });
        forced = seg.linear.then_some(seg.b1);
        start = seg.end;
        i = seg.to;
        segs.push(seg);
    }
    segs
}

/// The tempo points for a stretch: one per segment, a point ending the last ramp, time
/// signatures from the accents, and the tempo kept after the last beat. `after` is the
/// previous stretch's last click.
fn run_points(run: &[Click], segs: &[Seg], after: f64) -> Vec<TempoPoint> {
    let n = run.len();
    let mut points: Vec<TempoPoint> =
        segs.iter().map(|s| TempoPoint { linear: s.linear, ..TempoPoint::new(s.start, s.b0) }).collect();
    if let Some(last) = segs.last().filter(|s| s.linear) {
        points.push(TempoPoint::new(last.end, last.b1));
    }

    // Bars start at the accents. Beats before the first one (a pickup, or a stretch with no
    // accent at all) end a bar that starts before the stretch, where no click source plays, so
    // none of them sounds as a first beat.
    let accents: Vec<usize> = (0..n).filter(|&k| run[k].accent).collect();
    let pickup = accents.first().copied().unwrap_or(n);
    let first_bar = match accents.as_slice() {
        [a, b, ..] => (b - a) as u32,
        _ => 4,
    };
    let bpm = points[0].bpm;
    let beat = 60.0 / bpm;
    if pickup > 0 {
        let lead = if pickup as u32 <= first_bar && !accents.is_empty() { first_bar } else { pickup as u32 + 1 };
        // As many beats before the stretch as fit after the previous one, at least one.
        let room = ((run[0].t - after) / beat - 0.5).floor().max(0.0) as u32;
        let before = (lead - pickup as u32).min(room);
        if before > 0 {
            let start = run[0].t - f64::from(before) * beat;
            points.insert(0, TempoPoint { time_sig: Some((pickup as u32 + before, 4)), ..TempoPoint::new(start, bpm) });
        } else {
            points[0].time_sig = Some((pickup.max(1) as u32, 4));
        }
    } else {
        // The stretch starts a bar of its own, after a bar of whole beats that ends on it, so
        // the tempo before the stretch puts no beat just before its first click.
        let room = ((run[0].t - after) / beat - 0.5).floor().max(0.0) as u32;
        if room > 0 {
            let start = run[0].t - f64::from(room) * beat;
            points.insert(0, TempoPoint { time_sig: Some((room, 4)), ..TempoPoint::new(start, bpm) });
        }
        points.iter_mut().find(|p| p.time == run[0].t).unwrap().time_sig = Some((first_bar, 4));
    }
    let mut sigs: Vec<(usize, u32)> = Vec::new();
    for (k, &a) in accents.iter().enumerate() {
        // The last bar holds every click left, so a closing hit on its "one" is not accented.
        let last = |prev: u32| prev.max((n - a) as u32 + u32::from((n - a) as u32 > prev));
        let len = accents.get(k + 1).map_or_else(|| last(sigs.last().map_or(first_bar, |s| s.1)), |&b| (b - a) as u32);
        if k == 0 || sigs.last().is_none_or(|s| s.1 != len) {
            sigs.push((a, len));
        }
    }
    // A bar starts exactly on the beat the tempo gives, which is within the tolerance of the
    // recorded click but not on it: a signature change off the beat would start a bar of its own
    // just after the beat before it.
    let tempo_only = TempoMap { bpm: points[0].bpm, beats_per_bar: 4, beat_unit: 4, points: points.clone() };
    let tl = tempo_only.timeline();
    let beat_time = |beat: usize| {
        let s = segs.iter().find(|s| s.from <= beat && beat <= s.to).unwrap();
        if beat == s.from { s.start } else { tl.time_at_qn(tl.qn_at(s.start) + (beat - s.from) as f64) }
    };
    for (beat, num) in sigs {
        let t = beat_time(beat);
        match points.iter_mut().find(|p| (p.time - t).abs() < 1e-9) {
            Some(p) => p.time_sig = Some((num, 4)),
            None => {
                // Inside a segment: a point at the tempo there, keeping a ramp going.
                let k = points.partition_point(|p| p.time < t);
                let prev = &points[k - 1];
                let bpm = match points.get(k).filter(|_| prev.linear) {
                    Some(next) => prev.bpm + (next.bpm - prev.bpm) * (t - prev.time) / (next.time - prev.time),
                    None => prev.bpm,
                };
                let p = TempoPoint { linear: prev.linear, time_sig: Some((num, 4)), ..TempoPoint::new(t, bpm) };
                points.insert(k, p);
            }
        }
    }
    points
}

/// Onsets: where the 1 ms energy rises well above the 20 ms before it, at least 80 ms apart:
/// the offset in seconds, the peak in the 25 ms after it and its strongest frequency.
fn onsets(x: &[f32], sr: f64) -> Vec<(f64, f32, f64)> {
    let hop = (sr / 1000.0) as usize;
    let energy: Vec<f32> = x.chunks(hop).map(|c| c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).collect();
    let peak_energy = energy.iter().copied().fold(0.0, f32::max);
    let floor = peak_energy * 1e-4;
    let mut out = Vec::new();
    let mut last = -1000i64;
    for i in 20..energy.len() {
        if (i as i64) - last < 80 {
            continue;
        }
        let before = energy[i - 20..i - 2].iter().sum::<f32>() / 18.0;
        if energy[i] > floor && energy[i] > 8.0 * before.max(floor * 0.01) {
            let start = i * hop;
            let seg = &x[start..(start + (sr * 0.025) as usize).min(x.len())];
            let peak = seg.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            let first = seg.iter().position(|s| s.abs() > peak * 0.1).unwrap_or(0);
            out.push(((start + first) as f64 / sr, peak, dominant(&seg[first..], sr)));
            last = i as i64;
        }
    }
    out
}

/// The strongest frequency between 150 Hz and 2.5 kHz in the first 20 ms (Hann window, 10 Hz steps).
fn dominant(seg: &[f32], sr: f64) -> f64 {
    let n = ((sr * 0.02) as usize).min(seg.len());
    if n < 16 {
        return 0.0;
    }
    let w: Vec<f64> = (0..n)
        .map(|i| seg[i] as f64 * (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / (n - 1) as f64).cos()))
        .collect();
    let mut best = (0.0, 0.0);
    let mut f = 150.0;
    while f < 2500.0 {
        let k = std::f64::consts::TAU * f / sr;
        let (re, im) = w.iter().enumerate().fold((0.0, 0.0), |(re, im), (i, x)| {
            let a = k * i as f64;
            (re + x * a.cos(), im - x * a.sin())
        });
        let p = re * re + im * im;
        if p > best.1 {
            best = (f, p);
        }
        f += 10.0;
    }
    best.0
}
