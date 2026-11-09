//! An OSC control surface as REAPER runs one: messages from the device's address map to
//! actions through a `.ReaperOSC` pattern file, and every value the patterns name goes back
//! as feedback when it changes.

pub mod codec;
pub mod pattern;

use std::collections::HashMap;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use scythe_core::surface::{OSC_LEARN, OSC_RECEIVE, OSC_SEND, OscSurface as OscConfig};
use scythe_core::tempo::format_time;
use scythe_session::{AutoTarget, Session};
use serde_json::json;

use scythe_core::TrackId;

use crate::view::{self, TrackView, View};
use crate::{Waker, exec, run_action, set_pan, set_volume};
use codec::{Arg, Message};
use pattern::{Flag, PatternFile};

/// A feedback value in the forms a pattern's flag can ask for.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    /// `n` gets `normalized`, `f` gets `raw`, `s` gets `text`.
    Number {
        normalized: f64,
        raw: f64,
        text: String,
    },
    /// `b` and `t` get 0 or 1.
    Switch(bool),
    Text(String),
}

impl Value {
    fn level(gain: f64) -> Self {
        Self::Number { normalized: view::gain_to_fader(gain), raw: view::db_value(gain), text: view::db_text(gain) }
    }

    fn pan(pan: f64) -> Self {
        Self::Number { normalized: (pan + 1.0) / 2.0, raw: pan, text: view::pan_text(pan) }
    }

    fn arg(&self, flag: Flag) -> Option<Arg> {
        match (self, flag) {
            (Self::Number { normalized, .. }, Flag::Normalized) => Some(Arg::Float(*normalized as f32)),
            (Self::Number { raw, .. }, Flag::Float) => Some(Arg::Float(*raw as f32)),
            (Self::Number { text, .. } | Self::Text(text), Flag::Str) => Some(Arg::Str(text.clone())),
            (Self::Switch(on), Flag::Binary | Flag::Toggle) => Some(Arg::Float(if *on { 1.0 } else { 0.0 })),
            _ => None,
        }
    }
}

/// Where the device's view of the project stands.
#[derive(Debug, Clone)]
struct DeviceState {
    track_count: usize,
    marker_count: usize,
    region_count: usize,
    /// The first track of the bank, from 0.
    track_bank: usize,
    marker_bank: usize,
    region_bank: usize,
    /// The device's selected track: 0 is the master.
    selected: usize,
}

pub struct OscSurface {
    config: OscConfig,
    patterns: PatternFile,
    socket: UdpSocket,
    device: Option<SocketAddr>,
    rx: Receiver<(Message, SocketAddr)>,
    state: DeviceState,
    sent: HashMap<String, Arg>,
    /// Messages waiting for the wait between packets to pass.
    queue: Vec<Message>,
    sent_at: Instant,
    /// Rewind or fast forward held, and since when the playhead last moved for it.
    shuttle: Option<(f64, Instant)>,
    /// Problems to report once: a pattern file that did not read cleanly.
    pub warnings: Vec<String>,
}

impl OscSurface {
    pub fn open(config: &OscConfig, wake: Waker) -> Result<Self, String> {
        let (patterns, warnings) = load_patterns(&config.pattern);
        let listen = config.flags & OSC_RECEIVE != 0;
        let bind: SocketAddr = if listen { ([0, 0, 0, 0], config.local_port).into() } else { ([0, 0, 0, 0], 0).into() };
        let socket =
            UdpSocket::bind(bind).map_err(|e| format!("cannot listen on UDP port {}: {e}", config.local_port))?;
        let device = (config.device_ip.as_str(), config.device_port).to_socket_addrs().ok().and_then(|mut a| a.next());
        let (tx, rx) = channel();
        if listen {
            let reader = socket.try_clone().map_err(|e| e.to_string())?;
            std::thread::Builder::new()
                .name("scythe-osc".into())
                .spawn(move || {
                    let mut buf = vec![0u8; 65536];
                    while let Ok((n, from)) = reader.recv_from(&mut buf) {
                        let Some(messages) = codec::decode(&buf[..n]) else { continue };
                        if messages.into_iter().any(|m| tx.send((m, from)).is_err()) {
                            break;
                        }
                        wake();
                    }
                })
                .map_err(|e| e.to_string())?;
        }
        let s = &patterns.settings;
        let state = DeviceState {
            track_count: s.track_count,
            marker_count: s.marker_count,
            region_count: s.region_count,
            track_bank: 0,
            marker_bank: 0,
            region_bank: 0,
            selected: 0,
        };
        Ok(Self {
            config: config.clone(),
            patterns,
            socket,
            device,
            rx,
            state,
            sent: HashMap::new(),
            queue: Vec::new(),
            sent_at: Instant::now(),
            shuttle: None,
            warnings,
        })
    }

