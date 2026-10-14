//! The domain: projects, workspaces, sessions, agent state, device vocabulary and typed
//! setting keys. Pure data and rules; persistence lives in `hangar-store-sqlite`.

mod agent_state;
pub mod changes;
pub mod device;
mod model;
pub mod ports;
pub mod review;
mod setting;
mod slug;

pub use agent_state::{AcpTurn, AgentState};
pub use model::{
    AgentKind, Project, ProjectId, Session, SessionId, SessionStatus, Timestamp, Workspace,
    WorkspaceGroupId, WorkspaceId, WorkspaceStatus,
};
pub use setting::Setting;
pub use slug::{BRANCH_PREFIX, branch_name, slugify, unique_slug, worktree_path};
