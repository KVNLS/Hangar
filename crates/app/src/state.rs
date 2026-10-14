//! UI-independent state: the project tree and which workspace each pane shows.

use hangar_core::{Project, ProjectId, SessionStatus, Workspace, WorkspaceId, WorkspaceStatus};
use serde::{Deserialize, Serialize};

pub const PANE_COUNT: usize = 2;

/// Smallest and largest share of the width the first pane may be restored to.
const SPLIT_RANGE: (f32, f32) = (0.1, 0.9);

#[derive(Clone, Debug, PartialEq)]
pub struct ProjectEntry {
    pub project: Project,
    /// Active and archived workspaces, in creation order.
    pub workspaces: Vec<Workspace>,
}

impl ProjectEntry {
    pub fn active(&self) -> impl Iterator<Item = &Workspace> {
        self.with_status(WorkspaceStatus::Active)
    }

    pub fn archived(&self) -> impl Iterator<Item = &Workspace> {
        self.with_status(WorkspaceStatus::Archived)
    }

    fn with_status(&self, status: WorkspaceStatus) -> impl Iterator<Item = &Workspace> {
        self.workspaces.iter().filter(move |w| w.status == status)
    }
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

    pub fn entry(&self, id: ProjectId) -> Option<&ProjectEntry> {
        self.entries.iter().find(|e| e.project.id == id)
    }

    pub fn workspace(&self, id: WorkspaceId) -> Option<&Workspace> {
        self.entries
            .iter()
            .flat_map(|e| &e.workspaces)
            .find(|w| w.id == id)
    }

    /// Whether the workspace exists and is active (shown in the main list and in panes).
    pub fn is_active(&self, id: WorkspaceId) -> bool {
        self.workspace(id)
            .is_some_and(|w| w.status == WorkspaceStatus::Active)
    }

    /// Returns `false` if the workspace is unknown.
    pub fn set_status(&mut self, id: WorkspaceId, status: WorkspaceStatus) -> bool {
        match self
            .entries
            .iter_mut()
            .flat_map(|e| &mut e.workspaces)
            .find(|w| w.id == id)
        {
            Some(workspace) => {
                workspace.status = status;
                true
            }
            None => false,
        }
    }

    pub fn remove_workspace(&mut self, id: WorkspaceId) -> Option<Workspace> {
        self.entries.iter_mut().find_map(|entry| {
            let index = entry.workspaces.iter().position(|w| w.id == id)?;
            Some(entry.workspaces.remove(index))
        })
    }

    pub fn remove_project(&mut self, id: ProjectId) -> Option<ProjectEntry> {
        let index = self.entries.iter().position(|e| e.project.id == id)?;
        Some(self.entries.remove(index))
    }
}

/// What the window restores at startup: which workspace each pane shows, the focused pane and
/// the first pane's share of the width.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneLayout {
    pub panes: Vec<Option<WorkspaceId>>,
    pub focused: usize,
    pub split: Option<f32>,
}

/// The first pane's share of the total width, if the sizes describe a usable split.
pub fn split_ratio(first: f32, total: f32) -> Option<f32> {
    if !(first.is_finite() && total.is_finite()) || total <= 0.0 {
        return None;
    }
    Some((first / total).clamp(SPLIT_RANGE.0, SPLIT_RANGE.1))
}