    /// The port it listens on (the configured one, or the one the system gave it).
    pub fn local_port(&self) -> u16 {
        self.socket.local_addr().map_or(self.config.local_port, |a| a.port())
    }

    /// Handles what the device sent, then sends the feedback that changed.
    pub fn pump(&mut self, session: &mut Session) {
        while let Ok((message, _)) = self.rx.try_recv() {
            let value = message.args.first().and_then(Arg::as_f64);
            if self.config.flags & OSC_LEARN != 0 && session.control_osc(&message.address, value) {
                continue;
            }
            self.handle(session, &message);
        }
        if let Some((speed, at)) = self.shuttle {
            let dt = at.elapsed().as_secs_f64();
            let to = (session.position() + speed * dt).max(0.0);
            let _ = exec(session, json!({ "cmd": "transport.seek", "position": to }));
            self.shuttle = Some((speed, Instant::now()));
        }
        if self.config.flags & OSC_SEND != 0 && self.device.is_some() {
            let view = View::read(session);
            self.feedback(&view);
        }
    }

    fn handle(&mut self, session: &mut Session, m: &Message) {
        let Some((action, pattern, values)) = self.patterns.find(&m.address) else { return };
        let (action, pattern) = (action.to_owned(), pattern.clone());
        let numbers: Vec<Vec<usize>> =
            values.iter().map(|v| v.iter().filter_map(|s| s.trim().parse::<usize>().ok()).collect()).collect();
        let arg = m.args.first();
        let num = arg.and_then(Arg::as_f64);
        let on = num.is_none_or(|v| v > 0.0);
        // A `t` pattern triggers on no argument or a non-zero one; a release (0) does nothing.
        if pattern.flag == Flag::Toggle && !on {
            return;
        }
        let slot = |i: usize| numbers.get(i).and_then(|v| v.first().copied());
        let track = |s: &Self| -> Option<usize> {
            if pattern.segments.first().map(String::as_str) == Some("master") {
                return Some(0);
            }
            match slot(0) {
                Some(k) if pattern.segments.first().map(String::as_str) == Some("track") && pattern.wildcards() > 0 => {
                    (k >= 1).then(|| s.state.track_bank + k)
                }
                _ => Some(s.state.selected),
            }
        };
        let view = View::read(session);
        match action.as_str() {
            "TRACK_VOLUME" | "MASTER_VOLUME" => {
                let (Some(t), Some(v)) = (track(self).and_then(|n| view.track(n)), num) else { return };
                let gain = match pattern.flag {
                    Flag::Normalized => view::fader_to_gain(v),
                    Flag::Float => scythe_core::units::db_to_gain(v),
                    _ => return,
                };
                set_volume(session, t.id, gain);
            }
            "TRACK_PAN" | "MASTER_PAN" => {
                let (Some(t), Some(v)) = (track(self).and_then(|n| view.track(n)), num) else { return };
                if pattern.flag == Flag::Normalized {
                    set_pan(session, t.id, v * 2.0 - 1.0);
                }
            }
            "TRACK_VOLUME_TOUCH" | "TRACK_PAN_TOUCH" => {
                let Some(id) = track(self).and_then(|n| view.track(n)).and_then(|t| t.id) else { return };
                if !on {
                    let target = if action == "TRACK_VOLUME_TOUCH" { AutoTarget::Volume } else { AutoTarget::Pan };
                    session.release_automation(id, target);
                }
            }
            "TRACK_MUTE" | "TRACK_SOLO" | "TRACK_REC_ARM" | "TRACK_MONITOR" => {
                let Some(n) = track(self) else { return };
                let Some(t) = view.track(n) else { return };
                if n == 0 {
                    return;
                }
                let (cmd, field, now) = match action.as_str() {
                    "TRACK_MUTE" => ("track.mute", "muted", t.muted),
                    "TRACK_SOLO" => ("track.solo", "solo", t.solo),
                    "TRACK_REC_ARM" => ("track.arm", "armed", t.armed),
                    _ => ("track.monitor", "monitor", t.monitor),
                };
                let value = if pattern.flag == Flag::Toggle { !now } else { on };
                let _ = exec(session, json!({ "cmd": cmd, "track": track_ref(t), field: value }));
            }
            "TRACK_SELECT" => {
                if let Some(id) = track(self).and_then(|n| view.track(n)).and_then(|t| t.id).filter(|_| on) {
                    session.select_track(id);
                }
            }
            "TRACK_NAME" => {
                let (Some(n), Some(name)) = (track(self), arg.and_then(Arg::as_str)) else { return };
                if let Some(t) = view.track(n).filter(|t| t.id.is_some()) {
                    let _ = exec(session, json!({ "cmd": "track.rename", "track": track_ref(t), "name": name }));
                }
            }
            "TRACK_SEND_VOLUME" | "TRACK_SEND_PAN" => {
                let (n, send) = if pattern.wildcards() >= 2 {
                    (slot(0).map(|k| self.state.track_bank + k), slot(1))
                } else {
                    (Some(self.state.selected), slot(0))
                };
                let (Some(n), Some(send), Some(v)) = (n, send, num) else { return };
                let Some(t) = view.track(n).filter(|t| t.id.is_some()) else { return };
                if pattern.flag != Flag::Normalized || send == 0 || send > t.sends.len() {
                    return;
                }
                let c = if action == "TRACK_SEND_VOLUME" {
                    json!({ "cmd": "send.volume", "track": track_ref(t), "send": send - 1, "volume": view::fader_to_gain(v) })
                } else {
                    json!({ "cmd": "send.pan", "track": track_ref(t), "send": send - 1, "pan": (v * 2.0 - 1.0).clamp(-1.0, 1.0) })
                };
                let _ = exec(session, c);
            }
            "FX_BYPASS" => {
                let (n, fx) = match pattern.wildcards() {
                    2 => (slot(0).map(|k| self.state.track_bank + k), slot(1)),
                    1 => (Some(self.state.selected), slot(0)),
                    _ => (Some(self.state.selected), Some(1)),
                };
                let (Some(n), Some(fx)) = (n, fx) else { return };
                let Some(t) = view.track(n) else { return };
                if fx == 0 || fx > t.fx.len() {
                    return;
                }
                let _ = exec(
                    session,
                    json!({ "cmd": "fx.bypass", "track": track_ref(t), "index": fx - 1, "bypassed": !on }),
                );
            }
            "PLAY" => {
                let _ = exec(session, json!({ "cmd": "transport.play" }));
            }
            "STOP" => {
                let _ = exec(session, json!({ "cmd": "transport.stop" }));
            }
            "PAUSE" => {
                let _ = exec(session, json!({ "cmd": "transport.pause" }));
            }
            "RECORD" => {
                let _ = exec(session, json!({ "cmd": "transport.record" }));
            }
            "REPEAT" => {
                let _ = exec(session, json!({ "cmd": "transport.loop", "enabled": !view.loop_enabled }));
            }
            "METRONOME" => session.toggle_metronome(),
            "SOLO_RESET" => {
                run_action(session, "40340");
            }
            "REWIND" | "FORWARD" => {
                let speed = if action == "REWIND" { -4.0 } else { 4.0 };
                self.shuttle = on.then(|| (speed, Instant::now()));
            }
            "GOTO_MARKER" | "GOTO_REGION" => {
                let number = if pattern.flag == Flag::Int { num.map(|v| v as i64) } else { slot(0).map(|v| v as i64) };
                if let Some(number) = number.filter(|n| *n >= 0) {
                    let region = action == "GOTO_REGION";
                    let _ = exec(session, json!({ "cmd": "marker.goto", "marker": number, "region": region }));
                }
            }
            "TEMPO" => {
                let bpm = match pattern.flag {
                    Flag::Float => num,
                    Flag::Normalized => num.map(|v| 40.0 + v.clamp(0.0, 1.0) * 256.0),
                    _ => None,
                };
                if let Some(bpm) = bpm.filter(|b| (20.0..=400.0).contains(b)) {
                    set_tempo(session, bpm);
                }
            }
            "ACTION" => {
                let id = match pattern.flag {
                    Flag::Int => num.map(|v| (v as i64).to_string()),
                    Flag::Str => arg.and_then(Arg::as_str).map(str::to_owned),
                    _ => values.first().and_then(|v| v.first()).cloned(),
                };
                if let Some(id) = id {
                    run_action(session, &id);
                }
            }
            "DEVICE_TRACK_COUNT" | "DEVICE_MARKER_COUNT" | "DEVICE_REGION_COUNT" => {
                let count = if pattern.flag == Flag::Int { num.map(|v| v as usize) } else { slot(0) };
                if let Some(c) = count.filter(|c| *c <= 1024) {
                    match action.as_str() {
                        "DEVICE_TRACK_COUNT" => self.state.track_count = c,
                        "DEVICE_MARKER_COUNT" => self.state.marker_count = c,
                        _ => self.state.region_count = c,
                    }
                    self.sent.clear();
                }
            }
            "DEVICE_TRACK_SELECT" => {
                let k = if pattern.flag == Flag::Int { num.map(|v| v as usize) } else { slot(0) };
                if let Some(k) = k.filter(|k| *k >= 1) {
                    self.state.selected = (self.state.track_bank + k).min(view.tracks.len());
                }
            }
            "DEVICE_PREV_TRACK" => self.state.selected = self.state.selected.saturating_sub(1),
            "DEVICE_NEXT_TRACK" => self.state.selected = (self.state.selected + 1).min(view.tracks.len()),
            "DEVICE_TRACK_BANK_SELECT" => {
                let b = if pattern.flag == Flag::Int { num.map(|v| v as usize) } else { slot(0) };
                if let Some(b) = b.filter(|b| *b >= 1) {
                    self.state.track_bank = (b - 1) * self.state.track_count;
                }
            }
            "DEVICE_PREV_TRACK_BANK" => {
                self.state.track_bank = self.state.track_bank.saturating_sub(self.state.track_count)
            }
            "DEVICE_NEXT_TRACK_BANK" => {
                if self.state.track_bank + self.state.track_count < view.tracks.len() {
                    self.state.track_bank += self.state.track_count;
                }
            }
            "DEVICE_MARKER_BANK_SELECT" => {
                let b = if pattern.flag == Flag::Int { num.map(|v| v as usize) } else { slot(0) };
                if let Some(b) = b.filter(|b| *b >= 1) {
                    self.state.marker_bank = (b - 1) * self.state.marker_count;
                }
            }
            "DEVICE_PREV_MARKER_BANK" => {
                self.state.marker_bank = self.state.marker_bank.saturating_sub(self.state.marker_count)
            }
            "DEVICE_NEXT_MARKER_BANK" => {
                if self.state.marker_bank + self.state.marker_count < view.markers.len() {
                    self.state.marker_bank += self.state.marker_count;
                }
            }
            "SETLIST_PLAY" => {
                run_action(session, "_SCYTHE_SETLIST_PLAY");
            }
            "SETLIST_STOP" => {
                run_action(session, "_SCYTHE_SETLIST_STOP");
            }
            "SETLIST_NEXT" => {
                run_action(session, "_SCYTHE_SETLIST_NEXT");
            }
            "SETLIST_PREVIOUS" => {
                run_action(session, "_SCYTHE_SETLIST_PREVIOUS");
            }
            _ => {}
        }
    }

