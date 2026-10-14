//! UI-independent state: the project tree and which workspace each pane shows.

use std::ops::Range;

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

/// What the window restores at startup: which workspace each pane shows, how the panes are
/// arranged in columns, the focused pane and the panes' shares of the space.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct PaneLayout {
    /// In column order: top to bottom in each column, columns left to right.
    pub panes: Vec<Option<WorkspaceId>>,
    pub focused: usize,
    /// How many panes each column stacks; empty (layouts saved before panes could be split
    /// down) means one pane per column.
    #[serde(default)]
    pub columns: Vec<usize>,
    /// Each column's share of the width; empty means equal shares.
    #[serde(default)]
    pub sizes: Vec<f32>,
    /// Each column's panes' shares of its height; empty means equal shares.
    #[serde(default)]
    pub rows: Vec<Vec<f32>>,
    /// Layouts saved with two fixed panes: the first pane's share.
    #[serde(default, skip_serializing)]
    pub split: Option<f32>,
    /// Width of the right panel (device, changes and files share it); missing in layouts saved
    /// when each tab had its own width.
    #[serde(default)]
    pub panel_width: Option<f32>,
    /// Width of the sidebar; missing in layouts saved before it could be resized.
    #[serde(default)]
    pub sidebar_width: Option<f32>,
}

impl PaneLayout {
    /// The saved right panel width, clamped; the default when missing or unusable.
    pub fn panel_width(&self) -> f32 {
        self.panel_width
            .and_then(panel_width)
            .unwrap_or(PANEL_DEFAULT_WIDTH)
    }

    /// The saved sidebar width, clamped; the default when missing or unusable.
    pub fn sidebar_width(&self) -> f32 {
        self.sidebar_width
            .and_then(sidebar_width)
            .unwrap_or(SIDEBAR_DEFAULT_WIDTH)
    }

