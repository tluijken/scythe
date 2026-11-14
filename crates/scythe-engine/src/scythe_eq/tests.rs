use std::f64::consts::PI;

use super::filters::{Design, Prototype, Shape};
use super::params::{BAND_STRIDE, GLOBAL, band, global};
use super::*;
use crate::fx::Effect;

fn ctx(sr: u32) -> ProcessContext<'static> {
    ProcessContext { sample_rate: sr, ..crate::stock::testing::context() }
}

fn set_band(eq: &mut ScytheEq, b: usize, shape: Shape, freq: f64, gain: f64, q: f64) {
    let id = |o: u32| b as u32 * BAND_STRIDE + o;
    eq.set(id(band::USED), 1.0);
    eq.set(id(band::SHAPE), shape.index() as f64);
    eq.set(id(band::FREQ), freq);
    eq.set(id(band::GAIN), gain);
    eq.set(id(band::Q), q);
}

fn sine(f: f64, amp: f64, n: usize, sr: f64) -> Vec<f32> {
    (0..n).map(|i| (amp * (2.0 * PI * f * i as f64 / sr).sin()) as f32).collect()
}

/// Amplitude of the `f` component of `x` (a whole number of periods is not needed: the
/// window is long).
fn amplitude(x: &[f32], f: f64, sr: f64) -> f64 {
    let n = x.len();
    let (mut re, mut im, mut wsum) = (0.0, 0.0, 0.0);
    for (i, v) in x.iter().enumerate() {
        let w = 0.5 - 0.5 * (2.0 * PI * i as f64 / n as f64).cos();
        let ph = 2.0 * PI * f * i as f64 / sr;
        re += *v as f64 * w * ph.cos();
        im += *v as f64 * w * ph.sin();
        wsum += w;
    }
    2.0 * (re * re + im * im).sqrt() / wsum
}

fn db(x: f64) -> f64 {
    20.0 * x.max(1e-30).log10()
}

/// Runs `l`/`r` through the effect in blocks of 256 (as a host would).
fn run(eq: &mut ScytheEq, l: &mut [f32], r: &mut [f32], sr: u32) {
    let c = ctx(sr);
    for (a, b) in l.chunks_mut(256).zip(r.chunks_mut(256)) {
        eq.process(a, b, &c);
    }
}

fn gain_at(eq: &mut ScytheEq, f: f64, sr: u32) -> f64 {
    let n = sr as usize;
    let mut l = sine(f, 0.25, n, sr as f64);
    let mut r = l.clone();
    eq.reset();
    run(eq, &mut l, &mut r, sr);
    let tail = n / 2;
    db(amplitude(&l[tail..], f, sr as f64) / 0.25)
}

#[test]
fn an_empty_eq_is_transparent() {
    let mut eq = ScytheEq::new(48000);
    let input = sine(440.0, 0.5, 4800, 48000.0);
    let (mut l, mut r) = (input.clone(), input.clone());
    run(&mut eq, &mut l, &mut r, 48000);
    assert_eq!(eq.latency(), 0);
    assert!(l.iter().zip(&input).all(|(a, b)| (a - b).abs() < 1e-6));
}

#[test]
fn zero_latency_bands_measure_like_their_prototype_at_every_rate() {
    for sr in [44100u32, 48000, 96000, 192000] {
        let mut eq = ScytheEq::new(sr);
        set_band(&mut eq, 0, Shape::Bell, 1000.0, 9.0, 2.0);
        set_band(&mut eq, 1, Shape::HighShelf, 8000.0, -6.0, std::f64::consts::FRAC_1_SQRT_2);
        let p = |f: f64| {
            let a = Prototype::design(&Design {
                shape: Shape::Bell,
                freq: 1000.0,
                gain_db: 9.0,
                q: 2.0,
                ..Default::default()
            });
            let b = Prototype::design(&Design {
                shape: Shape::HighShelf,
                freq: 8000.0,
                gain_db: -6.0,
                q: std::f64::consts::FRAC_1_SQRT_2,
                ..Default::default()
            });
            db(a.magnitude(f) * b.magnitude(f))
        };
        for f in [100.0, 1000.0, 3000.0, 12000.0] {
            let got = gain_at(&mut eq, f, sr);
            assert!((got - p(f)).abs() < 0.15, "{sr} Hz rate, {f} Hz: {got:.2} vs {:.2}", p(f));
        }
    }
}

