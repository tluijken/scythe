//! MCP's JSON-RPC methods: lifecycle, tools, resources, prompts, subscriptions and logging.
//! Transport-agnostic: the stdio loop and the HTTP endpoint hand each message to
//! [`McpServer::handle`] (or [`McpServer::handle_from`] naming the client), and give it a
//! [`Notifier`] for what the server sends unasked. Every client and tool call is reported to
//! the session (`agents.*`), which shows them to the person at Scythe and may pause them.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use scythe_session::api::agents::{PAUSED, reads_only};
use scythe_session::api::{ApiError, ErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backend::Backend;
use crate::summary;
use crate::tools::{self, Action, CommandArgs, ProjectArgs, ProjectFormat, Selection, StatusArgs, ToolsArgs};

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
    happened since you last looked. Someone may be watching Scythe while you work: before each step, \
    say in a few words what you are doing with set_status (\"Balancing drums against the \
    reference\"). The user can pause agents; changes are then refused while reading still works.";

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

/// The MCP transport a server serves, which the session shows for its clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// `scythe-mcp`: one client, the process that started it.
    Stdio,
    /// Scythe's `/mcp`: a client per MCP session.
    Http,
}

impl Transport {
    fn name(self) -> &'static str {
        match self {
            Self::Stdio => "stdio",
            Self::Http => "http",
        }
    }
}

/// Who a client said it is in `initialize`.
#[derive(Debug, Clone)]
struct ClientInfo {
    name: String,
    version: Option<String>,
}

impl Default for ClientInfo {
    fn default() -> Self {
        Self { name: "MCP client".into(), version: None }
    }
}

/// A client id no other process or session has.
fn fresh_id(prefix: &str) -> String {
    static COUNT: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
    format!("{prefix}-{}-{nanos:08x}-{}", std::process::id(), COUNT.fetch_add(1, Ordering::Relaxed))
}

/// What the views of one server share: the session, where notifications go, what the client
/// asked to hear about, and the clients.
struct Shared {
    backend: Arc<dyn Backend>,
    notifier: RwLock<Option<Notifier>>,
    subscriptions: Mutex<HashSet<String>>,
    /// The lowest level forwarded as `notifications/message`, when the client set one.
    log_level: Mutex<Option<usize>>,
    watching: Mutex<bool>,
    transport: Transport,
    clients: Mutex<HashMap<String, ClientInfo>>,
    /// The client of messages that name none: the stdio peer, or the HTTP client that
    /// initialized last (for clients that do not send their session id back).
    default_client: Mutex<String>,
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

    fn client(&self, id: &str) -> ClientInfo {
        self.clients.lock().unwrap().get(id).cloned().unwrap_or_default()
    }

    /// The fields the `agents.*` commands identify a client by.
    fn tag(&self, id: &str) -> Value {
        let info = self.client(id);
        json!({"client": id, "name": info.name, "version": info.version, "transport": self.transport.name()})
    }

    fn hello(&self, id: &str, ttl: Option<Duration>) -> Result<Value, ApiError> {
        let mut command = self.tag(id);
        command["cmd"] = "agents.hello".into();
        if let Some(ttl) = ttl {
            command["ttl"] = ttl.as_secs().into();
        }
        self.backend.command(command)
    }
}

/// How a tool call reaches the session: through `agents.run` for call `call` when the session
/// tracks it, plainly otherwise.
#[derive(Debug, Clone, Copy, Default)]
struct Call {
    id: Option<u64>,
    paused: bool,
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

    /// A stdio server listing the tools `selection` picks.
    pub fn with_tools(backend: impl Backend + 'static, selection: Selection) -> Self {
        Self::serving(backend, selection, Transport::Stdio)
    }

    /// A server for `transport`.
    pub fn serving(backend: impl Backend + 'static, selection: Selection, transport: Transport) -> Self {
        let default_client = match transport {
            Transport::Stdio => fresh_id("stdio"),
            Transport::Http => "http".to_owned(),
        };
        let shared = Shared {
            backend: Arc::new(backend),
            notifier: RwLock::new(None),
            subscriptions: Mutex::default(),
            log_level: Mutex::default(),
            watching: Mutex::new(false),
            transport,
            clients: Mutex::default(),
            default_client: Mutex::new(default_client),
        };
        Self { shared: Arc::new(shared), tools: Arc::new(RwLock::new(selection)) }
    }

    /// Another view of the same server with its own tool selection; notifications,
    /// subscriptions and clients stay shared.
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

