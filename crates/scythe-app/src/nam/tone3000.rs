//! TONE3000 (tone3000.com, formerly ToneHunt): signing in with OAuth 2.0 and PKCE through the
//! system browser and a loopback redirect, listing tones and downloading their models. The app
//! registers with TONE3000 for a publishable key (the OAuth client id); there is no secret to
//! keep. See `docs/tone3000.md`.

use std::io::{BufRead, BufReader, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

pub const DEFAULT_BASE_URL: &str = "https://www.tone3000.com/api/v1";

/// Where Scythe keeps its own settings (`$XDG_CONFIG_HOME/scythe`).
pub fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(std::env::temp_dir)
        .join("scythe")
}

/// How Scythe reaches TONE3000: `tone3000.json` in the config folder, overridden by
/// `SCYTHE_TONE3000_CLIENT_ID`, `SCYTHE_TONE3000_REDIRECT_PORT` and `SCYTHE_TONE3000_BASE_URL`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Config {
    /// The app's publishable key (`t3k_pub_…`), sent as the OAuth client id.
    #[serde(default)]
    pub client_id: Option<String>,
    /// Port of the loopback redirect; 0 takes a free one. A fixed port is needed when the
    /// app's redirect URIs are registered with TONE3000, since only those are accepted then.
    #[serde(default)]
    pub redirect_port: u16,
    #[serde(default)]
    pub base_url: Option<String>,
}

impl Config {
    pub fn path() -> PathBuf {
        config_dir().join("tone3000.json")
    }

    pub fn load() -> Self {
        let mut config: Self =
            std::fs::read(Self::path()).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        config.apply_env(|key| std::env::var(key).ok());
        config
    }

    fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) {
        if let Some(id) = var("SCYTHE_TONE3000_CLIENT_ID").filter(|v| !v.trim().is_empty()) {
            self.client_id = Some(id.trim().to_owned());
        }
        if let Some(port) = var("SCYTHE_TONE3000_REDIRECT_PORT").and_then(|v| v.trim().parse().ok()) {
            self.redirect_port = port;
        }
        if let Some(url) = var("SCYTHE_TONE3000_BASE_URL").filter(|v| !v.trim().is_empty()) {
            self.base_url = Some(url.trim().to_owned());
        }
    }

    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, serde_json::to_vec_pretty(self).unwrap_or_default())
    }

    pub fn base_url(&self) -> &str {
        self.base_url.as_deref().unwrap_or(DEFAULT_BASE_URL).trim_end_matches('/')
    }

    pub fn client_id(&self) -> Option<&str> {
        self.client_id.as_deref().filter(|c| !c.is_empty())
    }
}

/// The tokens of a signed-in session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Unix seconds after which the access token is no longer accepted.
    #[serde(default)]
    pub expires_at: Option<u64>,
}

impl Tokens {
    fn expiring(&self, now: u64) -> bool {
        self.expires_at.is_some_and(|t| now + 60 >= t)
    }
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Keeps the session's tokens in a file only the user can read.
#[derive(Debug, Clone)]
pub struct TokenStore {
    path: PathBuf,
}

impl TokenStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn default_location() -> Self {
        Self::new(config_dir().join("tone3000-session.json"))
    }

    #[cfg(test)]
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    pub fn load(&self) -> Option<Tokens> {
        serde_json::from_slice(&std::fs::read(&self.path).ok()?).ok()
    }

