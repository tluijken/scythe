//! MIDI clock out and MTC chase through ALSA virtual ports. Skipped where there is no ALSA
//! sequencer.

use std::time::{Duration, Instant};

use scythe_engine::Engine;
use scythe_engine::midi_io::virtual_ports::{Recorder, output};
use scythe_engine::midi_sync::{ChaseConfig, ChaseSource, ClockPort, SyncConfig};

/// The full name the system lists for a port whose name contains `part`.
fn port(part: &str, outputs: bool) -> Option<String> {
    let names = if outputs { scythe_engine::midi_io::output_names() } else { scythe_engine::midi_io::input_names() };
    names.into_iter().find(|n| n.contains(part))
}

fn engine() -> Engine {
    let mut engine = Engine::start_null(std::sync::Arc::default(), 48000, None);
    let project = scythe_core::rpp::project_from_rpp("<REAPER_PROJECT 0.1 \"7.73\" 0\n  TEMPO 120 4 4\n>\n").unwrap();
    engine.sync(&project);
    engine
}

#[test]
fn clock_out_reaches_a_virtual_port() {
    let Ok(recorder) = Recorder::new("wp5clockprobe") else {
        eprintln!("no ALSA sequencer; skipping");
        return;
    };
    let Some(name) = port("wp5clockprobe", true) else {
        eprintln!("virtual port not listed; skipping");
        return;
    };
    let mut engine = engine();
    engine.set_midi_output_devices(&[(4, name)]).unwrap();
    engine
        .set_sync(SyncConfig { clock: vec![ClockPort { port: 4, clock: true, spp: true, reset: false }], chase: None });
    std::thread::sleep(Duration::from_millis(150));
    recorder.take();
    engine.play();
    std::thread::sleep(Duration::from_millis(1000));
    engine.stop();
    std::thread::sleep(Duration::from_millis(200));
    let got = recorder.take();
    let first = got.iter().position(|m| m.1 != [0xf2, 0, 0]).unwrap();
    assert_eq!(got[first].1, [0xfa], "start from the project start: {:02x?}", &got[..4]);
    let ticks: Vec<Instant> = got.iter().filter(|m| m.1 == [0xf8]).map(|m| m.0).collect();
    assert!(ticks.len() >= 40, "{} ticks", ticks.len());
    let span = ticks.last().unwrap().duration_since(ticks[0]).as_secs_f64() / (ticks.len() - 1) as f64;
    assert!((span - 0.5 / 24.0).abs() < 0.002, "24 ticks per quarter note at 120 BPM: {span}");
    let tail: Vec<&Vec<u8>> = got.iter().rev().take(2).map(|m| &m.1).collect();
    assert!(tail[1] == &vec![0xfc] && tail[0][0] == 0xf2, "stop, then the song position: {tail:02x?}");
}

#[test]
fn chases_mtc_from_a_virtual_port() {
    let Ok(mut out) = output("wp5mtcprobe") else {
        eprintln!("no ALSA sequencer; skipping");
        return;
    };
    let Some(name) = port("wp5mtcprobe", false) else {
        eprintln!("virtual port not listed; skipping");
        return;
    };
    let mut engine = engine();
    engine.set_midi_input_devices(&[(7, name)]).unwrap();
    let chase = ChaseConfig { source: ChaseSource::Mtc(Some(7)), offset: 2.0, resync: 0.1, freewheel: 0.3 };
    engine.set_sync(SyncConfig { clock: Vec::new(), chase: Some(chase) });
    std::thread::sleep(Duration::from_millis(100));
    // 25 fps from 00:00:10:00: a quarter frame every 10 ms.
    let rate = scythe_engine::midi_sync::TimecodeRate::from_rpp(25.0, 0);
    let start = Instant::now();
    out.send(&scythe_engine::midi_sync::mtc_full_frame(250, rate)).unwrap();
    for k in 0..80i64 {
        let piece = (k % 8) as u8;
        out.send(&scythe_engine::midi_sync::mtc_quarter_frame(piece, 250 + 2 * (k / 8), rate)).unwrap();
        let next = start + Duration::from_millis(10 * (k as u64 + 1));
        std::thread::sleep(next.saturating_duration_since(Instant::now()));
    }
    let elapsed = start.elapsed().as_secs_f64();
    assert!(engine.transport().is_playing(), "playing with the timecode");
    let pos = engine.transport().position_seconds();
    let expected = 10.0 + elapsed - 2.0;
    assert!((pos - expected).abs() < 0.15, "at {pos}, timecode says {expected}");
    std::thread::sleep(Duration::from_millis(600));
    assert!(!engine.transport().is_playing(), "stopped once the timecode stopped");
}

/// The timecode generator set to MTC (`SEND 2`) on a track with a MIDI hardware output: what
/// REAPER 7.73 sent from 01:00:00:00 at 25 fps, a full frame and then quarter frames 10 ms
/// apart, each run of eight naming every other frame.
#[test]
fn timecode_generator_sends_mtc() {
    let Ok(recorder) = Recorder::new("wp5mtcout") else {
        eprintln!("no ALSA sequencer; skipping");
        return;
    };
    let Some(name) = port("wp5mtcout", true) else {
        eprintln!("virtual port not listed; skipping");
        return;
    };
    let mut engine = engine();
    let project = scythe_core::rpp::project_from_rpp(
        "<REAPER_PROJECT 0.1 \"7.73\" 0\n  TEMPO 120 4 4\n  <TRACK\n    MIDIOUT 64\n    <ITEM\n      POSITION 0\n      LENGTH 10\n      <SOURCE LTC\n        STARTTIME 3600\n        FRAMERATE 25 0\n        SEND 2\n        USERDATA 0 0 0 0\n      >\n    >\n  >\n>\n",
    )
    .unwrap();
    assert_eq!(project.tracks[0].midi_out, Some((2, 0)));
    engine.sync(&project);
    engine.set_midi_output_devices(&[(2, name)]).unwrap();
    std::thread::sleep(Duration::from_millis(150));
    engine.play();
    std::thread::sleep(Duration::from_millis(500));
    engine.stop();
    std::thread::sleep(Duration::from_millis(100));
    let got: Vec<(Instant, Vec<u8>)> =
        recorder.take().into_iter().filter(|m| m.1[0] == 0xf0 || m.1[0] == 0xf1).collect();
    assert_eq!(got[0].1, [0xf0, 0x7f, 0x7f, 1, 1, 0x21, 0, 0, 0, 0xf7]);
    let pieces: Vec<u8> = got[1..17].iter().map(|m| m.1[1]).collect();
    assert_eq!(
        pieces,
        [0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x61, 0x72, 0x02, 0x10, 0x20, 0x30, 0x40, 0x50, 0x61, 0x72]
    );
    let qf: Vec<Instant> = got[1..].iter().map(|m| m.0).collect();
    let span = qf.last().unwrap().duration_since(qf[0]).as_secs_f64() / (qf.len() - 1) as f64;
    assert!((span - 0.01).abs() < 0.001, "a quarter frame every 10 ms: {span}");
}
