//! ScyEQ-Pro's editor (`scyeq_ui`) on the plugin's shared half: changes go to the host as
//! parameter gestures through the instance's `GuiContext`.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use plugin_format::{GuiContext, Shared, egui};
use scyeq_dsp::probe::FxProbe;
use scyeq_dsp::{Meters, ScytheEq, params};
use scyeq_ui::{CompareSource, EditorState, EqHost, Instance, InstanceList, ReferenceAudio, ViewState};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::plugin::ScyEqShared;

/// One open editor window.
pub(crate) struct Editor {
    shared: Arc<ScyEqShared>,
    cx: GuiContext,
    state: EditorState,
    themed: bool,
}

impl Editor {
    pub(crate) fn new(shared: Arc<ScyEqShared>, cx: GuiContext) -> Self {
        Self { shared, cx, state: EditorState::default(), themed: false }
    }

    pub(crate) fn frame(&mut self, root: &mut egui::Ui) {
        if !std::mem::replace(&mut self.themed, true) {
            scyeq_ui::theme::install(root.ctx());
        }
        let view = self.shared.view();
        let mut host = PluginHost { shared: &self.shared, cx: &self.cx, values: self.shared.values(), view };
        egui::CentralPanel::default().show(root, |ui| scyeq_ui::show(ui, &mut host, &mut self.state));
        if host.view.instances {
            let ctx = root.ctx().clone();
            let mut list = ThisInstance { shared: &self.shared, cx: &self.cx };
            scyeq_ui::instances_window(&ctx, &mut list, &mut host.view.instances);
        }
        self.shared.set_view(host.view);
    }
}

impl Drop for Editor {
    fn drop(&mut self) {
        let cx = &self.cx;
        self.state.end_gesture(|id| cx.end(id));
    }
}

/// The instance behind an editor, for one frame.
struct PluginHost<'a> {
    shared: &'a ScyEqShared,
    cx: &'a GuiContext,
    values: Vec<f64>,
    view: ViewState,
}

impl EqHost for PluginHost<'_> {
    fn values(&self) -> &[f64] {
        &self.values
    }

    fn begin(&mut self, id: u32) {
        self.cx.begin(id);
    }

    fn set(&mut self, id: u32, value: f64) {
        self.cx.set(id, value);
        if let Some(at) = self.values.get_mut(id as usize) {
            *at = self.shared.param(id);
        }
    }

    fn end(&mut self, id: u32) {
        self.cx.end(id);
    }

    /// Made on a copy of the values, then sent as a gesture per changed parameter: a host
    /// records each, and gets a dirty mark for the edit as a whole.
    fn edit(&mut self, _label: &str, f: &mut dyn FnMut(&mut ScytheEq)) {
        let mut eq = ScytheEq::with_values(self.values.clone(), self.shared.sample_rate());
        f(&mut eq);
        let mut changed = false;
        for id in params::ids() {
            let (old, new) = (self.values[id as usize], eq.value(id));
            if old == new {
                continue;
            }
            self.cx.begin(id);
            self.cx.set(id, new);
            self.cx.end(id);
            self.values[id as usize] = self.shared.param(id);
            changed = true;
        }
        if changed {
            self.cx.mark_dirty();
        }
    }

    fn meters(&self) -> Option<Arc<Meters>> {
        self.shared.meters()
    }

    fn probe(&self) -> Option<Arc<FxProbe>> {
        Some(self.shared.probe().clone())
    }

    fn latency(&self) -> usize {
        self.shared.latency() as usize
    }

    fn sample_rate(&self) -> f64 {
        self.shared.sample_rate() as f64
    }

    fn instances(&self) -> Vec<Instance> {
        Vec::new()
    }

    fn compare_sources(&self) -> Vec<CompareSource> {
        Vec::new()
    }

    fn source_probe(&self, _id: egui::Id) -> Option<Arc<FxProbe>> {
        None
    }

    fn pick_reference(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new().add_filter("Audio", &["wav", "flac", "mp3", "ogg", "aif", "aiff"]).pick_file()
    }

    fn decode_reference(&self) -> Option<scyeq_ui::DecodeFn> {
        Some(load_reference)
    }

    fn view(&mut self) -> &mut ViewState {
        &mut self.view
    }
}

