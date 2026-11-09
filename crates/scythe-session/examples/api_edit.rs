//! Edits a project through the control API and saves it, for comparing Scythe's editing with
//! REAPER's: `api_edit in.RPP commands.json out.RPP`. `commands.json` is a list of commands;
//! a string `"@t:i"` anywhere in one names the `i`-th item (by position) on track `t` as the
//! project stands when the command runs, and `"@Tt"` track `t`. Media loads before the first
//! command, so transient detection sees the audio.

use std::sync::Arc;
use std::time::{Duration, Instant};

use scythe_core::render_settings::ResampleMode;
use scythe_engine::{Engine, MediaPool};
use scythe_session::Session;
use scythe_session::api::execute_json;
use serde_json::Value;

fn resolve(v: &mut Value, s: &Session) {
    match v {
        Value::String(text) if text.starts_with("@T") => {
            let id = text[2..].parse::<usize>().ok().and_then(|t| s.project.tracks.get(t)).map(|t| t.id.0);
            *v = id.map_or(Value::Null, Value::from);
        }
        Value::String(text) if text.starts_with('@') => {
            let mut parts = text[1..].split(':').map(|p| p.parse::<usize>().unwrap_or(0));
            let (t, i) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
            let id = s.project.tracks.get(t).and_then(|tr| {
                let mut items: Vec<_> = tr.items.iter().collect();
                items.sort_by(|a, b| a.position.total_cmp(&b.position));
                items.get(i).map(|it| it.id.0)
            });
            *v = id.map_or(Value::Null, Value::from);
        }
        Value::Array(list) => list.iter_mut().for_each(|x| resolve(x, s)),
        Value::Object(map) => map.values_mut().for_each(|x| resolve(x, s)),
        _ => {}
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, commands, output] = &args[..] else {
        eprintln!("usage: api_edit in.RPP commands.json out.RPP");
        std::process::exit(2);
    };
    let pool = Arc::new(MediaPool::with_resample(ResampleMode::PLAYBACK));
    let mut session = Session::new(Engine::start_offline(pool, 48_000));
    if let Err(e) = session.open_path(std::path::Path::new(input)) {
        eprintln!("{input}: {e}");
        std::process::exit(1);
    }
    let start = Instant::now();
    while session.media_loading() && start.elapsed() < Duration::from_secs(60) {
        session.poll();
        std::thread::sleep(Duration::from_millis(20));
    }
    let list: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(commands).expect("commands")).expect("json");
    for mut c in list {
        resolve(&mut c, &session);
        if let Err(e) = execute_json(&mut session, c.clone()) {
            eprintln!("{c}: {e}");
        }
    }
    if let Err(e) = session.save_to(std::path::Path::new(output)) {
        eprintln!("{output}: {e}");
        std::process::exit(1);
    }
}
