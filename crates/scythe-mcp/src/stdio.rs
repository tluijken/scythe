//! MCP's stdio transport: one JSON-RPC message per line in, one answer per line out.

use std::io::{self, BufRead, Write};

use serde_json::Value;

use crate::protocol::{self, McpServer};

/// Answers messages from `input` on `output` until `input` ends.
pub fn serve(server: &McpServer, input: impl BufRead, mut output: impl Write) -> io::Result<()> {
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
            writeln!(output, "{answer}")?;
            output.flush()?;
        }
    }
    Ok(())
}