#[test]
fn linear_phase_is_symmetric_and_reports_its_latency() {
    let sr = 48000;
    let mut eq = ScytheEq::new(sr);
    set_band(&mut eq, 0, Shape::Bell, 2000.0, 10.0, 1.5);
    set_band(&mut eq, 1, Shape::LowCut, 120.0, 0.0, std::f64::consts::FRAC_1_SQRT_2);
    eq.set(GLOBAL + global::PHASE, 2.0);
    let latency = eq.latency();
    assert_eq!(latency, 2048, "medium resolution at 48 kHz");
    let n = latency * 4;
    let mut l = vec![0.0f32; n];
    l[0] = 1.0;
    let mut r = l.clone();
    run(&mut eq, &mut l, &mut r, sr);
    let peak = l.iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
    assert_eq!(peak, latency);
    for k in 1..latency - 300 {
        assert!((l[latency - k] - l[latency + k]).abs() < 1e-5, "tap {k}");
    }
    for f in [60.0, 500.0, 2000.0, 8000.0] {
        let want = db(Prototype::design(&Design {
            shape: Shape::Bell,
            freq: 2000.0,
            gain_db: 10.0,
            q: 1.5,
            ..Default::default()
        })
        .magnitude(f)
            * Prototype::design(&Design {
                shape: Shape::LowCut,
                freq: 120.0,
                q: std::f64::consts::FRAC_1_SQRT_2,
                ..Default::default()
            })
            .magnitude(f));
        let got = gain_at(&mut eq, f, sr);
        assert!((got - want).abs() < 0.3, "{f} Hz: {got:.2} vs {want:.2}");
    }
}

#[test]
fn natural_phase_follows_the_analog_response_with_short_latency() {
    let sr = 48000;
    let mut eq = ScytheEq::new(sr);
    set_band(&mut eq, 0, Shape::Bell, 15000.0, 8.0, 1.0);
    set_band(&mut eq, 1, Shape::Bell, 200.0, -6.0, 1.0);
    eq.set(GLOBAL + global::PHASE, 1.0);
    assert_eq!(eq.latency(), 1024);
    let want = |f: f64| {
        db(Prototype::design(&Design { shape: Shape::Bell, freq: 15000.0, gain_db: 8.0, q: 1.0, ..Default::default() })
            .magnitude(f)
            * Prototype::design(&Design {
                shape: Shape::Bell,
                freq: 200.0,
                gain_db: -6.0,
                q: 1.0,
                ..Default::default()
            })
            .magnitude(f))
    };
    for f in [200.0, 1000.0, 15000.0, 20000.0] {
        let got = gain_at(&mut eq, f, sr);
        assert!((got - want(f)).abs() < 0.3, "{f} Hz: {got:.2} vs {:.2}", want(f));
    }
}

#[test]
fn bands_act_on_their_stereo_placement() {
    let sr = 48000;
    for mode in [0.0, 2.0] {
        let mut eq = ScytheEq::new(sr);
        eq.set(GLOBAL + global::PHASE, mode);
        set_band(&mut eq, 0, Shape::Bell, 1000.0, -12.0, 1.0);
        eq.set(band::STEREO, 1.0);
        let n = 48000;
        let input = sine(1000.0, 0.25, n, sr as f64);
        let (mut l, mut r) = (input.clone(), input.clone());
        run(&mut eq, &mut l, &mut r, sr);
        let half = n / 2;
        assert!((db(amplitude(&l[half..], 1000.0, 48000.0) / 0.25) + 12.0).abs() < 0.2, "left cut, mode {mode}");
        assert!(db(amplitude(&r[half..], 1000.0, 48000.0) / 0.25).abs() < 0.1, "right untouched, mode {mode}");
        // A side band leaves a mono signal alone; a mid band cuts both channels.
        eq.set(band::STEREO, 4.0);
        let (mut l, mut r) = (input.clone(), input.clone());
        eq.reset();
        run(&mut eq, &mut l, &mut r, sr);
        assert!(db(amplitude(&l[half..], 1000.0, 48000.0) / 0.25).abs() < 0.1, "side on mono, mode {mode}");
        eq.set(band::STEREO, 3.0);
        let (mut l, mut r) = (input.clone(), input.clone());
        eq.reset();
        run(&mut eq, &mut l, &mut r, sr);
        for ch in [&l, &r] {
            assert!(
                (db(amplitude(&ch[half..], 1000.0, 48000.0) / 0.25) + 12.0).abs() < 0.2,
                "mid on mono, mode {mode}"
            );
        }
    }
}

