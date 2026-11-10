//! The AI agents connected over MCP: what they are doing, the user's pause, and the reports
//! the MCP bridges make. `agents.activity`, `agents.pause`, `agents.undo` and
//! `agents.disconnect` are for people (the window, the web UI); the MCP server (Scythe's
//! `/mcp` and `scythe-mcp`) sends the rest around each tool call: `agents.call` before it,
//! its commands wrapped in `agents.run` (so their undo steps carry the agent's name and the
//! pause holds), `agents.done` after.

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json as value};

use super::{ApiError, execute_json, json};
use crate::Session;
use crate::agents::{Activity, AgentUndo, Hello};

/// What a paused agent is told.
pub const PAUSED: &str = "paused by the user in Scythe: changes are refused until they resume, reading still works. \
     Tell the user what you were about to do and wait";

/// Calls the event carries; `agents.activity` answers up to all kept.
pub const EVENT_CALLS: usize = 30;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "cmd")]
pub enum AgentsCommand {
    /// The agents connected over MCP (name, version, transport, since when, what they say
    /// they are doing), their recent tool calls with what came of them and the undo steps they
    /// made, whether agents are paused, and the undo step "undo last agent change" would revert.
    #[serde(rename = "agents.activity")]
    Activity {
        /// How many of the newest calls to answer (all kept, up to 200, when left out).
        #[serde(default)]
        calls: Option<usize>,
    },
    /// Pauses agents (their tool calls that change anything are refused; reading still works)
    /// or lets them go on.
    #[serde(rename = "agents.pause")]
    Pause {
        #[serde(default = "yes")]
        paused: bool,
    },
    /// Undoes the last undo step when an agent made it; refused when the user's own edit is on
    /// top.
    #[serde(rename = "agents.undo")]
    Undo,
    /// Disconnects an agent (every agent when `client` is left out): its calls are refused
    /// until it opens a new MCP session.
    #[serde(rename = "agents.disconnect")]
    Disconnect {
        #[serde(default)]
        client: Option<String>,
    },
    /// From an MCP server: a client connected or is still there.
    #[serde(rename = "agents.hello")]
    Hello {
        /// The MCP session or bridge process.
        client: String,
        /// The client's name from `initialize`.
        name: String,
        #[serde(default)]
        version: Option<String>,
        /// `http` or `stdio`.
        transport: String,
        /// Seconds it stays listed without another hello or call.
        #[serde(default)]
        ttl: Option<u64>,
    },
    /// From an MCP server: a client ended its session.
    #[serde(rename = "agents.bye")]
    Bye { client: String },
    /// From an MCP server: what the agent says it is doing (`set_status`); empty clears it.
    #[serde(rename = "agents.status")]
    Status {
        client: String,
        name: String,
        #[serde(default)]
        version: Option<String>,
        transport: String,
        text: String,
    },
    /// From an MCP server: a tool call starts. Answers its number as `call`, and whether
    /// agents are `paused`.
    #[serde(rename = "agents.call")]
    Call {
        client: String,
        name: String,
        #[serde(default)]
        version: Option<String>,
        transport: String,
        tool: String,
        /// The control API command the tool runs, when it is one.
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        arguments: Value,
    },
    /// From an MCP server: runs `command` for call `call`, refused while agents are paused
    /// unless it only reads. Its undo steps are marked as the agent's.
    #[serde(rename = "agents.run")]
    Run { call: u64, command: Value },
    /// From an MCP server: call `call` ended, failing with `error` when given.
    #[serde(rename = "agents.done")]
    Done {
        call: u64,
        #[serde(default)]
        error: Option<String>,
    },
}

fn yes() -> bool {
    true
}

/// The agent who made the step undo would revert, with its label.
pub fn agent_undo(session: &Session) -> Option<AgentUndo> {
    let agent = session.history.undo_by()?;
    Some(AgentUndo { label: session.history.undo_label()?.to_owned(), agent: agent.to_owned() })
}

pub fn activity(session: &Session, calls: usize) -> Activity {
    session.agents.activity(calls, agent_undo(session))
}

/// Whether the command `cmd` only reads, judged by its action's words, so new commands get a
/// sensible answer too. MCP tools are annotated read-only by it, and the pause lets these
/// through.
pub fn reads_only(cmd: &str) -> bool {
    const READS: [&str; 25] = [
        "list", "get", "state", "status", "catalog", "params", "snapshot", "search", "info", "songs", "export", "show",
        "peaks", "health", "devices", "evaluate", "since", "meters", "probe", "job", "compare", "file", "mix",
        "whoami", "activity",
    ];
    if cmd == "script.run" {
        return false;
    }
    let action = cmd.rsplit('.').next().unwrap_or(cmd);
    action.split('_').any(|word| READS.contains(&word))
}

