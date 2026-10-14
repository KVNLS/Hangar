//! The domain: projects, workspaces, sessions, agent state, device vocabulary and typed
//! setting keys. Pure data and rules; persistence lives in `hangar-store-sqlite`.

mod agent_state;
pub mod device;
mod model;
mod setting;
mod slug;

pub use agent_state::{AcpTurn, AgentState};
pub use model::{
    AgentKind, Project, ProjectId, Session, SessionId, SessionStatus, Timestamp, Workspace,
    WorkspaceId, WorkspaceStatus,
};
pub use setting::Setting;
pub use slug::slugify;
