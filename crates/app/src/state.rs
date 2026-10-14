//! UI-independent state: the project tree and which workspace each pane shows.

use std::ops::Range;

use hangar_core::{Project, ProjectId, SessionStatus, Workspace, WorkspaceId, WorkspaceStatus};
use serde::{Deserialize, Serialize};

use crate::device_panel::{DeviceKey, LogsSettings};
use hangar_core::device::LogLevel;

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
mod tests;