    /// Writes the tokens to a fresh file created with mode 0600 and moves it into place, so
    /// they are never readable by others, not even for a moment.
    pub fn save(&self, tokens: &Tokens) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        let _ = std::fs::remove_file(&tmp);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp)?;
        file.write_all(&serde_json::to_vec(tokens).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, &self.path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    pub fn clear(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// An HTTP request as the client makes them.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub method: &'static str,
    pub url: String,
    pub bearer: Option<String>,
    /// Sent as `application/x-www-form-urlencoded`.
    pub form: Option<Vec<(String, String)>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub status: u16,
    pub body: Vec<u8>,
    /// Seconds from a `Retry-After` header.
    pub retry_after: Option<u64>,
}

/// Sends requests; tests put recorded responses behind it.
pub trait Http: Send + Sync {
    fn send(&self, request: &Request) -> Result<Response, String>;
}

pub struct UreqHttp {
    agent: ureq::Agent,
}

impl Default for UreqHttp {
    fn default() -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent(concat!("Scythe/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Self { agent }
    }
}

/// Model files are small; this only stops a runaway download.
const MAX_BODY: u64 = 256 * 1024 * 1024;

impl Http for UreqHttp {
    fn send(&self, request: &Request) -> Result<Response, String> {
        let auth = request.bearer.as_ref().map(|t| format!("Bearer {t}"));
        let result = match (request.method, &request.form) {
            ("POST", Some(form)) => {
                let mut r = self.agent.post(&request.url);
                if let Some(a) = &auth {
                    r = r.header("Authorization", a);
                }
                r.send_form(form.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            }
            ("POST", None) | ("PUT", _) => {
                let mut r =
                    if request.method == "PUT" { self.agent.put(&request.url) } else { self.agent.post(&request.url) };
                if let Some(a) = &auth {
                    r = r.header("Authorization", a);
                }
                r.send_empty()
            }
            (method, _) => {
                let mut r =
                    if method == "DELETE" { self.agent.delete(&request.url) } else { self.agent.get(&request.url) };
                if let Some(a) = &auth {
                    r = r.header("Authorization", a);
                }
                r.call()
            }
        };
        let mut response = result.map_err(|e| e.to_string())?;
        let status = response.status().as_u16();
        let retry_after =
            response.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
        let body = response.body_mut().with_config().limit(MAX_BODY).read_to_vec().map_err(|e| e.to_string())?;
        Ok(Response { status, body, retry_after })
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ApiError {
    #[error("not signed in to TONE3000")]
    NotSignedIn,
    #[error("no TONE3000 client id set up (see docs/tone3000.md)")]
    NoClientId,
    #[error("TONE3000 session expired, sign in again")]
    SessionExpired,
    #[error("TONE3000 does not allow this for Scythe's key: {0}")]
    Forbidden(String),
    #[error("TONE3000 rate limit reached{}", .0.map(|s| format!(", try again in {s} s")).unwrap_or_default())]
    RateLimited(Option<u64>),
    #[error("TONE3000 answered {0}: {1}")]
    Status(u16, String),
    #[error("could not reach TONE3000: {0}")]
    Network(String),
    #[error("unexpected TONE3000 response: {0}")]
    Decode(String),
}

/// An error body as the OAuth endpoints send it.
#[derive(Debug, Deserialize)]
struct ErrorBody {
    #[serde(default)]
    error: Option<serde_json::Value>,
    #[serde(default)]
    error_description: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

fn error_text(body: &[u8]) -> String {
    match serde_json::from_slice::<ErrorBody>(body) {
        Ok(e) => e
            .error_description
            .or(e.message)
            .or_else(|| {
                e.error.map(|v| match v {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                })
            })
            .unwrap_or_default(),
        Err(_) => String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned(),
    }
}

/// TONE3000 sends `null` for empty lists and unset fields, which `#[serde(default)]` alone
/// only covers when the key is missing.
fn nullable<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// User ids are UUID strings; makes and tags may come either way.
fn any_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        None | Some(serde_json::Value::Null) => String::new(),
        Some(serde_json::Value::String(s)) => s,
        Some(other) => other.to_string(),
    })
}

/// Counts and page numbers: a number, a number sent as a string, or null (read as 0).
fn count<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: TryFrom<u64> + Default,
{
    use serde::de::Error;
    let n = match Option::<serde_json::Value>::deserialize(deserializer)? {
        None | Some(serde_json::Value::Null) => return Ok(T::default()),
        Some(serde_json::Value::String(s)) if s.trim().is_empty() => return Ok(T::default()),
        Some(serde_json::Value::Number(n)) => n.as_u64().or_else(|| n.as_f64().map(|f| f.max(0.0) as u64)),
        Some(serde_json::Value::String(s)) => s.trim().parse().ok(),
        Some(_) => None,
    };
    n.and_then(|n| T::try_from(n).ok()).ok_or_else(|| D::Error::custom("not a count"))
}

/// A count that may be absent; numbers sent as strings are taken.
fn optional_count<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        Some(serde_json::Value::Number(n)) => n.as_i64().or_else(|| n.as_f64().map(|f| f as i64)),
        Some(serde_json::Value::String(s)) => s.trim().parse().ok(),
        _ => None,
    })
}

/// A flag sent as a boolean, a number or a string.
fn flag<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        Some(serde_json::Value::Bool(b)) => Some(b),
        Some(serde_json::Value::Number(n)) => Some(n.as_f64().is_some_and(|v| v != 0.0)),
        Some(serde_json::Value::String(s)) => Some(matches!(s.as_str(), "true" | "1")),
        _ => None,
    })
}

/// Text that is sometimes sent as a number.
fn text<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(match Option::<serde_json::Value>::deserialize(deserializer)? {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(s)) => Some(s),
        Some(other) => Some(other.to_string()),
    })
}

