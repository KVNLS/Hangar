//! Projects, workspaces, sessions, event bus and SQLite persistence.

mod model;
mod slug;

pub use model::{
    AgentKind, Project, ProjectId, Session, SessionId, SessionStatus, Timestamp, Workspace,
    WorkspaceId, WorkspaceStatus,
};
pub use slug::slugify;
