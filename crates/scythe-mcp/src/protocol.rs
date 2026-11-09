//! MCP's JSON-RPC methods: lifecycle, tools, resources and prompts. Transport-agnostic: the
//! stdio loop and the HTTP endpoint hand each message to [`McpServer::handle`].

use std::sync::Arc;

use scythe_session::api::ApiError;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::backend::Backend;
use crate::summary;
use crate::tools::{self, Action, CommandArgs, ProjectArgs, ProjectFormat};

/// The revisions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const CONTROL_API_DOC: &str = include_str!("../../../docs/control-api.md");
const MCP_DOC: &str = include_str!("../../../docs/mcp.md");

const INSTRUCTIONS: &str = "Scythe is a REAPER-compatible DAW. Call get_project first: it shows the \
    tracks with the ids and names other tools take as `track` (\"master\" names the master). Times \
    are in seconds, levels in dB (`db`) or as gain (`volume`, 1.0 = 0 dB), pan from -1 to 1. Every \
    change is an undo step in Scythe, so project_undo reverts it. Paths are on the machine running \
    Scythe. Changes show up live in Scythe's window.";

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const RESOURCE_NOT_FOUND: i64 = -32002;

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

/// Answers MCP messages by running control API commands on a [`Backend`].
#[derive(Clone)]
pub struct McpServer {
    backend: Arc<dyn Backend>,
}

impl McpServer {
    pub fn new(backend: impl Backend + 'static) -> Self {
        Self { backend: Arc::new(backend) }
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
            "tools/list" => Ok(json!({"tools": tools::all().iter().map(|t| &t.definition).collect::<Vec<_>>()})),
            "tools/call" => self.call_tool(&params),
            "resources/list" => Ok(json!({"resources": resources()})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "resources/read" => self.read_resource(&params),
            "prompts/list" => Ok(json!({"prompts": prompts()})),
            "prompts/get" => get_prompt(&params),
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
                Ok(args) => self.backend.command(Value::Object(args.command)).map(|v| render(&v)),
                Err(e) => return Ok(tool_error(&format!("invalid arguments: {e}"))),
            },
            Action::Command(cmd) => {
                let mut command = arguments;
                command["cmd"] = cmd.as_str().into();
                self.backend.command(command).map(|v| render(&v))
            }
        };
        Ok(match answer {
            Ok(text) => json!({"content": [{"type": "text", "text": text}], "isError": false}),
            Err(e) => tool_error(&describe(&e)),
        })
    }

    fn project(&self, args: &ProjectArgs) -> Result<String, ApiError> {
        let snapshot = self.backend.command(json!({"cmd": "snapshot"}))?;
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
                ("application/json", self.backend.command(json!({"cmd": "snapshot"})).map_err(failed)?.to_string())
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
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str();
    let version = asked.filter(|v| PROTOCOL_VERSIONS.contains(v)).unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {
            "tools": {"listChanged": false},
            "resources": {"subscribe": false, "listChanged": false},
            "prompts": {"listChanged": false},
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
         "description": "The open project in a few lines per track, items included.", "mimeType": "text/plain"},
        {"uri": "scythe://project", "name": "project", "title": "Project snapshot",
         "description": "The open project as JSON: transport, tracks with FX, sends and items, markers.",
         "mimeType": "application/json"},
        {"uri": "scythe://docs/control-api", "name": "control-api", "title": "Control API reference",
         "description": "Every command scythe_command accepts, with its arguments and answers.",
         "mimeType": "text/markdown"},
        {"uri": "scythe://docs/mcp", "name": "mcp", "title": "Scythe MCP server",
         "description": "How this server is set up and what its tools do.", "mimeType": "text/markdown"},
    ])
}

fn prompts() -> Value {
    json!([
        {"name": "mix_check", "title": "Mix check",
         "description": "Inspect levels, routing and effects of the open project and report problems.",
         "arguments": [{"name": "focus", "description": "What to look at most, such as \"vocals\" or \"the drum bus\".", "required": false}]},
        {"name": "monitor_mix", "title": "Monitor mix change",
         "description": "Change a musician's in-ear monitor mix the way they ask.",
         "arguments": [
            {"name": "musician", "description": "Whose mix, such as \"Drummer\".", "required": true},
            {"name": "request", "description": "What they asked for, such as \"more bass, less keys\".", "required": true}]},
        {"name": "record_setup", "title": "Recording setup",
         "description": "Check and prepare tracks, inputs and monitoring for recording.",
         "arguments": [{"name": "tracks", "description": "The tracks to record, such as \"Vox and Gtr\".", "required": false}]},
    ])
}

fn get_prompt(params: &Value) -> Result<Value, RpcError> {
    let argument = |name: &str| params["arguments"][name].as_str().filter(|s| !s.trim().is_empty());
    let (description, text) = match params["name"].as_str() {
        Some("mix_check") => {
            let focus = argument("focus").map(|f| format!(", paying most attention to {f}")).unwrap_or_default();
            let text = format!(
                "Check the mix of the open Scythe project{focus}. Start with get_project; use fx_list, \
                 fx_params and send_list where you need more detail. Look for: tracks muted or soloed by \
                 accident; faders far above 0 dB or at -inf; pan at the extremes; sends to muted tracks or \
                 at odd levels; effects bypassed or not running; tracks left armed or monitoring their \
                 input; tracks without items; folder structure that hides tracks; effects and level on \
                 the master. Report findings as a short list naming tracks by name and id, then propose \
                 concrete changes as tool calls, but make none until I agree."
            );
            ("Inspect levels, routing and effects", text)
        }
        Some("monitor_mix") => {
            let musician = argument("musician").unwrap_or("the musician");
            let request = argument("request").unwrap_or("a better balance");
            let text = format!(
                "{musician} asks for this in their monitor mix: \"{request}\". Find their mix with mix_list \
                 (create one with mix_create only if there is none, and tell me). Change it with \
                 mix_level, mix_pan, mix_mute or mix_more_me in small steps, at most 3 dB at a time, \
                 and never touch the main mix (track faders) or other musicians' mixes. Then list what \
                 you changed, from and to, so it can be undone."
            );
            ("Adjust a monitor mix", text)
        }
        Some("record_setup") => {
            let tracks = argument("tracks").map(|t| format!(" for {t}")).unwrap_or_default();
            let text = format!(
                "Prepare the open Scythe project for recording{tracks}. Start with get_project. For each \
                 track to record: arm it, check its record input (track_input) and whether input \
                 monitoring should be on; disarm tracks that should not record. Check the metronome and \
                 where the edit cursor is, since recording starts there. Tell me what you set and what I \
                 still need to check by ear; do not start recording yourself."
            );
            ("Prepare tracks for recording", text)
        }
        Some(other) => return Err(RpcError::new(INVALID_PARAMS, format!("no prompt \"{other}\""))),
        None => return Err(RpcError::new(INVALID_PARAMS, "give the prompt's \"name\"")),
    };
    Ok(json!({
        "description": description,
        "messages": [{"role": "user", "content": {"type": "text", "text": text}}],
    }))
}
