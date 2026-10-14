//! MCP device tools for agents: each Hangar workspace exposes its device (simulator or emulator)
//! to the agent running in its terminal as an MCP server. When the user allows it, the same
//! server offers orchestration tools that create and drive child workspaces.
//!
//! # Architecture
//!
//! ```text
//! agent ──stdio (MCP, JSON lines)──▶ `Hangar mcp-bridge` ──Unix socket──▶ Hangar app
//!                                      run_bridge()        {session, message}   serve() → handle()
//!                                                                              → ToolHost::call()
//! ```
//!
//! - [`run_bridge`] is the agent-side process: the agent launches the Hangar binary in bridge
//!   mode as a stdio MCP server. It wraps every JSON-RPC line in an envelope
//!   `{"session": "...", "message": ...}` and copies the app's plain JSON-RPC response lines back.
//!   When the app is unreachable it answers each request with an explanatory JSON-RPC error.
//! - [`bind`] + [`serve`] run in the app: one thread per connection and per message, each message
//!   answered by [`handle`], which implements the MCP subset (`initialize`, `ping`, `tools/list`,
//!   `tools/call`) and calls into the app's [`ToolHost`]. The engine is stateless.
//!
//! # Security model
//!
//! - The socket is created with mode 0600 (put it in a 0700 directory), so only the user's own
//!   processes can connect.
//! - Each workspace has its own random session secret ([`new_session_secret`], plain hex), handed
//!   to its agent through the `HANGAR_MCP_SESSION` environment variable (the socket path goes in
//!   `HANGAR_MCP_SOCKET`). Every message carries it and the host resolves it to that workspace's
//!   device only, so an agent cannot drive another workspace's device. Unknown secrets get
//!   JSON-RPC error `-32001`.
//! - The variable names avoid TOKEN/SECRET/KEY/AUTH-like words and the value is not JWT-shaped:
//!   some agent CLIs (Gemini CLI) strip such variables from the environment of MCP servers.
//! - This crate never writes session secrets to disk or logs them.

mod base64;
mod bridge;
pub mod hook;
mod orchestration;
mod protocol;
mod screenshot;
mod server;
mod tools;

pub use bridge::{run_bridge, run_bridge_from_env};
pub use orchestration::{ChildWorkspace, CreatedWorkspace, StartOutcome, TranscriptEntry};
pub use protocol::{
    AGENT_SIGNAL_METHOD, INVALID_PARAMS, INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR,
    SUPPORTED_PROTOCOL_VERSIONS, ToolHost, UNAVAILABLE, UNKNOWN_SESSION, handle,
};
pub use screenshot::{MAX_SIDE, Screenshot, ScreenshotError, encode_screenshot};
pub use server::{bind, handle_envelope, serve, serve_connection};
pub use tools::{
    Button, Content, DEFAULT_LOG_LINES, DEFAULT_SWIPE_MS, DEFAULT_TRANSCRIPT_MESSAGES,
    DeviceStatus, MAX_LOG_LINES, MAX_SWIPE_MS, MAX_TRANSCRIPT_MESSAGES, ParseError, Press, Tool,
    ToolError, ToolOutput, definitions,
};

/// Environment variable carrying the workspace's session secret to the bridge.
pub const SESSION_ENV: &str = "HANGAR_MCP_SESSION";
/// Environment variable carrying the app's socket path to the bridge.
pub const SOCKET_ENV: &str = "HANGAR_MCP_SOCKET";

/// A fresh session secret: 64 lowercase hex characters, 244 of their bits random.
pub fn new_session_secret() -> String {
    let (a, b) = (uuid::Uuid::new_v4(), uuid::Uuid::new_v4());
    format!("{}{}", a.simple(), b.simple())
}

#[cfg(test)]
mod tests {
    #[test]
    fn session_secret_is_plain_hex() {
        let s = super::new_session_secret();
        assert_eq!(s.len(), 64);
        assert!(
            s.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_ne!(s, super::new_session_secret());
    }
}
