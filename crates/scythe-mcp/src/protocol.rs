//! MCP's JSON-RPC methods: lifecycle, tools, resources, prompts, subscriptions and logging.
//! Transport-agnostic: the stdio loop and the HTTP endpoint hand each message to
//! [`McpServer::handle`], and give it a [`Notifier`] for what the server sends unasked.

use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use scythe_session::api::ApiError;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backend::Backend;
use crate::summary;
use crate::tools::{self, Action, CommandArgs, ProjectArgs, ProjectFormat, Selection, ToolsArgs};

/// The revisions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const CONTROL_API_DOC: &str = include_str!("../../../docs/control-api.md");
const MCP_DOC: &str = include_str!("../../../docs/mcp.md");

const INSTRUCTIONS: &str = "Scythe is a REAPER-compatible DAW. Call get_project first: it shows the \
    tracks with the ids and names other tools take as `track` (\"master\" names the master). Times \
    are in seconds, levels in dB (`db`) or as gain (`volume`, 1.0 = 0 dB), pan from -1 to 1. Every \
    change is an undo step in Scythe, so project_undo reverts it. Paths are on the machine running \
    Scythe. Changes show up live in Scythe's window. A compact set of tools is listed; scythe_tools \
    lists more domains, and scythe_command runs any command. Measuring tools (analysis_*, \
    media_probe) render offline and wait for the result; nothing is played. events_since tells what \
    happened since you last looked.";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const RESOURCE_NOT_FOUND: i64 = -32002;

/// How long a tool waits for a measuring job.
const JOB_TIMEOUT: Duration = Duration::from_secs(900);
/// How often subscriptions and log forwarding look for news.
const WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// Delivers a notification (a JSON-RPC message without an id) to the client.
pub type Notifier = Arc<dyn Fn(&Value) + Send + Sync>;

struct RpcError {
    code: i64,
    message: String,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// The answer to a message that was not JSON at all.
pub fn parse_error(error: impl std::fmt::Display) -> Value {
    error_response(Value::Null, PARSE_ERROR, &format!("not JSON: {error}"))
}

fn notification(method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "method": method, "params": params})
}

/// MCP log levels, least severe first.
const LEVELS: [&str; 8] = ["debug", "info", "notice", "warning", "error", "critical", "alert", "emergency"];

/// What the views of one server share: the session, where notifications go, and what the
/// client asked to hear about.
struct Shared {
    backend: Arc<dyn Backend>,
    notifier: RwLock<Option<Notifier>>,
    subscriptions: Mutex<HashSet<String>>,
    /// The lowest level forwarded as `notifications/message`, when the client set one.
    log_level: Mutex<Option<usize>>,
    watching: Mutex<bool>,
}

impl Shared {
    fn notify(&self, message: &Value) {
        let notifier = self.notifier.read().unwrap().clone();
        if let Some(n) = notifier {
            n(message);
        }
    }

    fn wants_watching(&self) -> bool {
        !self.subscriptions.lock().unwrap().is_empty() || self.log_level.lock().unwrap().is_some()
    }
}

/// Answers MCP messages by running control API commands on a [`Backend`].
#[derive(Clone)]
pub struct McpServer {
    shared: Arc<Shared>,
    tools: Arc<RwLock<Selection>>,
}

impl McpServer {
    pub fn new(backend: impl Backend + 'static) -> Self {
        Self::with_tools(backend, Selection::default())
    }

    /// A server listing the tools `selection` picks.
    pub fn with_tools(backend: impl Backend + 'static, selection: Selection) -> Self {
        let shared = Shared {
            backend: Arc::new(backend),
            notifier: RwLock::new(None),
            subscriptions: Mutex::default(),
            log_level: Mutex::default(),
            watching: Mutex::new(false),
        };
        Self { shared: Arc::new(shared), tools: Arc::new(RwLock::new(selection)) }
    }

    /// Another view of the same server with its own tool selection; notifications and
    /// subscriptions stay shared.
    pub fn view(&self, selection: Selection) -> Self {
        Self { shared: self.shared.clone(), tools: Arc::new(RwLock::new(selection)) }
    }

    /// Where notifications go from now on.
    pub fn set_notifier(&self, notifier: Notifier) {
        *self.shared.notifier.write().unwrap() = Some(notifier);
    }

    pub fn selection(&self) -> Selection {
        self.tools.read().unwrap().clone()
    }

