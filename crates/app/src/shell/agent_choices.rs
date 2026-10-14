//! Which agent new sessions start: PATH, install state, choices and prefill.

use super::*;

impl HangarApp {
    pub(super) fn workspace_agent(&self, ws: WorkspaceId) -> Option<WorkspaceAgent> {
        let live = self.live.sessions.get(&ws)?;
        Some(WorkspaceAgent {
            name: live.agent_name.clone(),
            kind: live.session.agent.clone(),
            live: !live.session.status.is_terminal(),
        })
    }

    /// Shows the workspace's agent with `text` typed into its input, not sent.
    pub(super) fn prefill_agent(
        &mut self,
        ws: WorkspaceId,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_screen(Screen::Workspace, window, cx);
        self.show_pane_tab(ws, run::PaneTab::Agent, window, cx);
        let Some(live) = self
            .live
            .sessions
            .get(&ws)
            .filter(|l| !l.session.status.is_terminal())
        else {
            window.push_notification(
                Notification::warning("The workspace's agent is not running"),
                cx,
            );
            return;
        };
        if let Err(err) = live.prefill(text, window, cx) {
            tracing::warn!(%err, workspace = %ws, "could not type into the agent");
            window.push_notification(
                Notification::error(format!("Could not type into the agent: {err:#}")),
                cx,
            );
        }
    }

    pub(super) fn save_agent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let agent = self.agent_setup.agent.to_string();
        self.persist(window, cx, move |store| ops::save_agent(store, &agent));
    }

    pub(super) fn on_path(
        &mut self,
        path: ResolvedPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let PathSource::Process(reason) = &path.source {
            window.push_notification(
                Notification::warning(format!(
                    "Could not read PATH from the login shell; agents may not be found. {reason}"
                )),
                cx,
            );
        }
        self.use_path(path, window, cx);
    }

    /// Applies a resolved `PATH`: `gh`, agent install state and the setup checklist follow it.
    pub(super) fn use_path(
        &mut self,
        path: ResolvedPath,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let gh = crate::adapters::code_host(
            path.value.clone(),
            std::env::var_os("HANGAR_GH").filter(|p| !p.is_empty()),
        );
        self.changes_panel
            .update(cx, |panel, cx| panel.set_gh(gh, cx));
        self.agent_setup.path = Some(path);
        // Starts restored from the last run waited for the PATH their agents are found on.
        if !self.work.loading {
            self.drain_agent_queue(window, cx);
        }
        self.refresh_agent_choices(window, cx);
        self.check_setup(cx);
    }

    pub(super) fn refresh_agent_choices(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(value) = self.agent_setup.path.as_ref().map(|p| p.value.clone()) else {
            return;
        };
        // find_program stats files; cheap enough for five presets but keep it off this thread.
        cx.spawn_in(window, async move |this, cx| {
            let choices = cx
                .background_executor()
                .spawn(async move { agents::choices(Some(&value)) })
                .await;
            let _ = this.update(cx, |this, cx| this.set_agent_choices(choices, cx));
        })
        .detach();
    }

    fn set_agent_choices(&mut self, choices: Vec<AgentChoice>, cx: &mut Context<Self>) {
        if !self.agent_setup.agent_chosen_by_user {
            self.agent_setup.agent = agents::default_agent(&choices).into();
        }
        let installed: Vec<&str> = choices
            .iter()
            .filter(|c| c.installed)
            .map(|c| c.id.as_str())
            .collect();
        tracing::info!(?installed, agent = %self.agent_setup.agent, "detected agents");
        self.agent_setup.agent_choices = choices;
        cx.notify();
    }
}
