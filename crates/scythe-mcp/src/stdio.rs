//! MCP's stdio transport: one JSON-RPC message per line in, one answer per line out, and the
//! server's notifications in between.

use std::io::{self, BufRead, Write};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::protocol::{self, McpServer};

/// Answers messages from `input` on `output` until `input` ends, which ends the client's
/// session.
pub fn serve(server: &McpServer, input: impl BufRead, output: impl Write + Send + 'static) -> io::Result<()> {
    let output = Arc::new(Mutex::new(output));
    let out = output.clone();
    server.set_notifier(Arc::new(move |message: &Value| {
        let mut o = out.lock().unwrap();
        let _ = writeln!(o, "{message}").and_then(|()| o.flush());
    }));
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let answer = match serde_json::from_str::<Value>(&line) {
            Ok(message) => server.handle(message),
            Err(e) => Some(protocol::parse_error(e)),
        };
        if let Some(answer) = answer {
            let mut o = output.lock().unwrap();
            writeln!(o, "{answer}")?;
            o.flush()?;
        }
    }
    server.end_default_client();
    Ok(())
}