/// The instance list with the one instance this editor belongs to.
struct ThisInstance<'a> {
    shared: &'a ScyEqShared,
    cx: &'a GuiContext,
}

impl ThisInstance<'_> {
    fn id() -> egui::Id {
        egui::Id::unique("scyeq-plugin-this-instance")
    }
}

impl InstanceList for ThisInstance<'_> {
    fn instance_list(&self) -> Vec<Instance> {
        let label = self.shared.track().name.unwrap_or_else(|| "ScyEQ-Pro".to_owned());
        vec![Instance { id: Self::id(), label }]
    }

    fn instance_values(&self, id: egui::Id) -> Option<Vec<f64>> {
        (id == Self::id()).then(|| self.shared.values())
    }

    fn instance_enabled(&self, _id: egui::Id) -> bool {
        true
    }

    fn toggle_instance_bypass(&mut self, _id: egui::Id) {}

    fn instance_open(&self, id: egui::Id) -> bool {
        id == Self::id()
    }

    fn toggle_instance_open(&mut self, _id: egui::Id) {}

    fn begin_instance(&mut self, id: egui::Id, param: u32) {
        if id == Self::id() {
            self.cx.begin(param);
        }
    }

    fn set_instance(&mut self, id: egui::Id, param: u32, value: f64) {
        if id == Self::id() {
            self.cx.set(param, value);
        }
    }

    fn end_instance(&mut self, id: egui::Id, param: u32) {
        if id == Self::id() {
            self.cx.end(param);
        }
    }
}

/// A reference file for EQ Match, decoded with Symphonia as Scythe decodes media.
fn load_reference(path: &Path) -> Result<ReferenceAudio, String> {
    let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut format = symphonia::default::get_probe()
        .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| e.to_string())?;
    let track = format.default_track(TrackType::Audio).ok_or("no audio track")?;
    let track_id = track.id;
    let codec = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("no audio codec parameters")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(codec, &AudioDecoderOptions::default())
        .map_err(|e| e.to_string())?;
    let mut sample_rate = codec.sample_rate.unwrap_or(0);
    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut planes: Vec<Vec<f32>> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) | Err(SymphoniaError::ResetRequired) => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                sample_rate = buf.spec().rate();
                buf.copy_to_vecs_planar::<f32>(&mut planes);
                let frames = buf.frames();
                if channels.len() < planes.len() {
                    let have = channels.first().map_or(0, Vec::len);
                    channels.resize_with(planes.len(), || vec![0.0; have]);
                }
                for (dst, src) in channels.iter_mut().zip(&planes) {
                    dst.extend_from_slice(&src[..frames.min(src.len())]);
                }
            }
            // A corrupt packet is skipped; the rest of the stream is usually fine.
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    if channels.is_empty() || sample_rate == 0 {
        return Err("the file holds no audio".to_owned());
    }
    channels.truncate(2);
    Ok(ReferenceAudio { channels, sample_rate })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16-bit PCM WAV file of `frames` interleaved.
    fn wav(rate: u32, channels: u16, frames: &[i16]) -> Vec<u8> {
        let data = (frames.len() * 2) as u32;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data.to_le_bytes());
        for s in frames {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    #[test]
    fn a_reference_file_decodes_to_planar_channels() {
        let path = std::env::temp_dir().join(format!("scyeq-plugin-reference-{}.wav", std::process::id()));
        std::fs::write(&path, wav(44_100, 2, &[16384, -16384, 8192, 0])).unwrap();
        let audio = load_reference(&path);
        std::fs::remove_file(&path).unwrap();
        let audio = audio.unwrap();
        assert_eq!(audio.sample_rate, 44_100);
        assert_eq!(audio.channels, vec![vec![0.5, 0.25], vec![-0.5, 0.0]]);
    }

    #[test]
    fn a_file_that_is_no_audio_is_an_error() {
        let path = std::env::temp_dir().join(format!("scyeq-plugin-not-audio-{}.wav", std::process::id()));
        std::fs::write(&path, b"not audio").unwrap();
        let audio = load_reference(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(audio.is_err());
        assert!(load_reference(Path::new("/nonexistent/reference.wav")).is_err());
    }
}
