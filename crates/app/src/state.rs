//! UI-independent state: the project tree and which workspace each pane shows.

use hangar_core::{Project, ProjectId, SessionStatus, Workspace, WorkspaceId};

pub const PANE_COUNT: usize = 2;

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectEntry {
    pub project: Project,
    pub workspaces: Vec<Workspace>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectList {
    entries: Vec<ProjectEntry>,
}

impl ProjectList {
    pub fn new(entries: Vec<ProjectEntry>) -> Self {
        Self { entries }
    }

    pub fn entries(&self) -> &[ProjectEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Adds the project unless it is already listed.
    pub fn add_project(&mut self, project: Project) {
        if self.project(project.id).is_none() {
            self.entries.push(ProjectEntry {
                project,
                workspaces: Vec::new(),
            });
        }
    }

    /// Adds or replaces the workspace under its project. Returns `false` if the project is
    /// unknown.
    pub fn upsert_workspace(&mut self, workspace: Workspace) -> bool {
        let Some(entry) = self
            .entries
            .iter_mut()
            .find(|e| e.project.id == workspace.project_id)
        else {
            return false;
        };
        match entry.workspaces.iter_mut().find(|w| w.id == workspace.id) {
            Some(existing) => *existing = workspace,
            None => entry.workspaces.push(workspace),
        }
        true
    }

    pub fn project(&self, id: ProjectId) -> Option<&Project> {
        self.entries.iter().map(|e| &e.project).find(|p| p.id == id)
    }

    pub fn workspace(&self, id: WorkspaceId) -> Option<&Workspace> {
        self.entries
            .iter()
            .flat_map(|e| &e.workspaces)
            .find(|w| w.id == id)
    }
}

/// Which workspace each tiled pane shows, and which pane has focus.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Panes {
    slots: [Option<WorkspaceId>; PANE_COUNT],
    focused: usize,
}

impl Panes {
    pub fn slot(&self, pane: usize) -> Option<WorkspaceId> {
        self.slots.get(pane).copied().flatten()
    }

    pub fn focused(&self) -> usize {
        self.focused
    }

    pub fn focus(&mut self, pane: usize) {
        if pane < PANE_COUNT {
            self.focused = pane;
        }
    }

    /// Shows the workspace in the focused pane, or focuses the pane already showing it, so a
    /// terminal is never on screen twice. Returns the pane.
    pub fn show(&mut self, workspace: WorkspaceId) -> usize {
        if let Some(pane) = self.slots.iter().position(|s| *s == Some(workspace)) {
            self.focused = pane;
        } else {
            self.slots[self.focused] = Some(workspace);
        }
        self.focused
    }

    pub fn is_shown(&self, workspace: WorkspaceId) -> bool {
        self.slots.contains(&Some(workspace))
    }
}

/// Short status for pane headers and the sidebar.
pub fn status_label(status: &SessionStatus) -> String {
    match status {
        SessionStatus::Starting => "starting".to_owned(),
        SessionStatus::Running => "running".to_owned(),
        SessionStatus::Exited { code } => format!("exited {code}"),
        SessionStatus::Failed { message } => format!("failed: {message}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{Panes, ProjectList, status_label};
    use hangar_core::{Project, SessionStatus, Timestamp, Workspace, WorkspaceId, WorkspaceStatus};

    fn workspace(project: &Project, name: &str) -> Workspace {
        Workspace {
            id: WorkspaceId::new(),
            project_id: project.id,
            name: name.into(),
            slug: name.into(),
            branch: format!("hangar/{name}"),
            base: "main".into(),
            worktree_path: format!("/wt/{name}").into(),
            status: WorkspaceStatus::Active,
            created_at: Timestamp::now(),
        }
    }

    #[test]
    fn project_list_adds_and_updates() {
        let mut list = ProjectList::default();
        let project = Project::new("app", "/repo", "main");
        list.add_project(project.clone());
        list.add_project(project.clone());
        assert_eq!(list.entries().len(), 1);

        let mut ws = workspace(&project, "a");
        assert!(list.upsert_workspace(ws.clone()));
        ws.name = "renamed".into();
        assert!(list.upsert_workspace(ws.clone()));
        assert_eq!(list.entries()[0].workspaces.len(), 1);
        assert_eq!(list.workspace(ws.id).unwrap().name, "renamed");

        let orphan = workspace(&Project::new("other", "/o", "main"), "b");
        assert!(!list.upsert_workspace(orphan.clone()));
        assert!(list.workspace(orphan.id).is_none());
    }

    #[test]
    fn panes_show_in_focused_pane_without_duplicates() {
        let (a, b, c) = (WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new());
        let mut panes = Panes::default();
        assert_eq!(panes.show(a), 0);
        panes.focus(1);
        assert_eq!(panes.show(b), 1);
        assert_eq!((panes.slot(0), panes.slot(1)), (Some(a), Some(b)));

        // Already visible in pane 0: focus moves there instead of duplicating.
        assert_eq!(panes.show(a), 0);
        assert_eq!(panes.focused(), 0);
        assert_eq!(panes.slot(1), Some(b));

        assert_eq!(panes.show(c), 0);
        assert!(!panes.is_shown(a));
        assert!(panes.is_shown(c));

        panes.focus(7);
        assert_eq!(panes.focused(), 0);
    }

    #[test]
    fn status_labels() {
        assert_eq!(status_label(&SessionStatus::Running), "running");
        assert_eq!(status_label(&SessionStatus::Exited { code: 2 }), "exited 2");
        assert_eq!(
            status_label(&SessionStatus::Failed {
                message: "x".into()
            }),
            "failed: x"
        );
    }
}