    fn backend(&self) -> &dyn Backend {
        &*self.shared.backend
    }

    /// Answers one JSON-RPC message (or a batch of them); `None` for notifications and for
    /// responses, which need no answer.
    pub fn handle(&self, message: Value) -> Option<Value> {
        match message {
            Value::Array(batch) if !batch.is_empty() => {
                let answers: Vec<Value> = batch.into_iter().filter_map(|m| self.handle_one(m)).collect();
                (!answers.is_empty()).then_some(Value::Array(answers))
            }
            message => self.handle_one(message),
        }
    }

    fn handle_one(&self, message: Value) -> Option<Value> {
        let Value::Object(mut message) = message else {
            return Some(error_response(Value::Null, INVALID_REQUEST, "a message is a JSON-RPC 2.0 object"));
        };
        let id = message.remove("id");
        let Some(method) = message.get("method").and_then(Value::as_str).map(str::to_owned) else {
            if message.contains_key("result") || message.contains_key("error") {
                return None;
            }
            return Some(error_response(id.unwrap_or(Value::Null), INVALID_REQUEST, "no method"));
        };
        let id = id?;
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Some(error_response(id, INVALID_REQUEST, "\"jsonrpc\" must be \"2.0\""));
        }
        let params = message.remove("params").unwrap_or_else(|| json!({}));
        Some(match self.request(&method, params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => error_response(id, e.code, &e.message),
        })
    }

    fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => {
                let selection = self.selection();
                Ok(json!({"tools": selection.tools().iter().map(|t| &t.definition).collect::<Vec<_>>()}))
            }
            "tools/call" => self.call_tool(&params),
            "resources/list" => Ok(json!({"resources": resources()})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "resources/read" => self.read_resource(&params),
            "resources/subscribe" => self.subscribe(&params, true),
            "resources/unsubscribe" => self.subscribe(&params, false),
            "prompts/list" => Ok(json!({"prompts": crate::prompts::list()})),
            "prompts/get" => {
                let (description, text) = crate::prompts::get(&params).map_err(|e| RpcError::new(INVALID_PARAMS, e))?;
                Ok(json!({
                    "description": description,
                    "messages": [{"role": "user", "content": {"type": "text", "text": text}}],
                }))
            }
            "logging/setLevel" => {
                let level = params["level"].as_str().unwrap_or("");
                let at = LEVELS
                    .iter()
                    .position(|l| *l == level)
                    .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("no log level \"{level}\"")))?;
                *self.shared.log_level.lock().unwrap() = Some(at);
                self.watch();
                Ok(json!({}))
            }
            _ => Err(RpcError::new(METHOD_NOT_FOUND, format!("no method \"{method}\""))),
        }
    }

    fn call_tool(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params["name"].as_str().ok_or_else(|| RpcError::new(INVALID_PARAMS, "give the tool's \"name\""))?;
        let tool = tools::find(name).ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("no tool \"{name}\"")))?;
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(Value::Object(a)) => Value::Object(a.clone()),
            Some(_) => return Ok(tool_error("the arguments must be an object")),
        };
        let answer = match &tool.action {
            Action::Project => match ProjectArgs::deserialize(&arguments) {
                Ok(args) => self.project(&args),
                Err(e) => return Ok(tool_error(&format!("invalid arguments: {e}"))),
            },
            Action::Passthrough => match CommandArgs::deserialize(&arguments) {
                Ok(args) => self.run(Value::Object(args.command)).map(|v| render(&v)),
                Err(e) => return Ok(tool_error(&format!("invalid arguments: {e}"))),
            },
            Action::Tools => match ToolsArgs::deserialize(&arguments) {
                Ok(args) => self.choose_tools(&args).map_err(ApiError::invalid),
                Err(e) => return Ok(tool_error(&format!("invalid arguments: {e}"))),
            },
            Action::Command(cmd) => {
                let mut command = arguments;
                command["cmd"] = cmd.as_str().into();
                self.run(command).map(|v| render(&v))
            }
        };
        Ok(match answer {
            Ok(text) => json!({"content": [{"type": "text", "text": text}], "isError": false}),
            Err(e) => tool_error(&describe(&e)),
        })
    }

    /// Runs a command; a measuring job it starts is waited for, so the tool answers with the
    /// result.
    fn run(&self, command: Value) -> Result<Value, ApiError> {
        let answer = self.backend().command(command)?;
        let Some(job) = answer["job"].as_u64().filter(|_| answer["status"] == "running") else {
            return finished(answer);
        };
        let deadline = Instant::now() + JOB_TIMEOUT;
        let mut pause = Duration::from_millis(20);
        loop {
            std::thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_millis(250));
            let state = self.backend().command(json!({"cmd": "analysis.job", "job": job}))?;
            if state["status"] != "running" {
                return finished(state);
            }
            if Instant::now() > deadline {
                return Err(ApiError::failed(format!(
                    "job {job} is still running after {} s; ask again with analysis_job",
                    JOB_TIMEOUT.as_secs()
                )));
            }
        }
    }

    fn choose_tools(&self, args: &ToolsArgs) -> Result<String, String> {
        let before = self.selection();
        let mut next = before.clone();
        next.enable(args.enable.iter().map(String::as_str))?;
        next.disable(args.disable.iter().map(String::as_str))?;
        if next != before {
            *self.tools.write().unwrap() = next.clone();
            self.shared.notify(&notification("notifications/tools/list_changed", json!({})));
        }
        let domains: Vec<Value> = tools::domain_names()
            .into_iter()
            .map(|d| {
                let count = tools::all().iter().filter(|t| t.domain == d).count();
                json!({"domain": d, "tools": count, "listed": next.all || next.domains.contains(d)})
            })
            .collect();
        Ok(json!({"selection": next.describe(), "listed": next.tools().len(), "domains": domains}).to_string())
    }

    fn project(&self, args: &ProjectArgs) -> Result<String, ApiError> {
        let snapshot = self.backend().command(json!({"cmd": "snapshot"}))?;
        Ok(match args.format {
            ProjectFormat::Summary => summary::project(&snapshot, args.items),
            ProjectFormat::Json => snapshot.to_string(),
        })
    }

    fn read_resource(&self, params: &Value) -> Result<Value, RpcError> {
        let uri = params["uri"].as_str().ok_or_else(|| RpcError::new(INVALID_PARAMS, "give the resource's \"uri\""))?;
        let failed = |e: ApiError| RpcError::new(INTERNAL_ERROR, describe(&e));
        let (mime, text) = match uri {
            "scythe://project" => {
                ("application/json", self.backend().command(json!({"cmd": "snapshot"})).map_err(failed)?.to_string())
            }
            "scythe://project/summary" => {
                ("text/plain", self.project(&ProjectArgs { items: true, ..Default::default() }).map_err(failed)?)
            }
            "scythe://docs/control-api" => ("text/markdown", CONTROL_API_DOC.to_owned()),
            "scythe://docs/mcp" => ("text/markdown", MCP_DOC.to_owned()),
            _ => return Err(RpcError::new(RESOURCE_NOT_FOUND, format!("no resource {uri}"))),
        };
        Ok(json!({"contents": [{"uri": uri, "mimeType": mime, "text": text}]}))
    }

    fn subscribe(&self, params: &Value, on: bool) -> Result<Value, RpcError> {
        let uri = params["uri"].as_str().ok_or_else(|| RpcError::new(INVALID_PARAMS, "give the resource's \"uri\""))?;
        if !matches!(uri, "scythe://project" | "scythe://project/summary") {
            return Err(RpcError::new(
                RESOURCE_NOT_FOUND,
                format!("{uri} does not change; subscribe to scythe://project"),
            ));
        }
        {
            let mut subs = self.shared.subscriptions.lock().unwrap();
            if on {
                subs.insert(uri.to_owned());
            } else {
                subs.remove(uri);
            }
        }
        if on {
            self.watch();
        }
        Ok(json!({}))
    }

    /// Starts the thread that turns the session's events into notifications, once.
    fn watch(&self) {
        let mut watching = self.shared.watching.lock().unwrap();
        if *watching {
            return;
        }
        let shared: Weak<Shared> = Arc::downgrade(&self.shared);
        *watching = std::thread::Builder::new().name("scythe-mcp-watch".into()).spawn(move || watch(shared)).is_ok();
    }
}