    /// An id for a new MCP session (`Mcp-Session-Id`).
    pub fn new_client(&self) -> String {
        fresh_id(self.shared.transport.name())
    }

    /// Whether a client with `id` initialized and has not ended its session.
    pub fn knows(&self, id: &str) -> bool {
        self.shared.clients.lock().unwrap().contains_key(id)
    }

    /// The client ended its session (HTTP `DELETE`, or stdin closed).
    pub fn end_client(&self, id: &str) {
        if self.shared.clients.lock().unwrap().remove(id).is_some() {
            let _ = self.backend().command(json!({"cmd": "agents.bye", "client": id}));
        }
    }

    /// Ends the session of the client of messages that name none.
    pub fn end_default_client(&self) {
        let id = self.shared.default_client.lock().unwrap().clone();
        self.end_client(&id);
    }

    /// Tells the session the client is still there, to stay listed for `ttl`.
    pub fn keep(&self, id: &str, ttl: Duration) {
        if self.knows(id) {
            let _ = self.shared.hello(id, Some(ttl));
        }
    }

    /// Says hello for every client every `every` while the server lives, listed for `ttl`
    /// each time: a bridge that goes away silently is forgotten soon after, and one started
    /// before Scythe shows up once Scythe runs.
    pub fn keep_alive(&self, every: Duration, ttl: Duration) {
        let shared: Weak<Shared> = Arc::downgrade(&self.shared);
        let _ = std::thread::Builder::new().name("scythe-mcp-alive".into()).spawn(move || {
            loop {
                std::thread::sleep(every);
                let Some(shared) = shared.upgrade() else { return };
                let ids: Vec<String> = shared.clients.lock().unwrap().keys().cloned().collect();
                for id in ids {
                    let _ = shared.hello(&id, Some(ttl));
                }
            }
        });
    }

    /// Answers one JSON-RPC message (or a batch of them) from the client messages name by
    /// default; `None` for notifications and for responses, which need no answer.
    pub fn handle(&self, message: Value) -> Option<Value> {
        let client = self.shared.default_client.lock().unwrap().clone();
        self.handle_from(&client, message)
    }

    /// [`Self::handle`] for the client `client` (an MCP session id).
    pub fn handle_from(&self, client: &str, message: Value) -> Option<Value> {
        match message {
            Value::Array(batch) if !batch.is_empty() => {
                let answers: Vec<Value> = batch.into_iter().filter_map(|m| self.handle_one(client, m)).collect();
                (!answers.is_empty()).then_some(Value::Array(answers))
            }
            message => self.handle_one(client, message),
        }
    }

    fn handle_one(&self, client: &str, message: Value) -> Option<Value> {
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
        Some(match self.request(client, &method, params) {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err(e) => error_response(id, e.code, &e.message),
        })
    }