/// Tone and model ids are numbers, but a number sent as a string is taken too.
fn numeric_id<'de, D>(deserializer: D) -> Result<i64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;
    match Option::<serde_json::Value>::deserialize(deserializer)? {
        None | Some(serde_json::Value::Null) => Ok(0),
        Some(serde_json::Value::Number(n)) => n.as_i64().ok_or_else(|| D::Error::custom(format!("id {n}"))),
        Some(serde_json::Value::String(s)) => s.parse().map_err(|_| D::Error::custom(format!("non-numeric id {s:?}"))),
        Some(other) => Err(D::Error::custom(format!("id {other}"))),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct User {
    #[serde(default, deserialize_with = "any_id")]
    pub id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub username: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

impl User {
    /// TONE3000 sets a display name only for verified creators.
    pub fn shown_name(&self) -> &str {
        self.display_name.as_deref().filter(|n| !n.is_empty()).unwrap_or(&self.username)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Named {
    #[serde(default, deserialize_with = "any_id")]
    pub id: String,
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Tone {
    #[serde(default, deserialize_with = "numeric_id")]
    pub id: i64,
    #[serde(default, deserialize_with = "nullable")]
    pub title: String,
    #[serde(default, deserialize_with = "text")]
    pub description: Option<String>,
    #[serde(default, deserialize_with = "text")]
    pub gear: Option<String>,
    #[serde(default, deserialize_with = "text")]
    pub format: Option<String>,
    #[serde(default, deserialize_with = "text")]
    pub license: Option<String>,
    #[serde(default)]
    pub user: Option<User>,
    #[serde(default, deserialize_with = "nullable")]
    pub makes: Vec<Named>,
    #[serde(default, deserialize_with = "nullable")]
    pub tags: Vec<Named>,
    #[serde(default, deserialize_with = "nullable")]
    pub sizes: Vec<String>,
    #[serde(default, deserialize_with = "optional_count")]
    pub models_count: Option<i64>,
    #[serde(default, deserialize_with = "optional_count")]
    pub irs_count: Option<i64>,
    #[serde(default, deserialize_with = "optional_count")]
    pub downloads_count: Option<i64>,
    #[serde(default, deserialize_with = "optional_count")]
    pub favorites_count: Option<i64>,
    #[serde(default, deserialize_with = "flag")]
    pub is_favorite: Option<bool>,
    #[serde(default)]
    pub url: Option<String>,
}

impl Tone {
    pub fn author(&self) -> Option<&str> {
        self.user.as_ref().map(User::shown_name)
    }

    pub fn is_ir(&self) -> bool {
        self.format.as_deref() == Some("ir") || self.gear.as_deref() == Some("ir")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Model {
    #[serde(default, deserialize_with = "numeric_id")]
    pub id: i64,
    #[serde(default, deserialize_with = "numeric_id")]
    pub tone_id: i64,
    #[serde(default, deserialize_with = "nullable")]
    pub name: String,
    #[serde(default, deserialize_with = "text")]
    pub size: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    pub model_url: String,
    /// `1`, `2` or `custom`; absent for impulse responses.
    #[serde(default)]
    pub architecture_version: Option<serde_json::Value>,
    #[serde(default, deserialize_with = "text")]
    pub content_hash: Option<String>,
}

impl Model {
    pub fn architecture(&self) -> Option<String> {
        match self.architecture_version.as_ref()? {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Null => None,
            other => Some(other.to_string()),
        }
    }
}

/// One page of a list. Trending and latest send only `data`.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(bound(deserialize = "T: Deserialize<'de>"))]
pub struct Page<T> {
    #[serde(default, deserialize_with = "nullable")]
    pub data: Vec<T>,
    #[serde(default, deserialize_with = "count")]
    pub page: u32,
    #[serde(default, deserialize_with = "count")]
    pub total_pages: u32,
    #[serde(default, deserialize_with = "count")]
    pub total: u64,
}

/// The lists the TONE3000 tab offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToneList {
    Favorited,
    Downloaded,
    Created,
    Trending,
    Latest,
}

impl ToneList {
    pub const ALL: [ToneList; 5] = [Self::Favorited, Self::Downloaded, Self::Created, Self::Trending, Self::Latest];

    pub fn label(self) -> &'static str {
        match self {
            Self::Favorited => "Favourites",
            Self::Downloaded => "Downloaded",
            Self::Created => "My tones",
            Self::Trending => "Trending",
            Self::Latest => "Latest",
        }
    }

    fn path(self) -> &'static str {
        match self {
            Self::Favorited => "/tones/favorited",
            Self::Downloaded => "/tones/downloaded",
            Self::Created => "/tones/created",
            Self::Trending => "/tones/trending",
            Self::Latest => "/tones/latest",
        }
    }

    fn paged(self) -> bool {
        matches!(self, Self::Favorited | Self::Downloaded | Self::Created)
    }
}

/// `%`-encodes a query value.
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let (k, v) = p.split_once('=').unwrap_or((p, ""));
            (decode(k), decode(v))
        })
        .collect()
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the system's random source");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// A PKCE verifier and its S256 challenge (RFC 7636).
#[derive(Debug, Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        Self::from_verifier(random_token(32))
    }

    pub fn from_verifier(verifier: String) -> Self {
        let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge }
    }
}

/// What the browser flow asks TONE3000 to do besides signing in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Prompt {
    /// Sign in only.
    SignIn,
    /// Let the user browse TONE3000 and pick a tone, which comes back with the redirect.
    SelectTone,
    /// Open a known tone, or let the user pick a replacement when it is gone.
    LoadTone(i64),
}

/// A browser sign-in waiting for its redirect on a loopback port.
pub struct Authorization {
    pub url: String,
    pub redirect_uri: String,
    pkce: Pkce,
    state: String,
    listener: TcpListener,
}

/// What came back with the redirect.
#[derive(Debug, Clone, PartialEq)]
pub struct Callback {
    pub code: String,
    pub tone_id: Option<i64>,
}

impl Authorization {
    /// Listens on `127.0.0.1` and builds the authorize URL to open in the browser.
    pub fn begin(config: &Config, prompt: &Prompt) -> Result<Self, ApiError> {
        let client_id = config.client_id().ok_or(ApiError::NoClientId)?;
        let listener = TcpListener::bind(("127.0.0.1", config.redirect_port))
            .map_err(|e| ApiError::Network(format!("loopback port: {e}")))?;
        let port = listener.local_addr().map_err(|e| ApiError::Network(e.to_string()))?.port();
        let redirect_uri = format!("http://127.0.0.1:{port}/callback");
        let pkce = Pkce::new();
        let state = random_token(16);
        let mut url = format!(
            "{}/oauth/authorize?client_id={}&redirect_uri={}&response_type=code&code_challenge={}\
             &code_challenge_method=S256&state={}&format=nam",
            config.base_url(),
            encode(client_id),
            encode(&redirect_uri),
            pkce.challenge,
            state
        );
        match prompt {
            Prompt::SignIn => {}
            Prompt::SelectTone => url.push_str("&prompt=select_tone"),
            Prompt::LoadTone(id) => url.push_str(&format!("&prompt=load_tone&tone_id={id}")),
        }
        Ok(Self { url, redirect_uri, pkce, state, listener })
    }