    /// Every feedback message the patterns name, for the state in `view`.
    fn messages(&self, view: &View) -> Vec<Message> {
        let mut out = Vec::new();
        let mut put = |action: &str, values: &[usize], value: Value| {
            for p in self.patterns.patterns(action) {
                if p.wildcards() != values.len() {
                    continue;
                }
                if let (Some(arg), Some(address)) = (value.arg(p.flag), p.address(values)) {
                    out.push(Message::new(address, vec![arg]));
                }
            }
        };
        let playing = view.playing || view.recording;
        put("PLAY", &[], Value::Switch(view.playing && !view.paused));
        put("STOP", &[], Value::Switch(!playing && !view.paused));
        put("PAUSE", &[], Value::Switch(view.paused));
        put("RECORD", &[], Value::Switch(view.recording));
        put("REPEAT", &[], Value::Switch(view.loop_enabled));
        put("METRONOME", &[], Value::Switch(view.metronome));
        put("ANY_SOLO", &[], Value::Switch(view.any_solo()));
        put(
            "TEMPO",
            &[],
            Value::Number {
                normalized: ((view.tempo - 40.0) / 256.0).clamp(0.0, 1.0),
                raw: view.tempo,
                text: trim_number(view.tempo),
            },
        );
        put("TIME", &[], Value::Number { normalized: 0.0, raw: view.position, text: format_time(view.position) });
        put("BEAT", &[], Value::Text(view.bar_beat.clone()));
        let (loop_start, loop_end) = view.loop_range.unwrap_or((0.0, 0.0));
        put("LOOP_START_TIME", &[], Value::Number { normalized: 0.0, raw: loop_start, text: String::new() });
        put("LOOP_END_TIME", &[], Value::Number { normalized: 0.0, raw: loop_end, text: String::new() });
        let last = view.last_marker();
        put("LAST_MARKER_NAME", &[], Value::Text(last.map(|m| m.name.clone()).unwrap_or_default()));
        put("LAST_MARKER_NUMBER", &[], Value::Text(last.map(|m| m.number.to_string()).unwrap_or_default()));
        put("LAST_MARKER_TIME", &[], raw(last.map_or(0.0, |m| m.position)));
        let region = view.current_region();
        put("LAST_REGION_NAME", &[], Value::Text(region.map(|m| m.name.clone()).unwrap_or_default()));
        put("LAST_REGION_NUMBER", &[], Value::Text(region.map(|m| m.number.to_string()).unwrap_or_default()));
        put("LAST_REGION_TIME", &[], raw(region.map_or(0.0, |m| m.position)));
        put("LAST_REGION_LENGTH", &[], raw(region.map_or(0.0, |m| m.end.unwrap_or(m.position) - m.position)));
        for k in 1..=self.state.marker_count {
            let m = view.markers.get(self.state.marker_bank + k - 1);
            put("MARKER_NAME", &[k], Value::Text(m.map(|m| m.name.clone()).unwrap_or_default()));
            put("MARKER_NUMBER", &[k], Value::Text(m.map(|m| m.number.to_string()).unwrap_or_default()));
            put("MARKER_TIME", &[k], raw(m.map_or(0.0, |m| m.position)));
        }
        for k in 1..=self.state.region_count {
            let m = view.regions.get(self.state.region_bank + k - 1);
            put("REGION_NAME", &[k], Value::Text(m.map(|m| m.name.clone()).unwrap_or_default()));
            put("REGION_NUMBER", &[k], Value::Text(m.map(|m| m.number.to_string()).unwrap_or_default()));
            put("REGION_TIME", &[k], raw(m.map_or(0.0, |m| m.position)));
            put("REGION_LENGTH", &[k], raw(m.map_or(0.0, |m| m.end.unwrap_or(m.position) - m.position)));
        }
        let master = &view.master;
        put("MASTER_VOLUME", &[], Value::level(master.volume));
        put("MASTER_PAN", &[], Value::pan(master.pan));
        put("MASTER_VU", &[], vu(master.peak.0.max(master.peak.1)));
        put("MASTER_VU_L", &[], vu(master.peak.0));
        put("MASTER_VU_R", &[], vu(master.peak.1));
        for k in 1..=self.state.track_count {
            let n = self.state.track_bank + k;
            put("DEVICE_TRACK_SELECT", &[k], Value::Switch(self.state.selected == n));
        }
        let counts = (self.patterns.settings.send_count, self.patterns.settings.fx_count);
        let selected = view.track(self.state.selected).cloned().unwrap_or_else(|| empty_track(self.state.selected));
        track_feedback(&mut put, &[], &selected, view.selected, counts);
        let fx = selected.fx.first();
        put("FX_NAME", &[], Value::Text(fx.map(|f| f.name.clone()).unwrap_or_default()));
        put("FX_NUMBER", &[], Value::Text(fx.map(|_| "1".to_owned()).unwrap_or_default()));
        put("FX_BYPASS", &[], Value::Switch(fx.is_some_and(|f| !f.enabled)));
        for k in 1..=self.state.track_count {
            let n = self.state.track_bank + k;
            let t = view.track(n).cloned().unwrap_or_else(|| empty_track(n));
            track_feedback(&mut put, &[k], &t, view.selected, counts);
        }
        let s = &view.setlist;
        put("SETLIST_NAME", &[], Value::Text(s.name.clone().unwrap_or_default()));
        let song = s.current.as_ref();
        put("SETLIST_SONG", &[], Value::Text(song.map(|e| e.name.clone()).unwrap_or_default()));
        put("SETLIST_SONG_NUMBER", &[], Value::Text(song.map(|e| (e.entry + 1).to_string()).unwrap_or_default()));
        put("SETLIST_NEXT_SONG", &[], Value::Text(s.next.as_ref().map(|e| e.name.clone()).unwrap_or_default()));
        out
    }

