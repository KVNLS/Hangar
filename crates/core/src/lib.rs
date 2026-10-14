//! Projects, workspaces, sessions, agent state and SQLite persistence.

mod agent_state;
mod model;
mod setting;
mod slug;
mod store;

pub use agent_state::{AcpTurn, AgentState};
pub use model::{
    AgentKind, Project, ProjectId, Session, SessionId, SessionStatus, Timestamp, Workspace,
    WorkspaceId, WorkspaceStatus,
};
pub use setting::Setting;
pub use slug::slugify;
pub use store::{SCHEMA_VERSION, Store, StoreError, default_db_path};