    /// Waits for the browser to come back, answering it with a page that says what happened.
    /// Gives up after `timeout` or when `cancel` is set.
    pub fn wait(self, timeout: Duration, cancel: &AtomicBool) -> Result<(Callback, Pkce, String), ApiError> {
        self.listener.set_nonblocking(true).map_err(|e| ApiError::Network(e.to_string()))?;
        let deadline = Instant::now() + timeout;
        loop {
            if cancel.load(Ordering::Relaxed) || Instant::now() > deadline {
                return Err(ApiError::Network("sign-in was not completed".into()));
            }
            let stream = match self.listener.accept() {
                Ok((s, _)) => s,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                }
                Err(e) => return Err(ApiError::Network(e.to_string())),
            };
            if let Some(result) = self.answer(stream) {
                return result.map(|cb| (cb, self.pkce.clone(), self.redirect_uri.clone()));
            }
        }
    }

    /// Reads one request; `None` when it was not the redirect (a favicon, say).
    fn answer(&self, mut stream: TcpStream) -> Option<Result<Callback, ApiError>> {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
        let mut line = String::new();
        BufReader::new(&stream).read_line(&mut line).ok()?;
        let target = line.split_whitespace().nth(1)?.to_owned();
        let (path, query) = target.split_once('?').unwrap_or((&target, ""));
        if path != "/callback" {
            let _ = stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            return None;
        }
        let result = parse_callback(&query_pairs(query), &self.state);
        let message = match &result {
            Ok(_) => "Signed in to TONE3000. You can close this tab and return to Scythe.".to_owned(),
            Err(e) => format!("Scythe could not sign in: {e}"),
        };
        let body = format!(
            "<!doctype html><meta charset=utf-8><title>Scythe</title>\
             <body style=\"font-family:sans-serif;background:#1a1b18;color:#f8f8f2;padding:3em\"><p>{message}</p>"
        );
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        Some(result)
    }
}

fn parse_callback(pairs: &[(String, String)], state: &str) -> Result<Callback, ApiError> {
    let get = |k: &str| pairs.iter().find(|(key, _)| key == k).map(|(_, v)| v.as_str());
    if get("canceled") == Some("true") {
        return Err(ApiError::Network("sign-in was cancelled".into()));
    }
    if let Some(error) = get("error") {
        return Err(ApiError::Forbidden(error.to_owned()));
    }
    if get("state") != Some(state) {
        return Err(ApiError::Decode("the redirect's state does not match the request".into()));
    }
    let code = get("code").filter(|c| !c.is_empty()).ok_or_else(|| ApiError::Decode("no code".into()))?;
    Ok(Callback { code: code.to_owned(), tone_id: get("tone_id").and_then(|t| t.parse().ok()) })
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
}

/// The TONE3000 API on behalf of the signed-in user.
pub struct Client {
    http: Arc<dyn Http>,
    base: String,
    client_id: Option<String>,
    tokens: Mutex<Option<Tokens>>,
    store: Option<TokenStore>,
    debug_log: Option<PathBuf>,
}

impl Client {
    pub fn new(http: Arc<dyn Http>, config: &Config, store: Option<TokenStore>) -> Self {
        let tokens = store.as_ref().and_then(TokenStore::load);
        Self {
            http,
            base: config.base_url().to_owned(),
            client_id: config.client_id().map(str::to_owned),
            tokens: Mutex::new(tokens),
            store,
            debug_log: None,
        }
    }

    /// Where responses that could not be read are written, so a changed API can be looked into.
    /// Only the request path, the error and the body go there, never a token or a header.
    pub fn with_debug_log(mut self, path: PathBuf) -> Self {
        self.debug_log = Some(path);
        self
    }

    pub fn signed_in(&self) -> bool {
        self.tokens.lock().unwrap().is_some()
    }

    pub fn sign_out(&self) {
        *self.tokens.lock().unwrap() = None;
        if let Some(store) = &self.store {
            store.clear();
        }
    }

    fn keep(&self, response: TokenResponse) -> Result<(), ApiError> {
        let tokens = Tokens {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            expires_at: response.expires_in.map(|s| unix_now() + s),
        };
        if let Some(store) = &self.store {
            store.save(&tokens).map_err(|e| ApiError::Network(format!("storing the session: {e}")))?;
        }
        *self.tokens.lock().unwrap() = Some(tokens);
        Ok(())
    }

    fn token_request(&self, form: Vec<(String, String)>) -> Result<TokenResponse, ApiError> {
        let request =
            Request { method: "POST", url: format!("{}/oauth/token", self.base), bearer: None, form: Some(form) };
        let response = self.http.send(&request).map_err(ApiError::Network)?;
        match response.status {
            200..=299 => serde_json::from_slice(&response.body).map_err(|e| ApiError::Decode(e.to_string())),
            400 | 401 if error_text(&response.body).contains("invalid_grant") => Err(ApiError::SessionExpired),
            status => Err(ApiError::Status(status, error_text(&response.body))),
        }
    }

    /// Trades the redirect's code for tokens and keeps them.
    pub fn exchange_code(&self, code: &str, verifier: &str, redirect_uri: &str) -> Result<(), ApiError> {
        let client_id = self.client_id.clone().ok_or(ApiError::NoClientId)?;
        let form = vec![
            ("grant_type".into(), "authorization_code".into()),
            ("code".into(), code.into()),
            ("code_verifier".into(), verifier.into()),
            ("redirect_uri".into(), redirect_uri.into()),
            ("client_id".into(), client_id),
        ];
        let response = self.token_request(form)?;
        self.keep(response)
    }

