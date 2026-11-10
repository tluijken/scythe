//! Headless Chromium driven over the Chrome DevTools Protocol.

use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{Proc, find_tool, free_port, request};

pub struct Browser {
    proc: Proc,
    page: tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
    next: u64,
    /// Exceptions thrown in the page and `console.error` messages, as text.
    pub errors: Vec<String>,
}

impl Browser {
    /// Starts headless Chromium with a profile in `dir`, or `None` (saying the test is
    /// skipped) when there is none.
    pub fn start(dir: &Path, test: &str) -> Option<Self> {
        let Some(chromium) = find_tool("SCYTHE_CHROMIUM", "chromium") else {
            eprintln!("SKIPPED {test}: no chromium on PATH and SCYTHE_CHROMIUM not set");
            return None;
        };
        let port = free_port();
        let profile = dir.join("chromium-profile");
        let args = [
            "--headless=new".to_owned(),
            format!("--remote-debugging-port={port}"),
            format!("--user-data-dir={}", profile.display()),
            "--no-first-run".to_owned(),
            "--no-default-browser-check".to_owned(),
            "--disable-gpu".to_owned(),
            "--disable-extensions".to_owned(),
            "--window-size=1280,900".to_owned(),
            "about:blank".to_owned(),
        ];
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let mut proc = Proc::spawn_tool(dir, "chromium", &chromium, &args);
        let devtools: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let start = Instant::now();
        let ws_url = loop {
            assert!(proc.alive(), "chromium ended:\n{}", proc.tail());
            assert!(start.elapsed() < Duration::from_secs(30), "no DevTools endpoint:\n{}", proc.tail());
            if TcpStream::connect(devtools).is_ok() {
                let (status, _, body) = request(devtools, "GET", "/json/list", &[], None);
                let list: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let page = list.as_array().and_then(|l| l.iter().find(|t| t["type"] == "page"));
                if let (200, Some(url)) = (status, page.and_then(|p| p["webSocketDebuggerUrl"].as_str())) {
                    break url.to_owned();
                }
            }
            std::thread::sleep(Duration::from_millis(100));
        };
        let (mut page, _) = tungstenite::connect(ws_url).expect("DevTools WebSocket");
        if let tungstenite::stream::MaybeTlsStream::Plain(s) = page.get_mut() {
            s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        }
        let mut browser = Self { proc, page, next: 0, errors: Vec::new() };
        browser.call("Runtime.enable", json!({}));
        browser.call("Page.enable", json!({}));
        Some(browser)
    }

    fn note(&mut self, message: &Value) {
        match message["method"].as_str() {
            Some("Runtime.exceptionThrown") => {
                let d = &message["params"]["exceptionDetails"];
                let text = d["exception"]["description"].as_str().or(d["text"].as_str()).unwrap_or("exception");
                self.errors.push(text.to_owned());
            }
            Some("Runtime.consoleAPICalled") if message["params"]["type"] == "error" => {
                let args = message["params"]["args"].as_array().cloned().unwrap_or_default();
                let text: Vec<String> = args
                    .iter()
                    .map(|a| a["value"].as_str().map(str::to_owned).unwrap_or_else(|| a["description"].to_string()))
                    .collect();
                self.errors.push(format!("console.error: {}", text.join(" ")));
            }
            _ => {}
        }
    }

    /// Sends a DevTools command and waits for its answer.
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        let message = json!({"id": id, "method": method, "params": params});
        self.page.send(tungstenite::Message::text(message.to_string())).expect("DevTools send");
        let start = Instant::now();
        loop {
            assert!(start.elapsed() < Duration::from_secs(30), "no answer to {method}:\n{}", self.proc.tail());
            let m = match self.page.read() {
                Ok(tungstenite::Message::Text(t)) => serde_json::from_str::<Value>(t.as_str()).unwrap(),
                Ok(_) => continue,
                Err(tungstenite::Error::Io(e))
                    if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
                {
                    continue;
                }
                Err(e) => panic!("DevTools: {e}"),
            };
            if m["id"] == id {
                assert!(m["error"].is_null(), "{method}: {}", m["error"]);
                return m["result"].clone();
            }
            self.note(&m);
        }
    }

    pub fn navigate(&mut self, url: &str) {
        self.call("Page.navigate", json!({"url": url}));
    }

    /// Evaluates `expression` in the page (awaiting a promise) and returns its value.
    pub fn eval(&mut self, expression: &str) -> Value {
        self.try_eval(expression).unwrap_or_else(|e| panic!("{expression} threw {e}"))
    }

    /// [`Self::eval`], with what the expression threw as the error.
    pub fn try_eval(&mut self, expression: &str) -> Result<Value, Value> {
        let r = self
            .call("Runtime.evaluate", json!({"expression": expression, "awaitPromise": true, "returnByValue": true}));
        match r.get("exceptionDetails") {
            Some(e) => Err(e.clone()),
            None => Ok(r["result"]["value"].clone()),
        }
    }

    /// Evaluates `expression` until it is true.
    #[track_caller]
    pub fn wait_for(&mut self, what: &str, expression: &str) {
        let start = Instant::now();
        // While the page is still loading the expression may throw (no body yet): not true yet.
        while self.try_eval(expression).ok() != Some(Value::Bool(true)) {
            let body = self.eval("document.body ? document.body.innerText.slice(0, 400) : ''");
            assert!(start.elapsed() < Duration::from_secs(20), "timed out waiting for {what}; page shows {body}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}