/// Jumping a band's gain by 18 dB glides: the output never steps further between frames
/// than the louder steady signal does.
#[test]
fn parameter_changes_glide_without_zipper_steps() {
    let sr = 48000;
    let mut eq = ScytheEq::new(sr);
    set_band(&mut eq, 0, Shape::Bell, 200.0, -6.0, 1.0);
    let input = sine(200.0, 0.25, 24000, sr as f64);
    let (mut l, mut r) = (input.clone(), input.clone());
    let c = ctx(sr);
    let mut max_step = 0.0f32;
    for (k, (a, b)) in l.chunks_mut(64).zip(r.chunks_mut(64)).enumerate() {
        if k == 150 {
            eq.set(band::GAIN, 12.0);
        }
        if k == 250 {
            eq.set(band::FREQ, 400.0);
        }
        eq.process(a, b, &c);
    }
    for w in l[4800..].windows(2) {
        max_step = max_step.max((w[1] - w[0]).abs());
    }
    // A 200 Hz sine of amplitude 0.25·4 (+12 dB) steps by at most 2π·200/48000 per frame.
    let steady = 1.0 * 2.0 * PI as f32 * 200.0 / 48000.0;
    assert!(max_step < steady * 1.05, "{max_step} vs {steady}");
}

#[test]
fn changing_shape_or_bypassing_fades_instead_of_clicking() {
    let sr = 48000;
    let mut eq = ScytheEq::new(sr);
    set_band(&mut eq, 0, Shape::Bell, 1000.0, 12.0, 1.0);
    let input = sine(100.0, 0.25, 24000, sr as f64);
    let (mut l, mut r) = (input.clone(), input.clone());
    let c = ctx(sr);
    for (k, (a, b)) in l.chunks_mut(64).zip(r.chunks_mut(64)).enumerate() {
        match k {
            100 => eq.set(band::SHAPE, Shape::LowCut.index() as f64),
            200 => eq.set(band::ENABLED, 0.0),
            _ => {}
        }
        eq.process(a, b, &c);
    }
    let step = l.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0f32, f32::max);
    assert!(step < 0.02, "{step}");
}

#[test]
fn auto_gain_offsets_a_broad_boost() {
    let mut eq = ScytheEq::new(48000);
    set_band(&mut eq, 0, Shape::Bell, 1000.0, 6.0, 0.3);
    eq.set(GLOBAL + global::AUTO_GAIN, 1.0);
    let target = eq.auto_target();
    assert!(target < -3.0 && target > -6.0, "{target}");
    let g = gain_at(&mut eq, 3000.0, 48000);
    let plain =
        db(Prototype::design(&Design { shape: Shape::Bell, freq: 1000.0, gain_db: 6.0, q: 0.3, ..Default::default() })
            .magnitude(3000.0));
    assert!((g - (plain + target)).abs() < 0.1);
}

#[test]
fn gain_scale_and_output_section() {
    let mut eq = ScytheEq::new(48000);
    set_band(&mut eq, 0, Shape::Bell, 1000.0, 10.0, 1.0);
    eq.set(GLOBAL + global::GAIN_SCALE, 50.0);
    assert!((gain_at(&mut eq, 1000.0, 48000) - 5.0).abs() < 0.1);
    eq.set(GLOBAL + global::GAIN_SCALE, -100.0);
    assert!((gain_at(&mut eq, 1000.0, 48000) + 10.0).abs() < 0.1);
    eq.set(GLOBAL + global::GAIN_SCALE, 100.0);
    eq.set(GLOBAL + global::OUTPUT, -6.0);
    assert!((gain_at(&mut eq, 1000.0, 48000) - 4.0).abs() < 0.1);
    eq.set(GLOBAL + global::PAN, 100.0);
    let mut l = sine(50.0, 0.5, 4800, 48000.0);
    let mut r = l.clone();
    run(&mut eq, &mut l, &mut r, 48000);
    assert!(l[2400..].iter().all(|v| v.abs() < 1e-4) && r[2400..].iter().any(|v| v.abs() > 0.1));
    eq.set(GLOBAL + global::PAN, 0.0);
    eq.set(GLOBAL + global::WIDTH, 0.0);
    let mut l = sine(50.0, 0.5, 4800, 48000.0);
    let mut r = vec![0.0; 4800];
    run(&mut eq, &mut l, &mut r, 48000);
    assert!(l[2400..].iter().zip(&r[2400..]).all(|(a, b)| (a - b).abs() < 1e-5), "mono at zero width");
}