    /// Gets a new access token. TONE3000 rotates refresh tokens, so the new pair replaces the
    /// stored one at once.
    pub fn refresh(&self) -> Result<(), ApiError> {
        let client_id = self.client_id.clone().ok_or(ApiError::NoClientId)?;
        let refresh = self.tokens.lock().unwrap().as_ref().and_then(|t| t.refresh_token.clone());
        let Some(refresh) = refresh else { return Err(ApiError::SessionExpired) };
        let form = vec![
            ("grant_type".into(), "refresh_token".into()),
            ("refresh_token".into(), refresh),
            ("client_id".into(), client_id),
        ];
        match self.token_request(form) {
            Ok(response) => self.keep(response),
            Err(ApiError::SessionExpired) => {
                self.sign_out();
                Err(ApiError::SessionExpired)
            }
            Err(e) => Err(e),
        }
    }

    fn access_token(&self) -> Result<String, ApiError> {
        let expiring = {
            let tokens = self.tokens.lock().unwrap();
            let t = tokens.as_ref().ok_or(ApiError::NotSignedIn)?;
            t.expiring(unix_now()) && t.refresh_token.is_some()
        };
        if expiring {
            self.refresh()?;
        }
        self.tokens.lock().unwrap().as_ref().map(|t| t.access_token.clone()).ok_or(ApiError::NotSignedIn)
    }

    /// Sends an authorised request, refreshing the session once when the token was refused.
    fn send(&self, method: &'static str, url: String) -> Result<Vec<u8>, ApiError> {
        let mut retried = false;
        loop {
            let request = Request { method, url: url.clone(), bearer: Some(self.access_token()?), form: None };
            let response = self.http.send(&request).map_err(ApiError::Network)?;
            match response.status {
                200..=299 => return Ok(response.body),
                401 if !retried => {
                    retried = true;
                    self.refresh()?;
                }
                401 => return Err(ApiError::SessionExpired),
                403 => return Err(ApiError::Forbidden(error_text(&response.body))),
                429 => return Err(ApiError::RateLimited(response.retry_after)),
                status => return Err(ApiError::Status(status, error_text(&response.body))),
            }
        }
    }

    fn get<T: DeserializeOwned>(&self, path_and_query: &str) -> Result<T, ApiError> {
        let body = self.send("GET", format!("{}{path_and_query}", self.base))?;
        serde_json::from_slice(&body).map_err(|e| {
            let error = ApiError::Decode(e.to_string());
            match self.debug_log.as_ref().map(|log| log_response(log, path_and_query, &error, &body)) {
                Some(Ok(())) => ApiError::Decode(format!("{e} (response written to tone3000-debug.log)")),
                _ => error,
            }
        })
    }

    pub fn user(&self) -> Result<User, ApiError> {
        self.get("/user")
    }

    pub fn tones(&self, list: ToneList, page: u32) -> Result<Page<Tone>, ApiError> {
        if list.paged() {
            self.get(&format!("{}?page={}&page_size=25", list.path(), page.max(1)))
        } else {
            self.get(list.path())
        }
    }

    /// Full-text search. TONE3000 limits it more tightly than the rest, and only some keys may
    /// use it at all.
    pub fn search(&self, query: &str, page: u32) -> Result<Page<Tone>, ApiError> {
        self.get(&format!("/tones/search?query={}&page={}&page_size=25", encode(query), page.max(1)))
    }

    pub fn tone(&self, id: i64) -> Result<Tone, ApiError> {
        self.get(&format!("/tones/{id}"))
    }

    pub fn models(&self, tone_id: i64) -> Result<Vec<Model>, ApiError> {
        let mut out = Vec::new();
        let mut page = 1;
        loop {
            let p: Page<Model> = self.get(&format!("/models?tone_id={tone_id}&page={page}&page_size=100"))?;
            out.extend(p.data);
            if page >= p.total_pages {
                return Ok(out);
            }
            page += 1;
        }
    }

    pub fn set_favorite(&self, tone_id: i64, favorite: bool) -> Result<(), ApiError> {
        let method = if favorite { "PUT" } else { "DELETE" };
        self.send(method, format!("{}/tones/{tone_id}/favorite", self.base)).map(|_| ())
    }

    /// A model's file, fetched with the user's token from its `model_url`.
    pub fn download(&self, model: &Model) -> Result<Vec<u8>, ApiError> {
        self.send("GET", model.model_url.clone())
    }
}

