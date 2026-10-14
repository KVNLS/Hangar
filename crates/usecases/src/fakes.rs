//! Hand-written test doubles for the ports: in-memory, recording what they are asked to do.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use hangar_core::changes::{Cost, DiffStat, UsageEntry};
use hangar_core::ports::{
    BranchTip, ChangeCounts, ListedWorktree, NewWorktree, PortError, PortResult, Store,
    VersionControl,
};
use hangar_core::{
    Project, ProjectId, Session, SessionId, SessionStatus, Timestamp, Workspace, WorkspaceId,
    WorkspaceStatus,
};

#[derive(Default)]
pub struct FakeStore {
    pub projects: Mutex<Vec<Project>>,
    pub workspaces: Mutex<Vec<Workspace>>,
    pub sessions: Mutex<Vec<Session>>,
    pub settings: Mutex<HashMap<String, String>>,
    pub usage: Mutex<Vec<UsageEntry>>,
    /// `insert_workspace` fails.
    pub refuse_workspaces: bool,
    /// `insert_session` fails.
    pub refuse_sessions: bool,
}

fn missing(what: &str) -> PortError {
    PortError::new(format!("{what} not found"))
}

impl Store for FakeStore {
    fn insert_project(&self, project: &Project) -> PortResult<()> {
        self.projects.lock().unwrap().push(project.clone());
        Ok(())
    }
    fn list_projects(&self) -> PortResult<Vec<Project>> {
        Ok(self.projects.lock().unwrap().clone())
    }
    fn get_project(&self, id: ProjectId) -> PortResult<Project> {
        let projects = self.projects.lock().unwrap();
        projects
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| missing("project"))
    }
    fn delete_project(&self, id: ProjectId) -> PortResult<()> {
        self.projects.lock().unwrap().retain(|p| p.id != id);
        Ok(())
    }
    fn insert_workspace(&self, workspace: &Workspace) -> PortResult<()> {
        if self.refuse_workspaces {
            return Err(PortError::new("disk full"));
        }
        self.workspaces.lock().unwrap().push(workspace.clone());
        Ok(())
    }
    fn list_workspaces(&self, project: ProjectId) -> PortResult<Vec<Workspace>> {
        let workspaces = self.workspaces.lock().unwrap();
        Ok(workspaces
            .iter()
            .filter(|w| w.project_id == project)
            .cloned()
            .collect())
    }
    fn get_workspace(&self, id: WorkspaceId) -> PortResult<Workspace> {
        let workspaces = self.workspaces.lock().unwrap();
        workspaces
            .iter()
            .find(|w| w.id == id)
            .cloned()
            .ok_or_else(|| missing("workspace"))
    }
    fn update_workspace_status(&self, id: WorkspaceId, status: WorkspaceStatus) -> PortResult<()> {
        let mut workspaces = self.workspaces.lock().unwrap();
        let ws = workspaces
            .iter_mut()
            .find(|w| w.id == id)
            .ok_or_else(|| missing("workspace"))?;
        ws.status = status;
        Ok(())
    }
    fn delete_workspace(&self, id: WorkspaceId) -> PortResult<()> {
        self.workspaces.lock().unwrap().retain(|w| w.id != id);
        Ok(())
    }
    fn insert_session(&self, session: &Session) -> PortResult<()> {
        if self.refuse_sessions {
            return Err(PortError::new("disk full"));
        }
        self.sessions.lock().unwrap().push(session.clone());
        Ok(())
    }
    fn list_sessions(&self, workspace: WorkspaceId) -> PortResult<Vec<Session>> {
        let sessions = self.sessions.lock().unwrap();
        Ok(sessions
            .iter()
            .filter(|s| s.workspace_id == workspace)
            .cloned()
            .collect())
    }
    fn update_session_status(&self, id: SessionId, status: &SessionStatus) -> PortResult<Session> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| missing("session"))?;
        session.status = status.clone();
        if status.is_terminal() && session.ended_at.is_none() {
            session.ended_at = Some(Timestamp::now());
        }
        Ok(session.clone())
    }
    fn finish_session(
        &self,
        id: SessionId,
        status: &SessionStatus,
        changes: Option<DiffStat>,
    ) -> PortResult<Session> {
        let updated = self.update_session_status(id, status)?;
        if changes.is_none() {
            return Ok(updated);
        }
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| missing("session"))?;
        session.changes = changes;
        Ok(session.clone())
    }
    fn list_sessions_since(&self, since: Timestamp) -> PortResult<Vec<Session>> {
        let mut ended: Vec<Session> = self
            .sessions
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.ended_at.is_some_and(|at| at >= since))
            .cloned()
            .collect();
        ended.sort_by_key(|s| std::cmp::Reverse(s.ended_at));
        Ok(ended)
    }
    fn record_session_cost(&self, id: SessionId, cost: &Cost) -> PortResult<()> {
        let mut sessions = self.sessions.lock().unwrap();
        let session = sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| missing("session"))?;
        session.cost = Some(cost.clone());
        Ok(())
    }
    fn record_usage(&self, entry: &UsageEntry) -> PortResult<()> {
        self.usage.lock().unwrap().push(entry.clone());
        Ok(())
    }
    fn list_usage_since(&self, since: Timestamp) -> PortResult<Vec<UsageEntry>> {
        let mut rows: Vec<UsageEntry> = self
            .usage
            .lock()
            .unwrap()
            .iter()
            .filter(|e| e.at >= since)
            .cloned()
            .collect();
        rows.sort_by_key(|e| e.at);
        Ok(rows)
    }
    fn read_setting(&self, key: &str) -> PortResult<Option<String>> {
        Ok(self.settings.lock().unwrap().get(key).cloned())
    }
    fn write_setting(&self, key: &str, json: &str) -> PortResult<()> {
        self.settings
            .lock()
            .unwrap()
            .insert(key.to_owned(), json.to_owned());
        Ok(())
    }
    fn remove_setting(&self, key: &str) -> PortResult<()> {
        self.settings.lock().unwrap().remove(key);
        Ok(())
    }
}

