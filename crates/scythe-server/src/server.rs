//! The control API over HTTP and WebSocket (see `docs/control-api.md`), and MCP for AI agents
//! over Streamable HTTP at `/mcp` (see `docs/mcp.md`). The server runs on a thread and tokio
//! runtime of its own; commands reach the session through a [`ControlHandle`] and are
//! answered on the session's thread.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use scythe_mcp::{Backend, McpServer, Selection};
use scythe_session::Waker;
use scythe_session::access::Scope;
use scythe_session::api::{ApiError, Command, ErrorKind, Event};
use scythe_session::control::{self, ControlHandle, ControlPump, EventSink};
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::access::Access;

/// Bumped when a change to the protocol would break existing clients.
pub const PROTOCOL: u32 = 1;

const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerConfig {
    pub addr: SocketAddr,
    /// The admin token: required from every client when set, as `Authorization: Bearer <token>`
    /// or `?token=`. Access links (see [`crate::access`]) carry tokens of their own.
    pub token: Option<String>,
    /// Where access links are kept; in memory only when `None`.
    pub access_file: Option<PathBuf>,
    /// The folders the web UI's file browser offers; the home folder when empty.
    pub folders: Vec<PathBuf>,
}

impl ServerConfig {
    /// Listening on `addr` with `token`, no stored links and the default folders.
    pub fn new(addr: SocketAddr, token: Option<String>) -> Self {
        Self { addr, token, access_file: None, folders: Vec::new() }
    }

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

/// An event as it goes out: the event, and its text as the admin receives it.
struct Published {
    event: Event,
    text: Arc<str>,
}

fn event_text(event: &Event) -> String {
    let mut value = serde_json::to_value(event).unwrap_or_else(|_| json!({}));
    value["type"] = "event".into();
    value.to_string()
}

#[derive(Clone)]
struct AppState {
    control: ControlHandle,
    events: broadcast::Sender<Arc<Published>>,
    access: Arc<Access>,
    folders: Arc<Vec<PathBuf>>,
    addr: SocketAddr,
    mcp: McpServer,
    /// Views of `mcp` listing other tools, by the `?tools=` they were asked for.
    mcp_views: Arc<std::sync::Mutex<HashMap<String, McpServer>>>,
    /// MCP notifications, for the clients holding `GET /mcp` open.
    mcp_events: broadcast::Sender<Arc<str>>,
    /// WebSocket clients connected now.
    clients: Arc<AtomicUsize>,
}

impl ControlServer {
    /// Binds `config.addr` and serves. The returned pump belongs to the session's thread,
    /// which calls it to run commands and publish events; `wake` tells that thread a command
    /// is waiting.
    pub fn start(config: ServerConfig, wake: Waker) -> std::io::Result<(Self, ControlPump)> {
        let access = Access::load(config.token, config.access_file).map_err(std::io::Error::other)?;
        let listener = std::net::TcpListener::bind(config.addr)?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let (events, _) = broadcast::channel::<Arc<Published>>(1024);
        let publish = events.clone();
        let sink: EventSink = Arc::new(move |event: &Event| {
            if publish.receiver_count() == 0 {
                return;
            }
            let _ = publish.send(Arc::new(Published { event: event.clone(), text: event_text(event).into() }));
        });
        let (handle, pump) = control::channel(wake, sink);
        let access = Arc::new(access);
        let mcp = McpServer::new(McpBackend { local: scythe_mcp::Local(handle.clone()), access: access.clone(), addr });
        let (mcp_events, _) = broadcast::channel::<Arc<str>>(256);
        let notify = mcp_events.clone();
        mcp.set_notifier(Arc::new(move |message: &Value| {
            let _ = notify.send(message.to_string().into());
        }));
        let folders = if config.folders.is_empty() {
            std::env::var_os("HOME").map(PathBuf::from).into_iter().collect()
        } else {
            config.folders
        };
        let state = AppState {
            control: handle,
            events,
            access,
            folders: Arc::new(folders),
            addr,
            mcp,
            mcp_views: Arc::default(),
            mcp_events,
            clients: Arc::default(),
        };
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
        .route("/api/peaks", get(peaks))
        .route("/api/files", get(files))
        .route("/api/ws", get(websocket))
        .route("/mcp", post(mcp).get(mcp_stream).delete(mcp_delete))
        .route_layer(middleware::from_fn_with_state(state.clone(), authorize))
        .route("/api/health", get(health))
        .fallback(crate::web::serve)
        .with_state(state)
}

fn refuse(status: StatusCode, kind: &str, message: &str) -> Response {
    (status, Json(json!({"ok": false, "error": {"kind": kind, "message": message}}))).into_response()
}

/// Finds the scope of the token presented and hands it to the handler; an unknown token is
/// refused here.
async fn authorize(State(state): State<AppState>, mut request: Request, next: Next) -> Response {
    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let query = request.uri().query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token=")));
    let Some(scope) = state.access.scope(bearer.or(query)) else {
        return refuse(StatusCode::UNAUTHORIZED, "unauthorized", "a valid token is required");
    };
    request.extensions_mut().insert(scope);
    next.run(request).await
}

async fn health(State(state): State<AppState>) -> Json<Value> {
    Json(json!({
        "ok": true,
        "name": "scythe",
        "protocol": PROTOCOL,
        "token": state.access.has_admin_token(),
        "clients": state.clients.load(Ordering::Relaxed),
    }))
}

/// Queues a command for a client with `scope` and waits for its answer.
async fn run(state: &AppState, scope: Scope, command: Command) -> Result<Value, ApiError> {
    let (tx, rx) = oneshot::channel();
    if !state.control.send_as(scope, command, move |answer| {
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
                ErrorKind::Forbidden => StatusCode::FORBIDDEN,
            };
            (status, Json(json!({"ok": false, "error": error}))).into_response()
        }
    }
}

fn parse(value: Value) -> Result<Command, ApiError> {
    serde_json::from_value(value).map_err(|e| ApiError::invalid(e.to_string()))
}

fn is_access(value: &Value) -> bool {
    value.get("cmd").and_then(Value::as_str).is_some_and(|c| c.starts_with("access."))
}

/// Addresses phones on the network can reach this server at, for the links' QR codes.
fn origins(addr: SocketAddr) -> Vec<String> {
    let ip = addr.ip();
    if ip.is_loopback() {
        return Vec::new();
    }
    if !ip.is_unspecified() {
        return vec![format!("http://{addr}")];
    }
    // Connecting a UDP socket sends nothing; it only makes the system pick the interface it
    // would route through, which is the address the LAN knows this machine by.
    let probe = std::net::UdpSocket::bind("0.0.0.0:0").and_then(|s| s.connect("192.0.2.1:9").and(s.local_addr()));
    match probe {
        Ok(local) if !local.ip().is_unspecified() => vec![format!("http://{}:{}", local.ip(), addr.port())],
        _ => Vec::new(),
    }
}

/// `access.*` commands, which the server answers itself: the links live here, not in the
/// session.
fn access_command(state: &AppState, scope: &Scope, value: Value) -> Result<Value, ApiError> {
    access_answer(&state.access, state.addr, scope, value)
}

/// Answers an `access.*` command, giving each link the addresses its page is reached at (what
/// its QR code holds).
fn access_answer(access: &Access, addr: SocketAddr, scope: &Scope, value: Value) -> Result<Value, ApiError> {
    let listing = value.get("cmd").and_then(Value::as_str) == Some("access.list");
    let mut answer = access.execute(scope, value)?;
    let origins = origins(addr);
    let with_urls = |link: &mut Value| {
        if let Some(path) = link.get("path").and_then(Value::as_str).map(str::to_owned) {
            let urls: Vec<String> = origins.iter().map(|o| format!("{o}{path}")).collect();
            link["urls"] = urls.into();
        }
    };
    match answer.get_mut("links").and_then(Value::as_array_mut) {
        Some(links) => links.iter_mut().for_each(with_urls),
        None => with_urls(&mut answer),
    }
    if listing {
        answer["origins"] = origins.into();
    }
    Ok(answer)
}

/// The session for MCP tools, with the access links the server keeps.
struct McpBackend {
    local: scythe_mcp::Local,
    access: Arc<Access>,
    addr: SocketAddr,
}

impl Backend for McpBackend {
    fn command(&self, command: Value) -> Result<Value, ApiError> {
        if is_access(&command) {
            return access_answer(&self.access, self.addr, &Scope::Admin, command);
        }
        self.local.command(command)
    }
}

async fn command(State(state): State<AppState>, Extension(scope): Extension<Scope>, body: Bytes) -> Response {
    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return respond(Err(ApiError::invalid(format!("not JSON: {e}")))),
    };
    if is_access(&value) {
        return respond(access_command(&state, &scope, value));
    }
    match parse(value) {
        Ok(command) => respond(run(&state, scope, command).await),
        Err(e) => respond(Err(e)),
    }
}

