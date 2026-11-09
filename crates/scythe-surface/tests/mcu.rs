mod common;

use scythe_core::units::gain_to_db;
use scythe_surface::hui::Hui;
use scythe_surface::mcu::{self, Mcu};
use scythe_surface::view::View;

fn has(out: &[Vec<u8>], msg: &[u8]) -> bool {
    out.iter().any(|m| m == msg)
}

fn lcd(out: &[Vec<u8>], model: u8, offset: u8) -> Option<String> {
    out.iter().find_map(|m| match m.as_slice() {
        [0xf0, 0x00, 0x00, 0x66, id, 0x12, o, text @ .., 0xf7] if *id == model && *o == offset => {
            Some(String::from_utf8_lossy(text).into_owned())
        }
        _ => None,
    })
}

#[test]
fn mackie_control_shows_the_strips_as_reaper_does() {
    let mut s = common::session("mcu-feedback");
    let mut m = Mcu::new(false, 0);
    let out = m.feedback(&View::read(&s), 0);
    // Scribble strips: six characters and a space per strip, blank for empty strips.
    assert_eq!(lcd(&out, 0x14, 0).as_deref(), Some("Kick   "));
    assert_eq!(lcd(&out, 0x14, 7).as_deref(), Some("Bass   "));
    assert_eq!(lcd(&out, 0x14, 14).as_deref(), Some("Vox    "));
    assert_eq!(lcd(&out, 0x14, 21).as_deref(), Some("       "));
    // Meter mode per strip, as REAPER sets it.
    assert!(has(&out, &[0xf0, 0x00, 0x00, 0x66, 0x14, 0x20, 0x03, 0x03, 0xf7]));
    // Faders: REAPER's fader curve over 14 bits; an empty strip sits at the floor (41).
    assert!(has(&out, &mcu::pitch_bend(0, 0.716)));
    assert!(has(&out, &[0xe3, 41, 0]));
    // Mute on Bass, solo on Vox, V-pot rings centred on empty strips (6).
    assert!(has(&out, &[0x90, 0x11, 0x7f]));
    assert!(has(&out, &[0x90, 0x0a, 0x7f]));
    assert!(has(&out, &[0xb0, 0x33, 0x06]));
    assert!(has(&out, &[0xb0, 0x31, mcu::pan_ring(-0.25)]));
    // Stopped: the stop LED lit, beats shown, time display at bar 1 beat 1 as REAPER sends it.
    assert!(has(&out, &[0x90, 0x5d, 0x7f]));
    assert!(has(&out, &[0x90, 0x72, 0x7f]));
    for (cc, v) in [
        (0x40, 0x30),
        (0x41, 0x30),
        (0x42, 0x30),
        (0x43, 0),
        (0x44, 0),
        (0x45, 0x31),
        (0x46, 0),
        (0x47, 0x31),
        (0x48, 0),
        (0x49, 0),
    ] {
        assert!(has(&out, &[0xb0, cc, v]), "time digit {cc:x}");
    }
    // Nothing changed: nothing more to send.
    assert!(m.feedback(&View::read(&s), 0).is_empty());
    s.edit("rename", |p| p.tracks[0].name = "Kick In".into());
    let out = m.feedback(&View::read(&s), 0);
    assert_eq!(
        out,
        vec![vec![0xf0, 0x00, 0x00, 0x66, 0x14, 0x12, 0x00, b'K', b'i', b'c', b'k', b' ', b'I', b' ', 0xf7]]
    );
}

#[test]
fn mackie_control_input_reaches_the_state_reaper_reached() {
    // The device messages of a scripted session against REAPER 7.73 (`MCU 0 0 0 0 0`), and the
    // state REAPER ended up in, read back with GetMediaTrackInfo_Value.
    let mut s = common::session("mcu-input");
    let mut m = Mcu::new(false, 0);
    let mut bank = 0;
    for msg in [
        &[0x90, 0x68, 0x7f][..],
        &[0xe0, 0x00, 0x40],
        &[0x90, 0x68, 0x00],
        &[0x90, 0x10, 0x7f],
        &[0x90, 0x10, 0x00],
        &[0x90, 0x09, 0x7f],
        &[0x90, 0x09, 0x00],
        &[0x90, 0x1a, 0x7f],
        &[0x90, 0x1a, 0x00],
        &[0xb0, 0x11, 0x41],
        &[0xb0, 0x12, 0x03],
        &[0x90, 0x01, 0x7f],
        &[0x90, 0x01, 0x00],
        &[0x90, 0x59, 0x7f],
        &[0x90, 0x59, 0x00],
    ] {
        m.handle(&mut s, &mut bank, msg);
    }
    let kick = common::track(&s, 0);
    // REAPER: 0.279976 (-11.06 dB); Scythe's fader curve is interpolated from measured points.
    assert!((gain_to_db(kick.volume) - gain_to_db(0.279976)).abs() < 0.1, "{}", gain_to_db(kick.volume));
    assert!(kick.muted);
    let bass = common::track(&s, 1);
    assert!((bass.pan - -0.2823).abs() < 1e-4, "{}", bass.pan);
    assert!(bass.solo && bass.armed && bass.muted);
    let vox = common::track(&s, 2);
    assert!((vox.pan - 0.5968).abs() < 1e-4, "{}", vox.pan);
    assert_eq!(m.selected(), Some(vox.id));
    assert!(s.metronome());
    let out = m.feedback(&View::read(&s), bank);
    assert!(has(&out, &[0x90, 0x1a, 0x7f]), "select LED");
    assert!(has(&out, &[0x90, 0x4c, 0x7f]), "trim/read automation LED for the selected track");
    assert!(has(&out, &[0x90, 0x59, 0x7f]), "click LED");
}

