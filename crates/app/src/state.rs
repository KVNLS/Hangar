//! UI-independent state: the project tree and which workspace each pane shows.

use hangar_core::{Project, ProjectId, SessionStatus, Workspace, WorkspaceId, WorkspaceStatus};
use serde::{Deserialize, Serialize};

use crate::device_panel::{DeviceKey, LogsSettings};
use hangar_devices::LogLevel;

/// Most panes the window may be split into.
pub const MAX_PANES: usize = 4;

/// Smallest share of the width a pane may be restored to.
const MIN_PANE_SHARE: f32 = 0.1;

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
/// each pane's share of the width.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneLayout {
    pub panes: Vec<Option<WorkspaceId>>,
    pub focused: usize,
    /// Each pane's share of the width; empty means equal shares.
    #[serde(default)]
    pub sizes: Vec<f32>,
    /// Layouts saved with two fixed panes: the first pane's share.
    #[serde(default, skip_serializing)]
    pub split: Option<f32>,
}

impl PaneLayout {
    /// Rebuilds panes from the layout, keeping only workspaces `keep` accepts, never showing one
    /// twice and dropping empty panes (at least one pane remains). Returns the panes and their
    /// sanitised width shares.
    pub fn restore(&self, keep: impl Fn(WorkspaceId) -> bool) -> (Panes, Vec<f32>) {
        let saved_sizes = match self.split {
            Some(split) if self.sizes.is_empty() && self.panes.len() == 2 => {
                vec![split, 1. - split]
            }
            _ => self.sizes.clone(),
        };
        let mut panes = Panes {
            slots: Vec::new(),
            focused: 0,
        };
        let mut sizes = Vec::new();
        let mut focused = None;
        for (pane, id) in self.panes.iter().enumerate() {
            let Some(id) = *id else { continue };
            if !keep(id) || panes.is_shown(id) || panes.count() == MAX_PANES {
                continue;
            }
            if pane == self.focused {
                focused = Some(panes.count());
            }
            panes.slots.push(Some(id));
            sizes.push(saved_sizes.get(pane).copied().unwrap_or(f32::NAN));
        }
        if panes.slots.is_empty() {
            panes.slots.push(None);
        }
        panes.focused = focused.unwrap_or(0);
        let sizes = sanitize_shares(&sizes, panes.count());
        (panes, sizes)
    }
}

/// Each pane's share of the total width, if the sizes describe a usable layout.
pub fn pane_shares(sizes: &[f32], total: f32) -> Vec<f32> {
    if !total.is_finite() || total <= 0.0 {
        return Vec::new();
    }
    let shares: Vec<f32> = sizes.iter().map(|s| s / total).collect();
    sanitize_shares(&shares, sizes.len())
}

/// Shares normalised to sum to one with no pane under [`MIN_PANE_SHARE`]; anything unusable
/// (wrong count, a single pane, non-positive or non-finite values) means equal shares (empty).
pub fn sanitize_shares(shares: &[f32], count: usize) -> Vec<f32> {
    if count < 2 || shares.len() != count || !shares.iter().all(|s| s.is_finite() && *s > 0.0) {
        return Vec::new();
    }
    let normalise = |v: Vec<f32>| {
        let sum: f32 = v.iter().sum();
        v.into_iter().map(|s| s / sum).collect::<Vec<f32>>()
    };
    let shares = normalise(shares.to_vec());
    normalise(shares.into_iter().map(|s| s.max(MIN_PANE_SHARE)).collect())
}

/// Which workspace each tiled pane shows, and which pane has focus. Always at least one pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Panes {
    slots: Vec<Option<WorkspaceId>>,
    focused: usize,
}

impl Default for Panes {
    fn default() -> Self {
        Self {
            slots: vec![None],
            focused: 0,
        }
    }
}

impl Panes {
    pub fn count(&self) -> usize {
        self.slots.len()
    }

    pub fn slot(&self, pane: usize) -> Option<WorkspaceId> {
        self.slots.get(pane).copied().flatten()
    }

    pub fn focused(&self) -> usize {
        self.focused
    }

    pub fn focus(&mut self, pane: usize) {
        if pane < self.count() {
            self.focused = pane;
        }
    }

