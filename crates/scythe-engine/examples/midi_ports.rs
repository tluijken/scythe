//! Lists the MIDI inputs and outputs scythe can open. With `--loopback <output>` it sends a
//! note to that output and reports what the inputs receive (wire it to an input, or use ALSA's
//! "Midi Through" port). With `--play <device>` the engine plays a MIDI item on a track that
//! sends to output `device` and reports what arrives back.
//!
//! cargo run -p scythe-engine --example midi_ports [-- --loopback "Midi Through:Midi Through Port-0 14:0"]

use std::time::Duration;

use scythe_engine::midi_io::{MidiInputs, MidiOut, input_names, output_names};

fn main() {
    println!("inputs:  {:?}", input_names());
    println!("outputs: {:?}", output_names());
    let (inputs, mut rings) = match MidiInputs::open_all() {
        Ok(open) => open,
        Err(e) => {
            println!("cannot open inputs: {e}");
            return;
        }
    };
    println!("opened {} input(s): {:?}", rings.len(), inputs.names);
    let args: Vec<String> = std::env::args().collect();
    if let Some(dev) = args.iter().position(|a| a == "--play").and_then(|i| args.get(i + 1)?.parse::<u8>().ok()) {
        let mut project = scythe_core::rpp::project_from_rpp(
            "<REAPER_PROJECT 0.1 \"7.73\" 0\n  TEMPO 120 4 4\n  <TRACK\n    <ITEM\n      POSITION 0.1\n      LENGTH 1\n      <SOURCE MIDI\n        HASDATA 1 960 QN\n        E 0 90 3c 64\n        E 480 b0 01 40\n        E 480 80 3c 00\n        E 960 b0 7b 00\n      >\n    >\n  >\n>\n",
        )
        .unwrap();
        project.tracks[0].midi_out = Some((dev, 3));
        let mut engine = scythe_engine::Engine::start_null(std::sync::Arc::default(), 48000, None);
        engine.sync(&project);
        engine.set_midi_outputs(true).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        let started = std::time::Instant::now();
        engine.play();
        std::thread::sleep(Duration::from_millis(900));
        engine.stop();
        std::thread::sleep(Duration::from_millis(200));
        for (name, ring) in inputs.names.iter().zip(&mut rings) {
            while let Ok(m) = ring.midi.pop() {
                println!("{:>5} ms {name}: {:02x?}", started.elapsed().as_millis(), &m.bytes[..m.len as usize]);
            }
        }
        return;
    }
    let Some(port) = args.iter().position(|a| a == "--loopback").and_then(|i| args.get(i + 1)) else { return };
    let mut out = MidiOut::open(port).expect("open output");
    out.send(&[0x92, 60, 100]).unwrap();
    out.send(&[0xb2, 1, 64]).unwrap();
    out.send(&[0x82, 60, 0]).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    for (name, ring) in inputs.names.iter().zip(&mut rings) {
        while let Ok(m) = ring.midi.pop() {
            println!("{name}: {:02x?}", &m.bytes[..m.len as usize]);
        }
    }
}
