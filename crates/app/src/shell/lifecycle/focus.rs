//! Selecting workspaces and focusing panes.

use super::*;

impl HangarApp {
    pub(in crate::shell) fn select_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout.panes.show(workspace_id);
        self.focus_terminal(window, cx);
        self.panes_changed(window, cx);
        cx.notify();
    }

    pub(in crate::shell) fn focus_pane(
        &mut self,
        pane: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.layout.panes.focused() != pane {
            self.layout.panes.focus(pane);
            self.focus_terminal(window, cx);
            self.panes_changed(window, cx);
            cx.notify();
        }
    }

    pub(in crate::shell) fn focus_terminal(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.layout.panes.slot(self.layout.panes.focused()) else {
            return;
        };
        let handle = match self.run_view(id, self.pane_tab(id)) {
            Some(view) => Some(view.focus_handle(cx)),
            None => self
                .live
                .sessions
                .get(&id)
                .map(|live| live.focus_handle(cx)),
        };
        if let Some(handle) = handle {
            window.focus(&handle, cx);
        }
    }

    /// Switches the workspace's pane between its agent and run terminals.
    pub(in crate::shell) fn show_pane_tab(
        &mut self,
        id: WorkspaceId,
        tab: run::PaneTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.live.pane_tabs.insert(id, tab);
        if let Some(pane) = self.layout.panes.pane_of(id) {
            self.focus_pane(pane, window, cx);
        }
        self.focus_terminal(window, cx);
        cx.notify();
    }

    /// The first active workspace with this slug, for dev hooks.
    pub(in crate::shell) fn workspace_by_slug(&self, slug: &str) -> Option<WorkspaceId> {
        self.work
            .projects
            .entries()
            .iter()
            .flat_map(|e| &e.workspaces)
            .find(|w| w.slug == slug)
            .map(|w| w.id)
    }
}