    /// The pane showing the workspace.
    pub fn pane_of(&self, workspace: WorkspaceId) -> Option<usize> {
        self.slots.iter().position(|s| *s == Some(workspace))
    }

    /// Shows the workspace in the focused pane, or focuses the pane already showing it, so a
    /// terminal is never on screen twice. Returns the pane.
    pub fn show(&mut self, workspace: WorkspaceId) -> usize {
        if let Some(pane) = self.pane_of(workspace) {
            self.focused = pane;
        } else {
            self.slots[self.focused] = Some(workspace);
        }
        self.focused
    }

    pub fn is_shown(&self, workspace: WorkspaceId) -> bool {
        self.slots.contains(&Some(workspace))
    }

    /// Adds an empty pane right of the focused one and focuses it. Returns the new pane, or
    /// `None` when the window already has [`MAX_PANES`].
    pub fn split(&mut self) -> Option<usize> {
        if self.count() >= MAX_PANES {
            return None;
        }
        self.focused += 1;
        self.slots.insert(self.focused, None);
        Some(self.focused)
    }

    /// Removes a pane (its workspace keeps running, just hidden). Closing the focused pane
    /// focuses its left neighbour. The last pane is never closed. Returns whether it was.
    pub fn close(&mut self, pane: usize) -> bool {
        if self.count() < 2 || pane >= self.count() {
            return false;
        }
        self.slots.remove(pane);
        if self.focused > pane || (self.focused == pane && pane > 0) {
            self.focused -= 1;
        }
        true
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

    pub fn layout(&self, sizes: &[f32]) -> PaneLayout {
        PaneLayout {
            panes: self.slots.clone(),
            focused: self.focused,
            sizes: sanitize_shares(sizes, self.count()),
            split: None,
        }
    }
}

/// Width range of the changes panel, in pixels.
pub const CHANGES_WIDTH_RANGE: (f32, f32) = (260.0, 1600.0);
const CHANGES_DEFAULT_WIDTH: f32 = 520.0;

/// The changes panel's visibility and width, saved across launches.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChangesLayout {
    pub open: bool,
    pub width: f32,
}

impl Default for ChangesLayout {
    fn default() -> Self {
        Self {
            open: false,
            width: CHANGES_DEFAULT_WIDTH,
        }
    }
}

impl ChangesLayout {
    /// Clamps the width; anything unusable means the default.
    pub fn sanitized(self) -> Self {
        Self {
            open: self.open,
            width: changes_width(self.width).unwrap_or(CHANGES_DEFAULT_WIDTH),
        }
    }
}

/// A usable panel width, clamped to [`CHANGES_WIDTH_RANGE`].
pub fn changes_width(width: f32) -> Option<f32> {
    width
        .is_finite()
        .then(|| width.clamp(CHANGES_WIDTH_RANGE.0, CHANGES_WIDTH_RANGE.1))
}

/// Width range of the device panel, in pixels.
pub const DEVICE_WIDTH_RANGE: (f32, f32) = (240.0, 1200.0);
/// Roughly a phone at 1:1 points (402 pt wide) plus the panel's padding.
const DEVICE_DEFAULT_WIDTH: f32 = 420.0;

/// The device panel's visibility, width, selected device and stats overlay, saved across
/// launches. One panel per window: the device is shared by all workspaces.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DeviceLayout {
    pub open: bool,
    pub width: f32,
    #[serde(default)]
    pub selected: Option<DeviceKey>,
    /// Debug overlay with fps and capture → paint latency.
    #[serde(default)]
    pub stats: bool,
    /// The logs pane under the screen is open.
    #[serde(default)]
    pub logs: bool,
    /// Lowest level shown in the logs pane (`LogLevel::as_str`); unknown means all.
    #[serde(default)]
    pub log_level: Option<String>,
}

impl Default for DeviceLayout {
    fn default() -> Self {
        Self {
            open: false,
            width: DEVICE_DEFAULT_WIDTH,
            selected: None,
            stats: false,
            logs: false,
            log_level: None,
        }
    }
}

impl DeviceLayout {
    /// The saved logs pane state; an unknown level means all.
    pub fn logs_settings(&self) -> LogsSettings {
        LogsSettings {
            open: self.logs,
            min_level: self
                .log_level
                .as_deref()
                .and_then(|l| l.parse().ok())
                .unwrap_or(LogLevel::Verbose),
        }
    }