    /// Rebuilds panes from the layout, keeping only workspaces `keep` accepts, never showing one
    /// twice and dropping empty panes and columns (at least one pane remains). Shares that no
    /// longer fit are sanitised.
    pub fn restore(&self, keep: impl Fn(WorkspaceId) -> bool) -> Panes {
        let saved_sizes = match self.split {
            Some(split) if self.sizes.is_empty() && self.panes.len() == 2 => {
                vec![split, 1. - split]
            }
            _ => self.sizes.clone(),
        };
        let saved_columns = if !self.columns.is_empty()
            && self.columns.iter().all(|n| *n > 0)
            && self.columns.iter().sum::<usize>() == self.panes.len()
        {
            self.columns.clone()
        } else {
            vec![1; self.panes.len()]
        };
        let mut panes = Panes {
            slots: Vec::new(),
            columns: Vec::new(),
            focused: 0,
            shares: PaneShares::default(),
        };
        let mut column_sizes = Vec::new();
        let mut focused = None;
        let mut start = 0;
        for (column, len) in saved_columns.into_iter().enumerate() {
            let mut row_sizes = Vec::new();
            for pane in start..start + len {
                let Some(id) = self.panes[pane] else { continue };
                if !keep(id) || panes.is_shown(id) || panes.count() == MAX_PANES {
                    continue;
                }
                if pane == self.focused {
                    focused = Some(panes.count());
                }
                panes.slots.push(Some(id));
                let saved = self.rows.get(column).and_then(|r| r.get(pane - start));
                row_sizes.push(saved.copied().unwrap_or(f32::NAN));
            }
            start += len;
            if !row_sizes.is_empty() {
                panes.columns.push(row_sizes.len());
                panes
                    .shares
                    .rows
                    .push(sanitize_shares(&row_sizes, row_sizes.len()));
                column_sizes.push(saved_sizes.get(column).copied().unwrap_or(f32::NAN));
            }
        }
        if panes.slots.is_empty() {
            return Panes::default();
        }
        panes.focused = focused.unwrap_or(0);
        panes.shares.columns = sanitize_shares(&column_sizes, panes.columns.len());
        panes
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

/// `shares` if there are `count` of them, else equal shares.
fn or_equal(shares: &[f32], count: usize) -> Vec<f32> {
    if shares.len() == count {
        shares.to_vec()
    } else {
        vec![1. / count as f32; count]
    }
}

/// The columns' shares of the width and each column's panes' shares of its height, once the
/// user has resized them (or they were restored); empty means equal shares.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PaneShares {
    pub columns: Vec<f32>,
    /// One entry per column.
    pub rows: Vec<Vec<f32>>,
}

/// Which workspace each tiled pane shows, how the panes stack in columns, which pane has focus
/// and how they share the space. Always at least one pane. Panes are numbered in column order:
/// top to bottom in each column, columns left to right.
#[derive(Clone, Debug, PartialEq)]
pub struct Panes {
    slots: Vec<Option<WorkspaceId>>,
    /// How many panes each column stacks, left to right.
    columns: Vec<usize>,
    focused: usize,
    shares: PaneShares,
}

impl Default for Panes {
    fn default() -> Self {
        Self {
            slots: vec![None],
            columns: vec![1],
            focused: 0,
            shares: PaneShares {
                columns: Vec::new(),
                rows: vec![Vec::new()],
            },
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

    /// The panes of each column, left to right.
    pub fn columns(&self) -> Vec<Range<usize>> {
        let mut start = 0;
        self.columns
            .iter()
            .map(|len| {
                start += len;
                start - len..start
            })
            .collect()
    }

    pub fn column_count(&self) -> usize {
        self.columns.len()
    }

    /// The column holding the pane (the last one for an unknown pane).
    pub fn column_of(&self, pane: usize) -> usize {
        self.columns()
            .iter()
            .position(|range| range.contains(&pane))
            .unwrap_or(self.columns.len() - 1)
    }

    pub fn shares(&self) -> &PaneShares {
        &self.shares
    }

    /// The columns' shares of the width (equal unless resized).
    pub fn column_shares(&self) -> Vec<f32> {
        or_equal(&self.shares.columns, self.column_count())
    }

    /// The column's panes' shares of its height (equal unless resized).
    pub fn row_shares(&self, column: usize) -> Vec<f32> {
        let len = self.columns.get(column).copied().unwrap_or(1);
        or_equal(
            self.shares.rows.get(column).map_or(&[][..], Vec::as_slice),
            len,
        )
    }

    /// Takes the columns' shares as resized by the user. Returns whether they fit.
    pub fn set_column_shares(&mut self, shares: Vec<f32>) -> bool {
        let fits = shares.len() == self.column_count() && shares.len() > 1;
        if fits {
            self.shares.columns = shares;
        }
        fits
    }

    /// Takes a column's panes' shares as resized by the user. Returns whether they fit.
    pub fn set_row_shares(&mut self, column: usize, shares: Vec<f32>) -> bool {
        let fits = self.columns.get(column) == Some(&shares.len()) && shares.len() > 1;
        if fits {
            self.shares.rows[column] = shares;
        }
        fits
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

    /// Adds a column with one empty pane right of the focused pane's column, gives it half that
    /// column's width and focuses it. Returns the new pane, or `None` when the window already
    /// has [`MAX_PANES`].
    pub fn split(&mut self) -> Option<usize> {
        if self.count() >= MAX_PANES {
            return None;
        }
        let column = self.column_of(self.focused);
        let mut shares = self.column_shares();
        shares[column] /= 2.;
        shares.insert(column + 1, shares[column]);
        let pane = self.columns()[column].end;
        self.slots.insert(pane, None);
        self.columns.insert(column + 1, 1);
        self.shares.columns = sanitize_shares(&shares, self.column_count());
        self.shares.rows.insert(column + 1, Vec::new());
        self.focused = pane;
        Some(pane)
    }

    /// Adds an empty pane below the focused one, gives it half its height and focuses it.
    /// Returns the new pane, or `None` when the window already has [`MAX_PANES`].
    pub fn split_down(&mut self) -> Option<usize> {
        if self.count() >= MAX_PANES {
            return None;
        }
        let column = self.column_of(self.focused);
        let row = self.focused - self.columns()[column].start;
        let mut shares = self.row_shares(column);
        shares[row] /= 2.;
        shares.insert(row + 1, shares[row]);
        let pane = self.focused + 1;
        self.slots.insert(pane, None);
        self.columns[column] += 1;
        self.shares.rows[column] = sanitize_shares(&shares, self.columns[column]);
        self.focused = pane;
        Some(pane)
    }

    /// Removes a pane (its workspace keeps running, just hidden); its space goes to the rest of
    /// its column, or to the other columns if it was alone. Closing the focused pane focuses the
    /// pane before it. The last pane is never closed. Returns whether it was.
    pub fn close(&mut self, pane: usize) -> bool {
        if self.count() < 2 || pane >= self.count() {
            return false;
        }
        let column = self.column_of(pane);
        if self.columns[column] == 1 {
            let mut shares = self.column_shares();
            shares.remove(column);
            self.columns.remove(column);
            self.shares.rows.remove(column);
            self.shares.columns = sanitize_shares(&shares, self.column_count());
        } else {
            let mut shares = self.row_shares(column);
            shares.remove(pane - self.columns()[column].start);
            self.columns[column] -= 1;
            self.shares.rows[column] = sanitize_shares(&shares, self.columns[column]);
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

    pub fn layout(&self) -> PaneLayout {
        PaneLayout {
            panes: self.slots.clone(),
            focused: self.focused,
            columns: self.columns.clone(),
            sizes: self.shares.columns.clone(),
            rows: self.shares.rows.clone(),
            split: None,
            panel_width: None,
            sidebar_width: None,
        }
    }
}

/// Width range of the right panel (device, changes, files), in pixels.
pub const PANEL_WIDTH_RANGE: (f32, f32) = (360.0, 1600.0);
pub const PANEL_DEFAULT_WIDTH: f32 = 440.0;

/// A usable right panel width, clamped to [`PANEL_WIDTH_RANGE`].
pub fn panel_width(width: f32) -> Option<f32> {
    width
        .is_finite()
        .then(|| width.clamp(PANEL_WIDTH_RANGE.0, PANEL_WIDTH_RANGE.1))
}

/// Width range of the sidebar, in pixels.
pub const SIDEBAR_WIDTH_RANGE: (f32, f32) = (200.0, 480.0);
pub const SIDEBAR_DEFAULT_WIDTH: f32 = 256.0;

/// A usable sidebar width, clamped to [`SIDEBAR_WIDTH_RANGE`].
pub fn sidebar_width(width: f32) -> Option<f32> {
    width
        .is_finite()
        .then(|| width.clamp(SIDEBAR_WIDTH_RANGE.0, SIDEBAR_WIDTH_RANGE.1))
}

/// Whether the changes tab of the right panel is open, saved across launches.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ChangesLayout {
    pub open: bool,
}

/// The device tab's visibility, selected device and stats overlay, saved across
/// launches. One panel per window: the device is shared by all workspaces.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DeviceLayout {
    pub open: bool,
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
        ChangesLayout, DeviceLayout, PANEL_DEFAULT_WIDTH, PANEL_WIDTH_RANGE, PaneLayout, Panes,
        ProjectList, SIDEBAR_DEFAULT_WIDTH, SIDEBAR_WIDTH_RANGE, pane_shares, panel_width,
        sanitize_shares, sidebar_width, status_label,
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
        assert!(panes.set_column_shares(vec![0.3, 0.7]));
        assert!(!panes.set_column_shares(vec![1.0]));

        let layout = panes.layout();
        assert_eq!(layout.panes, vec![Some(a), Some(b)]);
        assert_eq!(layout.focused, 1);
        assert_eq!(layout.columns, [1, 1]);
        assert_eq!(layout.sizes, [0.3, 0.7]);
        assert_eq!(layout.restore(|_| true), panes);

        // Unknown workspaces, duplicates and empty panes are dropped; focus follows its pane.
        let saved = PaneLayout {
            panes: vec![Some(gone), None, Some(a), Some(a), Some(b)],
            focused: 4,
            sizes: vec![0.2, 0.2, 0.2, 0.2, 0.2],
            ..Default::default()
        };
        let restored = saved.restore(|id| id != gone);
        assert_eq!((restored.slot(0), restored.slot(1)), (Some(a), Some(b)));
        assert_eq!((restored.count(), restored.focused()), (2, 1));
        assert_eq!(restored.shares().columns, [0.5, 0.5]);

        // Nothing left: one empty pane.
        assert_eq!(saved.restore(|_| false), Panes::default());

        assert!(panes.clear(a));
        assert!(!panes.clear(a));
        assert_eq!((panes.slot(0), panes.slot(1)), (None, Some(b)));
    }

    #[test]
    fn panes_split_down_into_columns() {
        let (a, b, c, d) = (
            WorkspaceId::new(),
            WorkspaceId::new(),
            WorkspaceId::new(),
            WorkspaceId::new(),
        );
        let mut panes = Panes::default();
        panes.show(a);
        assert_eq!(panes.split_down(), Some(1));
        panes.show(b);
        assert_eq!(panes.layout().columns, [2]);
        assert_eq!(panes.row_shares(0), [0.5, 0.5]);
        // Split right of the bottom pane: a new column after the first.
        assert_eq!(panes.split(), Some(2));
        panes.show(c);
        assert_eq!(panes.columns(), [0..2, 2..3]);
        assert_eq!(panes.column_of(1), 0);
        assert_eq!(panes.column_shares(), [0.5, 0.5]);
        // Split the top pane down: it shares its half of the height.
        panes.focus(0);
        assert_eq!(panes.split_down(), Some(1));
        panes.show(d);
        assert_eq!(panes.columns(), [0..3, 3..4]);
        assert_eq!(panes.row_shares(0), [0.25, 0.25, 0.5]);
        assert_eq!(
            (0..4).map(|p| panes.slot(p)).collect::<Vec<_>>(),
            [Some(a), Some(d), Some(b), Some(c)]
        );
        assert_eq!(panes.split_down(), None);
        assert_eq!(panes.split(), None);

        // The round trip keeps the columns and their row shares.
        let layout = panes.layout();
        assert_eq!(layout.columns, [3, 1]);
        assert_eq!(layout.restore(|_| true), panes);

        // Closing a pane gives its height to its column; the last pane of a column takes the
        // column with it.
        assert!(panes.close(1));
        assert_eq!(panes.columns(), [0..2, 2..3]);
        assert_eq!(panes.row_shares(0), [1. / 3., 2. / 3.]);
        assert_eq!(panes.focused(), 0);
        assert!(panes.close(2));
        assert_eq!(panes.layout().columns, [2]);
        assert!(panes.shares().columns.is_empty());
        assert!(panes.set_row_shares(0, vec![0.4, 0.6]));
        assert!(!panes.set_row_shares(0, vec![1.0]));
        assert!(!panes.set_row_shares(3, vec![0.4, 0.6]));
        assert!(panes.close(0));
        assert_eq!(panes.layout().columns, [1]);
        assert_eq!(panes.slot(0), Some(b));
    }

    #[test]
    fn column_layouts_restore_without_dropped_panes() {
        let (a, b, c) = (WorkspaceId::new(), WorkspaceId::new(), WorkspaceId::new());
        let saved = PaneLayout {
            panes: vec![Some(a), Some(b), None, Some(c)],
            focused: 3,
            columns: vec![2, 1, 1],
            sizes: vec![0.5, 0.2, 0.3],
            rows: vec![vec![0.7, 0.3]],
            ..Default::default()
        };
        let restored = saved.restore(|_| true);
        // The empty column is dropped with its share.
        assert_eq!(restored.columns(), [0..2, 2..3]);
        assert_eq!(restored.focused(), 2);
        assert_eq!(restored.row_shares(0), [0.7, 0.3]);
        let columns = restored.column_shares();
        assert!((columns[0] - 0.5 / 0.8).abs() < 1e-5);

        // A column that loses a pane keeps one pane, no row shares.
        let restored = saved.restore(|id| id != b);
        assert_eq!(restored.columns(), [0..1, 1..2]);
        assert_eq!(restored.shares().rows, [Vec::<f32>::new(), Vec::new()]);

        // Columns that do not add up are read as one pane per column.
        let broken = PaneLayout {
            columns: vec![3, 3],
            ..saved.clone()
        };
        assert_eq!(broken.restore(|_| true).column_count(), 3);
    }

    #[test]
    fn legacy_two_pane_layouts_migrate() {
        let (a, b) = (WorkspaceId::new(), WorkspaceId::new());
        let json = format!(r#"{{"panes":["{a}",null],"focused":1,"split":0.3}}"#);
        let saved: PaneLayout = serde_json::from_str(&json).unwrap();
        let panes = saved.restore(|_| true);
        assert_eq!(
            (panes.count(), panes.slot(0), panes.focused()),
            (1, Some(a), 0)
        );
        assert!(panes.shares().columns.is_empty());

        let json = format!(r#"{{"panes":["{a}","{b}"],"focused":1,"split":0.25}}"#);
        let saved: PaneLayout = serde_json::from_str(&json).unwrap();
        let panes = saved.restore(|_| true);
        assert_eq!((panes.count(), panes.focused()), (2, 1));
        assert_eq!(panes.columns(), [0..1, 1..2]);
        assert_eq!(panes.shares().columns, [0.25, 0.75]);
        assert!(
            !serde_json::to_string(&panes.layout())
                .unwrap()
                .contains("split")
        );

        // Saved before columns: one pane per column, sizes are the columns' widths.
        let json = format!(r#"{{"panes":["{a}","{b}"],"focused":0,"sizes":[0.4,0.6]}}"#);
        let saved: PaneLayout = serde_json::from_str(&json).unwrap();
        let panes = saved.restore(|_| true);
        assert_eq!(panes.column_count(), 2);
        assert_eq!(panes.shares().columns, [0.4, 0.6]);
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
    fn panel_width_shared_and_migrated() {
        let saved = |json: &str| serde_json::from_str::<PaneLayout>(json).unwrap();
        // Saved before the tabs shared a width: the default.
        assert_eq!(
            saved(r#"{"panes":[null],"focused":0}"#).panel_width(),
            PANEL_DEFAULT_WIDTH
        );
        assert_eq!(
            saved(r#"{"panes":[null],"focused":0,"panel_width":500.0}"#).panel_width(),
            500.0
        );
        assert_eq!(
            saved(r#"{"panes":[null],"focused":0,"panel_width":10.0}"#).panel_width(),
            PANEL_WIDTH_RANGE.0
        );
        assert_eq!(panel_width(f32::NAN), None);
        assert_eq!(panel_width(1e9), Some(PANEL_WIDTH_RANGE.1));
        // Old per-tab widths are ignored.
        let changes: ChangesLayout =
            serde_json::from_str(r#"{"open":true,"width":520.0}"#).unwrap();
        assert_eq!(changes, ChangesLayout { open: true });
        let device: DeviceLayout = serde_json::from_str(r#"{"open":true,"width":420.0}"#).unwrap();
        assert!(device.open);
        assert_eq!(
            crate::ui::RIGHT_PANEL_WIDTH,
            gpui_kit::px(PANEL_DEFAULT_WIDTH)
        );
        assert_eq!(
            crate::ui::RIGHT_PANEL_MIN_WIDTH,
            gpui_kit::px(PANEL_WIDTH_RANGE.0)
        );
    }

    #[test]
    fn sidebar_width_saved_and_migrated() {
        let saved = |json: &str| serde_json::from_str::<PaneLayout>(json).unwrap();
        assert_eq!(
            saved(r#"{"panes":[null],"focused":0}"#).sidebar_width(),
            SIDEBAR_DEFAULT_WIDTH
        );
        assert_eq!(
            saved(r#"{"panes":[null],"focused":0,"sidebar_width":300.0}"#).sidebar_width(),
            300.0
        );
        assert_eq!(
            saved(r#"{"panes":[null],"focused":0,"sidebar_width":50.0}"#).sidebar_width(),
            SIDEBAR_WIDTH_RANGE.0
        );
        assert_eq!(sidebar_width(f32::NAN), None);
        assert_eq!(
            crate::ui::SIDEBAR_WIDTH,
            gpui_kit::px(SIDEBAR_DEFAULT_WIDTH)
        );
        assert_eq!(
            crate::ui::SIDEBAR_MIN_WIDTH,
            gpui_kit::px(SIDEBAR_WIDTH_RANGE.0)
        );
    }

    #[test]
    fn device_layout_logs() {
        let ok = DeviceLayout {
            open: true,
            selected: Some(DeviceKey {
                platform: Platform::Ios,
                id: "U".into(),
            }),
            stats: true,
            logs: true,
            log_level: Some("warn".into()),
        };
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
