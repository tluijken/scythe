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
        self.0
            .with(|e| {
                e.params()
                    .into_iter()
                    .map(|p| {
                        let (value, text) = e.param(p.id).unwrap_or((p.default, String::new()));
                        FxParam { name: p.name, min: p.min, max: p.max, value, text }
                    })
                    .collect()
            })
            .unwrap_or_default()
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
