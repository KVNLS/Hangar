//! The main window: project sidebar and two tiled terminal panes.

mod sidebar;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::anyhow;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::sidebar::{Sidebar, SidebarCollapsible};
use gpui_kit::component::{
    ActiveTheme as _, IconName, Sizable as _, StyledExt as _, WindowExt as _, h_flex, h_resizable,
    resizable_panel, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_agents::AgentHandle;
use hangar_core::{Project, ProjectId, Session, SessionStatus, Store, WorkspaceId};
use hangar_terminal::ExitInfo;

use crate::agents::{self, AgentChoice};
use crate::login_env::{self, PathSource, ResolvedPath};
use crate::ops::{self, StartedSession};
use crate::paths;
use crate::state::{PANE_COUNT, Panes, ProjectEntry, ProjectList, status_label};
use crate::terminal_view::{TerminalEvent, TerminalView, TerminalViewOptions, TerminalWaker};
use sidebar::{ProjectSection, WorkspaceRow};

/// Debug/automation hooks, read once at startup. `HANGAR_DEV_OPEN=<repo>` adds the project and
/// creates one workspace running `HANGAR_DEV_AGENT` (default `shell`), then types
/// `HANGAR_DEV_INPUT` (followed by Enter) into it.
#[derive(Clone, Debug, Default)]
pub struct DevOptions {
    pub open: Option<PathBuf>,
    pub workspace: String,
    pub agent: String,
    pub input: Option<String>,
}

impl DevOptions {
    pub fn from_env() -> Self {
        let var = |key: &str| std::env::var(key).ok().filter(|v| !v.is_empty());
        Self {
            open: var("HANGAR_DEV_OPEN").map(PathBuf::from),
            workspace: var("HANGAR_DEV_WORKSPACE").unwrap_or_else(|| "dev".to_owned()),
            agent: var("HANGAR_DEV_AGENT").unwrap_or_else(|| agents::FALLBACK_AGENT.to_owned()),
            input: var("HANGAR_DEV_INPUT"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct AgentOption {
    id: SharedString,
    title: SharedString,
}

impl SelectItem for AgentOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

type AgentSelect = SelectState<Vec<AgentOption>>;

struct LiveSession {
    session: Session,
    agent_name: SharedString,
    /// Owns the process; dropping it kills the program.
    handle: AgentHandle,
    view: Entity<TerminalView>,
    _exit: Subscription,
}

struct NewWorkspaceInput {
    project_id: ProjectId,
    input: Entity<InputState>,
    _submit: Subscription,
}

pub struct HangarApp {
    store: Option<Arc<Store>>,
    projects: ProjectList,
    loading: bool,
    load_error: Option<SharedString>,
    path: Option<ResolvedPath>,
    agent_select: Entity<AgentSelect>,
    agent: SharedString,
    agent_chosen_by_user: bool,
    sessions: HashMap<WorkspaceId, LiveSession>,
    panes: Panes,
    new_workspace: Option<NewWorkspaceInput>,
    creating: HashSet<ProjectId>,
    _subscriptions: Vec<Subscription>,
}

impl HangarApp {
    pub fn new(dev: DevOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Install state is unknown until the login PATH arrives; list names only for now.
        let options = agent_options(
            &hangar_agents::presets()
                .into_iter()
                .map(|a| AgentChoice {
                    id: a.id,
                    name: a.display_name,
                    installed: true,
                })
                .collect::<Vec<_>>(),
        );
        let agent: SharedString = agents::FALLBACK_AGENT.into();
        let selected = options.iter().position(|o| o.id == agent);
        let agent_select = cx.new(|cx| {
            SelectState::new(
                options,
                selected.map(|row| gpui_kit::component::IndexPath::default().row(row)),
                window,
                cx,
            )
        });
        let select_sub = cx.subscribe_in(
            &agent_select,
            window,
            |this, _, event: &SelectEvent<Vec<AgentOption>>, _, cx| {
                let SelectEvent::Confirm(Some(id)) = event else {
                    return;
                };
                this.agent = id.clone();
                this.agent_chosen_by_user = true;
                cx.notify();
            },
        );

        let db = paths::db_path();
        cx.spawn_in(window, async move |this, cx| {
            let executor = cx.background_executor().clone();
            let path_task = executor.spawn(async { login_env::resolve() });
            let loaded = executor
                .spawn(async move {
                    let db = db.ok_or_else(|| {
                        anyhow!("cannot locate the database: HOME is not set (set HANGAR_DB)")
                    })?;
                    let store = ops::open_store(&db)?;
                    let entries = ops::load(&store)?;
                    tracing::info!(db = %db.display(), projects = entries.len(), "loaded store");
                    anyhow::Ok((store, entries))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| this.on_loaded(loaded, window, cx));
            let path = path_task.await;
            let _ = this.update_in(cx, |this, window, cx| this.on_path(path, window, cx));
            if let Some(repo) = dev.open.clone() {
                let _ = this.update_in(cx, |this, window, cx| this.dev_open(repo, dev, window, cx));
            }
        })
        .detach();

        Self {
            store: None,
            projects: ProjectList::default(),
            loading: true,
            load_error: None,
            path: None,
            agent_select,
            agent,
            agent_chosen_by_user: false,
            sessions: HashMap::new(),
            panes: Panes::default(),
            new_workspace: None,
            creating: HashSet::new(),
            _subscriptions: vec![select_sub],
        }
    }

    fn report(&mut self, err: &anyhow::Error, window: &mut Window, cx: &mut Context<Self>) {
        tracing::error!("{err:#}");
        window.push_notification(Notification::error(format!("{err:#}")), cx);
    }

    fn on_loaded(
        &mut self,
        loaded: anyhow::Result<(Arc<Store>, Vec<ProjectEntry>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.loading = false;
        match loaded {
            Ok((store, entries)) => {
                self.store = Some(store);
                self.projects = ProjectList::new(entries);
            }
            Err(err) => {
                self.load_error = Some(format!("{err:#}").into());
                self.report(&err, window, cx);
            }
        }
        cx.notify();
    }

    fn on_path(&mut self, path: ResolvedPath, window: &mut Window, cx: &mut Context<Self>) {
        if let PathSource::Process(reason) = &path.source {
            window.push_notification(
                Notification::warning(format!(
                    "Could not read PATH from the login shell; agents may not be found. {reason}"
                )),
                cx,
            );
        }
        // find_program stats files; cheap enough for five presets but keep it off this thread.
        let value = path.value.clone();
        self.path = Some(path);
        cx.spawn_in(window, async move |this, cx| {
            let choices = cx
                .background_executor()
                .spawn(async move { agents::choices(Some(&value)) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.set_agent_choices(choices, window, cx)
            });
        })
        .detach();
    }

    fn set_agent_choices(
        &mut self,
        choices: Vec<AgentChoice>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.agent_chosen_by_user {
            self.agent = agents::default_agent(&choices).into();
        }
        let installed: Vec<&str> = choices
            .iter()
            .filter(|c| c.installed)
            .map(|c| c.id.as_str())
            .collect();
        tracing::info!(?installed, agent = %self.agent, "detected agents");
        let agent = self.agent.clone();
        self.agent_select.update(cx, |select, cx| {
            select.set_items(agent_options(&choices), window, cx);
            select.set_selected_value(&agent, window, cx);
        });
        cx.notify();
    }

    fn pick_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add Project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(err)) => {
                    let _ = this.update_in(cx, |this, window, cx| this.report(&err, window, cx));
                    None
                }
            };
            if let Some(path) = path {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.add_project(path, window, cx).detach();
                });
            }
        })
        .detach();
    }

    fn add_project(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Project>> {
        let Some(store) = self.store.clone() else {
            self.report(&anyhow!("the database is not open"), window, cx);
            return Task::ready(None);
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ops::add_project(&store, &path) })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(project) => {
                    tracing::info!(name = %project.name, repo = %project.repo_path.display(), "project added");
                    this.projects.add_project(project.clone());
                    cx.notify();
                    Some(project)
                }
                Err(err) => {
                    this.report(&err, window, cx);
                    None
                }
            })
            .ok()
            .flatten()
        })
    }

    fn open_new_workspace(
        &mut self,
        project_id: ProjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Workspace name"));
        let submit = cx.subscribe_in(
            &input,
            window,
            move |this, input, event: &InputEvent, window, cx| {
                if !matches!(event, InputEvent::PressEnter { .. }) {
                    return;
                }
                let name = input.read(cx).value().trim().to_owned();
                if name.is_empty() {
                    return;
                }
                this.new_workspace = None;
                let agent = this.agent.to_string();
                this.create_workspace(project_id, name, agent, window, cx)
                    .detach();
                cx.notify();
            },
        );
        input.update(cx, |state, cx| state.focus(window, cx));
        self.new_workspace = Some(NewWorkspaceInput {
            project_id,
            input,
            _submit: submit,
        });
        cx.notify();
    }

    fn close_new_workspace(&mut self, cx: &mut Context<Self>) {
        self.new_workspace = None;
        cx.notify();
    }

    /// Creates the worktree and workspace off the main thread, shows it in the focused pane and
    /// starts `agent_id` in it. Resolves once the agent has started (or failed).
    fn create_workspace(
        &mut self,
        project_id: ProjectId,
        name: String,
        agent_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<WorkspaceId>> {
        let prepared = (|| {
            let store = self
                .store
                .clone()
                .ok_or_else(|| anyhow!("the database is not open"))?;
            let project = self
                .projects
                .project(project_id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown project"))?;
            let root = paths::worktrees_root()
                .ok_or_else(|| anyhow!("cannot locate the worktree directory: HOME is not set"))?;
            anyhow::Ok((store, project, root))
        })();
        let (store, project, root) = match prepared {
            Ok(prepared) => prepared,
            Err(err) => {
                self.report(&err, window, cx);
                return Task::ready(None);
            }
        };
        self.creating.insert(project_id);
        cx.notify();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ops::create_workspace(&store, &project, &name, &root) })
                .await;
            let started = this
                .update_in(cx, |this, window, cx| {
                    this.creating.remove(&project_id);
                    cx.notify();
                    match result {
                        Ok(workspace) => {
                            tracing::info!(
                                name = %workspace.name,
                                branch = %workspace.branch,
                                path = %workspace.worktree_path.display(),
                                "workspace created"
                            );
                            let id = workspace.id;
                            this.projects.upsert_workspace(workspace);
                            this.panes.show(id);
                            Some((id, this.start_session(id, agent_id, window, cx)))
                        }
                        Err(err) => {
                            this.report(&err, window, cx);
                            None
                        }
                    }
                })
                .ok()
                .flatten();
            let (id, start) = started?;
            start.await;
            Some(id)
        })
    }

    /// Spawns the agent off the main thread and attaches a terminal view to it. Resolves to
    /// whether it started.
    fn start_session(
        &mut self,
        workspace_id: WorkspaceId,
        agent_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let prepared = (|| {
            let store = self
                .store
                .clone()
                .ok_or_else(|| anyhow!("the database is not open"))?;
            let workspace = self
                .projects
                .workspace(workspace_id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown workspace"))?;
            let adapter =
                agents::preset(&agent_id).ok_or_else(|| anyhow!("unknown agent `{agent_id}`"))?;
            anyhow::Ok((store, workspace, adapter))
        })();
        let (store, workspace, adapter) = match prepared {
            Ok(prepared) => prepared,
            Err(err) => {
                self.report(&err, window, cx);
                return Task::ready(false);
            }
        };
        let path = self.path.as_ref().map(|p| p.value.clone());
        let waker = TerminalWaker::new();
        let hook = waker.hook();
        let agent_name: SharedString = adapter.display_name.clone().into();

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    ops::start_session(&store, &adapter, &workspace, path.as_deref(), hook)
                })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(started) => {
                    this.attach_session(started, agent_name, waker, window, cx);
                    true
                }
                Err(err) => {
                    this.report(&err, window, cx);
                    false
                }
            })
            .unwrap_or(false)
        })
    }

    fn attach_session(
        &mut self,
        started: StartedSession,
        agent_name: SharedString,
        waker: TerminalWaker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let StartedSession { session, handle } = started;
        let workspace_id = session.workspace_id;
        tracing::info!(
            workspace = %workspace_id,
            session = %session.id,
            program = %handle.program().display(),
            pid = ?handle.session().pid(),
            "session started"
        );
        let terminal = Arc::clone(handle.session());
        let view =
            cx.new(|cx| TerminalView::attach(terminal, waker, TerminalViewOptions::default(), cx));
        let exit = cx.subscribe_in(
            &view,
            window,
            move |this, _, event: &TerminalEvent, window, cx| match event {
                TerminalEvent::Exited(exit) => this.on_session_exit(workspace_id, exit, window, cx),
            },
        );
        self.sessions.insert(
            workspace_id,
            LiveSession {
                session,
                agent_name,
                handle,
                view,
                _exit: exit,
            },
        );
        if self.panes.slot(self.panes.focused()) == Some(workspace_id) {
            self.focus_terminal(window, cx);
        }
        cx.notify();
    }

    fn on_session_exit(
        &mut self,
        workspace_id: WorkspaceId,
        exit: &Result<ExitInfo, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let status = ops::exit_status(exit);
        let Some(live) = self.sessions.get_mut(&workspace_id) else {
            return;
        };
        tracing::info!(workspace = %workspace_id, session = %live.session.id, ?status, "session ended");
        live.session.status = status.clone();
        let session_id = live.session.id;
        cx.notify();
        let Some(store) = self.store.clone() else {
            return;
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ops::finish_session(&store, session_id, &status) })
                .await;
            if let Err(err) = result {
                let _ = this.update_in(cx, |this, window, cx| this.report(&err, window, cx));
            }
        })
        .detach();
    }

    fn select_workspace(
        &mut self,
        workspace_id: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.panes.show(workspace_id);
        self.focus_terminal(window, cx);
        cx.notify();
    }

    fn focus_pane(&mut self, pane: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.panes.focused() != pane {
            self.panes.focus(pane);
            self.focus_terminal(window, cx);
            cx.notify();
        }
    }

    fn focus_terminal(&self, window: &mut Window, cx: &mut Context<Self>) {
        let live = self
            .panes
            .slot(self.panes.focused())
            .and_then(|id| self.sessions.get(&id));
        if let Some(live) = live {
            let handle = live.view.focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    fn dev_open(
        &mut self,
        repo: PathBuf,
        dev: DevOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::info!(repo = %repo.display(), "HANGAR_DEV_OPEN");
        let add = self.add_project(ops::canonical(&repo), window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let project = add.await?;
            let create = this
                .update_in(cx, |this, window, cx| {
                    this.create_workspace(project.id, dev.workspace, dev.agent, window, cx)
                })
                .ok()?;
            let workspace_id = create.await?;
            if let Some(input) = dev.input {
                this.update(cx, |this, _| {
                    let Some(live) = this.sessions.get(&workspace_id) else {
                        return;
                    };
                    if let Err(err) = live.handle.session().write(format!("{input}\r")) {
                        tracing::error!(%err, "HANGAR_DEV_INPUT: write failed");
                    }
                })
                .ok()?;
            }
            Some(())
        })
        .detach();
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let app = cx.entity().downgrade();
        let focused_workspace = self.panes.slot(self.panes.focused());
        let sections = self.projects.entries().iter().map(|entry| {
            let project_id = entry.project.id;
            ProjectSection {
                app: app.clone(),
                project_id,
                name: entry.project.name.clone().into(),
                workspaces: entry
                    .workspaces
                    .iter()
                    .map(|ws| WorkspaceRow {
                        id: ws.id,
                        name: ws.name.clone().into(),
                        status: self
                            .sessions
                            .get(&ws.id)
                            .map(|live| short_status(&live.session.status).into()),
                        active: focused_workspace == Some(ws.id),
                    })
                    .collect(),
                input: self
                    .new_workspace
                    .as_ref()
                    .filter(|n| n.project_id == project_id)
                    .map(|n| n.input.clone()),
                creating: self.creating.contains(&project_id),
                collapsed: false,
            }
        });

        let header = h_flex()
            .w_full()
            .justify_between()
            .items_center()
            .child(div().font_semibold().child("Hangar"))
            .child(
                Button::new("add-project")
                    .ghost()
                    .small()
                    .icon(IconName::FolderOpen)
                    .label("Add project")
                    .on_click(cx.listener(|this, _, window, cx| this.pick_project(window, cx))),
            );

        let notice = if self.loading {
            Some(SharedString::from("Loading…"))
        } else if let Some(err) = &self.load_error {
            Some(err.clone())
        } else if self.projects.is_empty() {
            Some("No projects yet. Add a Git repository to start.".into())
        } else {
            None
        };

        let footer = v_flex()
            .w_full()
            .gap_1()
            .when_some(notice, |this, notice| {
                let color = if self.load_error.is_some() {
                    cx.theme().danger
                } else {
                    cx.theme().muted_foreground
                };
                this.child(div().text_xs().text_color(color).child(notice))
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("Agent for new workspaces"),
            )
            .child(Select::new(&self.agent_select).small());

        Sidebar::new("projects")
            .collapsible(SidebarCollapsible::None)
            .w(px(260.))
            .header(header)
            .children(sections)
            .footer(footer)
    }

    fn render_pane(&self, pane: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let focused = self.panes.focused() == pane;
        let workspace_id = self.panes.slot(pane);
        let workspace = workspace_id.and_then(|id| self.projects.workspace(id));
        let live = workspace_id.and_then(|id| self.sessions.get(&id));

        let header = h_flex()
            .h_8()
            .px_2()
            .gap_3()
            .flex_shrink_0()
            .items_center()
            .text_xs()
            .border_b_1()
            .border_color(theme.border)
            .when(focused, |this| this.bg(theme.secondary))
            .text_color(theme.muted_foreground);
        let header = match workspace {
            None => header.child("Empty pane"),
            Some(ws) => {
                let (agent, status, color) = match live {
                    Some(live) => (
                        live.agent_name.clone(),
                        status_label(&live.session.status),
                        match live.session.status {
                            SessionStatus::Running | SessionStatus::Starting => theme.success,
                            SessionStatus::Exited { code: 0 } => theme.muted_foreground,
                            _ => theme.danger,
                        },
                    ),
                    None => (
                        "no agent".into(),
                        "not running".to_owned(),
                        theme.muted_foreground,
                    ),
                };
                let can_start = live.is_none_or(|l| l.session.status.is_terminal());
                let ws_id = ws.id;
                let label = if live.is_some() { "Restart" } else { "Start" };
                header
                    .child(
                        div()
                            .font_semibold()
                            .text_color(theme.foreground)
                            .child(ws.name.clone()),
                    )
                    .child(ws.branch.clone())
                    .child(agent)
                    .child(div().text_color(color).child(status))
                    .child(div().flex_1())
                    .when(can_start, |this| {
                        this.child(
                            Button::new(("start", pane))
                                .ghost()
                                .xsmall()
                                .icon(IconName::Play)
                                .label(format!("{label} {}", self.agent_display_name()))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let agent = this.agent.to_string();
                                    this.panes.show(ws_id);
                                    this.start_session(ws_id, agent, window, cx).detach();
                                })),
                        )
                    })
            }
        };

        let body = div().flex_1().min_h_0().w_full();
        let body = match live {
            Some(live) => body.child(live.view.clone()),
            None => body
                .flex()
                .items_center()
                .justify_center()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child(if workspace.is_some() {
                    "No agent running in this workspace."
                } else {
                    "Select a workspace in the sidebar to show it here."
                }),
        };

        v_flex()
            .id(("pane", pane))
            .size_full()
            .border_1()
            .border_color(if focused { theme.ring } else { theme.border })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.focus_pane(pane, window, cx);
                }),
            )
            .child(header)
            .child(body)
    }

    fn agent_display_name(&self) -> String {
        agents::preset(&self.agent)
            .map(|a| a.display_name)
            .unwrap_or_else(|| self.agent.to_string())
    }
}

impl Render for HangarApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panes = h_resizable("panes");
        for pane in 0..PANE_COUNT {
            panes = panes.child(resizable_panel().child(self.render_pane(pane, cx)));
        }
        h_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_sidebar(cx))
            .child(div().flex_1().min_w_0().h_full().child(panes))
    }
}

fn agent_options(choices: &[AgentChoice]) -> Vec<AgentOption> {
    choices
        .iter()
        .map(|c| AgentOption {
            id: c.id.clone().into(),
            title: if c.installed {
                c.name.clone().into()
            } else {
                format!("{} (not found)", c.name).into()
            },
        })
        .collect()
}

fn short_status(status: &SessionStatus) -> String {
    match status {
        SessionStatus::Failed { .. } => "failed".to_owned(),
        other => status_label(other),
    }
}
