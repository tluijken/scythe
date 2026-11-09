#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use scythe_core::render_settings::ResampleMode;
use scythe_engine::{Engine, MediaPool};
use scythe_session::Session;

/// Three tracks as in the REAPER probes: Kick at 0 dB, Bass at -6 dB panned 25% left and
/// muted, Vox at +6 dB panned 50% right and soloed; markers 1 and 2 at 4 and 8 seconds.
pub const PROJECT: &str = r#"<REAPER_PROJECT 0.1 "7.73/linux-x86_64" 1700000000
  SAMPLERATE 48000 1 0
  TEMPO 120 4 4
  <TRACK
    NAME Kick
    VOLPAN 1 0 -1 -1 1
  >
  <TRACK
    NAME Bass
    VOLPAN 0.5 -0.25 -1 -1 1
    MUTESOLO 1 0 0
  >
  <TRACK
    NAME Vox
    VOLPAN 2 0.5 -1 -1 1
    MUTESOLO 0 2 0
  >
  MARKER 1 4 "Verse" 0
  MARKER 2 8 "Chorus" 0
>
"#;

pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("scythe-surface-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn session(name: &str) -> Session {
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    let mut s = Session::new(Engine::start_offline(pool, 48_000));
    let path = temp_dir(name).join("p.RPP");
    std::fs::write(&path, PROJECT).unwrap();
    s.open_path(&path).unwrap();
    s
}

pub fn track(s: &Session, i: usize) -> &scythe_core::Track {
    &s.project.tracks[i]
}