    fn feedback(&mut self, view: &View) {
        let Some(device) = self.device else { return };
        let wait = Duration::from_millis(self.config.wait_ms as u64);
        for m in self.messages(view) {
            let Some(arg) = m.args.first() else { continue };
            if self.sent.get(&m.address) != Some(arg) {
                self.sent.insert(m.address.clone(), arg.clone());
                self.queue.retain(|q| q.address != m.address);
                self.queue.push(m);
            }
        }
        if self.queue.is_empty() || self.sent_at.elapsed() < wait {
            return;
        }
        let max = (self.config.max_packet as usize).max(64);
        let mut size = 0;
        let take = self
            .queue
            .iter()
            .take_while(|m| {
                size += m.encode().len() + 4;
                size <= max * 16
            })
            .count()
            .max(1);
        let batch: Vec<Message> = self.queue.drain(..take).collect();
        for packet in codec::pack(&batch, max) {
            let _ = self.socket.send_to(&packet, device);
        }
        self.sent_at = Instant::now();
    }
}

fn raw(v: f64) -> Value {
    Value::Number { normalized: 0.0, raw: v, text: String::new() }
}

fn vu(peak: f32) -> Value {
    let position = view::meter_position(peak, 60.0);
    Value::Number { normalized: position, raw: position, text: String::new() }
}