#[test]
fn state_survives_save_and_load() {
    let mut eq = ScytheEq::new(48000);
    set_band(&mut eq, 5, Shape::HighCut, 9000.0, 0.0, 0.9);
    eq.set(5 * BAND_STRIDE + band::SLOPE, 6.0);
    eq.set(GLOBAL + global::CHARACTER, 2.0);
    let state = eq.save_state().unwrap();
    let back = ScytheEq::from_state(&state, 44100).unwrap();
    assert_eq!(back.values(), eq.values());
    assert_eq!(back.band(5).design.slope, 6);
}

#[test]
fn silence_after_a_burst_stays_free_of_subnormals() {
    let mut eq = ScytheEq::new(48000);
    set_band(&mut eq, 0, Shape::LowCut, 15.0, 0.0, 8.0);
    eq.set(band::SLOPE, 8.0);
    set_band(&mut eq, 1, Shape::Bell, 30.0, 12.0, 30.0);
    let mut l = vec![0.0f32; 48000 * 20];
    l[0] = 1.0;
    let mut r = l.clone();
    run(&mut eq, &mut l, &mut r, 48000);
    assert!(l.iter().all(|v| *v == 0.0 || v.is_normal()));
    for b in &eq.bands[..2] {
        for st in &b.state {
            assert!(st.values().all(|v| v == 0.0 || v.is_normal()));
        }
    }
}

#[test]
fn character_modes_add_their_harmonics() {
    for (kind, even, odd) in [(0.0, false, false), (1.0, false, true), (2.0, true, true)] {
        let mut eq = ScytheEq::new(48000);
        eq.set(GLOBAL + global::CHARACTER, kind);
        let mut l = sine(1000.0, 0.7, 48000, 48000.0);
        let mut r = l.clone();
        run(&mut eq, &mut l, &mut r, 48000);
        let h = |f: f64| db(amplitude(&l[24000..], f, 48000.0) / 0.7);
        assert_eq!(h(2000.0) > -80.0, even, "kind {kind}: 2nd {}", h(2000.0));
        assert_eq!(h(3000.0) > -80.0, odd, "kind {kind}: 3rd {}", h(3000.0));
        assert!(h(1000.0).abs() < 1.0);
    }
}

fn dynamic_band(eq: &mut ScytheEq, b: usize, mode: f64, range: f64, threshold: f64) {
    set_band(eq, b, Shape::Bell, 1000.0, 0.0, 2.0);
    let id = |o: u32| b as u32 * BAND_STRIDE + o;
    eq.set(id(band::DYNAMIC), mode);
    eq.set(id(band::RANGE), range);
    eq.set(id(band::THRESHOLD), threshold);
    eq.set(id(band::ATTACK), 2.0);
    eq.set(id(band::RELEASE), 50.0);
}

#[test]
fn dynamic_bands_cut_only_while_their_band_is_loud() {
    for mode in [0.0, 2.0, 1.0] {
        let sr = 48000;
        let mut eq = ScytheEq::new(sr);
        eq.set(GLOBAL + global::PHASE, mode);
        dynamic_band(&mut eq, 0, 1.0, -9.0, -30.0);
        let loud = gain_at(&mut eq, 1000.0, sr);
        assert!((loud + 9.0).abs() < 0.3, "mode {mode}: loud {loud}");
        let quiet = {
            let n = sr as usize;
            let mut l = sine(1000.0, 0.005, n, sr as f64);
            let mut r = l.clone();
            eq.reset();
            run(&mut eq, &mut l, &mut r, sr);
            db(amplitude(&l[n / 2..], 1000.0, sr as f64) / 0.005)
        };
        assert!(quiet.abs() < 0.2, "mode {mode}: quiet {quiet}");
        let away = gain_at(&mut eq, 6000.0, sr);
        assert!(away.abs() < 0.2, "mode {mode}: out of band {away}");
    }
}

