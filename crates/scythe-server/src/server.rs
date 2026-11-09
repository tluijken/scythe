//! The control API over HTTP and WebSocket (see `docs/control-api.md`). The server runs on a
//! thread and tokio runtime of its own; commands reach the session through a
//! [`ControlHandle`] and are answered on the session's thread.

use std::net::SocketAddr;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use scythe_session::Waker;
use scythe_session::api::{ApiError, Command, ErrorKind, Event};
use scythe_session::control::{self, ControlHandle, ControlPump, EventSink};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};

/// Bumped when a change to the protocol would break existing clients.
pub const PROTOCOL: u32 = 1;

const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub addr: SocketAddr,
    /// Required from every client when set, as `Authorization: Bearer <token>` or `?token=`.
    pub token: Option<String>,
}

impl ServerConfig {
    /// Whether clients on other machines can reach the server.
    pub fn is_public(&self) -> bool {
        !self.addr.ip().is_loopback()
    }
}

/// A running server; dropping it stops it.
pub struct ControlServer {
    addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

#[derive(Clone)]
struct AppState {
    control: ControlHandle,
    events: broadcast::Sender<Arc<str>>,
    token: Option<Arc<str>>,
}

impl ControlServer {
    /// Binds `config.addr` and serves. The returned pump belongs to the session's thread,
    /// which calls it to run commands and publish events; `wake` tells that thread a command
    /// is waiting.
    pub fn start(config: ServerConfig, wake: Waker) -> std::io::Result<(Self, ControlPump)> {
        let listener = std::net::TcpListener::bind(config.addr)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let (events, _) = broadcast::channel::<Arc<str>>(1024);
        let publish = events.clone();
        let sink: EventSink = Arc::new(move |event: &Event| {
            if publish.receiver_count() == 0 {
                return;
            }
            let mut value = serde_json::to_value(event).unwrap_or_else(|_| json!({}));
            value["type"] = "event".into();
            let _ = publish.send(value.to_string().into());
        });
        let (handle, pump) = control::channel(wake, sink);
        let state = AppState { control: handle, events, token: config.token.map(Into::into) };
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("scythe-control")
            .enable_all()
            .build()?;
        let (shutdown, stop) = oneshot::channel::<()>();
        let thread = std::thread::Builder::new().name("scythe-control".into()).spawn(move || {
            runtime.block_on(async move {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => return eprintln!("control server: {e}"),
                };
                tokio::select! {
                    served = axum::serve(listener, router(state)) => {
                        if let Err(e) = served {
                            eprintln!("control server: {e}");
                        }
                    }
                    _ = stop => {}
                }
            });
            // Open WebSockets would keep a graceful shutdown waiting forever.
            runtime.shutdown_timeout(Duration::from_millis(500));
        })?;
        Ok((Self { addr, shutdown: Some(shutdown), thread: Some(thread) }, pump))
    }

    /// The address it listens on, with the port the system picked when asked for port 0.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        if let Some(stop) = self.shutdown.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/command", post(command))
        .route("/api/snapshot", get(snapshot))
        .route("/api/ws", get(websocket))
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize))
        .route("/api/health", get(health))
        .with_state(state)
}