/// `120` for 120 bpm, `92.5` for 92.5: as few digits as the value needs.
fn trim_number(v: f64) -> String {
    let s = format!("{v:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn empty_track(number: usize) -> TrackView {
    TrackView {
        id: None,
        number,
        name: format!("Track {number}"),
        volume: 0.0,
        pan: 0.0,
        muted: false,
        solo: false,
        armed: false,
        monitor: false,
        auto_mode: scythe_core::AutoMode::TrimRead,
        peak: (0.0, 0.0),
        sends: Vec::new(),
        fx: Vec::new(),
    }
}

/// A track's feedback; empty send and FX slots up to the device's counts read as REAPER sends
/// them (`Send 2`, no FX name).
fn track_feedback(
    put: &mut impl FnMut(&str, &[usize], Value),
    slot: &[usize],
    t: &TrackView,
    selected: Option<TrackId>,
    (sends, fx_count): (usize, usize),
) {
    put("TRACK_NAME", slot, Value::Text(t.name.clone()));
    put("TRACK_NUMBER", slot, Value::Text(t.number.to_string()));
    put("TRACK_SELECT", slot, Value::Switch(t.id.is_some() && t.id == selected));
    put("TRACK_VOLUME", slot, Value::level(t.volume));
    put("TRACK_PAN", slot, Value::pan(t.pan));
    put("TRACK_MUTE", slot, Value::Switch(t.muted));
    put("TRACK_SOLO", slot, Value::Switch(t.solo));
    put("TRACK_REC_ARM", slot, Value::Switch(t.armed));
    put("TRACK_MONITOR", slot, Value::Switch(t.monitor));
    put("TRACK_VU", slot, vu(t.peak.0.max(t.peak.1)));
    put("TRACK_VU_L", slot, vu(t.peak.0));
    put("TRACK_VU_R", slot, vu(t.peak.1));
    for s in 1..=sends {
        let send = t.sends.get(s - 1);
        let at: Vec<usize> = slot.iter().copied().chain([s]).collect();
        put("TRACK_SEND_NAME", &at, Value::Text(send.map_or_else(|| format!("Send {s}"), |s| s.name.clone())));
        put("TRACK_SEND_VOLUME", &at, Value::level(send.map_or(0.0, |s| s.volume)));
        let pan = send.map_or(0.0, |s| s.pan);
        put(
            "TRACK_SEND_PAN",
            &at,
            Value::Number { normalized: (pan + 1.0) / 2.0, raw: pan, text: view::send_pan_text(pan) },
        );
    }
    for i in 0..fx_count.max(t.fx.len()) {
        let fx = t.fx.get(i);
        let at: Vec<usize> = slot.iter().copied().chain([i + 1]).collect();
        put("FX_NAME", &at, Value::Text(fx.map(|f| f.name.clone()).unwrap_or_default()));
        put("FX_NUMBER", &at, Value::Text(fx.map(|_| (i + 1).to_string()).unwrap_or_default()));
        put("FX_BYPASS", &at, Value::Switch(fx.is_some_and(|f| !f.enabled)));
    }
}

fn track_ref(t: &TrackView) -> serde_json::Value {
    match t.id {
        Some(id) => json!(id.0),
        None => json!("master"),
    }
}

/// Folders searched for pattern files: Scythe's own, then REAPER's.
pub fn pattern_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = scythe_session::preferences_path().parent() {
        dirs.push(dir.join("OSC"));
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
    if let Some(config) = config {
        dirs.push(config.join("REAPER").join("OSC"));
    }
    dirs
}

/// The pattern files that can be picked, by name.
pub fn pattern_names() -> Vec<String> {
    let mut names: Vec<String> = pattern_dirs()
        .iter()
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().and_then(|n| n.strip_suffix(".ReaperOSC")).map(str::to_owned))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Reads the pattern file `name` names (a path, or a name in the pattern folders, with or
/// without `.ReaperOSC`); Scythe's default when empty or not found.
pub fn load_patterns(name: &str) -> (PatternFile, Vec<String>) {
    let name = name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case("Default") || name.eq_ignore_ascii_case("Default.ReaperOSC") {
        return (PatternFile::parse(pattern::DEFAULT), Vec::new());
    }
    let file = if name.ends_with(".ReaperOSC") { name.to_owned() } else { format!("{name}.ReaperOSC") };
    let candidates: Vec<PathBuf> = if Path::new(name).is_absolute() {
        vec![PathBuf::from(name)]
    } else {
        pattern_dirs().into_iter().map(|d| d.join(&file)).collect()
    };
    for path in candidates {
        if let Ok(text) = std::fs::read_to_string(&path) {
            let f = PatternFile::parse(&text);
            let warnings = f.warnings.iter().map(|w| format!("{}:{w}", path.display())).collect();
            return (f, warnings);
        }
    }
    (PatternFile::parse(pattern::DEFAULT), vec![format!("OSC pattern file {file} not found; using the default")])
}

/// Sets the tempo playing at the position: the project tempo, or the tempo marker in force.
fn set_tempo(session: &mut Session, bpm: f64) {
    let at = session.position();
    let marker = session.project.tempo.points.iter().rposition(|p| p.time <= at + 1e-9);
    match marker {
        Some(index) => {
            let _ = exec(session, json!({ "cmd": "tempo.edit", "index": index, "bpm": bpm }));
        }
        None => session.edit_merging("Change tempo", "tempo", |p| p.tempo.bpm = bpm),
    }
}
