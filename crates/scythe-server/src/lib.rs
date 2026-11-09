//! Serving a Scythe session to other programs: the control API over HTTP and WebSocket, and
//! running a session with no window at all (`scythe-server`, `scythe --headless`).

pub mod config;
pub mod headless;
pub mod server;
pub mod web;

pub use server::{ControlServer, ServerConfig};

/// Where the control API listens unless told otherwise: this machine only.
pub const DEFAULT_ADDR: &str = "127.0.0.1:8770";