    /// Clamps the width; anything unusable means the default.
    pub fn sanitized(self) -> Self {
        Self {
            width: device_width(self.width).unwrap_or(DEVICE_DEFAULT_WIDTH),
            ..self
        }
    }
}

/// A usable device panel width, clamped to [`DEVICE_WIDTH_RANGE`].
pub fn device_width(width: f32) -> Option<f32> {
    width
        .is_finite()
        .then(|| width.clamp(DEVICE_WIDTH_RANGE.0, DEVICE_WIDTH_RANGE.1))
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
    use super::{
        CHANGES_WIDTH_RANGE, ChangesLayout, DEVICE_WIDTH_RANGE, DeviceLayout, PaneLayout, Panes,
        ProjectList, changes_width, device_width, pane_shares, sanitize_shares, status_label,
    };
    use crate::device_panel::{DeviceKey, LogsSettings, Platform};
    use hangar_core::{Project, SessionStatus, Timestamp, Workspace, WorkspaceId, WorkspaceStatus};
    use hangar_devices::LogLevel;

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
        assert_eq!(panes.count(), 1);
        assert_eq!(panes.show(a), 0);
        assert_eq!(panes.split(), Some(1));
        assert_eq!(panes.show(b), 1);
        assert_eq!((panes.slot(0), panes.slot(1)), (Some(a), Some(b)));

        // Already visible in pane 0: focus moves there instead of duplicating.
        assert_eq!(panes.show(a), 0);
        assert_eq!(panes.focused(), 0);
        assert_eq!(panes.slot(1), Some(b));
        assert_eq!(panes.pane_of(b), Some(1));

        assert_eq!(panes.show(c), 0);
        assert!(!panes.is_shown(a));
        assert!(panes.is_shown(c));

        panes.focus(7);
        assert_eq!(panes.focused(), 0);
    }

    #[test]
    fn panes_split_and_close() {
        let (a, b, c) = (WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new());
        let mut panes = Panes::default();
        assert!(!panes.close(0), "the last pane stays");
        panes.show(a);
        panes.split();
        panes.show(b);
        // Splitting the first pane inserts between a and b.
        panes.focus(0);
        assert_eq!(panes.split(), Some(1));
        panes.show(c);
        assert_eq!(
            (0..3).map(|p| panes.slot(p)).collect::<Vec<_>>(),
            [Some(a), Some(c), Some(b)]
        );
        assert_eq!(panes.split(), Some(2));
        assert_eq!(panes.count(), 4);
        assert_eq!(panes.split(), None);

        // Closing the focused pane focuses its left neighbour.
        assert!(panes.close(2));
        assert_eq!((panes.count(), panes.focused()), (3, 1));
        // Closing a pane left of the focused one keeps the same pane focused.
        assert!(panes.close(0));
        assert_eq!((panes.slot(panes.focused()), panes.count()), (Some(c), 2));
        // Closing the first pane while focused focuses the new first.
        panes.focus(0);
        assert!(panes.close(0));
        assert_eq!((panes.slot(0), panes.focused()), (Some(b), 0));
        assert!(!panes.close(0));
        assert!(!panes.close(5));
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
        panes.split();
        panes.show(b);

        let layout = panes.layout(&[0.3, 0.7]);
        assert_eq!(layout.panes, vec![Some(a), Some(b)]);
        assert_eq!(layout.focused, 1);
        assert_eq!(
            layout.restore(|_| true),
            (panes.clone(), layout.sizes.clone())
        );

        // Unknown workspaces, duplicates and empty panes are dropped; focus follows its pane.
        let saved = PaneLayout {
            panes: vec![Some(gone), None, Some(a), Some(a), Some(b)],
            focused: 4,
            sizes: vec![0.2, 0.2, 0.2, 0.2, 0.2],
            split: None,
        };
        let (restored, sizes) = saved.restore(|id| id != gone);
        assert_eq!((restored.slot(0), restored.slot(1)), (Some(a), Some(b)));
        assert_eq!((restored.count(), restored.focused()), (2, 1));
        assert_eq!(sizes, [0.5, 0.5]);

        // Nothing left: one empty pane.
        let (restored, sizes) = saved.restore(|_| false);
        assert_eq!((restored, sizes), (Panes::default(), Vec::new()));

        assert!(panes.clear(a));
        assert!(!panes.clear(a));
        assert_eq!((panes.slot(0), panes.slot(1)), (None, Some(b)));
    }

    #[test]
    fn legacy_two_pane_layouts_migrate() {
        let (a, b) = (WorkspaceId::new(), WorkspaceId::new());
        let json = format!(r#"{{"panes":["{a}",null],"focused":1,"split":0.3}}"#);
        let saved: PaneLayout = serde_json::from_str(&json).unwrap();
        let (panes, sizes) = saved.restore(|_| true);
        assert_eq!(
            (panes.count(), panes.slot(0), panes.focused()),
            (1, Some(a), 0)
        );
        assert!(sizes.is_empty());

        let json = format!(r#"{{"panes":["{a}","{b}"],"focused":1,"split":0.25}}"#);
        let saved: PaneLayout = serde_json::from_str(&json).unwrap();
        let (panes, sizes) = saved.restore(|_| true);
        assert_eq!((panes.count(), panes.focused()), (2, 1));
        assert_eq!(sizes, [0.25, 0.75]);
        assert!(
            !serde_json::to_string(&panes.layout(&sizes))
                .unwrap()
                .contains("split")
        );
    }

    #[test]
    fn pane_shares_sanitised() {
        assert_eq!(pane_shares(&[300.0, 700.0], 1000.0), [0.3, 0.7]);
        assert_eq!(pane_shares(&[300.0, 700.0], 0.0), Vec::<f32>::new());
        assert_eq!(pane_shares(&[f32::NAN, 10.0], 10.0), Vec::<f32>::new());
        assert_eq!(pane_shares(&[500.0], 500.0), Vec::<f32>::new());
        // Normalised, and no pane under the minimum share.
        assert_eq!(sanitize_shares(&[1.0, 1.0], 2), [0.5, 0.5]);
        let tiny = sanitize_shares(&[0.01, 0.99], 2);
        assert!(tiny[0] > 0.09 && (tiny.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert_eq!(sanitize_shares(&[0.5, 0.5], 3), Vec::<f32>::new());
        assert_eq!(sanitize_shares(&[0.5, -0.5], 2), Vec::<f32>::new());
        assert_eq!(sanitize_shares(&[f32::INFINITY, 0.5], 2), Vec::<f32>::new());
    }

    #[test]
    fn changes_layout_sanitized() {
        let ok = ChangesLayout {
            open: true,
            width: 400.0,
        };
        assert_eq!(ok.sanitized(), ok);
        let narrow = ChangesLayout {
            open: false,
            width: 10.0,
        };
        assert_eq!(narrow.sanitized().width, CHANGES_WIDTH_RANGE.0);
        let broken = ChangesLayout {
            open: true,
            width: f32::NAN,
        };
        assert_eq!(broken.sanitized().width, ChangesLayout::default().width);
        assert!(broken.sanitized().open);
        assert_eq!(changes_width(1e9), Some(CHANGES_WIDTH_RANGE.1));
    }

    #[test]
    fn device_layout_sanitized() {
        let ok = DeviceLayout {
            open: true,
            width: 500.0,
            selected: Some(DeviceKey {
                platform: Platform::Ios,
                id: "U".into(),
            }),
            stats: true,
            logs: true,
            log_level: Some("warn".into()),
        };
        assert_eq!(ok.clone().sanitized(), ok);
        assert_eq!(
            ok.logs_settings(),
            LogsSettings {
                open: true,
                min_level: LogLevel::Warn
            }
        );
        assert_eq!(
            DeviceLayout::default().logs_settings(),
            LogsSettings::default()
        );
        let junk = DeviceLayout {
            log_level: Some("loud".into()),
            ..ok.clone()
        };
        assert_eq!(junk.logs_settings().min_level, LogLevel::Verbose);
        let broken = DeviceLayout {
            width: f32::NAN,
            ..ok.clone()
        };
        let fixed = broken.sanitized();
        assert_eq!(fixed.width, DeviceLayout::default().width);
        assert_eq!(fixed.selected, ok.selected);
        assert_eq!(device_width(1.0), Some(DEVICE_WIDTH_RANGE.0));
        assert_eq!(device_width(f32::INFINITY), None);
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
