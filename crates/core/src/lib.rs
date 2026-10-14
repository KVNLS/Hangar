//! Projects, workspaces, sessions, event bus and SQLite persistence.

mod agent_state;
mod bus;
mod model;
mod slug;
mod store;

pub use agent_state::{AcpTurn, AgentState};
pub use bus::{DEFAULT_BUS_CAPACITY, Event, EventBus};
pub use model::{
    AgentKind, Project, ProjectId, Session, SessionId, SessionStatus, Timestamp, Workspace,
    WorkspaceId, WorkspaceStatus,
};
pub use slug::slugify;
pub use store::{SCHEMA_VERSION, Store, StoreError, default_db_path};
