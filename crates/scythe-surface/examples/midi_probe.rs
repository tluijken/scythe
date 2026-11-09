//! A scripted MIDI device on ALSA virtual ports, for comparing what Scythe and REAPER send to a
//! control surface: `midi_probe <name> <script> <log> <seconds>`. The script has one message
//! per line, `<seconds> <hex bytes>`; everything received is logged as `<seconds> <hex bytes>`.
//! `midi_probe --list` prints the MIDI devices as surfaces number them.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use midir::os::unix::{VirtualInput, VirtualOutput};
use midir::{MidiInput, MidiOutput};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--list") {
        for (i, n) in scythe_surface::midi::input_names().iter().enumerate() {
            println!("in {i} {n}");
        }
        for (i, n) in scythe_surface::midi::output_names().iter().enumerate() {
            println!("out {i} {n}");
        }
        return;
    }
    let [_, name, script, log, seconds] = &args[..] else {
        eprintln!("usage: midi_probe <name> <script> <log> <seconds>");
        std::process::exit(2);
    };
    let seconds: f64 = seconds.parse().expect("seconds");
    let start = Instant::now();
    let log = Arc::new(Mutex::new(std::fs::File::create(log).expect("log")));
    let sink = log.clone();
    let input = MidiInput::new(&format!("{name}-dev")).expect("ALSA sequencer");
    let _in = input
        .create_virtual(
            &format!("{name}-in"),
            move |_, bytes, _| {
                let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
                let line = format!("{:.3} {}\n", start.elapsed().as_secs_f64(), hex.join(" "));
                let _ = sink.lock().unwrap().write_all(line.as_bytes());
            },
            (),
        )
        .expect("virtual input");
    let output = MidiOutput::new(&format!("{name}-dev")).expect("ALSA sequencer");
    let mut out = output.create_virtual(&format!("{name}-out")).expect("virtual output");
    let mut steps: Vec<(f64, Vec<u8>)> = std::fs::read_to_string(script)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut parts = l.split_whitespace();
            let at = parts.next().unwrap().parse().unwrap();
            (at, parts.map(|h| u8::from_str_radix(h, 16).unwrap()).collect())
        })
        .collect();
    steps.reverse();
    while start.elapsed().as_secs_f64() < seconds {
        while steps.last().is_some_and(|(at, _)| *at <= start.elapsed().as_secs_f64()) {
            let (at, bytes) = steps.pop().unwrap();
            let _ = out.send(&bytes);
            let hex: Vec<String> = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let _ = writeln!(log.lock().unwrap(), "{at:.3} > {}", hex.join(" "));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}