/// A stored ratio sanitised before use; anything unusable means "default split".
pub fn sanitize_split(split: Option<f32>) -> Option<f32> {
    split
        .filter(|s| s.is_finite() && *s > 0.0 && *s < 1.0)
        .map(|s| s.clamp(SPLIT_RANGE.0, SPLIT_RANGE.1))
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

    /// Empties every pane showing the workspace. Returns whether one did.
    pub fn clear(&mut self, workspace: WorkspaceId) -> bool {
        let mut cleared = false;
        for slot in &mut self.slots {
            if *slot == Some(workspace) {
                *slot = None;
                cleared = true;
            }
        }
        cleared
    }

    pub fn layout(&self, split: Option<f32>) -> PaneLayout {
        PaneLayout {
            panes: self.slots.to_vec(),
            focused: self.focused,
            split,
        }
    }

    /// Rebuilds panes from a saved layout, keeping only workspaces `keep` accepts and never
    /// showing one twice.
    pub fn from_layout(layout: &PaneLayout, keep: impl Fn(WorkspaceId) -> bool) -> Self {
        let mut panes = Self::default();
        for (pane, id) in layout.panes.iter().take(PANE_COUNT).enumerate() {
            if let Some(id) = id
                && keep(*id)
                && !panes.is_shown(*id)
            {
                panes.slots[pane] = Some(*id);
            }
        }
        panes.focus(layout.focused);
        panes
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
    use super::{PaneLayout, Panes, ProjectList, sanitize_split, split_ratio, status_label};
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
    fn archive_restore_and_remove() {
        let mut list = ProjectList::default();
        let project = Project::new("app", "/repo", "main");
        list.add_project(project.clone());
        let a = workspace(&project, "a");
        let b = workspace(&project, "b");
        list.upsert_workspace(a.clone());
        list.upsert_workspace(b.clone());

        assert!(list.set_status(a.id, WorkspaceStatus::Archived));
        assert!(!list.is_active(a.id));
        let entry = list.entry(project.id).unwrap();
        assert_eq!(entry.active().map(|w| w.id).collect::<Vec<_>>(), [b.id]);
        assert_eq!(entry.archived().map(|w| w.id).collect::<Vec<_>>(), [a.id]);

        assert!(list.set_status(a.id, WorkspaceStatus::Active));
        assert!(list.is_active(a.id));
        assert!(!list.set_status(WorkspaceId::new(), WorkspaceStatus::Active));

        assert_eq!(list.remove_workspace(a.id).map(|w| w.id), Some(a.id));
        assert!(list.remove_workspace(a.id).is_none());
        assert!(list.workspace(a.id).is_none());

        let removed = list.remove_project(project.id).unwrap();
        assert_eq!(removed.workspaces.len(), 1);
        assert!(list.is_empty());
        assert!(list.remove_project(project.id).is_none());
    }

    #[test]
    fn panes_clear_and_layout_round_trip() {
        let (a, b, gone) = (WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new());
        let mut panes = Panes::default();
        panes.show(a);
        panes.focus(1);
        panes.show(b);

        let layout = panes.layout(Some(0.3));
        assert_eq!(layout.panes, vec![Some(a), Some(b)]);
        assert_eq!(layout.focused, 1);
        assert_eq!(Panes::from_layout(&layout, |_| true), panes);

        // Unknown workspaces are dropped; duplicates and extra panes are ignored.
        let saved = PaneLayout {
            panes: vec![Some(gone), Some(a), Some(a)],
            focused: 9,
            split: None,
        };
        let restored = Panes::from_layout(&saved, |id| id != gone);
        assert_eq!((restored.slot(0), restored.slot(1)), (None, Some(a)));
        assert_eq!(restored.focused(), 0);

        assert!(panes.clear(a));
        assert!(!panes.clear(a));
        assert_eq!((panes.slot(0), panes.slot(1)), (None, Some(b)));
    }

    #[test]
    fn split_ratios() {
        assert_eq!(split_ratio(300.0, 1000.0), Some(0.3));
        assert_eq!(split_ratio(10.0, 1000.0), Some(0.1));
        assert_eq!(split_ratio(1.0, 0.0), None);
        assert_eq!(split_ratio(f32::NAN, 10.0), None);
        assert_eq!(sanitize_split(Some(0.5)), Some(0.5));
        assert_eq!(sanitize_split(Some(0.99)), Some(0.9));
        assert_eq!(sanitize_split(Some(1.5)), None);
        assert_eq!(sanitize_split(Some(f32::INFINITY)), None);
        assert_eq!(sanitize_split(None), None);
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