/// Records every change it is asked to make.
#[derive(Default)]
pub struct FakeGit {
    pub branches: Mutex<HashSet<String>>,
    pub listed: Vec<ListedWorktree>,
    pub calls: Mutex<Vec<String>>,
}

impl FakeGit {
    pub fn with_branches(names: &[&str]) -> Self {
        let git = Self::default();
        git.branches
            .lock()
            .unwrap()
            .extend(names.iter().map(|n| (*n).to_owned()));
        git
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
}

impl VersionControl for FakeGit {
    fn repo_root(&self, path: &Path) -> PortResult<PathBuf> {
        Ok(path.to_path_buf())
    }
    fn default_branch(&self, _: &Path) -> PortResult<Option<String>> {
        Ok(Some("main".to_owned()))
    }
    fn is_valid_branch_name(&self, _: &Path, name: &str) -> PortResult<bool> {
        Ok(!name.contains(' '))
    }
    fn branch_exists(&self, _: &Path, name: &str) -> PortResult<bool> {
        Ok(self.branches.lock().unwrap().contains(name))
    }
    fn local_branches(&self, _: &Path) -> PortResult<Vec<String>> {
        let mut names: Vec<String> = self.branches.lock().unwrap().iter().cloned().collect();
        names.sort();
        Ok(names)
    }
    fn branch_tips(&self, repo: &Path) -> PortResult<Vec<BranchTip>> {
        Ok(self
            .local_branches(repo)?
            .into_iter()
            .map(|name| BranchTip {
                name,
                short_sha: "0000000".to_owned(),
                committed_at: 0,
            })
            .collect())
    }
    fn add_worktree(&self, _: &Path, worktree: &NewWorktree) -> PortResult<PathBuf> {
        self.record(format!("add {} from {}", worktree.branch, worktree.base));
        self.branches
            .lock()
            .unwrap()
            .insert(worktree.branch.clone());
        Ok(worktree.path.clone())
    }
    fn worktrees(&self, _: &Path) -> PortResult<Vec<ListedWorktree>> {
        Ok(self.listed.clone())
    }
    fn remove_worktree(&self, _: &Path, path: &Path, force: bool) -> PortResult<()> {
        self.record(format!("remove {} force={force}", path.display()));
        Ok(())
    }
    fn prune_worktrees(&self, _: &Path) -> PortResult<()> {
        Ok(())
    }
    fn delete_branch(&self, _: &Path, branch: &str, force: bool) -> PortResult<()> {
        self.record(format!("delete-branch {branch} force={force}"));
        self.branches.lock().unwrap().remove(branch);
        Ok(())
    }
    fn change_counts(&self, _: &Path) -> PortResult<ChangeCounts> {
        Ok(ChangeCounts::default())
    }
    fn commits_ahead(&self, _: &Path, _: &str) -> PortResult<u32> {
        Ok(0)
    }
}
