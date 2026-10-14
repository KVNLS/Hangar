//! Agents driven over the Agent Client Protocol (JSON-RPC 2.0 on the agent's stdio).

mod adapter;
pub mod approval;
mod client;
pub mod protocol;
pub mod subagents;
pub mod usage;

pub use adapter::{AcpAdapter, AcpSession, acp_presets};
pub use client::{AcpEvent, PermissionRequest, RequestId};
pub use protocol::McpServer;