/// Compares without stopping at the first difference, so timing tells nothing about the token.
fn same_token(given: &str, token: &str) -> bool {
    given.len() == token.len() && given.bytes().zip(token.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

async fn authorize(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let Some(token) = state.token.as_deref() else { return next.run(request).await };
    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let query = request.uri().query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")));
    if bearer.or(query).is_some_and(|given| same_token(given, token)) {
        return next.run(request).await;
    }
    let body = json!({"ok": false, "error": {"kind": "unauthorized", "message": "a valid token is required"}});
    (StatusCode::UNAUTHORIZED, Json(body)).into_response()
}

async fn health() -> Json<Value> {
    Json(json!({"ok": true, "name": "scythe", "protocol": PROTOCOL}))
}

/// Queues a command and waits for its answer.
async fn run(state: &AppState, command: Command) -> Result<Value, ApiError> {
    let (tx, rx) = oneshot::channel();
    if !state.control.send(command, move |answer| {
        let _ = tx.send(answer);
    }) {
        return Err(ApiError::failed("the session has ended"));
    }
    match tokio::time::timeout(REPLY_TIMEOUT, rx).await {
        Ok(Ok(answer)) => answer,
        _ => Err(ApiError::failed("the session did not answer")),
    }
}

fn respond(answer: Result<Value, ApiError>) -> Response {
    match answer {
        Ok(result) => Json(json!({"ok": true, "result": result})).into_response(),
        Err(error) => {
            let status = match error.kind {
                ErrorKind::Invalid => StatusCode::BAD_REQUEST,
                ErrorKind::NotFound => StatusCode::NOT_FOUND,
                ErrorKind::Failed => StatusCode::CONFLICT,
            };
            (status, Json(json!({"ok": false, "error": error}))).into_response()
        }
    }
}

fn parse(value: Value) -> Result<Command, ApiError> {
    serde_json::from_value(value).map_err(|e| ApiError::invalid(e.to_string()))
}

async fn command(State(state): State<AppState>, body: Bytes) -> Response {
    let parsed = serde_json::from_slice(&body).map_err(|e| ApiError::invalid(format!("not JSON: {e}"))).and_then(parse);
    match parsed {
        Ok(command) => respond(run(&state, command).await),
        Err(e) => respond(Err(e)),
    }
}

async fn snapshot(State(state): State<AppState>) -> Response {
    let command = parse(json!({"cmd": "snapshot"}));
    match command {
        Ok(command) => respond(run(&state, command).await),
        Err(e) => respond(Err(e)),
    }
}

async fn websocket(State(state): State<AppState>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| serve_socket(socket, state))
}

fn ws_reply(id: &Value, answer: Result<Value, ApiError>) -> String {
    match answer {
        Ok(result) => json!({"type": "reply", "id": id, "ok": true, "result": result}),
        Err(error) => json!({"type": "reply", "id": id, "ok": false, "error": error}),
    }
    .to_string()
}

/// One WebSocket client: commands in (answered in the order sent), replies and every event
/// out.
async fn serve_socket(socket: WebSocket, state: AppState) {
    let (mut sink, mut stream) = socket.split();
    let (replies, mut pending) = mpsc::unbounded_channel::<String>();
    let mut events = state.events.subscribe();
    let hello = json!({"type": "hello", "protocol": PROTOCOL}).to_string();
    if sink.send(Message::Text(hello.into())).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            incoming = stream.next() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    let mut value: Value = match serde_json::from_str(text.as_str()) {
                        Ok(v) => v,
                        Err(e) => {
                            let _ = replies.send(ws_reply(&Value::Null, Err(ApiError::invalid(format!("not JSON: {e}")))));
                            continue;
                        }
                    };
                    let id = value.as_object_mut().and_then(|o| o.remove("id")).unwrap_or(Value::Null);
                    match parse(value) {
                        Ok(command) => {
                            let out = replies.clone();
                            let reply_id = id.clone();
                            let sent = state.control.send(command, move |answer| {
                                let _ = out.send(ws_reply(&reply_id, answer));
                            });
                            if !sent {
                                let _ = replies.send(ws_reply(&id, Err(ApiError::failed("the session has ended"))));
                            }
                        }
                        Err(e) => {
                            let _ = replies.send(ws_reply(&id, Err(e)));
                        }
                    }
                }
                Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            reply = pending.recv() => {
                let Some(reply) = reply else { break };
                if sink.send(Message::Text(reply.into())).await.is_err() {
                    break;
                }
            }
            event = events.recv() => match event {
                Ok(text) => {
                    if sink.send(Message::Text(String::from(&*text).into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}
