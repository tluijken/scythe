//! A TONE3000 stand-in on a loopback port, answering the way the real API does on a bad day:
//! nulls for empty fields, UUID user ids, numbers sent as strings, long descriptions, an
//! HTML error page from the proxy in front of it, and a model file Scythe can play.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

pub const TONE_AMP: i64 = 4711;
pub const TONE_IR: i64 = 815;
pub const TONE_ODD: i64 = 4712;

pub struct MockTone3000 {
    /// What Scythe's `base_url` is set to: `http://127.0.0.1:<port>/api/v1`.
    pub base: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl MockTone3000 {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let files = root.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let seen = seen.clone();
                let files = files.clone();
                std::thread::spawn(move || serve(stream, &files, &seen));
            }
        });
        Self { base: format!("{root}/api/v1"), requests }
    }

    /// Every request so far, as `METHOD /path?query`.
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    pub fn asked(&self, prefix: &str) -> bool {
        self.requests().iter().any(|r| r.starts_with(prefix))
    }
}

fn serve(stream: TcpStream, root: &str, seen: &Mutex<Vec<String>>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or("").to_owned(), parts.next().unwrap_or("").to_owned());
    let mut length = 0;
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).is_err() || header.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = header.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            length = v.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    let _ = reader.read_exact(&mut body);
    seen.lock().unwrap().push(format!("{method} {target}"));
    let (status, kind, body) = route(&method, &target, root);
    let mut out = stream;
    let head = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        if status < 400 { "OK" } else { "Error" },
        body.len()
    );
    let _ = out.write_all(head.as_bytes());
    let _ = out.write_all(&body);
}

const JSON: &str = "application/json";

