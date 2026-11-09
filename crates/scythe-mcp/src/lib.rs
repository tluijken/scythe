//! The Model Context Protocol for Scythe (see `docs/mcp.md`): AI agents drive a session
//! through tools made from the control API's commands. [`McpServer`] answers the JSON-RPC
//! messages; the `scythe-mcp` binary serves it over stdio and `scythe-server` over Streamable
//! HTTP at `/mcp`.

pub mod backend;
pub mod prompts;
pub mod protocol;
pub mod session;
pub mod stdio;
pub mod summary;
pub mod tools;

pub use backend::{Backend, Local, Remote};
pub use protocol::{McpServer, Notifier, PROTOCOL_VERSIONS};
pub use tools::Selection;