pub fn execute(session: &mut Session, command: AgentsCommand) -> Result<Value, ApiError> {
    match command {
        AgentsCommand::Activity { calls } => Ok(json(activity(session, calls.unwrap_or(usize::MAX)))),
        AgentsCommand::Pause { paused } => {
            session.agents.set_paused(paused);
            if paused {
                session.notify("Agents paused: their changes are refused until you resume");
            }
            Ok(value!({"paused": paused}))
        }
        AgentsCommand::Undo => {
            if session.history.undo_by().is_none() {
                return Err(ApiError::failed("the last undo step was not made by an agent"));
            }
            let label = session.history.undo_label().map(str::to_owned);
            session.undo();
            Ok(value!({"label": label, "undo": session.history.undo_label()}))
        }
        AgentsCommand::Disconnect { client } => {
            let count = session.agents.disconnect(client.as_deref());
            if client.is_some() && count == 0 {
                return Err(ApiError::not_found("no such agent is connected"));
            }
            Ok(value!({"disconnected": count}))
        }
        AgentsCommand::Hello { client, name, version, transport, ttl } => {
            let hello = Hello { client, name, version, transport, ttl: ttl.map(Duration::from_secs) };
            session.agents.hello(&hello).map_err(ApiError::failed)?;
            Ok(value!({"paused": session.agents.paused()}))
        }
        AgentsCommand::Bye { client } => {
            session.agents.bye(&client);
            Ok(value!({}))
        }
        AgentsCommand::Status { client, name, version, transport, text } => {
            let hello = Hello { client, name, version, transport, ttl: None };
            session.agents.set_status(&hello, &text).map_err(ApiError::failed)?;
            Ok(value!({"status": session.agents.status().map(|(_, s)| s)}))
        }
        AgentsCommand::Call { client, name, version, transport, tool, command, arguments } => {
            let hello = Hello { client, name, version, transport, ttl: None };
            let summary = describe(session, &tool, command.as_deref(), &arguments);
            let call = session.agents.begin(&hello, &tool, summary).map_err(ApiError::failed)?;
            Ok(value!({"call": call, "paused": session.agents.paused()}))
        }
        AgentsCommand::Run { call, command } => run(session, call, command),
        AgentsCommand::Done { call, error } => {
            session.agents.end(call, error);
            Ok(value!({}))
        }
    }
}

/// What a tool call does in words; an effect parameter by its name, which only the running
/// effect knows.
fn describe(session: &Session, tool: &str, command: Option<&str>, arguments: &Value) -> String {
    let (cmd, args) = match (tool, command) {
        ("scythe_command", _) => (arguments["command"]["cmd"].as_str(), arguments["command"].clone()),
        (_, Some(cmd)) => (Some(cmd), arguments.clone()),
        _ => (None, Value::Null),
    };
    if cmd == Some("fx.param") {
        let mut command = args.clone();
        command["cmd"] = "fx.param".into();
        if let (Some((effect, param)), Some(value)) =
            (super::fx::param_names(session, &command), args["value"].as_f64())
        {
            let track =
                crate::agents::track_name(&session.project, &args["track"]).unwrap_or_else(|| "the track".into());
            return format!("Set {effect} {param} on {track} to {}", (value * 100.0).round() / 100.0);
        }
    }
    crate::agents::summarize(&session.project, tool, command, arguments)
}

fn run(session: &mut Session, call: u64, command: Value) -> Result<Value, ApiError> {
    let agent = match session.agents.call(call) {
        Some(c) if c.ok.is_none() => c.agent.clone(),
        Some(_) => return Err(ApiError::failed(format!("call {call} has ended"))),
        None => return Err(ApiError::not_found(format!("no call {call}"))),
    };
    let cmd = command.get("cmd").and_then(Value::as_str).unwrap_or_default();
    if cmd.starts_with("agents.") && cmd != "agents.activity" {
        return Err(ApiError::forbidden("agents.* commands but agents.activity are for the people using Scythe"));
    }
    if session.agents.paused() && !reads_only(cmd) {
        return Err(ApiError::failed(PAUSED));
    }
    let since = session.history.next_serial();
    let revision = session.revision();
    let answer = execute_json(session, command);
    let undo = session.history.attribute_since(since, &agent);
    let changed = session.revision() != revision || !undo.is_empty();
    session.agents.record(call, undo, changed);
    answer
}