fn route(method: &str, target: &str, root: &str) -> (u16, &'static str, Vec<u8>) {
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    let param = |name: &str| {
        query.split('&').find_map(|kv| kv.split_once('=').filter(|(k, _)| *k == name).map(|(_, v)| v.to_owned()))
    };
    let json = |v: Value| (200, JSON, v.to_string().into_bytes());
    match (method, path) {
        ("POST", "/oauth/token") => json(json!({
            "access_token": "mock-access", "refresh_token": "mock-refresh", "token_type": "bearer",
            "expires_in": 3600, "scope": "read"
        })),
        ("GET", "/user") => json(json!({
            "id": "6b0f4f3e-2f7c-4c55-9d2a-1b8e0c4d7a11", "username": "stagehand", "display_name": null,
            "avatar_url": null, "bio": null, "links": null, "url": "https://www.tone3000.com/stagehand"
        })),
        ("GET", "/tones/favorited") => json(page(tones(), param("page"))),
        ("GET", "/tones/trending") => {
            json(json!({ "data": null, "page": null, "page_size": null, "total": null, "total_pages": null }))
        }
        ("GET", "/tones/downloaded") => (502, "text/html", BAD_GATEWAY.as_bytes().to_vec()),
        ("GET", "/tones/created") => (200, "text/html", CHALLENGE.as_bytes().to_vec()),
        ("GET", p) if p.starts_with("/tones/") => {
            let id = p.trim_start_matches("/tones/");
            match tones().into_iter().find(|t| t["id"].to_string().trim_matches('"') == id) {
                Some(t) => json(t),
                None => (404, JSON, br#"{"error":"not_found","message":"No such tone"}"#.to_vec()),
            }
        }
        ("PUT" | "DELETE", p) if p.ends_with("/favorite") => json(json!({ "ok": true })),
        ("GET", "/models") => {
            let tone: i64 = param("tone_id").and_then(|t| t.parse().ok()).unwrap_or(0);
            json(page(models(tone, root), param("page")))
        }
        ("GET", p) if p.starts_with("/files/") => (200, "application/octet-stream", MODEL.as_bytes().to_vec()),
        _ => (404, JSON, br#"{"error":"not_found"}"#.to_vec()),
    }
}

fn page(data: Vec<Value>, page: Option<String>) -> Value {
    let total = data.len().to_string();
    json!({ "data": data, "page": page.unwrap_or_else(|| "1".into()), "page_size": "25",
            "total": total, "total_pages": 1 })
}

fn user(id: &str, name: &str) -> Value {
    json!({ "id": id, "username": name, "display_name": null, "is_verified": null, "avatar_url": null, "url": null })
}

fn tones() -> Vec<Value> {
    let long = "Captured through a Friedman BE-100 Deluxe, BE channel, gain at two o'clock, \
                bright switch on, into a Two Notes Torpedo load box. "
        .repeat(30);
    vec![
        json!({
            "id": TONE_AMP, "user_id": "6b0f4f3e-2f7c-4c55-9d2a-1b8e0c4d7a11",
            "user": user("6b0f4f3e-2f7c-4c55-9d2a-1b8e0c4d7a11", "toneking"),
            "title": "Friedman BE-100 Deluxe", "description": long, "gear": "amp", "images": null,
            "is_public": true, "links": null, "format": "nam", "license": "t3k",
            "sizes": ["standard", "lite", "nano"], "makes": [{ "id": "3", "name": "Friedman" }], "tags": null,
            "models_count": "3", "irs_count": null, "downloads_count": "15204", "favorites_count": 211,
            "is_favorite": "true", "url": "https://www.tone3000.com/tones/friedman-be-100-deluxe-4711",
            "created_at": "2025-03-01T10:00:00Z", "published_at": null
        }),
        json!({
            "id": TONE_IR.to_string(), "user_id": null, "user": null,
            "title": "Mesa OS 4x12 SM57", "description": null, "gear": "cab", "images": [],
            "format": "ir", "license": null, "sizes": null, "makes": null, "tags": [],
            "models_count": 0, "irs_count": 4, "downloads_count": 90, "favorites_count": null, "is_favorite": 1
        }),
        json!({
            "id": TONE_ODD, "user": user("0f9d2c1a-0000-4000-8000-00000000beef", "fuzzhead"),
            "title": "Tone Bender MkII into a Plexi", "description": "",
            "gear": "pedal", "format": "nam", "sizes": ["feather"], "makes": [],
            "models_count": 1, "downloads_count": 3.0, "favorites_count": "7", "is_favorite": false
        }),
    ]
}

fn models(tone: i64, root: &str) -> Vec<Value> {
    let model = |id: i64, name: &str, size: Value| {
        json!({
            "id": id, "tone_id": tone.to_string(), "user_id": "6b0f4f3e-2f7c-4c55-9d2a-1b8e0c4d7a11",
            "name": name, "size": size, "model_url": format!("{root}/files/{id}.nam"),
            "architecture_version": 2, "content_hash": null, "metadata": null
        })
    };
    match tone {
        TONE_AMP => vec![
            model(90001, "BE ch, gain 2 o'clock", json!("standard")),
            model(90002, "BE ch, gain 2 o'clock (lite)", json!("lite")),
            model(90003, "", Value::Null),
        ],
        TONE_ODD => vec![model(90100, "Fuzz full", json!("feather"))],
        _ => Vec::new(),
    }
}

/// A WaveNet small enough to read in a moment: one layer that passes its input through.
const MODEL: &str = r#"{"version": "0.5.4", "architecture": "WaveNet", "sample_rate": 48000,
  "config": {"layers": [{"input_size": 1, "condition_size": 1, "head_size": 1, "channels": 1,
    "kernel_size": 2, "dilations": [1], "activation": "Hardtanh", "gated": false, "head_bias": true}],
    "head_scale": 1.0},
  "weights": [1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0]}"#;

const BAD_GATEWAY: &str = "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
<title>tone3000.com | 502: Bad gateway</title>\n<style>body{font-family:system-ui;margin:0;padding:0}\
.cf-error-details{max-width:60rem}</style>\n</head>\n<body>\n<div id=\"cf-wrapper\"><h1>Bad gateway</h1>\
<p>The web server reported a bad gateway error.</p></div>\n</body>\n</html>\n";

const CHALLENGE: &str = "<!DOCTYPE html><html><head><title>Just a moment...</title></head>\
<body><noscript>Enable JavaScript and cookies to continue</noscript></body></html>";
