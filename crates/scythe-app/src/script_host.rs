//! What the app does for scripts: its running effects, installed plugins, media and
//! dialogs (`script::Services`, `script::LiveFx`).

use std::path::Path;
use std::sync::Arc;

use scythe_core::FxSlot;
use scythe_engine::MediaPool;
use scythe_engine::fx::FxInstance;
use serde_json::json;

use crate::script::{FxParam, LiveFx, Services};

/// A running effect, as the inspector's parameter rows reach it.
pub struct LiveInstance(pub Arc<FxInstance>);

impl LiveFx for LiveInstance {
    fn params(&self) -> Vec<FxParam> {
        let params = self.0.with(|e| e.params()).unwrap_or_default();
        let ids: Vec<u32> = params.iter().map(|p| p.id).collect();
        params
            .into_iter()
            .zip(self.0.param_values(&ids))
            .map(|(p, v)| {
                let (value, text) = v.unwrap_or((p.default, String::new()));
                FxParam { name: p.name, min: p.min, max: p.max, value, text }
            })
            .collect()
    }

    fn set_param(&self, index: usize, value: f64) {
        self.0.with(|e| {
            if let Some(p) = e.params().get(index) {
                e.set_param(p.id, value);
            }
        });
    }

    fn is_instrument(&self) -> bool {
        self.0.instrument
    }
}

/// The app's side of a running script.
pub struct AppServices {
    /// Slots for Scythe's own effects and the installed plugins, by name.
    pub plugins: Vec<FxSlot>,
    pub pool: Arc<MediaPool>,
}

impl Services for AppServices {
    fn message_box(&self, text: &str, title: &str, kind: i64) -> i64 {
        let request = json!({"type": "message", "text": text, "title": title, "kind": kind});
        crate::dialog::ask(&request).and_then(|v| v.as_i64()).unwrap_or(1)
    }

    fn user_inputs(&self, title: &str, captions: &[String], values: &[String]) -> Option<Vec<String>> {
        let request = json!({"type": "inputs", "title": title, "captions": captions, "values": values});
        let answer = crate::dialog::ask(&request)?;
        Some(answer.as_array()?.iter().map(|v| v.as_str().unwrap_or("").to_owned()).collect())
    }

    fn plugin(&self, name: &str) -> Option<FxSlot> {
        self.plugins.iter().find(|s| crate::script::fx_name_matches(&s.name, name)).cloned()
    }

    fn media_length(&self, path: &Path) -> Option<f64> {
        match self.pool.ready(path) {
            Some(m) => Some(m.duration()),
            None => scythe_engine::media::decode_file(path).ok().map(|a| a.duration()),
        }
    }
}

/// Runs `script.run` from the control API: the script sees the session's project, selected
/// track and cursor, and may add any installed plugin by name; it shows no dialogs (a message
/// box answers OK, user inputs are cancelled), and one that defers is stopped after its first
/// pass.
pub fn control_runner() -> scythe_session::api::script::ScriptRunner {
    use scythe_session::api::script::{Language, ScriptRun};
    Box::new(|session, language, code| {
        let fx = session.engine.fx();
        let mut plugins: Vec<FxSlot> = scythe_engine::builtin::Builtin::ALL.iter().map(|b| b.slot()).collect();
        plugins.extend(fx.clap_plugins().iter().map(scythe_engine::fx::clap_slot));
        plugins.extend(fx.lv2_plugins().iter().map(scythe_engine::fx::lv2_slot));
        plugins.extend(fx.vst3_plugins().iter().map(scythe_engine::fx::vst3_slot));
        let services = std::rc::Rc::new(Headless { plugins, pool: session.engine.pool().clone() });
        let ctx = crate::script::ScriptContext {
            selected_tracks: session.selected_track.into_iter().collect(),
            cursor: session.project.cursor,
            playing: session.is_playing(),
            play_position: session.project.cursor,
            services: Some(services as std::rc::Rc<dyn Services>),
            undo: session.history.undo_label().map(str::to_owned),
            redo: session.history.redo_label().map(str::to_owned),
            dirty: session.dirty,
            ..Default::default()
        };
        let name = match language {
            Language::Lua => "control.lua",
            Language::Eel => "control.eel",
        };
        let (mut out, running, mut error) = match crate::script::run(code, name, &session.project, ctx.clone()) {
            Ok((out, running)) => (out, running, None),
            Err(e) => (*e.outcome, None, Some(e.message)),
        };
        if let Some(script) = running {
            let base = out.project.clone().unwrap_or_else(|| session.project.clone());
            let last = match script.stop(&base, ctx) {
                Ok(last) => last,
                Err(e) => {
                    error = Some(e.message);
                    *e.outcome
                }
            };
            out.console.push_str(&last.console);
            out.project = last.project.or(out.project);
            out.undo = last.undo.or(out.undo);
            out.cursor = last.cursor.or(out.cursor);
        }
        Ok(ScriptRun { project: out.project, console: out.console, undo: out.undo, cursor: out.cursor, error })
    })
}

/// What a script run over the control API gets of the app: plugins and media, no dialogs.
struct Headless {
    plugins: Vec<FxSlot>,
    pool: Arc<MediaPool>,
}

impl Services for Headless {
    fn message_box(&self, _text: &str, _title: &str, _kind: i64) -> i64 {
        1
    }

    fn user_inputs(&self, _title: &str, _captions: &[String], _values: &[String]) -> Option<Vec<String>> {
        None
    }

    fn plugin(&self, name: &str) -> Option<FxSlot> {
        self.plugins.iter().find(|s| crate::script::fx_name_matches(&s.name, name)).cloned()
    }

    fn media_length(&self, path: &Path) -> Option<f64> {
        match self.pool.ready(path) {
            Some(m) => Some(m.duration()),
            None => scythe_engine::media::decode_file(path).ok().map(|a| a.duration()),
        }
    }
}