/// Polls `events.since` while the server lives and someone listens: a project change notifies
/// the subscribers of `scythe://project`, other events go out as log messages.
fn watch(shared: Weak<Shared>) {
    let mut since: Option<u64> = None;
    let mut revision: Option<u64> = None;
    loop {
        std::thread::sleep(WATCH_INTERVAL);
        let Some(shared) = shared.upgrade() else { return };
        if !shared.wants_watching() {
            continue;
        }
        let Ok(answer) = shared.backend.command(json!({"cmd": "events.since", "since": since.unwrap_or(0)})) else {
            continue;
        };
        let first = since.is_none();
        since = answer["latest"].as_u64().or(since);
        let now = answer["revision"].as_u64();
        if revision.is_some() && now != revision {
            let subs: Vec<String> = shared.subscriptions.lock().unwrap().iter().cloned().collect();
            for uri in subs {
                shared.notify(&notification("notifications/resources/updated", json!({"uri": uri})));
            }
        }
        revision = now.or(revision);
        let level = *shared.log_level.lock().unwrap();
        let Some(min) = level.filter(|_| !first) else { continue };
        for event in answer["events"].as_array().into_iter().flatten() {
            let (level, text) = log_line(event);
            if LEVELS.iter().position(|l| *l == level).is_some_and(|at| at >= min) {
                shared.notify(&notification(
                    "notifications/message",
                    json!({"level": level, "logger": "scythe", "data": {"message": text, "event": event}}),
                ));
            }
        }
    }
}

