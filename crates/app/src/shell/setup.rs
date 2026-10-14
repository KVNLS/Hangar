//! The setup checklist: probed once the login `PATH` is known, again on "Re-check", and when
//! Hangar comes back to the front while the checklist is on screen (the user may have just run
//! one of its commands in a terminal).

use std::time::Instant;

use hangar_usecases::setup::{self as setup_uc, SetupReport};

use super::*;

/// Coming back to the front re-checks at most this often.
const RECHECK_ON_ACTIVATE: Duration = Duration::from_secs(10);

#[derive(Default)]
pub(super) struct SetupState {
    pub(super) report: Option<SetupReport>,
    pub(super) checking: bool,
    checked_at: Option<Instant>,
}

impl HangarApp {
    /// Probes the tools with the current `PATH`, in the background.
    pub(super) fn check_setup(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.agent_setup.path.clone() else {
            return;
        };
        self.setup.checking = true;
        self.setup.checked_at = Some(Instant::now());
        cx.notify();
        let path_error = match path.source {
            PathSource::LoginShell => None,
            PathSource::Process(reason) => Some(reason),
        };
        let gh = crate::adapters::code_host(
            path.value.clone(),
            std::env::var_os("HANGAR_GH").filter(|p| !p.is_empty()),
        );
        cx.spawn(async move |this, cx| {
            let report = cx
                .background_executor()
                .spawn(async move {
                    let probe = crate::adapters::tool_probe(path.value, path_error, gh);
                    setup_uc::check(probe.as_ref())
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let failing: Vec<&str> = report.failing().iter().map(|c| c.id.as_str()).collect();
                tracing::info!(?failing, ready = report.is_ready(), "setup checked");
                this.setup.report = Some(report);
                this.setup.checking = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// "Re-check": reads the login shell's `PATH` again (installing Homebrew changes it), then
    /// refreshes `gh`, the agents and the checklist.
    pub(super) fn recheck_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.setup.checking {
            return;
        }
        self.setup.checking = true;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let path = cx
                .background_executor()
                .spawn(async { login_env::resolve() })
                .await;
            let _ = this.update_in(cx, |this, window, cx| this.use_path(path, window, cx));
        })
        .detach();
    }

    pub(super) fn open_setup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings_tab(settings::SettingsTab::Setup, window, cx);
        self.set_screen(Screen::Settings, window, cx);
    }

    pub(super) fn on_window_activation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let due = self
            .setup
            .checked_at
            .is_some_and(|at| at.elapsed() >= RECHECK_ON_ACTIVATE);
        if window.is_window_active() && due && !self.setup.checking && self.setup_on_screen() {
            self.refresh_agent_choices(window, cx);
            self.check_setup(cx);
        }
    }

    /// The welcome screen or Settings › Setup is showing.
    fn setup_on_screen(&self) -> bool {
        match self.layout.screen {
            Screen::Settings => self.layout.settings_tab == settings::SettingsTab::Setup,
            Screen::Workspace => self.showing_welcome(),
            _ => false,
        }
    }

    pub(super) fn showing_welcome(&self) -> bool {
        self.work.projects.is_empty() && !self.work.loading && self.work.load_error.is_none()
    }
}
