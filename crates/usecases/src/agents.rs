//! Running many agents at once: the policies that decide when an agent may start and when the
//! user must be told that one waits on them. Pure state machines; the caller counts live agents
//! and applies the decisions.

mod attention;
mod queue;

pub use attention::{Attention, Observed};
pub use queue::{
    Admission, AgentQueue, DEFAULT_MAX_AGENTS, MAX_AGENTS_RANGE, StartRequest, clamp_limit,
};
