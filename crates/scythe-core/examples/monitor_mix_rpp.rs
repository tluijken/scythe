//! Writes a band project with monitor mixes, a cue bus and hardware outputs, for checking that
//! REAPER reads them back the same way (`--` then the output path).
//!
//! cargo run -p scythe-core --example monitor_mix_rpp -- /tmp/band.RPP

use scythe_core::monitor::{CueBusOptions, MonitorOutput};
use scythe_core::rpp::write_project;
use scythe_core::{HwOut, Project, SendMode};

fn main() {
    let path = std::env::args().nth(1).expect("output path");
    let mut p = Project::default();
    let ids: Vec<_> = ["Kick", "Snare", "Bass DI", "Guitar L", "Guitar R", "Lead vocal", "Click"]
        .into_iter()
        .map(|n| p.add_track(n))
        .collect();
    p.track_mut(ids[2]).unwrap().volume = 0.5;
    p.track_mut(ids[3]).unwrap().pan = -0.6;
    p.track_mut(ids[6]).unwrap().main_send = false;
    p.track_mut(ids[6]).unwrap().hw_outs.push(HwOut { volume: 0.7, ..HwOut::mono(7) });

    let opts = CueBusOptions { output: Some(MonitorOutput::Stereo(2)), ..CueBusOptions::default() };
    let drums = p.create_monitor_mix("Drummer", &ids, &ids[..2], &opts);
    p.set_monitor_level(drums, ids[6], 1.4);
    p.set_monitor_pan(drums, ids[3], -0.5);
    p.set_monitor_muted(drums, ids[4], true);
    p.set_more_me(drums, 6.0);
    let singer = p.copy_monitor_mix(drums, "Lead singer").unwrap();
    p.set_monitor_own(singer, &ids[5..6]);
    p.set_monitor_output(singer, Some(MonitorOutput::Mono(6)));
    let cue = CueBusOptions {
        mode: SendMode::PreFx,
        volumes_from_tracks: true,
        pans_from_tracks: true,
        output: Some(MonitorOutput::Stereo(8)),
        ..CueBusOptions::default()
    };
    p.create_cue_bus(&ids[2..5], &cue);
    p.master.hw_outs.push(HwOut { mode: SendMode::PostFx, volume: 0.5, ..HwOut::stereo(10) });
    write_project(&p, std::path::Path::new(&path)).expect("write");
    // The same dump `monitor_mix_readback.lua` prints from REAPER, to diff against.
    let g = |v: f64| format!("{v:.6}");
    let hw = |name: &str, o: &HwOut| {
        let f = [
            o.src_channels.to_string(),
            o.dst_channels.to_string(),
            o.mode.code().to_string(),
            g(o.volume),
            g(o.pan),
            (o.muted as u8).to_string(),
            (o.phase as u8).to_string(),
        ];
        println!("{name} hw {}", f.join(" "));
    };
    for o in &p.master.hw_outs {
        hw("MASTER", o);
    }
    for t in &p.tracks {
        let ext = |k: &str| t.ext(k).unwrap_or_default();
        println!(
            "{} mainsend {} ext [{}] [{}] [{}]",
            t.name,
            t.main_send as u8,
            ext("scythe_monitor"),
            ext("scythe_monitor_own"),
            ext("scythe_monitor_more_me")
        );
        for o in &t.hw_outs {
            hw(&t.name, o);
        }
        for s in &t.sends {
            let dest = &p.track(s.dest).unwrap().name;
            let f = [s.mode.code().to_string(), g(s.volume), g(s.pan), (s.muted as u8).to_string()];
            println!("{} send -> {dest} {}", t.name, f.join(" "));
        }
    }
}
