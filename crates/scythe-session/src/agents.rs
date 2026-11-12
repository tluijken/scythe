//! The AI agents driving the session over MCP (see `docs/mcp.md`): which are connected, what
//! each says it is doing, the tool calls they made with what came of them, and the user's
//! pause. Both MCP transports report here through the `agents.*` commands, so the window, the
//! web UI and the control API all see the same picture.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use scythe_core::{Project, TrackId};

/// Tool calls kept, newest last.
pub const CALLS_KEPT: usize = 200;
/// How long a finished call keeps the indicator lit.
pub const ACTIVE_FOR: Duration = Duration::from_secs(2);
/// How long a failed call shows.
pub const ERROR_FOR: Duration = Duration::from_secs(3);
/// How long a client that stops reporting stays listed, by transport, unless its hello says
/// otherwise: a stdio bridge says hello every few seconds, an HTTP client may go quiet for long.
const STDIO_TTL: Duration = Duration::from_secs(20);
const HTTP_TTL: Duration = Duration::from_secs(30 * 60);
/// Status text and summaries are cut to this many characters.
const TEXT_MAX: usize = 200;
const SUMMARY_MAX: usize = 120;

pub fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// Who an MCP client says it is, as the bridge reports it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hello {
    pub client: String,
    pub name: String,
    pub version: Option<String>,
    pub transport: String,
    /// How long it stays listed without news; the transport's default when `None`.
    pub ttl: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentClient {
    /// The MCP session (HTTP) or bridge process (stdio) it is reached through.
    pub id: String,
    /// The client's name from `initialize` (`clientInfo.title` or `name`).
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `http` (Scythe's `/mcp`) or `stdio` (`scythe-mcp`).
    pub transport: String,
    /// The access role it acts with; MCP needs the admin token.
    pub role: String,
    /// Wall-clock milliseconds since the Unix epoch.
    pub connected_ms: u64,
    /// When its last tool call started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_ms: Option<u64>,
    /// What it said it is doing (`set_status`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_ms: Option<u64>,
    /// Tool calls running now.
    pub running: usize,
    #[serde(skip)]
    seen: Option<Instant>,
    #[serde(skip)]
    ttl: Duration,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCall {
    pub id: u64,
    /// The client's id and name.
    pub client: String,
    pub agent: String,
    pub tool: String,
    /// What the call does, in words ("Set Vocals volume to -3 dB").
    pub summary: String,
    pub started_ms: u64,
    /// `None` while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// `None` while it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The undo steps it made, by their labels.
    #[serde(default)]
    pub undo: Vec<String>,
    /// Whether it changed the session (the project, the transport or settings).
    pub changed: bool,
    #[serde(skip)]
    started: Option<Instant>,
}

/// The `agents.activity` answer and the `agent_activity` event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    pub clients: Vec<AgentClient>,
    /// The newest calls, oldest first.
    pub calls: Vec<AgentCall>,
    /// Mutating tool calls are refused until the user resumes.
    pub paused: bool,
    /// A call runs or ended in the last two seconds.
    pub busy: bool,
    /// The newest status text any connected agent gave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The step undo would revert, when an agent made it: what "Undo last agent change" undoes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<AgentUndo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentUndo {
    pub label: String,
    pub agent: String,
}

/// How the indicator shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Light {
    /// No agent connected.
    Off,
    Idle,
    Busy,
    Error,
}

#[derive(Debug, Default)]
pub struct Agents {
    clients: Vec<AgentClient>,
    calls: VecDeque<AgentCall>,
    next_call: u64,
    paused: bool,
    /// Clients the user disconnected: refused until they connect anew.
    refused: HashSet<String>,
    last_done: Option<Instant>,
    last_error: Option<Instant>,
    /// Counts changes, so the pump knows when to publish.
    revision: u64,
}

impl Agents {
    pub fn revision(&self) -> u64 {
        self.revision
    }

    fn changed(&mut self) {
        self.revision += 1;
    }

    pub fn clients(&self) -> &[AgentClient] {
        &self.clients
    }

