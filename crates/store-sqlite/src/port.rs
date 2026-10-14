use hangar_core::changes::{Cost, DiffStat, UsageEntry};
use hangar_core::ports::{PortError, PortResult};
use hangar_core::{
    Project, ProjectId, Session, SessionId, SessionStatus, Timestamp, Workspace, WorkspaceId,
    WorkspaceStatus,
};

use crate::Store;

impl hangar_core::ports::Store for Store {
    fn insert_project(&self, project: &Project) -> PortResult<()> {
        Store::insert_project(self, project).map_err(PortError::new)
    }

    fn list_projects(&self) -> PortResult<Vec<Project>> {
        Store::list_projects(self).map_err(PortError::new)
    }

    fn get_project(&self, id: ProjectId) -> PortResult<Project> {
        Store::get_project(self, id).map_err(PortError::new)
    }

    fn delete_project(&self, id: ProjectId) -> PortResult<()> {
        Store::delete_project(self, id).map_err(PortError::new)
    }

    fn insert_workspace(&self, workspace: &Workspace) -> PortResult<()> {
        Store::insert_workspace(self, workspace).map_err(PortError::new)
    }

    fn list_workspaces(&self, project: ProjectId) -> PortResult<Vec<Workspace>> {
        Store::list_workspaces(self, project).map_err(PortError::new)
    }

    fn get_workspace(&self, id: WorkspaceId) -> PortResult<Workspace> {
        Store::get_workspace(self, id).map_err(PortError::new)
    }

    fn update_workspace_status(&self, id: WorkspaceId, status: WorkspaceStatus) -> PortResult<()> {
        Store::update_workspace_status(self, id, status).map_err(PortError::new)
    }

    fn delete_workspace(&self, id: WorkspaceId) -> PortResult<()> {
        Store::delete_workspace(self, id).map_err(PortError::new)
    }

    fn insert_session(&self, session: &Session) -> PortResult<()> {
        Store::insert_session(self, session).map_err(PortError::new)
    }

    fn list_sessions(&self, workspace: WorkspaceId) -> PortResult<Vec<Session>> {
        Store::list_sessions(self, workspace).map_err(PortError::new)
    }

    fn update_session_status(&self, id: SessionId, status: &SessionStatus) -> PortResult<Session> {
        Store::update_session_status(self, id, status).map_err(PortError::new)
    }

    fn finish_session(
        &self,
        id: SessionId,
        status: &SessionStatus,
        changes: Option<DiffStat>,
    ) -> PortResult<Session> {
        Store::finish_session(self, id, status, changes).map_err(PortError::new)
    }

    fn list_sessions_since(&self, since: Timestamp) -> PortResult<Vec<Session>> {
        Store::list_sessions_since(self, since).map_err(PortError::new)
    }

    fn record_session_cost(&self, id: SessionId, cost: &Cost) -> PortResult<()> {
        Store::record_session_cost(self, id, cost).map_err(PortError::new)
    }

    fn record_usage(&self, entry: &UsageEntry) -> PortResult<()> {
        Store::record_usage(self, entry).map_err(PortError::new)
    }

    fn list_usage_since(&self, since: Timestamp) -> PortResult<Vec<UsageEntry>> {
        Store::list_usage_since(self, since).map_err(PortError::new)
    }

    fn read_setting(&self, key: &str) -> PortResult<Option<String>> {
        Store::read_setting(self, key).map_err(PortError::new)
    }

    fn write_setting(&self, key: &str, json: &str) -> PortResult<()> {
        Store::write_setting(self, key, json).map_err(PortError::new)
    }

    fn remove_setting(&self, key: &str) -> PortResult<()> {
        Store::remove_setting(self, key).map_err(PortError::new)
    }

    fn setting_keys(&self, prefix: &str) -> PortResult<Vec<String>> {
        Store::setting_keys(self, prefix).map_err(PortError::new)
    }
}