/// An event as a log line, and its level.
fn log_line(event: &Value) -> (&'static str, String) {
    match event["event"].as_str() {
        Some("notice") => ("info", event["message"].as_str().unwrap_or_default().to_owned()),
        Some("project_changed") => ("debug", format!("project changed (revision {})", event["revision"])),
        Some("health") => {
            let status = event["status"].as_str().unwrap_or("?");
            let level = if status == "ok" { "info" } else { "warning" };
            (level, format!("audio {status}: {} xruns", event["xruns"]))
        }
        Some("transport") => {
            let state = if event["recording"] == true {
                "recording"
            } else if event["playing"] == true {
                "playing"
            } else {
                "stopped"
            };
            ("info", format!("transport {state} at {:.2} s", event["position"].as_f64().unwrap_or(0.0)))
        }
        Some("setlist") => ("info", "setlist changed".to_owned()),
        _ => ("debug", event.to_string()),
    }
}

/// A finished job's report as the tool's answer, or its failure as an error.
fn finished(answer: Value) -> Result<Value, ApiError> {
    if answer["status"] == "failed" {
        return Err(ApiError::failed(answer["error"].as_str().unwrap_or("the job failed").to_owned()));
    }
    Ok(answer)
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str();
    let version = asked.filter(|v| PROTOCOL_VERSIONS.contains(v)).unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {
            "tools": {"listChanged": true},
            "resources": {"subscribe": true, "listChanged": false},
            "prompts": {"listChanged": false},
            "logging": {},
        },
        "serverInfo": {"name": "scythe", "title": "Scythe", "version": env!("CARGO_PKG_VERSION")},
        "instructions": INSTRUCTIONS,
    })
}

/// A command's answer as tool output: snapshots as the readable overview, the rest as JSON.
fn render(value: &Value) -> String {
    if summary::is_snapshot(value) { summary::project(value, false) } else { value.to_string() }
}

fn describe(error: &ApiError) -> String {
    let kind = serde_json::to_value(error.kind).ok().and_then(|k| k.as_str().map(str::to_owned)).unwrap_or_default();
    format!("{kind}: {}", error.message)
}

fn tool_error(message: &str) -> Value {
    json!({"content": [{"type": "text", "text": message}], "isError": true})
}

fn resources() -> Value {
    json!([
        {"uri": "scythe://project/summary", "name": "project-summary", "title": "Project overview",
         "description": "The open project in a few lines per track, items included. Subscribe to hear when it changes.",
         "mimeType": "text/plain"},
        {"uri": "scythe://project", "name": "project", "title": "Project snapshot",
         "description": "The open project as JSON: transport, tracks with FX, sends and items, markers. Subscribe to hear when it changes.",
         "mimeType": "application/json"},
        {"uri": "scythe://docs/control-api", "name": "control-api", "title": "Control API reference",
         "description": "Every command scythe_command accepts, with its arguments and answers.",
         "mimeType": "text/markdown"},
        {"uri": "scythe://docs/mcp", "name": "mcp", "title": "Scythe MCP server",
         "description": "How this server is set up and what its tools do.", "mimeType": "text/markdown"},
    ])
}