async fn snapshot(State(state): State<AppState>, Extension(scope): Extension<Scope>) -> Response {
    match parse(json!({"cmd": "snapshot"})) {
        Ok(command) => respond(run(&state, scope, command).await),
        Err(e) => respond(Err(e)),
    }
}

/// `item.peaks` as a GET, so a browser can cache waveforms by address.
async fn peaks(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let number = |name: &str| query.get(name).and_then(|v| v.parse::<f64>().ok());
    let mut value = json!({"cmd": "item.peaks"});
    for name in ["item", "width"] {
        match query.get(name).and_then(|v| v.parse::<u64>().ok()) {
            Some(n) => value[name] = n.into(),
            None => return respond(Err(ApiError::invalid(format!("give \"{name}\" as a whole number")))),
        }
    }
    for name in ["from", "to"] {
        if let Some(t) = number(name) {
            value[name] = t.into();
        }
    }
    match parse(value) {
        Ok(command) => respond(run(&state, scope, command).await),
        Err(e) => respond(Err(e)),
    }
}

/// The file browser: folders and .RPP files under the configured folders, for the admin only.
async fn files(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !scope.is_admin() {
        return respond(Err(ApiError::forbidden("browsing files needs the admin token")));
    }
    let folders = state.folders.clone();
    let path = query.get("path").filter(|p| !p.is_empty()).map(PathBuf::from);
    let listing = tokio::task::spawn_blocking(move || crate::files::list(&folders, path.as_deref())).await;
    match listing {
        Ok(answer) => respond(answer.map(|l| serde_json::to_value(l).unwrap_or(Value::Null))),
        Err(_) => respond(Err(ApiError::failed("listing the folder failed"))),
    }
}

