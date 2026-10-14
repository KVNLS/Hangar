//! Changes/device panel sizing and pane layout persistence.

use super::*;

impl HangarApp {
    pub(super) fn save_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let layout = self.panes.layout(self.split);
        self.persist(window, cx, move |store| ops::save_layout(store, &layout));
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
        let projects = &self.projects;
        let busy = &self.busy;
        self.diffs
            .retain(|id, _| projects.is_active(*id) && !busy.contains(id));
        for pane in 0..PANE_COUNT {
            let Some(id) = self.panes.slot(pane) else {
                continue;
            };
            if self.diffs.contains_key(&id) || self.busy.contains(&id) {
                continue;
            }
            let Some(ws) = self
                .projects
                .workspace(id)
                .filter(|_| self.projects.is_active(id))
            else {
                continue;
            };
            let (path, base) = (ws.worktree_path.clone(), ws.base.clone());
            let diff = cx.new(|cx| WorkspaceDiff::start(id, path, base, cx));
            let observe = cx.observe(&diff, |_, _, cx| cx.notify());
            self.diffs.insert(
                id,
                DiffEntry {
                    diff,
                    _observe: observe,
                },
            );
        }
        let source = self.panes.slot(self.panes.focused()).and_then(|id| {
            let entry = self.diffs.get(&id)?;
            let ws = self.projects.workspace(id)?;
            let project = self.projects.project(ws.project_id)?;
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
        if self.screen == screen {
            return;
        }
        self.screen = screen;
        let wide = screen == Screen::Review;
        self.changes_panel
            .update(cx, |panel, cx| panel.set_wide(wide, window, cx));
        // The device panel is only on the workspace screen: stop streaming while it is hidden.
        if self.device.open {
            let shown = screen == Screen::Workspace;
            self.device_panel
                .update(cx, |panel, cx| panel.set_visible(shown, window, cx));
        }
        if !wide {
            window.focus(&self.focus, cx);
            self.focus_terminal(window, cx);
        }
        cx.notify();
    }

    pub(super) fn toggle_overview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let screen = if self.screen == Screen::Overview {
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
        self.files_open = open;
        cx.notify();
    }

    pub(super) fn set_changes_open(
        &mut self,
        open: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Device, changes and files are tabs of one right panel.
        if open && self.device.open {
            self.set_device_open(false, window, cx);
        }
        if open {
            self.files_open = false;
        }
        if self.changes.open != open {
            self.changes.open = open;
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
        self.set_changes_open(!self.changes.open, window, cx);
    }

    /// The pane header button: opens the panel on this pane, or closes it if it already
    /// shows this pane.
    pub(super) fn toggle_changes_for(
        &mut self,
        pane: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let showing_this = self.changes.open && self.panes.focused() == pane;
        self.focus_pane(pane, window, cx);
        self.set_changes_open(!showing_this, window, cx);
    }

    pub(super) fn save_changes_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let changes = self.changes;
        self.persist(window, cx, move |store| ops::save_changes(store, &changes));
    }

    pub(super) fn device_width_on_screen(&self) -> f32 {
        if self.device.open {
            self.device.width
        } else {
            0.
        }
    }

    pub(super) fn changes_width_on_screen(&self) -> f32 {
        if self.changes.open {
            self.changes.width
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
        if (width - self.changes.width).abs() < 0.5 {
            return;
        }
        self.changes.width = width;
        cx.notify();
        // Dropping the previous task cancels it: only the last width is written.
        self.save_changes = Some(cx.spawn_in(window, async move |this, cx| {
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
        if open && self.changes.open {
            self.set_changes_open(false, window, cx);
        }
        if open {
            self.files_open = false;
        }
        if self.device.open != open {
            self.device.open = open;
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
        self.set_device_open(!self.device.open, window, cx);
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
        let device = self.device.clone();
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
        if (width - self.device.width).abs() < 0.5 {
            return;
        }
        self.device.width = width;
        cx.notify();
        self.save_device = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SAVE_DEBOUNCE).await;
            let _ = this.update_in(cx, |this, window, cx| this.save_device_layout(window, cx));
        }));
    }

    /// Shows the saved workspaces (without starting agents) and re-applies the split once the
    /// pane group has been laid out.
    pub(super) fn restore_layout(
        &mut self,
        layout: &PaneLayout,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let projects = &self.projects;
        self.panes = Panes::from_layout(layout, |id| projects.is_active(id));
        self.split = sanitize_split(layout.split);
        let Some(split) = self.split else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            for _ in 0..40 {
                let applied = this
                    .update_in(cx, |this, window, cx| {
                        this.pane_sizes.update(cx, |sizes, cx| {
                            let total = sizes.container_size();
                            if sizes.sizes().len() < PANE_COUNT || total.as_f32() <= 0. {
                                return false;
                            }
                            sizes.resize_panel(0, total * split, window, cx);
                            tracing::debug!(split, sizes = ?sizes.sizes(), "pane split restored");
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
            tracing::warn!("pane split not restored: panes were never laid out");
        })
        .detach();
    }
}
