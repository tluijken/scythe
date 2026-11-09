use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use serde_json::Value;

/// A minimal HTTP/1.1 exchange: the status code and the JSON body.
pub fn http(addr: SocketAddr, method: &str, path: &str, token: Option<&str>, body: Option<&Value>) -> (u16, Value) {
    let auth = token.map(|t| format!("Bearer {t}"));
    let headers: Vec<(&str, &str)> = auth.iter().map(|a| ("Authorization", a.as_str())).collect();
    request(addr, method, path, &headers, body)
}

/// [`http`] with any headers.
pub fn request(
    addr: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
) -> (u16, Value) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let headers: String = headers.iter().map(|(name, value)| format!("{name}: {value}\r\n")).collect();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\n{headers}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, rest) = response.split_once("\r\n\r\n").expect("an HTTP response");
    let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).expect("a status code");
    let chunked = head.to_ascii_lowercase().contains("transfer-encoding: chunked");
    let text = if chunked { dechunk(rest) } else { rest.to_owned() };
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn dechunk(mut rest: &str) -> String {
    let mut out = String::new();
    while let Some((size, tail)) = rest.split_once("\r\n") {
        let n = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
        if n == 0 {
            break;
        }
        out.push_str(&tail[..n]);
        rest = &tail[n + 2..];
    }
    out
}

/// A GET with extra request headers: the status, the response head (lower-cased) and the body.
#[allow(dead_code, reason = "each test binary uses only some of the helpers")]
pub fn get_raw(addr: SocketAddr, path: &str, headers: &str) -> (u16, String, Vec<u8>) {
    let mut stream = TcpStream::connect(addr).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\n{headers}Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    let split = response.windows(4).position(|w| w == b"\r\n\r\n").expect("an HTTP response");
    let head = String::from_utf8_lossy(&response[..split]).to_ascii_lowercase();
    let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).expect("a status code");
    let body = response[split + 4..].to_vec();
    let body = if head.contains("transfer-encoding: chunked") {
        dechunk(&String::from_utf8_lossy(&body)).into_bytes()
    } else {
        body
    };
    (status, head, body)
}
