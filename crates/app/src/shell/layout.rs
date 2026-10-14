//! Changes/device panel sizing and pane layout persistence.

use super::*;

impl HangarApp {
    /// Saves the pane layout once it has been quiet for [`SAVE_DEBOUNCE`] (pane drags and splits
    /// come in bursts).
    pub(super) fn save_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Dropping the previous task cancels it: only the last layout is written.
        self.layout.save_panes = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.layout.save_panes = None;
                let layout = this.layout.panes.layout(&this.layout.pane_shares);
                this.persist(window, cx, move |store| ops::save_layout(store, &layout));
            });
        }));
    }

    /// Writes a pending layout save now (the app is quitting).
    pub(super) fn flush_layout(&mut self) {
        if self.layout.save_panes.take().is_none() {
            return;
        }
        let Some(store) = self.store.clone() else {
            return;
        };
        if let Err(err) =
            ops::save_layout(&store, &self.layout.panes.layout(&self.layout.pane_shares))
        {
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
        self.set_changes_open(!self.layout.changes.open, window, cx);
    }

    /// The pane header button: opens the panel on this pane, or closes it if it already
    /// shows this pane.
    pub(super) fn toggle_changes_for(
        &mut self,
        pane: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let showing_this = self.layout.changes.open && self.layout.panes.focused() == pane;
        self.focus_pane(pane, window, cx);
        self.set_changes_open(!showing_this, window, cx);
    }

    pub(super) fn save_changes_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changes = self.layout.changes;
        self.persist(window, cx, move |store| ops::save_changes(store, &changes));
    }

    pub(super) fn device_width_on_screen(&self) -> f32 {
        if self.layout.device.open {
            self.layout.device.width
        } else {
            0.
        }
    }

    pub(super) fn changes_width_on_screen(&self) -> f32 {
        if self.layout.changes.open {
            self.layout.changes.width
        } else {
            0.
        }
    }

    pub(super) fn resize_changes(
        &mut self,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let max = (window.viewport_size().width.as_f32()
            - SIDEBAR_WIDTH
            - MIN_PANES_WIDTH
            - self.device_width_on_screen())
        .max(CHANGES_WIDTH_RANGE.0);
        let Some(width) = changes_width(width.min(max)) else {
            return;
        };
        if (width - self.layout.changes.width).abs() < 0.5 {
            return;
        }
        self.layout.changes.width = width;
        cx.notify();
        // Dropping the previous task cancels it: only the last width is written.
        self.layout.save_changes = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update_in(cx, |this, window, cx| this.save_changes_layout(window, cx));
        }));
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

    pub(super) fn toggle_device(
        &mut self,
        _: &ToggleDevice,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_device_open(!self.layout.device.open, window, cx);
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

    pub(super) fn resize_device(
        &mut self,
        width: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let max = (window.viewport_size().width.as_f32()
            - SIDEBAR_WIDTH
            - MIN_PANES_WIDTH
            - self.changes_width_on_screen())
        .max(DEVICE_WIDTH_RANGE.0);
        let Some(width) = device_width(width.min(max)) else {
            return;
        };
        if (width - self.layout.device.width).abs() < 0.5 {
            return;
        }
        self.layout.device.width = width;
        cx.notify();
        self.layout.save_device = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update_in(cx, |this, window, cx| this.save_device_layout(window, cx));
        }));
    }

    /// Shows the saved workspaces (without starting agents) and re-applies their widths once the
    /// pane group has been laid out.
    pub(super) fn restore_layout(
        &mut self,
        layout: &PaneLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let projects = &self.work.projects;
        let (panes, shares) = layout.restore(|id| projects.is_active(id));
        self.layout.panes = panes;
        self.layout.pane_shares = shares;
        self.apply_pane_shares(window, cx);
    }

    /// Splits the focused pane in two; the new, empty pane on its right gets focus and half its
    /// width.
    pub(super) fn split_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut shares = self.current_shares();
        let Some(pane) = self.layout.panes.split() else {
            tracing::info!("not splitting: {MAX_PANES} panes already");
            return;
        };
        let half = shares[pane - 1] / 2.;
        shares[pane - 1] = half;
        shares.insert(pane, half);
        self.layout.pane_shares = shares;
        self.focus_terminal(window, cx);
        self.panes_changed(window, cx);
        self.apply_pane_shares(window, cx);
        cx.notify();
    }

    /// Closes a pane; its workspace keeps running and can be shown again from the sidebar. Its
    /// width goes to its neighbours.
    pub(super) fn close_pane(&mut self, pane: usize, window: &mut Window, cx: &mut Context<Self>) {
        let mut shares = self.current_shares();
        if !self.layout.panes.close(pane) {
            return;
        }
        shares.remove(pane);
        self.layout.pane_shares = sanitize_shares(&shares, self.layout.panes.count());
        self.focus_terminal(window, cx);
        self.panes_changed(window, cx);
        self.apply_pane_shares(window, cx);
        cx.notify();
    }

    /// The pane group's resizable state, saving the panes' shares whenever the user resizes them.
    pub(super) fn new_pane_sizes(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<ResizableState>, Subscription) {
        let pane_sizes = cx.new(|_| ResizableState::default());
        let sub = cx.subscribe_in(
            &pane_sizes,
            window,
            |this, sizes, _: &ResizablePanelEvent, window, cx| {
                let sizes = sizes.read(cx);
                let widths: Vec<f32> = sizes.sizes().iter().map(|s| s.as_f32()).collect();
                let shares = pane_shares(&widths, sizes.container_size().as_f32());
                if shares.len() == this.layout.panes.count() {
                    this.layout.pane_shares = shares;
                    this.save_layout(window, cx);
                }
            },
        );
        (pane_sizes, sub)
    }

    /// The panes' width shares, equal when none were set.
    fn current_shares(&self) -> Vec<f32> {
        let count = self.layout.panes.count();
        if self.layout.pane_shares.len() == count {
            self.layout.pane_shares.clone()
        } else {
            vec![1. / count as f32; count]
        }
    }

    /// Resizes the pane group to the current shares once it has as many panels as panes and they
    /// fill its width (new panels are only measured on the next layout).
    fn apply_pane_shares(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The group state pins the sizes it measured and appends or truncates panels when the
        // count changes, so its old sizes never fit again: start from fresh, evenly laid out panels.
        let (sizes, sub) = Self::new_pane_sizes(window, cx);
        self.layout.pane_sizes = sizes;
        self.layout._pane_sizes_sub = sub;
        let count = self.layout.panes.count();
        if count < 2 {
            return;
        }
        let shares = self.current_shares();
        self.layout.apply_shares = Some(cx.spawn_in(window, async move |this, cx| {
            for _ in 0..40 {
                let applied = this
                    .update_in(cx, |this, window, cx| {
                        this.layout.pane_sizes.update(cx, |sizes, cx| {
                            let total = sizes.container_size().as_f32();
                            let laid_out: f32 = sizes.sizes().iter().map(|s| s.as_f32()).sum();
                            if sizes.sizes().len() != count
                                || total <= 0.
                                || (laid_out - total).abs() > 1.
                            {
                                return false;
                            }
                            for (ix, share) in shares.iter().enumerate().take(count - 1) {
                                sizes.resize_panel(ix, px(total * share), window, cx);
                            }
                            tracing::debug!(?shares, sizes = ?sizes.sizes(), "pane widths applied");
                            true
                        })
                    })
                    .unwrap_or(true);
                if applied {
                    return;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(50))
                    .await;
            }
            tracing::warn!("pane widths not applied: panes were never laid out");
        }));
    }
}