#[test]
fn banks_flip_and_extenders() {
    let mut s = common::session("mcu-banks");
    for i in 0..10 {
        s.edit("add", |p| {
            let mut t = p.tracks[0].clone();
            t.id = scythe_core::TrackId(100 + i);
            t.name = format!("T{}", i + 4);
            p.tracks.push(t);
        });
    }
    let mut m = Mcu::new(false, 0);
    let mut x = Mcu::new(true, 8);
    let mut bank = 0;
    let view = View::read(&s);
    // The extender shows the next eight tracks, with its own SysEx model id.
    assert_eq!(lcd(&x.feedback(&view, bank), 0x15, 0).as_deref(), Some("T9     "));
    m.feedback(&view, bank);
    m.handle(&mut s, &mut bank, &[0x90, mcu::BANK_RIGHT, 0x7f]);
    assert_eq!(bank, 8);
    let view = View::read(&s);
    assert_eq!(lcd(&m.feedback(&view, bank), 0x14, 0).as_deref(), Some("T9     "));
    m.handle(&mut s, &mut bank, &[0x90, mcu::CHANNEL_LEFT, 0x7f]);
    assert_eq!(bank, 7);
    m.handle(&mut s, &mut bank, &[0x90, mcu::BANK_LEFT, 0x7f]);
    assert_eq!(bank, 0);
    // Flip: the fader moves the pan.
    m.handle(&mut s, &mut bank, &[0x90, mcu::FLIP, 0x7f]);
    m.handle(&mut s, &mut bank, &[0xe0, 0x7f, 0x7f]);
    assert!((common::track(&s, 0).pan - 1.0).abs() < 1e-9);
    let out = m.feedback(&View::read(&s), bank);
    assert!(has(&out, &[0x90, mcu::FLIP, 0x7f]));
}

#[test]
fn transport_buttons_and_footswitches() {
    let mut s = common::session("mcu-transport");
    let mut m = Mcu::new(false, 0);
    let mut bank = 0;
    m.handle(&mut s, &mut bank, &[0x90, mcu::PLAY, 0x7f]);
    assert!(s.is_playing());
    let out = m.feedback(&View::read(&s), bank);
    assert!(has(&out, &[0x90, mcu::PLAY, 0x7f]) && has(&out, &[0x90, mcu::STOP, 0x00]));
    m.handle(&mut s, &mut bank, &[0x90, mcu::STOP, 0x7f]);
    assert!(!s.is_playing());
    m.handle(&mut s, &mut bank, &[0x90, mcu::CYCLE, 0x7f]);
    assert!(s.project.loop_enabled);
    // F2 goes to marker 2; the right arrow to the next marker.
    m.handle(&mut s, &mut bank, &[0x90, mcu::F1 + 1, 0x7f]);
    assert_eq!(s.project.cursor, 8.0);
    m.handle(&mut s, &mut bank, &[0x90, mcu::LEFT, 0x7f]);
    assert_eq!(s.project.cursor, 4.0);
    // Master fader.
    m.handle(&mut s, &mut bank, &[0xe8, 0x00, 0x00]);
    assert_eq!(s.project.master.volume, 0.0);
}

#[test]
fn hui_strips_and_transport() {
    let mut s = common::session("hui");
    let mut h = Hui::new(0);
    let mut bank = 0;
    let out = h.feedback(&View::read(&s), bank);
    assert!(has(&out, &[0xf0, 0x00, 0x00, 0x66, 0x05, 0x00, 0x10, 0x00, b'K', b'i', b'c', b'k', 0xf7]));
    // Bass muted: zone 1, port 2 on.
    let at = out.iter().position(|m| m == &vec![0xb0, 0x0c, 0x01]).unwrap();
    assert!(out[at..].windows(2).any(|w| w[0] == [0xb0, 0x0c, 0x01] && w[1] == [0xb0, 0x2c, 0x42]));
    // Transport LEDs as REAPER sends them when stopped: play, stop and record dark.
    for port in [0x03, 0x04, 0x05] {
        assert!(out.windows(2).any(|w| w[0] == [0xb0, 0x0c, 0x0e] && w[1] == [0xb0, 0x2c, port]));
    }
    // Press mute on strip 1 (zone 0, port 2), then play.
    for msg in
        [&[0xb0, 0x0f, 0x00][..], &[0xb0, 0x2f, 0x42], &[0xb0, 0x2f, 0x02], &[0xb0, 0x0f, 0x0e], &[0xb0, 0x2f, 0x44]]
    {
        h.handle(&mut s, &mut bank, msg);
    }
    assert!(common::track(&s, 0).muted);
    assert!(s.is_playing());
    // A fader move: MSB then LSB.
    h.handle(&mut s, &mut bank, &[0xb0, 0x02, 0x00]);
    h.handle(&mut s, &mut bank, &[0xb0, 0x22, 0x00]);
    assert_eq!(common::track(&s, 2).volume, 0.0);
}
