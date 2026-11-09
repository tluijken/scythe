//! Where tool calls go: a session in this process (through a [`ControlHandle`]) or a running
//! Scythe's control API over HTTP.

use std::sync::mpsc;
use std::time::Duration;

use scythe_session::api::{ApiError, Command, ErrorKind};
use scythe_session::control::ControlHandle;
use serde_json::{Value, json};

/// The first `fx.catalog` scans for plugins, which can take a while.
const TIMEOUT: Duration = Duration::from_secs(120);

/// Runs one control API command (JSON with a `cmd` field) and answers with its result.
pub trait Backend: Send + Sync {
    fn command(&self, command: Value) -> Result<Value, ApiError>;
}

/// A session on another thread of this process, reached through its control channel.
pub struct Local(pub ControlHandle);

impl Backend for Local {
    fn command(&self, command: Value) -> Result<Value, ApiError> {
        let command: Command = serde_json::from_value(command).map_err(|e| ApiError::invalid(e.to_string()))?;
        let (tx, rx) = mpsc::channel();
        if !self.0.send(command, move |answer| {
            let _ = tx.send(answer);
        }) {
            return Err(ApiError::failed("the session has ended"));
        }
        rx.recv_timeout(TIMEOUT).unwrap_or_else(|_| Err(ApiError::failed("the session did not answer")))
    }
}

/// A running Scythe (`scythe-server`, `scythe --headless` or `scythe --control`), through
/// `POST /api/command`.
pub struct Remote {
    agent: ureq::Agent,
    url: String,
    token: Option<String>,
}

impl Remote {
    /// `url` is where the control API listens, such as `http://127.0.0.1:8770`.
    pub fn new(url: &str, token: Option<String>) -> Self {
        let config = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(TIMEOUT)).build();
        Self { agent: ureq::Agent::new_with_config(config), url: url.trim_end_matches('/').to_owned(), token }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    /// Whether Scythe answers at the URL, or why not.
    pub fn health(&self) -> Result<(), String> {
        let answer = self.request(None).map_err(|e| e.message)?;
        if answer["ok"] == true { Ok(()) } else { Err(format!("unexpected answer from {}: {answer}", self.url)) }
    }

    fn request(&self, command: Option<&Value>) -> Result<Value, ApiError> {
        let unreachable = |e: ureq::Error| ApiError::failed(format!("cannot reach Scythe at {}: {e}", self.url));
        let mut response = match command {
            Some(command) => {
                let mut request =
                    self.agent.post(format!("{}/api/command", self.url)).header("content-type", "application/json");
                if let Some(token) = &self.token {
                    request = request.header("authorization", format!("Bearer {token}"));
                }
                request.send(command.to_string()).map_err(unreachable)?
            }
            None => self.agent.get(format!("{}/api/health", self.url)).call().map_err(unreachable)?,
        };
        let text = response.body_mut().with_config().limit(256 << 20).read_to_string().map_err(unreachable)?;
        serde_json::from_str(&text)
            .map_err(|_| ApiError::failed(format!("{} did not answer like Scythe: {text}", self.url)))
    }
}

impl Backend for Remote {
    fn command(&self, command: Value) -> Result<Value, ApiError> {
        let answer = self.request(Some(&command))?;
        if answer["ok"] == true {
            return Ok(answer.get("result").cloned().unwrap_or(json!({})));
        }
        let error = &answer["error"];
        let message = error["message"].as_str().unwrap_or("Scythe refused the command").to_owned();
        let kind = match error["kind"].as_str() {
            Some("invalid") => ErrorKind::Invalid,
            Some("not_found") => ErrorKind::NotFound,
            Some("unauthorized") => return Err(ApiError::failed(format!("{message}: give scythe-mcp --token"))),
            _ => ErrorKind::Failed,
        };
        Err(ApiError { kind, message })
    }
}
