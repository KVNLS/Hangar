//! Right panel tabs and sizing, and pane layout persistence.

use super::*;

/// Tabs of the right panel; they share its width.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum PanelTab {
    #[default]
    Device,
    Changes,
    Files,
}

impl HangarApp {
    /// Saves the pane layout once it has been quiet for [`SAVE_DEBOUNCE`] (pane drags and splits
    /// come in bursts).
    pub(super) fn save_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Dropping the previous task cancels it: only the last layout is written.
        self.layout.save_panes = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.layout.save_panes = None;
                let layout = this.saved_layout();
                this.persist(window, cx, move |store| ops::save_layout(store, &layout));
            });
        }));
    }

    /// What [`Self::save_layout`] writes: the panes and the panel widths.
    fn saved_layout(&self) -> PaneLayout {
        PaneLayout {
            panel_width: Some(self.layout.panel_width),
            ..self.layout.panes.layout()
        }
    }

    /// Writes a pending layout save now (the app is quitting).
    pub(super) fn flush_layout(&mut self) {
        if self.layout.save_panes.take().is_none() {
            return;
        }
        let Some(store) = self.store.clone() else {
            return;
        };
        if let Err(err) = ops::save_layout(&store, &self.saved_layout()) {
            tracing::warn!("pane layout not saved on quit: {err:#}");
        }
    }

    /// Saves the pane layout and starts/stops live diffs to match what the panes show.
    pub(super) fn panes_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_layout(window, cx);
        self.sync_diffs(cx);
        self.sync_device_focus(window, cx);
    }

    /// Starts a live diff for every active workspace on screen, stops those of workspaces
    /// that are gone, archived or being removed, and points the changes panel at the focused
    /// pane's workspace.
    pub(super) fn sync_diffs(&mut self, cx: &mut Context<Self>) {
        let projects = &self.work.projects;
        let busy = &self.work.busy;
        self.live
            .diffs
            .retain(|id, _| projects.is_active(*id) && !busy.contains(id));
        for pane in 0..self.layout.panes.count() {
            let Some(id) = self.layout.panes.slot(pane) else {
                continue;
            };
            if self.live.diffs.contains_key(&id) || self.work.busy.contains(&id) {
                continue;
            }
            let Some(ws) = self
                .work
                .projects
                .workspace(id)
                .filter(|_| self.work.projects.is_active(id))
            else {
                continue;
            };
            let (path, base) = (ws.worktree_path.clone(), ws.base.clone());
            let diff = cx.new(|cx| WorkspaceDiff::start(id, path, base, cx));
            let observe = cx.observe(&diff, |_, _, cx| cx.notify());
            self.live.diffs.insert(
                id,
                DiffEntry {
                    diff,
                    _observe: observe,
                },
            );
        }
        let source = self
            .layout
            .panes
            .slot(self.layout.panes.focused())
            .and_then(|id| {
                let entry = self.live.diffs.get(&id)?;
                let ws = self.work.projects.workspace(id)?;
                let project = self.work.projects.project(ws.project_id)?;
                Some(PanelSource {
                    workspace: id,
                    name: SharedString::from(ws.name.clone()),
                    diff: entry.diff.clone(),
                    target: ReviewTarget {
                        workspace: ws.clone(),
                        repo: project.repo_path.clone(),
                    },
                })
            });
        let files = source.as_ref().map(|s| crate::files_view::FilesSource {
            workspace: s.workspace,
            root: s.target.workspace.worktree_path.clone(),
            diff: s.diff.clone(),
        });
        self.changes_panel
            .update(cx, |panel, cx| panel.set_source(source, cx));
        self.files_panel
            .update(cx, |panel, cx| panel.set_source(files, cx));
    }

    pub(super) fn set_screen(
        &mut self,
        screen: Screen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.layout.screen == screen {
            return;
        }
        self.layout.screen = screen;
        let wide = screen == Screen::Review;
        self.changes_panel
            .update(cx, |panel, cx| panel.set_wide(wide, window, cx));
        // The device panel is only on the workspace screen: stop streaming while it is hidden.
        if self.layout.device.open {
            let shown = screen == Screen::Workspace;
            self.device_panel
                .update(cx, |panel, cx| panel.set_visible(shown, window, cx));
        }
        match screen {
            Screen::Review => {}
            Screen::Overview | Screen::Settings => window.focus(&self.layout.screen_focus, cx),
            Screen::Workspace => {
                window.focus(&self.focus, cx);
                self.focus_terminal(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn toggle_overview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let screen = if self.layout.screen == Screen::Overview {
            Screen::Workspace
        } else {
            Screen::Overview
        };
        self.set_screen(screen, window, cx);
    }

    /// The right panel's open tab, if it is shown.
    pub(super) fn open_panel_tab(&self) -> Option<PanelTab> {
        if self.layout.device.open {
            Some(PanelTab::Device)
        } else if self.layout.changes.open {
            Some(PanelTab::Changes)
        } else if self.layout.files_open {
            Some(PanelTab::Files)
        } else {
            None
        }
    }

    pub(super) fn show_panel_tab(
        &mut self,
        tab: PanelTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match tab {
            PanelTab::Device => self.set_device_open(true, window, cx),
            PanelTab::Changes => self.set_changes_open(true, window, cx),
            PanelTab::Files => self.set_files_open(true, window, cx),
        }
    }

    pub(super) fn hide_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.set_device_open(false, window, cx);
        self.set_changes_open(false, window, cx);
        self.set_files_open(false, window, cx);
    }

    /// Hides the right panel, or shows it on the tab it last showed.
    pub(super) fn toggle_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.open_panel_tab() {
            Some(_) => self.hide_panel(window, cx),
            None => self.show_panel_tab(self.layout.panel_tab, window, cx),
        }
    }

    /// The Files tab (not saved: it reopens on Device or Changes).
    pub(super) fn set_files_open(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if open {
            self.set_device_open(false, window, cx);
            self.set_changes_open(false, window, cx);
            self.layout.panel_tab = PanelTab::Files;
        }
        self.layout.files_open = open;
        cx.notify();
    }

    pub(super) fn set_changes_open(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Device, changes and files are tabs of one right panel.
        if open && self.layout.device.open {
            self.set_device_open(false, window, cx);
        }
        if open {
            self.layout.files_open = false;
            self.layout.panel_tab = PanelTab::Changes;
        }
        if self.layout.changes.open != open {
            self.layout.changes.open = open;
            tracing::info!(open, "changes panel");
            self.save_changes_layout(window, cx);
            cx.notify();
        }
    }

    pub(super) fn toggle_changes(
        &mut self,
        _: &ToggleChanges,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.toggle_panel(window, cx);
    }

    /// The pane header button: shows the panel for this pane, or hides it if it already shows
    /// this pane.
    pub(super) fn toggle_panel_for(
        &mut self,
        pane: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let showing_this = self.open_panel_tab().is_some() && self.layout.panes.focused() == pane;
        self.focus_pane(pane, window, cx);
        if showing_this {
            self.hide_panel(window, cx);
        } else if self.open_panel_tab().is_none() {
            self.show_panel_tab(self.layout.panel_tab, window, cx);
        }
    }

    pub(super) fn save_changes_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changes = self.layout.changes;
        self.persist(window, cx, move |store| ops::save_changes(store, &changes));
    }

    /// Resizes the right panel (all its tabs), leaving the panes their minimum width.
    pub(super) fn resize_panel(&mut self, width: f32, window: &mut Window, cx: &mut Context<Self>) {
        let sidebar = if self.layout.sidebar_open {
            SIDEBAR_WIDTH
        } else {
            0.
        };
        let max = (window.viewport_size().width.as_f32() - sidebar - MIN_PANES_WIDTH)
            .max(PANEL_WIDTH_RANGE.0);
        let Some(width) = panel_width(width.min(max)) else {
            return;
        };
        if (width - self.layout.panel_width).abs() < 0.5 {
            return;
        }
        self.layout.panel_width = width;
        cx.notify();
        self.save_layout(window, cx);
    }

    pub(super) fn set_device_open(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if open && self.layout.changes.open {
            self.set_changes_open(false, window, cx);
        }
        if open {
            self.layout.files_open = false;
            self.layout.panel_tab = PanelTab::Device;
        }
        if self.layout.device.open != open {
            self.layout.device.open = open;
            tracing::info!(open, "device panel");
            self.save_device_layout(window, cx);
            cx.notify();
        }
        self.device_panel
            .update(cx, |panel, cx| panel.set_visible(open, window, cx));
    }

    /// Shows the device tab, or hides the panel if the device tab is showing.
    pub(super) fn toggle_device(
        &mut self,
        _: &ToggleDevice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.layout.device.open {
            self.hide_panel(window, cx);
        } else {
            self.set_device_open(true, window, cx);
        }
    }

    pub(super) fn toggle_device_stats(
        &mut self,
        _: &ToggleDeviceStats,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.device_panel.update(cx, |panel, cx| {
            let stats = !panel.stats();
            panel.set_stats(stats, cx);
        });
    }

    pub(super) fn save_device_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let device = self.layout.device.clone();
        self.persist(window, cx, move |store| ops::save_device(store, &device));
    }

    /// Shows the saved workspaces (without starting agents) and re-applies their sizes once the
    /// pane groups have been laid out.
    pub(super) fn restore_layout(
        &mut self,
        layout: &PaneLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let projects = &self.work.projects;
        self.layout.panes = layout.restore(|id| projects.is_active(id));
        self.layout.panel_width = layout.panel_width();
        self.apply_pane_shares(window, cx);
    }

    /// Adds a column right of the focused pane's one, holding a new, empty pane that gets focus.
    pub(super) fn split_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.panes.split().is_none() {
            tracing::info!("not splitting: {MAX_PANES} panes already");
            return;
        }
        self.panes_rearranged(window, cx);
    }

    /// Adds a new, empty pane below the focused one and focuses it.
    pub(super) fn split_pane_down(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.panes.split_down().is_none() {
            tracing::info!("not splitting: {MAX_PANES} panes already");
            return;
        }
        self.panes_rearranged(window, cx);
    }

    /// Closes a pane; its workspace keeps running and can be shown again from the sidebar. Its
    /// space goes to its neighbours.
    pub(super) fn close_pane(&mut self, pane: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.layout.panes.close(pane) {
            self.panes_rearranged(window, cx);
        }
    }

    /// Focuses pane `pane` (⌘⌥1…), if there is one.
    pub(super) fn focus_pane_number(
        &mut self,
        pane: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if pane < self.layout.panes.count() {
            self.focus_pane(pane, window, cx);
            self.focus_terminal(window, cx);
        }
    }

    fn panes_rearranged(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_terminal(window, cx);
        self.panes_changed(window, cx);
        self.apply_pane_shares(window, cx);
        cx.notify();
    }

    /// Fresh resizable states for the columns and for each column's panes, saving the shares
    /// whenever the user resizes them.
    pub(super) fn new_pane_groups(
        columns: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaneGroups {
        let column_sizes = cx.new(|_| ResizableState::default());
        let mut subscriptions = vec![cx.subscribe_in(
            &column_sizes,
            window,
            |this, sizes, _: &ResizablePanelEvent, window, cx| {
                let sizes = sizes.read(cx);
                let widths: Vec<f32> = sizes.sizes().iter().map(|s| s.as_f32()).collect();
                let shares = pane_shares(&widths, sizes.container_size().as_f32());
                if this.layout.panes.set_column_shares(shares) {
                    this.save_layout(window, cx);
                }
            },
        )];
        let mut row_sizes = Vec::with_capacity(columns);
        for column in 0..columns {
            let rows = cx.new(|_| ResizableState::default());
            subscriptions.push(cx.subscribe_in(
                &rows,
                window,
                move |this, sizes, _: &ResizablePanelEvent, window, cx| {
                    let sizes = sizes.read(cx);
                    let heights: Vec<f32> = sizes.sizes().iter().map(|s| s.as_f32()).collect();
                    let shares = pane_shares(&heights, sizes.container_size().as_f32());
                    if this.layout.panes.set_row_shares(column, shares) {
                        this.save_layout(window, cx);
                    }
                },
            ));
            row_sizes.push(rows);
        }
        PaneGroups {
            columns: column_sizes,
            rows: row_sizes,
            _subscriptions: subscriptions,
        }
    }

    /// Resizes the pane groups to the current shares once they have as many panels as panes and
    /// fill their space (new panels are only measured on the next layout).
    fn apply_pane_shares(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A group state pins the sizes it measured and appends or truncates panels when the
        // count changes, so its old sizes never fit again: start from fresh, evenly laid out panels.
        let panes = &self.layout.panes;
        let groups = Self::new_pane_groups(panes.column_count(), window, cx);
        let mut pending: Vec<(Entity<ResizableState>, Vec<f32>)> = Vec::new();
        if panes.column_count() > 1 && !panes.shares().columns.is_empty() {
            pending.push((groups.columns.clone(), panes.column_shares()));
        }
        for (column, rows) in groups.rows.iter().enumerate() {
            if panes
                .shares()
                .rows
                .get(column)
                .is_some_and(|r| !r.is_empty())
            {
                pending.push((rows.clone(), panes.row_shares(column)));
            }
        }
        self.layout.pane_groups = groups;
        if pending.is_empty() {
            self.layout.apply_shares = None;
            return;
        }
        self.layout.apply_shares = Some(cx.spawn_in(window, async move |this, cx| {
            for _ in 0..40 {
                let done = this
                    .update_in(cx, |_, window, cx| {
                        pending.retain(|(group, shares)| {
                            !group.update(cx, |sizes, cx| apply_shares(sizes, shares, window, cx))
                        });
                        pending.is_empty()
                    })
                    .unwrap_or(true);
                if done {
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
            }
            tracing::warn!("pane sizes not applied: panes were never laid out");
        }));
    }
}

/// The resizable states of the pane columns and of each column's panes.
pub(super) struct PaneGroups {
    pub(super) columns: Entity<ResizableState>,
    /// One per column.
    pub(super) rows: Vec<Entity<ResizableState>>,
    _subscriptions: Vec<Subscription>,
}

/// Resizes a laid out group to `shares`. Returns `false` while it is not laid out yet.
fn apply_shares(
    sizes: &mut ResizableState,
    shares: &[f32],
    window: &mut Window,
    cx: &mut Context<ResizableState>,
) -> bool {
    let total = sizes.container_size().as_f32();
    let laid_out: f32 = sizes.sizes().iter().map(|s| s.as_f32()).sum();
    if sizes.sizes().len() != shares.len() || total <= 0. || (laid_out - total).abs() > 1. {
        return false;
    }
    for (ix, share) in shares.iter().enumerate().take(shares.len() - 1) {
        sizes.resize_panel(ix, px(total * share), window, cx);
    }
    tracing::debug!(?shares, sizes = ?sizes.sizes(), "pane sizes applied");
    true
}