    pub fn calls(&self) -> impl DoubleEndedIterator<Item = &AgentCall> + ExactSizeIterator {
        self.calls.iter()
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn set_paused(&mut self, paused: bool) {
        if self.paused != paused {
            self.paused = paused;
            self.changed();
        }
    }

    /// The newest status text any connected agent gave, with the agent's name.
    pub fn status(&self) -> Option<(&str, &str)> {
        self.clients
            .iter()
            .filter(|c| c.status.is_some())
            .max_by_key(|c| c.status_ms)
            .and_then(|c| Some((c.name.as_str(), c.status.as_deref()?)))
    }

    pub fn running(&self) -> usize {
        self.clients.iter().map(|c| c.running).sum()
    }

    pub fn busy(&self, now: Instant) -> bool {
        self.running() > 0 || self.last_done.is_some_and(|t| now.duration_since(t) < ACTIVE_FOR)
    }

    pub fn light(&self, now: Instant) -> Light {
        if self.clients.is_empty() {
            Light::Off
        } else if self.last_error.is_some_and(|t| now.duration_since(t) < ERROR_FOR) {
            Light::Error
        } else if self.busy(now) {
            Light::Busy
        } else {
            Light::Idle
        }
    }

    /// When the light changes by itself next (a busy or error spell ending), for a window that
    /// sleeps between frames.
    pub fn light_changes_in(&self, now: Instant) -> Option<Duration> {
        let left = |at: Option<Instant>, d: Duration| at.and_then(|t| d.checked_sub(now.duration_since(t)));
        [left(self.last_done, ACTIVE_FOR), left(self.last_error, ERROR_FOR)].into_iter().flatten().min()
    }

    /// A client said hello: listed (again) and kept alive. Refused when the user disconnected it.
    pub fn hello(&mut self, hello: &Hello) -> Result<(), String> {
        self.touch(hello, true).map(|_| ())
    }

    fn touch(&mut self, hello: &Hello, renew: bool) -> Result<usize, String> {
        if self.refused.contains(&hello.client) {
            return Err(format!(
                "the user disconnected {} in Scythe; restart the MCP connection to connect again",
                hello.name
            ));
        }
        let default_ttl = if hello.transport == "stdio" { STDIO_TTL } else { HTTP_TTL };
        let now = Instant::now();
        if let Some(at) = self.clients.iter().position(|c| c.id == hello.client) {
            let c = &mut self.clients[at];
            c.seen = Some(now);
            if renew {
                c.ttl = hello.ttl.unwrap_or(default_ttl);
                if c.name != hello.name || c.version != hello.version {
                    c.name.clone_from(&hello.name);
                    c.version.clone_from(&hello.version);
                    self.changed();
                }
            }
            return Ok(at);
        }
        self.clients.push(AgentClient {
            id: hello.client.clone(),
            name: hello.name.clone(),
            version: hello.version.clone(),
            transport: hello.transport.clone(),
            role: "admin".into(),
            connected_ms: now_ms(),
            active_ms: None,
            status: None,
            status_ms: None,
            running: 0,
            seen: Some(now),
            ttl: hello.ttl.unwrap_or(default_ttl),
        });
        self.changed();
        Ok(self.clients.len() - 1)
    }

    /// A client ended its session.
    pub fn bye(&mut self, client: &str) {
        let before = self.clients.len();
        self.clients.retain(|c| c.id != client);
        if self.clients.len() != before {
            self.changed();
        }
    }

    /// The user cut a client off (all of them when `client` is `None`): it is no longer listed
    /// and its calls are refused.
    pub fn disconnect(&mut self, client: Option<&str>) -> usize {
        let gone: Vec<String> =
            self.clients.iter().filter(|c| client.is_none_or(|id| c.id == id)).map(|c| c.id.clone()).collect();
        for id in &gone {
            self.bye(id);
            self.refused.insert(id.clone());
        }
        gone.len()
    }

    pub fn set_status(&mut self, hello: &Hello, text: &str) -> Result<(), String> {
        let at = self.touch(hello, false)?;
        let text = clip(text.trim(), TEXT_MAX);
        let c = &mut self.clients[at];
        c.status = (!text.is_empty()).then_some(text);
        c.status_ms = Some(now_ms());
        self.changed();
        Ok(())
    }

    /// A tool call starts; answers its number.
    pub fn begin(&mut self, hello: &Hello, tool: &str, summary: String) -> Result<u64, String> {
        let at = self.touch(hello, false)?;
        let started = now_ms();
        let c = &mut self.clients[at];
        c.running += 1;
        c.active_ms = Some(started);
        let agent = c.name.clone();
        self.next_call += 1;
        self.calls.push_back(AgentCall {
            id: self.next_call,
            client: hello.client.clone(),
            agent,
            tool: tool.to_owned(),
            summary: clip(&summary, SUMMARY_MAX),
            started_ms: started,
            duration_ms: None,
            ok: None,
            error: None,
            undo: Vec::new(),
            changed: false,
            started: Some(Instant::now()),
        });
        while self.calls.len() > CALLS_KEPT {
            self.calls.pop_front();
        }
        self.changed();
        Ok(self.next_call)
    }

    pub fn call(&self, id: u64) -> Option<&AgentCall> {
        self.calls.iter().rev().find(|c| c.id == id)
    }

    fn call_mut(&mut self, id: u64) -> Option<&mut AgentCall> {
        self.calls.iter_mut().rev().find(|c| c.id == id)
    }

    /// What a command run for call `id` did.
    pub fn record(&mut self, id: u64, undo: Vec<String>, changed: bool) {
        if !changed && undo.is_empty() {
            return;
        }
        if let Some(call) = self.call_mut(id) {
            call.undo.extend(undo);
            call.changed |= changed;
            self.changed();
        }
    }

    /// Call `id` ended, failing with `error` when given.
    pub fn end(&mut self, id: u64, error: Option<String>) {
        let now = Instant::now();
        let Some(call) = self.call_mut(id) else { return };
        if call.ok.is_some() {
            return;
        }
        call.duration_ms = call.started.map(|t| now.duration_since(t).as_millis() as u64);
        call.ok = Some(error.is_none());
        call.error = error.map(|e| clip(&e, TEXT_MAX));
        let failed = call.ok == Some(false);
        let client = call.client.clone();
        if let Some(c) = self.clients.iter_mut().find(|c| c.id == client) {
            c.running = c.running.saturating_sub(1);
            c.seen = Some(now);
        }
        self.last_done = Some(now);
        if failed {
            self.last_error = Some(now);
        }
        self.changed();
    }

    /// Forgets clients that stopped reporting; calls of theirs still open end as cut off.
    pub fn expire(&mut self, now: Instant) {
        let gone: Vec<String> = self
            .clients
            .iter()
            .filter(|c| c.seen.is_some_and(|t| now.duration_since(t) > c.ttl))
            .map(|c| c.id.clone())
            .collect();
        for id in gone {
            let open: Vec<u64> = self.calls.iter().filter(|c| c.client == id && c.ok.is_none()).map(|c| c.id).collect();
            for call in open {
                self.end(call, Some("the agent went away".into()));
            }
            self.bye(&id);
        }
    }

    /// The state for clients, with the newest `calls` calls.
    pub fn activity(&self, calls: usize, undo: Option<AgentUndo>) -> Activity {
        Activity {
            clients: self.clients.clone(),
            calls: self.calls.iter().skip(self.calls.len().saturating_sub(calls)).cloned().collect(),
            paused: self.paused,
            busy: self.busy(Instant::now()),
            status: self.status().map(|(_, s)| s.to_owned()),
            undo,
        }
    }
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(max - 1).collect();
    cut.push('…');
    cut
}

/// A track named in a command's arguments, by its name.
pub fn track_name(project: &Project, value: &Value) -> Option<String> {
    match value {
        Value::Number(n) => {
            let id = n.as_u64()?;
            Some(project.track(TrackId(id)).map_or_else(|| format!("track {id}"), |t| t.name.clone()))
        }
        Value::String(s) if s.eq_ignore_ascii_case("master") => Some("Master".into()),
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// A plugin as people say it: "VST3: Curve 3 (AcmeAudio)" is "Curve 3".
pub fn plugin_name(name: &str) -> String {
    let name = name.split_once(": ").map_or(name, |(_, n)| n);
    let name = match name.rfind(" (") {
        Some(at) if name.ends_with(')') && at > 0 => &name[..at],
        _ => name,
    };
    name.to_owned()
}

fn number(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn trim(v: f64) -> String {
    let text = format!("{v:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn level(args: &Map<String, Value>) -> Option<String> {
    if let Some(db) = args.get("db").and_then(number) {
        return Some(format!("{} dB", trim(db)));
    }
    let gain = args.get("volume").and_then(number)?;
    Some(if gain <= 0.0 { "-inf dB".into() } else { format!("{} dB", trim(20.0 * gain.log10())) })
}

fn seconds(v: f64) -> String {
    scythe_core::tempo::format_time(v)
}

fn on_off(args: &Map<String, Value>, key: &str, on: &str, off: &str, toggle: &str) -> String {
    match args.get(key).and_then(Value::as_bool) {
        Some(true) => on.to_owned(),
        Some(false) => off.to_owned(),
        None => toggle.to_owned(),
    }
}

/// What a tool call does, in a few words: the common commands as a sentence, the rest as the
/// command and its arguments.
pub fn summarize(project: &Project, tool: &str, cmd: Option<&str>, arguments: &Value) -> String {
    let empty = Map::new();
    let (cmd, args) = match tool {
        "get_project" => return "Read the project".into(),
        "scythe_tools" => return "Choose the tools listed".into(),
        "scythe_command" => {
            let command = arguments.get("command").and_then(Value::as_object).unwrap_or(&empty);
            (command.get("cmd").and_then(Value::as_str).unwrap_or("?").to_owned(), command)
        }
        tool => {
            (cmd.map_or_else(|| tool.replacen('_', ".", 1), str::to_owned), arguments.as_object().unwrap_or(&empty))
        }
    };
    let track = |key: &str| args.get(key).and_then(|v| track_name(project, v));
    let t = track("track").unwrap_or_else(|| "the track".into());
    let fx = || {
        let index = args.get("index").and_then(Value::as_u64)?;
        let id = match args.get("track")? {
            Value::Number(n) => TrackId(n.as_u64()?),
            Value::String(name) => project.tracks.iter().find(|tr| &tr.name == name)?.id,
            _ => return None,
        };
        project.track(id)?.fx.get(index as usize).map(|slot| plugin_name(&slot.name))
    };
    let text = match cmd.as_str() {
        "track.volume" => level(args).map(|l| format!("Set {t} volume to {l}")),
        "track.pan" => args.get("pan").and_then(number).map(|p| {
            let pct = (p * 100.0).round();
            match pct {
                0.0 => format!("Pan {t} center"),
                p if p < 0.0 => format!("Pan {t} {}% left", -p),
                p => format!("Pan {t} {p}% right"),
            }
        }),
        "track.mute" => Some(on_off(args, "muted", "Mute ", "Unmute ", "Toggle mute on ") + &t),
        "track.solo" => Some(on_off(args, "solo", "Solo ", "Unsolo ", "Toggle solo on ") + &t),
        "track.add" => {
            Some(args.get("name").and_then(Value::as_str).map_or("Add a track".into(), |n| format!("Add track {n}")))
        }
        "track.delete" => Some(format!("Delete {t}")),
        "track.rename" => args.get("name").and_then(Value::as_str).map(|n| format!("Rename {t} to {n}")),
        "fx.add" => args.get("plugin").and_then(Value::as_str).map(|p| format!("Add {} to {t}", plugin_name(p))),
        "fx.remove" => Some(format!("Remove {} from {t}", fx().unwrap_or_else(|| "an effect".into()))),
        "fx.bypass" => Some(format!("Bypass {} on {t}", fx().unwrap_or_else(|| "an effect".into()))),
        "fx.param" => args.get("value").and_then(number).map(|v| {
            let param = args.get("param").and_then(Value::as_u64).unwrap_or_default();
            format!("Set {} parameter {param} on {t} to {}", fx().unwrap_or_else(|| "an effect".into()), trim(v))
        }),
        "fx.params" => Some(format!("Read the parameters of {} on {t}", fx().unwrap_or_else(|| "an effect".into()))),
        "send.add" => track("dest").map(|d| format!("Send {t} to {d}")),
        "send.volume" => level(args).map(|l| format!("Set a send of {t} to {l}")),
        "mix.create" => args.get("musician").and_then(Value::as_str).map(|m| format!("Create the monitor mix for {m}")),
        "mix.level" => match (track("source"), track("mix"), level(args)) {
            (Some(s), Some(m), Some(l)) => Some(format!("Set {s} in {m} to {l}")),
            _ => None,
        },
        "marker.add" => {
            let name = args.get("name").and_then(Value::as_str).filter(|n| !n.is_empty());
            let at = args.get("position").and_then(number).map(|p| format!(" at {}", seconds(p))).unwrap_or_default();
            let kind = if args.get("end").is_some_and(|e| !e.is_null()) { "region" } else { "marker" };
            Some(name.map_or(format!("Add a {kind}{at}"), |n| format!("Add {kind} \"{n}\"{at}")))
        }
        "transport.play" => Some("Play".into()),
        "transport.stop" => Some("Stop".into()),
        "transport.pause" => Some("Pause".into()),
        "transport.record" => Some("Record".into()),
        "transport.seek" => args.get("position").and_then(number).map(|p| format!("Go to {}", seconds(p))),
        "transport.state" => Some("Read the transport".into()),
        "project.save" => Some("Save the project".into()),
        "project.undo" => Some("Undo".into()),
        "project.redo" => Some("Redo".into()),
        "project.open" => args
            .get("path")
            .and_then(Value::as_str)
            .map(|p| format!("Open {}", std::path::Path::new(p).file_name().map_or(p.into(), |f| f.to_string_lossy()))),
        "project.new" => Some("Start a new project".into()),
        "media.import" => Some(format!("Import media to {t}")),
        "events.since" => Some("Check what happened".into()),
        "events.meters" => Some("Read the meters".into()),
        "analysis.mix" => Some("Measure the mix".into()),
        "analysis.compare" => Some("Compare the mix with a reference".into()),
        "analysis.match_eq" => Some(format!("Match EQ on {t}")),
        "analysis.masking" => Some("Find which groups mask each other".into()),
        _ => None,
    };
    text.unwrap_or_else(|| compact(project, &cmd, args))
}

/// `cmd key=value …`, tracks by name.
fn compact(project: &Project, cmd: &str, args: &Map<String, Value>) -> String {
    let mut parts = vec![cmd.to_owned()];
    for (key, value) in args {
        if key == "cmd" {
            continue;
        }
        let shown = match (key.as_str(), value) {
            ("track" | "dest" | "source" | "mix", v) => track_name(project, v).unwrap_or_else(|| v.to_string()),
            (_, Value::String(s)) => s.clone(),
            (_, v) => v.to_string(),
        };
        parts.push(format!("{key}={}", clip(&shown, 40)));
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn hello(id: &str, transport: &str) -> Hello {
        Hello {
            client: id.into(),
            name: "Claude Code".into(),
            version: Some("2.0".into()),
            transport: transport.into(),
            ttl: None,
        }
    }

    #[test]
    fn calls_are_tracked_and_bounded() {
        let mut a = Agents::default();
        let now = Instant::now();
        assert_eq!(a.light(now), Light::Off);
        a.hello(&hello("one", "http")).unwrap();
        assert_eq!(a.light(Instant::now()), Light::Idle);
        let id = a.begin(&hello("one", "http"), "track_volume", "Set Vocals volume to -3 dB".into()).unwrap();
        assert_eq!(a.running(), 1);
        assert_eq!(a.light(Instant::now()), Light::Busy);
        a.record(id, vec!["Track volume".into()], true);
        a.end(id, None);
        let call = a.call(id).unwrap();
        assert_eq!((call.ok, call.undo.clone(), call.changed), (Some(true), vec!["Track volume".to_owned()], true));
        assert!(call.duration_ms.is_some());
        assert_eq!(a.running(), 0);
        assert!(a.busy(Instant::now()));
        assert!(!a.busy(Instant::now() + ACTIVE_FOR + Duration::from_millis(10)));
        let failing = a.begin(&hello("one", "http"), "fx_add", "Add ReaEQ".into()).unwrap();
        a.end(failing, Some("no plugin".into()));
        assert_eq!(a.light(Instant::now()), Light::Error);
        for _ in 0..CALLS_KEPT + 10 {
            let id = a.begin(&hello("one", "http"), "transport_state", "Read".into()).unwrap();
            a.end(id, None);
        }
        assert_eq!(a.calls().len(), CALLS_KEPT);
        assert_eq!(a.activity(20, None).calls.len(), 20);
    }

    #[test]
    fn status_and_expiry() {
        let mut a = Agents::default();
        a.hello(&hello("s", "stdio")).unwrap();
        a.set_status(&hello("s", "stdio"), "  Balancing drums against the reference ").unwrap();
        assert_eq!(a.status(), Some(("Claude Code", "Balancing drums against the reference")));
        let open = a.begin(&hello("s", "stdio"), "analysis_mix", "Measure".into()).unwrap();
        a.expire(Instant::now() + STDIO_TTL + Duration::from_secs(1));
        assert!(a.clients().is_empty());
        assert_eq!(a.call(open).unwrap().ok, Some(false));
        a.hello(&hello("h", "http")).unwrap();
        a.expire(Instant::now() + STDIO_TTL + Duration::from_secs(1));
        assert_eq!(a.clients().len(), 1, "HTTP clients stay longer");
    }

    #[test]
    fn disconnected_clients_are_refused() {
        let mut a = Agents::default();
        a.hello(&hello("one", "stdio")).unwrap();
        a.hello(&hello("two", "http")).unwrap();
        assert_eq!(a.disconnect(Some("one")), 1);
        assert!(a.begin(&hello("one", "stdio"), "track_add", String::new()).unwrap_err().contains("disconnected"));
        assert!(a.hello(&hello("one", "stdio")).is_err());
        assert_eq!(a.clients().len(), 1);
        assert_eq!(a.disconnect(None), 1);
        assert!(a.clients().is_empty());
        assert!(a.hello(&hello("three", "http")).is_ok(), "a new session connects");
    }

    #[test]
    fn summaries_read_like_sentences() {
        let mut p = Project::default();
        let vocals = p.add_track("Vocals");
        p.add_track("Drums");
        let id = vocals.0;
        let s = |tool: &str, args: Value| summarize(&p, tool, None, &args);
        assert_eq!(s("track_volume", json!({"track": id, "db": -3})), "Set Vocals volume to -3 dB");
        assert_eq!(s("track_volume", json!({"track": "Drums", "volume": 0.5})), "Set Drums volume to -6.02 dB");
        assert_eq!(s("track_pan", json!({"track": "Drums", "pan": -0.25})), "Pan Drums 25% left");
        assert_eq!(s("track_mute", json!({"track": "master", "muted": true})), "Mute Master");
        assert_eq!(s("fx_add", json!({"track": id, "plugin": "VST3: Curve 3 (AcmeAudio)"})), "Add Curve 3 to Vocals");
        assert_eq!(s("send_add", json!({"track": "Drums", "dest": id})), "Send Drums to Vocals");
        assert_eq!(s("marker_add", json!({"position": 12.5, "name": "Chorus"})), "Add marker \"Chorus\" at 0:12.500");
        assert_eq!(
            s("scythe_command", json!({"command": {"cmd": "track.volume", "track": id, "db": -1.5}})),
            "Set Vocals volume to -1.5 dB"
        );
        assert_eq!(s("item_split", json!({"item": 4, "at": 2.0})), "item.split at=2.0 item=4");
        assert_eq!(s("get_project", json!({})), "Read the project");
    }
}
