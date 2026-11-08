//! Lists the MIDI inputs and outputs scythe can open. With `--loopback <output>` it sends a
//! note to that output and reports what the inputs receive (wire it to an input, or use ALSA's
//! "Midi Through" port).
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
    let Some(port) = args.iter().position(|a| a == "--loopback").and_then(|i| args.get(i + 1)) else { return };
    let mut out = MidiOut::open(port).expect("open output");
    out.send(&[0x92, 60, 100]).unwrap();
    out.send(&[0xb2, 1, 64]).unwrap();
    out.send(&[0x82, 60, 0]).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    for (name, ring) in inputs.names.iter().zip(&mut rings) {
        while let Ok(m) = ring.pop() {
            println!("{name}: {:02x?}", &m.bytes[..m.len as usize]);
        }
    }
}