#[test]
fn an_external_sidechain_drives_the_band() {
    let sr = 48000u32;
    let mut eq = ScytheEq::new(sr);
    dynamic_band(&mut eq, 0, 1.0, -12.0, -30.0);
    eq.set(band::SIDECHAIN, 1.0);
    let n = sr as usize;
    let mut l = sine(1000.0, 0.01, n, sr as f64);
    let mut r = l.clone();
    let key = sine(1000.0, 0.5, n, sr as f64);
    let c = ProcessContext { aux: Some((&key, &key)), ..ctx(sr) };
    eq.process(&mut l, &mut r, &c);
    let g = db(amplitude(&l[n / 2..], 1000.0, sr as f64) / 0.01);
    assert!((g + 12.0).abs() < 0.3, "{g}");
    assert!((eq.meters().dynamic(0) + 12.0).abs() < 0.1);
}

/// Two tones inside one spectral band: the loud one is pulled down, the quiet neighbour
/// keeps its level, which a plain dynamic band could not do.
#[test]
fn spectral_bands_tame_only_the_loud_bins() {
    let sr = 48000u32;
    let mut eq = ScytheEq::new(sr);
    dynamic_band(&mut eq, 0, 2.0, -12.0, -30.0);
    eq.set(band::Q, 0.7);
    assert_eq!(eq.latency(), 2048);
    let n = 2 * sr as usize;
    let a = sine(1000.0, 0.5, n, sr as f64);
    let b = sine(1400.0, 0.005, n, sr as f64);
    let mut l: Vec<f32> = a.iter().zip(&b).map(|(x, y)| x + y).collect();
    let mut r = l.clone();
    run(&mut eq, &mut l, &mut r, sr);
    let tail = &l[n / 2..];
    let loud = db(amplitude(tail, 1000.0, sr as f64) / 0.5);
    let quiet = db(amplitude(tail, 1400.0, sr as f64) / 0.005);
    assert!(loud < -9.0, "loud {loud}");
    assert!(quiet > -1.5, "quiet {quiet}");
    // Off again: the stage and its latency go.
    eq.set(band::DYNAMIC, 0.0);
    assert_eq!(eq.latency(), 0);
}

#[test]
fn solo_plays_the_band_region() {
    let sr = 48000u32;
    let mut eq = ScytheEq::new(sr);
    set_band(&mut eq, 0, Shape::Bell, 1000.0, 6.0, 4.0);
    eq.set(band::SOLO, 1.0);
    assert!(gain_at(&mut eq, 1000.0, sr).abs() < 0.2);
    assert!(gain_at(&mut eq, 100.0, sr) < -20.0);
}

/// Switching the phase mode, the linear phase resolution or spectral processing while a sine
/// plays crossfades: no step between samples is larger than the sine's own steepest.
#[test]
fn structural_switches_while_playing_do_not_click() {
    let sr = 48000u32;
    let (f, amp) = (220.0, 0.5);
    let mut eq = ScytheEq::new(sr);
    set_band(&mut eq, 1, Shape::Bell, 1000.0, 6.0, 1.0);
    let n = sr as usize;
    let input = sine(f, amp, n * 6, sr as f64);
    let (mut l, mut r) = (input.clone(), input.clone());
    let c = ctx(sr);
    type Switch = Box<dyn Fn(&mut ScytheEq)>;
    let switches: [(usize, Switch, usize); 5] = [
        (1, Box::new(|eq| eq.set(GLOBAL + global::PHASE, 2.0)), 2048),
        (2, Box::new(|eq| eq.set(GLOBAL + global::RESOLUTION, 3.0)), 8192),
        (3, Box::new(|eq| eq.set(GLOBAL + global::PHASE, 1.0)), 1024),
        (4, Box::new(|eq| dynamic_band(eq, 0, 2.0, -6.0, -40.0)), 1024 + 2048),
        (5, Box::new(|eq| eq.set(GLOBAL + global::PHASE, 0.0)), 2048),
    ];
    let mut next = 0;
    for (k, (a, b)) in l.chunks_mut(256).zip(r.chunks_mut(256)).enumerate() {
        if let Some((second, switch, latency)) = switches.get(next)
            && k * 256 >= second * n
        {
            switch(&mut eq);
            assert_eq!(eq.latency(), *latency, "switch {next}");
            next += 1;
        }
        eq.process(a, b, &c);
    }
    assert_eq!(next, switches.len());
    let steepest = (2.0 * PI * f / sr as f64 * amp) as f32;
    for (i, w) in l.windows(2).enumerate().skip(n / 2) {
        let step = (w[1] - w[0]).abs();
        assert!(
            step < 1.6 * steepest,
            "a step of {step} at {:.3} s (the sine's steepest is {steepest})",
            i as f64 / sr as f64
        );
    }
}