/// Whether `origin` is this machine or the host the request was sent to, so a web page
/// elsewhere cannot reach the session through a browser (DNS rebinding).
fn trusted_origin(origin: &str, host: Option<&str>) -> bool {
    let Some((_, authority)) = origin.split_once("://") else { return false };
    let authority = authority.trim_end_matches('/');
    if host == Some(authority) {
        return true;
    }
    let name = match authority.rfind(':') {
        Some(i) if !authority[i..].contains(']') => &authority[..i],
        _ => authority,
    };
    matches!(name, "localhost" | "127.0.0.1" | "[::1]")
}

/// One MCP message (or batch) per POST, answered as JSON; notifications get 202.
async fn mcp(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !scope.is_admin() {
        let body =
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "MCP needs the admin token"}});
        return (StatusCode::FORBIDDEN, Json(body)).into_response();
    }
    let text = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
    if !state.access.has_admin_token()
        && let Some(origin) = text(header::ORIGIN)
        && !trusted_origin(origin, text(header::HOST))
    {
        let body = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": "origin not allowed"}});
        return (StatusCode::FORBIDDEN, Json(body)).into_response();
    }
    let version = headers.get("mcp-protocol-version").and_then(|v| v.to_str().ok());
    if version.is_some_and(|v| !scythe_mcp::PROTOCOL_VERSIONS.contains(&v)) {
        let message = format!(
            "unsupported MCP-Protocol-Version; this server speaks {}",
            scythe_mcp::PROTOCOL_VERSIONS.join(", ")
        );
        let body = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": message}});
        return (StatusCode::BAD_REQUEST, Json(body)).into_response();
    }
    let message: Value = match serde_json::from_slice(&body) {
        Ok(m) => m,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(scythe_mcp::protocol::parse_error(e))).into_response(),
    };
    let server = match mcp_view(&state, query.get("tools")) {
        Ok(server) => server,
        Err(e) => {
            let body = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32600, "message": e}});
            return (StatusCode::BAD_REQUEST, Json(body)).into_response();
        }
    };
    // Tool calls wait for the session's thread to answer.
    match tokio::task::spawn_blocking(move || server.handle(message)).await {
        Ok(Some(answer)) => Json(answer).into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// The server for `?tools=`: the default one, or a view listing those tools, kept so a
/// client's later `scythe_tools` changes last.
fn mcp_view(state: &AppState, tools: Option<&String>) -> Result<McpServer, String> {
    let Some(spec) = tools.map(|t| t.trim()).filter(|t| !t.is_empty()) else { return Ok(state.mcp.clone()) };
    let mut views = state.mcp_views.lock().unwrap();
    if let Some(view) = views.get(spec) {
        return Ok(view.clone());
    }
    let view = state.mcp.view(Selection::parse(spec)?);
    views.insert(spec.to_owned(), view.clone());
    Ok(view)
}

/// The server's notifications (resource updates, tool list changes, log messages) as a
/// stream of server-sent events, for as long as the client holds it open.
async fn mcp_stream(State(state): State<AppState>, Extension(scope): Extension<Scope>) -> Response {
    if !scope.is_admin() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let events = state.mcp_events.subscribe();
    let stream = futures_util::stream::unfold(events, |mut events| async move {
        loop {
            match events.recv().await {
                Ok(text) => return Some((Ok::<_, std::convert::Infallible>(SseEvent::default().data(&*text)), events)),
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

/// Sessions are not tracked, so there is none to end.
async fn mcp_delete() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [(header::ALLOW, "GET, POST")]).into_response()
}

async fn websocket(
    State(state): State<AppState>,
    Extension(scope): Extension<Scope>,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade.on_upgrade(move |socket| serve_socket(socket, state, scope))
}

fn ws_reply(id: &Value, answer: Result<Value, ApiError>) -> String {
    match answer {
        Ok(result) => json!({"type": "reply", "id": id, "ok": true, "result": result}),
        Err(error) => json!({"type": "reply", "id": id, "ok": false, "error": error}),
    }
    .to_string()
}

/// Counts a WebSocket client for as long as it is connected.
struct Connected(Arc<AtomicUsize>);

impl Drop for Connected {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One WebSocket client: commands in (answered in the order sent), replies and the events its
/// scope may hear out.
async fn serve_socket(socket: WebSocket, state: AppState, scope: Scope) {
    state.clients.fetch_add(1, Ordering::Relaxed);
    let _connected = Connected(state.clients.clone());
    let (mut sink, mut stream) = socket.split();
    let (replies, mut pending) = mpsc::unbounded_channel::<String>();
    let mut events = state.events.subscribe();
    let hello = json!({"type": "hello", "protocol": PROTOCOL, "access": scope}).to_string();
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
                    if is_access(&value) {
                        let _ = replies.send(ws_reply(&id, access_command(&state, &scope, value)));
                        continue;
                    }
                    match parse(value) {
                        Ok(command) => {
                            let out = replies.clone();
                            let reply_id = id.clone();
                            let sent = state.control.send_as(scope.clone(), command, move |answer| {
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
                Ok(published) => {
                    let text = if scope.is_admin() {
                        String::from(&*published.text)
                    } else {
                        match scope.event(&published.event) {
                            Some(event) => event_text(&event),
                            None => continue,
                        }
                    };
                    if sink.send(Message::Text(text.into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => break,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_or_same_host_origins_are_trusted() {
        assert!(trusted_origin("http://localhost:3000", Some("127.0.0.1:8770")));
        assert!(trusted_origin("http://127.0.0.1", None));
        assert!(trusted_origin("http://[::1]:8770", None));
        assert!(trusted_origin("http://studio.lan:8770", Some("studio.lan:8770")));
        assert!(!trusted_origin("https://evil.example", Some("127.0.0.1:8770")));
        assert!(!trusted_origin("http://localhost.evil.example", None));
        assert!(!trusted_origin("null", None));
    }
}