    fn request(&self, client: &str, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => {
                self.connect(client, &params);
                Ok(initialize(&params))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                let selection = self.selection();
                Ok(json!({"tools": selection.tools().iter().map(|t| &t.definition).collect::<Vec<_>>()}))
            }
            "tools/call" => self.call_tool(client, &params),
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

    /// Remembers who the client is and tells the session; a session that is not up yet hears
    /// of it from [`Self::keep_alive`] or the first tool call.
    fn connect(&self, client: &str, params: &Value) {
        let info = &params["clientInfo"];
        let text = |key: &str| info[key].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        let name = text("title").or_else(|| text("name")).unwrap_or_else(|| ClientInfo::default().name);
        self.shared.clients.lock().unwrap().insert(client.to_owned(), ClientInfo { name, version: text("version") });
        *self.shared.default_client.lock().unwrap() = client.to_owned();
        let _ = self.shared.hello(client, None);
    }

    fn call_tool(&self, client: &str, params: &Value) -> Result<Value, RpcError> {
        let name = params["name"].as_str().ok_or_else(|| RpcError::new(INVALID_PARAMS, "give the tool's \"name\""))?;
        let tool = tools::find(name).ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("no tool \"{name}\"")))?;
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(Value::Object(a)) => Value::Object(a.clone()),
            Some(_) => return Ok(tool_error("the arguments must be an object")),
        };
        if tool.action == Action::Status {
            return Ok(match StatusArgs::deserialize(&arguments) {
                Ok(args) => self.set_status(client, &args.text),
                Err(e) => tool_error(&format!("invalid arguments: {e}")),
            });
        }
        let call = match self.begin(client, name, &tool.action, &arguments) {
            Ok(call) => call,
            Err(e) => return Ok(tool_error(&describe(&e))),
        };
        let answer = match &tool.action {
            Action::Project => match ProjectArgs::deserialize(&arguments) {
                Ok(args) => self.project(call, &args).map_err(|e| describe(&e)),
                Err(e) => Err(format!("invalid arguments: {e}")),
            },
            Action::Passthrough => match CommandArgs::deserialize(&arguments) {
                Ok(args) => self.run(call, Value::Object(args.command)).map(|v| render(&v)).map_err(|e| describe(&e)),
                Err(e) => Err(format!("invalid arguments: {e}")),
            },
            Action::Tools => match ToolsArgs::deserialize(&arguments) {
                Ok(args) => self.choose_tools(&args).map_err(|e| describe(&ApiError::invalid(e))),
                Err(e) => Err(format!("invalid arguments: {e}")),
            },
            Action::Command(cmd) => {
                let mut command = arguments;
                command["cmd"] = cmd.as_str().into();
                self.run(call, command).map(|v| render(&v)).map_err(|e| describe(&e))
            }
            Action::Status => unreachable!("answered above"),
        };
        if let Some(id) = call.id {
            let error = answer.as_ref().err();
            let _ = self.backend().command(json!({"cmd": "agents.done", "call": id, "error": error}));
        }
        Ok(match answer {
            Ok(text) => json!({"content": [{"type": "text", "text": text}], "isError": false}),
            Err(e) => tool_error(&e),
        })
    }

    /// Tells the session a tool call starts. A Scythe that does not know `agents.call` (an
    /// older one) runs the call untracked; one that refuses it (the user disconnected this
    /// agent) or cannot be reached fails it.
    fn begin(&self, client: &str, tool: &str, action: &Action, arguments: &Value) -> Result<Call, ApiError> {
        let mut command = self.shared.tag(client);
        command["cmd"] = "agents.call".into();
        command["tool"] = tool.into();
        command["arguments"] = arguments.clone();
        if let Action::Command(cmd) = action {
            command["command"] = cmd.as_str().into();
        }
        match self.backend().command(command) {
            Ok(answer) => Ok(Call { id: answer["call"].as_u64(), paused: answer["paused"] == true }),
            Err(e) if e.kind == ErrorKind::Invalid => Ok(Call::default()),
            Err(e) => Err(e),
        }
    }

    fn set_status(&self, client: &str, text: &str) -> Value {
        let mut command = self.shared.tag(client);
        command["cmd"] = "agents.status".into();
        command["text"] = text.into();
        match self.backend().command(command) {
            Ok(_) if text.trim().is_empty() => {
                json!({"content": [{"type": "text", "text": "status cleared"}], "isError": false})
            }
            Ok(_) => json!({"content": [{"type": "text", "text": "shown in Scythe"}], "isError": false}),
            Err(e) => tool_error(&describe(&e)),
        }
    }

    /// Runs one command of a tool call. The server keeps `access.*` itself, so those are
    /// checked against the pause here instead of in the session.
    fn exec(&self, call: Call, command: Value) -> Result<Value, ApiError> {
        let cmd = command["cmd"].as_str().unwrap_or_default();
        match call.id {
            Some(_) if cmd.starts_with("access.") => {
                if call.paused && !reads_only(cmd) {
                    return Err(ApiError::failed(PAUSED));
                }
                self.backend().command(command)
            }
            Some(id) => self.backend().command(json!({"cmd": "agents.run", "call": id, "command": command})),
            None => self.backend().command(command),
        }
    }

    /// Runs a command; a measuring job it starts is waited for, so the tool answers with the
    /// result.
    fn run(&self, call: Call, command: Value) -> Result<Value, ApiError> {
        let answer = self.exec(call, command)?;
        let Some(job) = answer["job"].as_u64().filter(|_| answer["status"] == "running") else {
            return finished(answer);
        };
        let deadline = Instant::now() + JOB_TIMEOUT;
        let mut pause = Duration::from_millis(20);
        loop {
            std::thread::sleep(pause);
            pause = (pause * 2).min(Duration::from_millis(250));
            let state = self.exec(call, json!({"cmd": "analysis.job", "job": job}))?;
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

    fn project(&self, call: Call, args: &ProjectArgs) -> Result<String, ApiError> {
        let snapshot = self.exec(call, json!({"cmd": "snapshot"}))?;
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
            "scythe://project/summary" => (
                "text/plain",
                self.project(Call::default(), &ProjectArgs { items: true, ..Default::default() }).map_err(failed)?,
            ),
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