/// In zero latency a dynamic low cut moves up while the lows it removes are loud, and a
/// dynamic notch is transparent until its region is loud, then as deep as its range.
#[test]
fn dynamic_cuts_and_notches_move_in_zero_latency() {
    let sr = 48000;
    let gain = |shape: Shape, freq: f64, tone: f64, amp: f64, dynamic: bool| {
        let mut eq = ScytheEq::new(sr);
        set_band(&mut eq, 0, shape, freq, 0.0, 4.0);
        if dynamic {
            eq.set(band::DYNAMIC, 1.0);
            eq.set(band::RANGE, -12.0);
            eq.set(band::THRESHOLD, -40.0);
            eq.set(band::ATTACK, 2.0);
        }
        let n = sr as usize;
        let mut l = sine(tone, amp, n, sr as f64);
        let mut r = l.clone();
        run(&mut eq, &mut l, &mut r, sr);
        db(amplitude(&l[n / 2..], tone, sr as f64) / amp)
    };
    let fixed = gain(Shape::LowCut, 100.0, 70.0, 0.5, false);
    let moved = gain(Shape::LowCut, 100.0, 70.0, 0.5, true);
    assert!(moved < fixed - 6.0, "low cut: static {fixed} dB, dynamic {moved} dB");
    let quiet = gain(Shape::LowCut, 100.0, 70.0, 0.001, true);
    assert!((quiet - gain(Shape::LowCut, 100.0, 70.0, 0.001, false)).abs() < 0.5, "quiet low cut {quiet}");
    let loud = gain(Shape::Notch, 1000.0, 1000.0, 0.5, true);
    assert!((loud + 12.0).abs() < 1.0, "loud notch {loud}");
    let quiet = gain(Shape::Notch, 1000.0, 1000.0, 0.001, true);
    assert!(quiet.abs() < 0.5, "quiet notch {quiet}");
    let mut eq = ScytheEq::new(sr);
    set_band(&mut eq, 0, Shape::Notch, 1000.0, 0.0, 4.0);
    eq.set(band::DYNAMIC, 1.0);
    eq.set(GLOBAL + global::PHASE, 2.0);
    assert_eq!(eq.band(0).dynamic, 0, "static in the FIR modes");
}

/// The 32-frame grid follows the sample position: a dynamic and a spectral band give the same
/// output whatever block sizes the host uses, and so does a stopped transport counting frames.
#[test]
fn dynamic_bands_ignore_block_size() {
    let sr = 48000u32;
    let mut seed = 11u64;
    let mut next = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        seed >> 33
    };
    let noise: Vec<f32> = (0..2 * sr as usize).map(|_| (next() as f32 / (1u64 << 31) as f32 - 0.5) * 0.8).collect();
    let random: Vec<usize> = std::iter::repeat_with(|| 1 + next() as usize % 700).take(noise.len()).collect();
    for mode in [0.0, 2.0] {
        let render = |sizes: &[usize], playing: bool, from: i64| {
            let mut eq = ScytheEq::new(sr);
            eq.set(GLOBAL + global::PHASE, mode);
            dynamic_band(&mut eq, 0, 1.0, -9.0, -30.0);
            dynamic_band(&mut eq, 1, 2.0, -12.0, -30.0);
            eq.set(BAND_STRIDE + band::FREQ, 4000.0);
            let (mut l, mut r) = (noise.clone(), noise.iter().map(|x| -0.5 * x).collect::<Vec<_>>());
            let mut at = 0;
            for &n in sizes.iter().cycle() {
                if at == l.len() {
                    break;
                }
                let n = n.min(l.len() - at);
                let c = ProcessContext { position: from + at as i64, playing, ..ctx(sr) };
                eq.process(&mut l[at..at + n], &mut r[at..at + n], &c);
                at += n;
            }
            (l, r)
        };
        let reference = render(&[512], true, 0);
        assert!(reference.0.iter().zip(&noise).any(|(a, b)| a != b));
        for (name, sizes, playing) in
            [("37", &[37][..], true), ("random", &random, true), ("random, stopped", &random, false)]
        {
            let out = render(sizes, playing, 0);
            assert!(out == reference, "mode {mode}, blocks of {name}");
        }
        // Playback starting part way into a grid cell.
        assert!(render(&random, true, 1013) == render(&[512], true, 1013), "mode {mode}, from 1013");
    }
}
