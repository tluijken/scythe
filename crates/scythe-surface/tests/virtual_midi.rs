#![cfg(unix)]
//! A Mackie Control on ALSA sequencer virtual ports, end to end through the surface host.
//! Skipped where there is no sequencer (no /dev/snd/seq).

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use midir::os::unix::{VirtualInput, VirtualOutput};
use midir::{MidiInput, MidiOutput};
use scythe_core::surface::{MidiKind, MidiSurface, Surface};
use scythe_surface::{SurfaceHost, midi};

#[test]
fn mackie_control_over_virtual_ports() {
    if !std::path::Path::new("/dev/snd/seq").exists() {
        eprintln!("no ALSA sequencer; skipped");
        return;
    }
    let name = format!("mcutest{}", std::process::id());
    let received = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
    let sink = received.clone();
    let device_in = MidiInput::new(&format!("{name}-dev")).unwrap();
    let _device_in = device_in
        .create_virtual(&format!("{name}-in"), move |_, bytes, _| sink.lock().unwrap().push(bytes.to_vec()), ())
        .unwrap();
    let device_out = MidiOutput::new(&format!("{name}-dev")).unwrap();
    let mut device_out = device_out.create_virtual(&format!("{name}-out")).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    let input = midi::input_names().iter().position(|n| n.contains(&format!("{name}-out"))).unwrap();
    let output = midi::output_names().iter().position(|n| n.contains(&format!("{name}-in"))).unwrap();

    let mut s = common::session("virtual-midi");
    s.prefs.surfaces = vec![Surface::Midi(MidiSurface::new(MidiKind::Mcu, input as i64, output as i64))];
    let mut host = SurfaceHost::new(Arc::new(|| {}));
    let pump = |host: &mut SurfaceHost, s: &mut scythe_session::Session, ms: u64| {
        let until = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < until {
            host.pump(s);
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    pump(&mut host, &mut s, 300);
    assert!(s.surface_status[0].running, "{:?}", s.surface_status);
    {
        let got = received.lock().unwrap();
        assert!(got.iter().any(|m| m == &[0xf0, 0x00, 0x00, 0x66, 0x14, 0x00, 0xf7]), "device query");
        let kick = [0xf0, 0x00, 0x00, 0x66, 0x14, 0x12, 0x00, b'K', b'i', b'c', b'k', b' ', b' ', b' ', 0xf7];
        assert!(got.iter().any(|m| m == &kick), "scribble strip");
        assert!(got.iter().any(|m| m == &[0x90, 0x11, 0x7f]), "Bass mute LED");
    }
    device_out.send(&[0x90, 0x10, 0x7f]).unwrap();
    device_out.send(&[0x90, 0x10, 0x00]).unwrap();
    device_out.send(&[0xe2, 0x00, 0x00]).unwrap();
    pump(&mut host, &mut s, 200);
    assert!(common::track(&s, 0).muted);
    assert_eq!(common::track(&s, 2).volume, 0.0);
    assert!(received.lock().unwrap().iter().any(|m| m == &[0x90, 0x10, 0x7f]), "Kick mute LED");
}