/// Opens `url` in the user's browser.
pub fn open_browser(url: &str) -> std::io::Result<()> {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    std::process::Command::new(opener)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Bodies above this are cut short in the debug log.
const LOGGED_BODY: usize = 256 * 1024;

/// Appends an unreadable response to `log`: when, which request, why, and the body.
fn log_response(log: &std::path::Path, request: &str, error: &ApiError, body: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new().create(true).append(true).open(log)?;
    writeln!(file, "--- {} GET {request}\n{error}\n{} bytes:", unix_now(), body.len())?;
    file.write_all(&body[..body.len().min(LOGGED_BODY)])?;
    writeln!(file)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// Answers requests from a queue of recorded responses and keeps what was asked.
    #[derive(Default)]
    struct Recorded {
        responses: Mutex<VecDeque<Response>>,
        requests: Mutex<Vec<Request>>,
    }

    impl Recorded {
        fn with(responses: impl IntoIterator<Item = (u16, &'static str)>) -> Arc<Self> {
            let queue =
                responses.into_iter().map(|(status, body)| Response { status, body: body.into(), retry_after: None });
            Arc::new(Self { responses: Mutex::new(queue.collect()), requests: Mutex::default() })
        }

        fn requests(&self) -> Vec<Request> {
            self.requests.lock().unwrap().clone()
        }
    }

    impl Http for Recorded {
        fn send(&self, request: &Request) -> Result<Response, String> {
            self.requests.lock().unwrap().push(request.clone());
            self.responses.lock().unwrap().pop_front().ok_or_else(|| "no recorded response".to_owned())
        }
    }

    fn config() -> Config {
        Config {
            client_id: Some("t3k_pub_test".into()),
            redirect_port: 0,
            base_url: Some("https://t3k.test/api/v1".into()),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scythe-t3k-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn signed_in(http: Arc<Recorded>, expires_at: Option<u64>) -> Client {
        let client = Client::new(http, &config(), None);
        *client.tokens.lock().unwrap() =
            Some(Tokens { access_token: "old".into(), refresh_token: Some("r1".into()), expires_at });
        client
    }

    const TONES: &str = include_str!("../../tests/fixtures/tone3000/tones_favorited.json");
    const MODELS: &str = include_str!("../../tests/fixtures/tone3000/models.json");
    const TOKEN: &str = include_str!("../../tests/fixtures/tone3000/token.json");

    #[test]
    fn pkce_challenge_matches_rfc_7636() {
        let pkce = Pkce::from_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into());
        assert_eq!(pkce.challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        let fresh = Pkce::new();
        assert_eq!(fresh.verifier.len(), 43);
        assert_ne!(fresh.verifier, Pkce::new().verifier);
    }

    #[test]
    fn config_takes_the_environment_over_the_file() {
        let mut c = Config { client_id: Some("from-file".into()), ..Config::default() };
        c.apply_env(|k| match k {
            "SCYTHE_TONE3000_CLIENT_ID" => Some("t3k_pub_env".into()),
            "SCYTHE_TONE3000_REDIRECT_PORT" => Some("53682".into()),
            _ => None,
        });
        assert_eq!((c.client_id(), c.redirect_port, c.base_url()), (Some("t3k_pub_env"), 53682, DEFAULT_BASE_URL));
        let mut empty = Config::default();
        empty.apply_env(|_| None);
        assert_eq!(empty.client_id(), None);
        assert_eq!(Authorization::begin(&empty, &Prompt::SignIn).err(), Some(ApiError::NoClientId));
    }

    #[cfg(unix)]
    #[test]
    fn token_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("tokens");
        let store = TokenStore::new(dir.join("session.json"));
        assert_eq!(store.load(), None);
        let tokens = Tokens { access_token: "a".into(), refresh_token: Some("r".into()), expires_at: Some(42) };
        std::fs::write(store.path(), "stale").unwrap();
        std::fs::set_permissions(store.path(), std::fs::Permissions::from_mode(0o644)).unwrap();
        store.save(&tokens).unwrap();
        let mode = std::fs::metadata(store.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(store.load(), Some(tokens));
        store.clear();
        assert_eq!(store.load(), None);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn authorize_url_and_loopback_redirect() {
        let auth = Authorization::begin(&config(), &Prompt::SelectTone).unwrap();
        assert!(auth.redirect_uri.starts_with("http://127.0.0.1:"));
        assert!(auth.url.starts_with("https://t3k.test/api/v1/oauth/authorize?client_id=t3k_pub_test&"));
        for part in ["response_type=code", "code_challenge_method=S256", "prompt=select_tone", "format=nam"] {
            assert!(auth.url.contains(part), "{part} in {}", auth.url);
        }
        assert!(auth.url.contains(&format!("redirect_uri={}", encode(&auth.redirect_uri))));
        assert!(auth.url.contains(&format!("code_challenge={}", auth.pkce.challenge)));
        let port: u16 = auth.redirect_uri.rsplit(':').next().unwrap().trim_end_matches("/callback").parse().unwrap();
        let state = auth.state.clone();
        let browser = std::thread::spawn(move || {
            let mut favicon = TcpStream::connect(("127.0.0.1", port)).unwrap();
            favicon.write_all(b"GET /favicon.ico HTTP/1.1\r\n\r\n").unwrap();
            let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
            write!(s, "GET /callback?code=abc%2B1&state={state}&tone_id=1234 HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
            let mut page = String::new();
            std::io::Read::read_to_string(&mut s, &mut page).unwrap();
            page
        });
        let (callback, pkce, redirect) = auth.wait(Duration::from_secs(10), &AtomicBool::new(false)).unwrap();
        assert_eq!(callback, Callback { code: "abc+1".into(), tone_id: Some(1234) });
        assert_eq!(pkce.verifier.len(), 43);
        assert!(redirect.ends_with("/callback"));
        assert!(browser.join().unwrap().contains("Signed in to TONE3000"));
    }

    #[test]
    fn callback_rejects_a_forged_state_and_reports_errors() {
        let pairs = |q: &str| query_pairs(q);
        assert!(matches!(parse_callback(&pairs("code=x&state=evil"), "good"), Err(ApiError::Decode(_))));
        assert!(matches!(
            parse_callback(&pairs("error=access_denied&state=good"), "good"),
            Err(ApiError::Forbidden(_))
        ));
        assert!(parse_callback(&pairs("canceled=true"), "good").is_err());
        assert_eq!(
            parse_callback(&pairs("code=x&state=good"), "good").unwrap(),
            Callback { code: "x".into(), tone_id: None }
        );
    }

    #[test]
    fn exchanges_the_code_and_stores_the_session() {
        let dir = scratch("exchange");
        let http = Recorded::with([(200, TOKEN)]);
        let store = TokenStore::new(dir.join("session.json"));
        let client = Client::new(http.clone(), &config(), Some(store.clone()));
        assert!(!client.signed_in());
        client.exchange_code("the-code", "the-verifier", "http://127.0.0.1:5000/callback").unwrap();
        let sent = &http.requests()[0];
        assert_eq!((sent.method, sent.url.as_str()), ("POST", "https://t3k.test/api/v1/oauth/token"));
        let form = sent.form.clone().unwrap();
        for (k, v) in [
            ("grant_type", "authorization_code"),
            ("code", "the-code"),
            ("code_verifier", "the-verifier"),
            ("redirect_uri", "http://127.0.0.1:5000/callback"),
            ("client_id", "t3k_pub_test"),
        ] {
            assert!(form.contains(&(k.to_owned(), v.to_owned())), "{k}={v}");
        }
        assert!(!form.iter().any(|(k, _)| k == "client_secret"));
        let stored = store.load().unwrap();
        assert_eq!(stored.access_token, "eyJ.access.1");
        assert_eq!(stored.refresh_token.as_deref(), Some("rt-1"));
        assert!(stored.expires_at.unwrap() > unix_now() + 3000);
        assert!(Client::new(http, &config(), Some(store)).signed_in());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn null_fields_read_as_empty() {
        let latest = r#"{"data":[{"id":9,"title":null,"tags":null,"sizes":null,
            "user":{"id":"913292c0-f036-4690-916d-043924eed513","username":null,"display_name":null},
            "makes":[{"id":"7","name":"Friedman"}],"models_count":null},{"id":"12","title":"t"}],
            "page":null,"total_pages":null,"total":null}"#;
        let http = Recorded::with([(200, latest), (200, r#"{"id":5,"username":"me","display_name":null}"#)]);
        let client = signed_in(http, None);
        let page = client.tones(ToneList::Latest, 1).unwrap();
        assert_eq!(page.data[0].id, 9);
        assert!(page.data[0].tags.is_empty() && page.data[0].sizes.is_empty());
        assert_eq!(page.data[0].user.as_ref().unwrap().id, "913292c0-f036-4690-916d-043924eed513");
        assert_eq!(page.data[1].id, 12, "a numeric id sent as a string");
        assert_eq!(page.data[0].author(), Some(""));
        assert_eq!(client.user().unwrap().shown_name(), "me");
    }

    #[test]
    fn lists_tones_and_their_models() {
        let http = Recorded::with([(200, TONES), (200, MODELS)]);
        let client = signed_in(http.clone(), None);
        let page = client.tones(ToneList::Favorited, 1).unwrap();
        assert_eq!((page.page, page.total_pages, page.total), (1, 1, 2));
        let tone = &page.data[0];
        assert_eq!(tone.title, "Friedman BE-100 Deluxe");
        assert_eq!(tone.author(), Some("Tone King"));
        assert_eq!(tone.gear.as_deref(), Some("amp"));
        assert_eq!(tone.makes[0].name, "Friedman");
        assert_eq!(tone.is_favorite, Some(true));
        assert!(page.data[1].is_ir());
        assert_eq!(page.data[1].author(), Some("cabguy"), "falls back to the username");
        let models = client.models(tone.id).unwrap();
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].architecture().as_deref(), Some("2"));
        assert_eq!(models[1].size.as_deref(), Some("lite"));
        let urls: Vec<_> = http.requests().iter().map(|r| r.url.clone()).collect();
        assert_eq!(
            urls,
            [
                "https://t3k.test/api/v1/tones/favorited?page=1&page_size=25",
                "https://t3k.test/api/v1/models?tone_id=4711&page=1&page_size=100",
            ]
        );
        assert!(http.requests().iter().all(|r| r.bearer.as_deref() == Some("old")));
    }

    #[test]
    fn refreshes_an_expired_token_once() {
        let http =
            Recorded::with([(401, r#"{"error":"unauthorized"}"#), (200, TOKEN), (200, r#"{"id":7,"username":"me"}"#)]);
        let client = signed_in(http.clone(), None);
        assert_eq!(client.user().unwrap().username, "me");
        let requests = http.requests();
        assert_eq!(requests[1].form.as_ref().unwrap()[0], ("grant_type".into(), "refresh_token".into()));
        assert!(requests[1].form.as_ref().unwrap().contains(&("refresh_token".into(), "r1".into())));
        assert_eq!(requests[2].bearer.as_deref(), Some("eyJ.access.1"));

        let ahead = Recorded::with([(200, TOKEN), (200, r#"{"data":[]}"#)]);
        let client = signed_in(ahead.clone(), Some(unix_now() + 10));
        assert!(client.tones(ToneList::Trending, 1).unwrap().data.is_empty());
        assert_eq!(ahead.requests()[0].url, "https://t3k.test/api/v1/oauth/token", "refreshed before expiry");
        assert_eq!(ahead.requests()[1].url, "https://t3k.test/api/v1/tones/trending");
    }

    #[test]
    fn a_dead_refresh_token_signs_out() {
        let http = Recorded::with([(401, "{}"), (400, r#"{"error":"invalid_grant"}"#)]);
        let client = signed_in(http, None);
        assert_eq!(client.user(), Err(ApiError::SessionExpired));
        assert!(!client.signed_in());
        assert_eq!(client.user(), Err(ApiError::NotSignedIn));
    }

    #[test]
    fn maps_rate_limits_and_refusals() {
        let http = Arc::new(Recorded::default());
        http.responses.lock().unwrap().extend([
            Response { status: 429, body: Vec::new(), retry_after: Some(30) },
            Response {
                status: 403,
                body: br#"{"message":"search requires a commercial agreement"}"#.to_vec(),
                retry_after: None,
            },
            Response { status: 204, body: Vec::new(), retry_after: None },
        ]);
        let client = signed_in(http.clone(), None);
        assert_eq!(client.search("plexi", 1), Err(ApiError::RateLimited(Some(30))));
        assert_eq!(
            client.search("5150 & co", 2),
            Err(ApiError::Forbidden("search requires a commercial agreement".into()))
        );
        assert!(http.requests()[1].url.ends_with("/tones/search?query=5150%20%26%20co&page=2&page_size=25"));
        client.set_favorite(4711, false).unwrap();
        assert_eq!(http.requests()[2].method, "DELETE");
        assert!(http.requests()[2].url.ends_with("/tones/4711/favorite"));
    }

    #[test]
    fn downloads_with_the_users_token() {
        let http = Recorded::with([(200, r#"{"architecture": "WaveNet"}"#)]);
        let client = signed_in(http.clone(), None);
        let model = Model { id: 1, model_url: "https://cdn.t3k.test/m/1.nam".into(), ..Model::default() };
        assert_eq!(client.download(&model).unwrap(), br#"{"architecture": "WaveNet"}"#);
        assert_eq!(http.requests()[0].url, "https://cdn.t3k.test/m/1.nam");
        assert_eq!(http.requests()[0].bearer.as_deref(), Some("old"));
    }

    /// Models as the live API sends them: UUID user ids, nulls for unset fields, numbers as
    /// strings, and pages spread over two requests.
    #[test]
    fn reads_models_shaped_like_live_responses() {
        let first = r#"{"data":[
            {"id":"501","created_at":"2026-01-02T03:04:05.000Z","updated_at":null,
             "user_id":"913292c0-f036-4690-916d-043924eed513","model_url":"https://cdn.t3k.test/501.nam",
             "name":"Clean ch","size":"standard","tone_id":"77","architecture_version":2,
             "content_hash":null,"metadata":null,"private":false},
            {"id":502,"user_id":"913292c0-f036-4690-916d-043924eed513","model_url":"https://cdn.t3k.test/502.nam",
             "name":null,"size":null,"tone_id":77,"architecture_version":null,"content_hash":12345}],
            "page":"1","page_size":"2","total":"3","total_pages":"2"}"#;
        let second = r#"{"data":[{"id":503,"tone_id":77,"name":"Lead (nano)","size":"nano",
            "architecture_version":"custom","model_url":"https://cdn.t3k.test/503.nam"}],
            "page":2,"page_size":2,"total":3,"total_pages":2.0}"#;
        let http = Recorded::with([(200, first), (200, second)]);
        let models = signed_in(http.clone(), None).models(77).unwrap();
        assert_eq!(models.iter().map(|m| m.id).collect::<Vec<_>>(), [501, 502, 503]);
        assert_eq!((models[0].tone_id, models[0].architecture().as_deref()), (77, Some("2")));
        assert_eq!((models[1].name.as_str(), models[1].size.as_deref(), models[1].architecture()), ("", None, None));
        assert_eq!(models[1].content_hash.as_deref(), Some("12345"));
        assert_eq!(models[2].architecture().as_deref(), Some("custom"));
        assert!(http.requests()[1].url.ends_with("/models?tone_id=77&page=2&page_size=100"));

        let tone: Tone = serde_json::from_str(
            r#"{"id":"77","title":"Plexi","description":null,"gear":"amp","license":null,"models_count":"3",
                "downloads_count":1520,"favorites_count":null,"is_favorite":0,"sizes":["standard","nano"],
                "user":{"id":"913292c0-f036-4690-916d-043924eed513","username":"capt"}}"#,
        )
        .unwrap();
        assert_eq!((tone.id, tone.models_count, tone.downloads_count), (77, Some(3), Some(1520)));
        assert_eq!((tone.favorites_count, tone.is_favorite), (None, Some(false)));
    }

    #[test]
    fn an_unreadable_response_is_reported_and_logged_without_the_token() {
        let dir = scratch("debuglog");
        let log = dir.join("nam/tone3000-debug.log");
        let http = Recorded::with([(200, r#"{"data":[{"id":{"nested":true}}]}"#)]);
        let client = signed_in(http, None).with_debug_log(log.clone());
        let Err(ApiError::Decode(why)) = client.models(5) else { panic!("a decode error") };
        assert!(why.contains("tone3000-debug.log"), "{why}");
        let written = std::fs::read_to_string(&log).unwrap();
        assert!(written.contains("GET /models?tone_id=5&page=1&page_size=100"), "{written}");
        assert!(written.contains(r#"{"nested":true}"#));
        assert!(!written.contains("old"), "the access token stays out of the log");
        std::fs::remove_dir_all(dir).ok();
    }
}
