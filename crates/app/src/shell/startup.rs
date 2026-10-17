//! Building the window and applying what the database loaded.

use super::*;

impl HangarApp {
    pub fn new(dev: DevOptions, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Install state is unknown until the login PATH arrives; list names only for now.
        let initial_choices: Vec<AgentChoice> = agents::all_presets()
            .into_iter()
            .map(|a| AgentChoice {
                id: a.id().to_owned(),
                name: a.display_name().to_owned(),
                installed: true,
                path: None,
            })
            .collect();
        let agent: SharedString = agents::FALLBACK_AGENT.into();
        let pane_groups = Self::new_pane_groups(1, window, cx);

        let changes_panel = cx.new(ChangesPanel::new);
        let shell = cx.entity().downgrade();
        changes_panel.update(cx, |panel, _| {
            panel.set_agent_probe(Box::new(move |ws, cx| {
                shell.upgrade()?.read(cx).workspace_agent(ws)
            }))
        });
        let changes_sub = cx.subscribe_in(
            &changes_panel,
            window,
            |this, _, event: &ChangesEvent, window, cx| match event {
                ChangesEvent::ExitReview => this.set_screen(Screen::Workspace, window, cx),
                ChangesEvent::OpenReview => this.set_screen(Screen::Review, window, cx),
                ChangesEvent::OpenSetup => this.open_setup(window, cx),
                ChangesEvent::AskAgent { workspace, text } => {
                    this.prefill_agent(*workspace, text, window, cx)
                }
            },
        );
        let files_panel = cx.new(FilesPanel::new);
        let files_sub = cx.subscribe_in(
            &files_panel,
            window,
            |this, _, event: &FilesEvent, window, cx| match event {
                FilesEvent::OpenWide => this.set_screen(Screen::Files, window, cx),
                FilesEvent::ExitWide => this.set_screen(Screen::Workspace, window, cx),
            },
        );
        let device_pool = cx.new(DevicePool::new);
        let pool_sub = cx.subscribe_in(
            &device_pool,
            window,
            |this, _, event: &PoolEvent, window, cx| match event {
                PoolEvent::Notice(level, message) => {
                    let notification = match level {
                        NoticeLevel::Info => {
                            this.show_toast(toast::ToastKind::Info, message.clone(), cx);
                            return;
                        }
                        NoticeLevel::Warning => Notification::warning(message.clone()),
                        NoticeLevel::Error => Notification::error(message.clone()),
                    };
                    window.push_notification(notification, cx);
                }
                PoolEvent::AgentNotice { ws, message } => {
                    if let Some(mcp) = &this.mcp {
                        mcp.notify(*ws, message.clone());
                    }
                }
            },
        );
        let pool_observer = cx.observe(&device_pool, |_, _, cx| cx.notify());
        let device_panel = cx.new(|cx| DevicePanel::new(device_pool.clone(), window, cx));
        let device_sub = cx.subscribe_in(
            &device_panel,
            window,
            |this, _, event: &DevicePanelEvent, window, cx| match event {
                DevicePanelEvent::Screenshot(ws) => this.screenshot_to_agent(*ws, window, cx),
                DevicePanelEvent::Selected(key) => {
                    this.layout.device.selected = key.clone();
                    this.save_device_layout(window, cx);
                }
                DevicePanelEvent::Stats(stats) => {
                    this.layout.device.stats = *stats;
                    this.save_device_layout(window, cx);
                }
                DevicePanelEvent::TookOver(result) => match result {
                    Ok(true) => {
                        this.show_toast(toast::ToastKind::Success, "You have the device", cx)
                    }
                    Ok(false) => this.show_toast(
                        toast::ToastKind::Info,
                        "You are next: a call in progress finishes first",
                        cx,
                    ),
                    Err(err) => this.show_toast(
                        toast::ToastKind::Info,
                        format!("Could not take over: {err}"),
                        cx,
                    ),
                },
                DevicePanelEvent::SwitchFailed(err) => this.show_toast(
                    toast::ToastKind::Info,
                    format!("Could not switch platform: {err}"),
                    cx,
                ),
                DevicePanelEvent::Logs(logs) => {
                    this.layout.device.logs = logs.open;
                    this.layout.device.log_level = Some(logs.min_level.as_str().to_owned());
                    this.save_device_layout(window, cx);
                }
            },
        );
        let sidebar_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter workspaces"));
        let filter_sub = cx.subscribe(&sidebar_filter, |_, _, _: &InputEvent, cx| cx.notify());
        let (settings_ui, settings_sub) = settings::SettingsUi::new(window, cx);
        let activation_sub = cx.observe_window_activation(window, |this, window, cx| {
            this.on_window_activation(window, cx)
        });
        let appearance_observer = cx.observe_window_appearance(window, |this, window, cx| {
            if this.layout.appearance == crate::ui::theme::Appearance::System {
                this.layout.appearance.apply(window, cx);
            }
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);

        let (ollama, ollama_subs) = ollama::OllamaState::new(window, cx);
        let device_limit_override = dev.device_limit;
        let db = paths::db_path();
        cx.spawn_in(window, async move |this, cx| {
            let executor = cx.background_executor().clone();
            let path_task = executor.spawn(async { login_env::resolve() });
            let loaded = executor.spawn(async move { open_and_load(db) }).await;
            let _ = this.update_in(cx, |this, window, cx| this.on_loaded(loaded, window, cx));
            let path = path_task.await;
            let _ = this.update_in(cx, |this, window, cx| this.on_path(path, window, cx));
            run_dev_hooks(this, dev, cx).await;
        })
        .detach();

        Self {
            store: None,
            changes_panel,
            files_panel,
            devices: Devices {
                pool: device_pool,
                panel: device_panel,
                limit_override: device_limit_override,
            },
            mcp: mcp::start(window, cx),
            focus,
            layout: Layout {
                panes: Panes::default(),
                pane_groups,
                apply_shares: None,
                palette: None,
                overview_by_project: false,
                settings_tab: Default::default(),
                appearance: Default::default(),
                _appearance_observer: appearance_observer,
                changes: ChangesLayout::default(),
                files_open: false,
                terminal: TerminalPrefs::default(),
                device: DeviceLayout::default(),
                panel_width: PANEL_DEFAULT_WIDTH,
                panel_tab: Default::default(),
                save_panes: None,
                sidebar_open: true,
                sidebar_width: SIDEBAR_DEFAULT_WIDTH,
                screen: Screen::Workspace,
                screen_focus: cx.focus_handle(),
                sidebar_filter,
                collapsed_projects: HashSet::new(),
                show_archived: HashSet::new(),
                toast: None,
            },
            agent_setup: AgentSetup {
                path: None,
                agent,
                agent_chosen_by_user: false,
                agent_choices: initial_choices,
                ollama,
                orchestration: false,
            },
            setup: setup::SetupState::default(),
            live: LiveState {
                sessions: HashMap::new(),
                saved_chats: HashMap::new(),
                workspace_agents: HashMap::new(),
                runs: HashMap::new(),
                port_holds: HashMap::new(),
                pane_tabs: HashMap::new(),
                diffs: HashMap::new(),
                agent_queue: Default::default(),
                saved_queue: Vec::new(),
                questions: HashMap::new(),
                starting: HashSet::new(),
                done_today: Default::default(),
                attention: attention::Attention::new(true),
            },
            work: WorkState {
                projects: ProjectList::default(),
                loading: true,
                load_error: None,
                new_workspace: None,
                compare: None,
                answer: None,
                child_creates: HashMap::new(),
                creating: HashSet::new(),
                busy: HashSet::new(),
            },
            settings_ui,
            _activity: Self::track_activity(cx),
            _subscriptions: vec![
                // ACP agents run in their own process group and some (codex-acp) outlive a
                // closed stdin, so quitting must stop them like ⌘W does.
                cx.on_app_quit(|this, _| {
                    for live in this.live.sessions.values() {
                        live.stop();
                    }
                    // The app exits right after this, before a terminal agent that ignored
                    // the hang-up would be killed in the background: wait for them here.
                    let deadline = std::time::Instant::now() + QUIT_GRACE;
                    for live in this.live.sessions.values() {
                        if let AgentSurface::Terminal { handle, .. } = &live.surface {
                            let left =
                                deadline.saturating_duration_since(std::time::Instant::now());
                            handle.session().kill_now(left);
                        }
                    }
                    this.flush_layout();
                    async {}
                }),
                settings_sub,
                changes_sub,
                files_sub,
                device_sub,
                filter_sub,
                pool_sub,
                pool_observer,
                activation_sub,
            ]
            .into_iter()
            .chain(ollama_subs)
            .collect(),
        }
    }

    pub(super) fn on_loaded(
        &mut self,
        loaded: anyhow::Result<(Arc<dyn Store>, Loaded)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.work.loading = false;
        match loaded {
            Ok((store, loaded)) => {
                self.store = Some(store.clone());
                self.changes_panel
                    .update(cx, |panel, cx| panel.set_store(store.clone(), cx));
                self.work.projects = ProjectList::new(loaded.entries);
                self.live.saved_chats = loaded.chats;
                self.live.workspace_agents = loaded.workspace_agents;
                // Workspaces from before this was saved: the agent of their saved conversation.
                for (id, chat) in &self.live.saved_chats {
                    self.live
                        .workspace_agents
                        .entry(*id)
                        .or_insert_with(|| chat.agent.clone());
                }
                let workspaces: Vec<WorkspaceRef> = self
                    .work
                    .projects
                    .entries()
                    .iter()
                    .flat_map(|e| {
                        e.workspaces
                            .iter()
                            .map(move |w| workspace_ref(&e.project, w))
                    })
                    .collect();
                self.devices.load(store, workspaces, cx);
                if let Some(layout) = loaded.layout {
                    self.restore_layout(&layout, window, cx);
                }
                if let Some(changes) = loaded.changes {
                    self.layout.changes = changes;
                }
                if let Some(device) = loaded.device {
                    self.layout.device = device;
                    let (selected, stats) = (
                        self.layout.device.selected.clone(),
                        self.layout.device.stats,
                    );
                    let open = self.layout.device.open;
                    let logs = self.layout.device.logs_settings();
                    self.devices.panel.update(cx, |panel, cx| {
                        panel.restore_logs(logs);
                        panel.restore(selected, stats, cx);
                        if open {
                            panel.set_visible(true, window, cx);
                        }
                    });
                }
                if let Some(appearance) = loaded.appearance {
                    self.layout.appearance = appearance;
                    appearance.apply(window, cx);
                }
                if let Some(ollama) = loaded.ollama {
                    self.restore_ollama(ollama, window, cx);
                }
                if let Some(on) = loaded.notify_attention {
                    self.live.attention.enabled = on;
                }
                if let Some(on) = loaded.notify_finished {
                    self.live.attention.notify_finished = on;
                }
                self.layout.terminal = TerminalPrefs {
                    option_as_meta: loaded.option_as_meta.unwrap_or_default(),
                    clipboard_write: loaded.clipboard_write.unwrap_or_default(),
                };
                if let Some(on) = loaded.orchestration {
                    self.apply_orchestration(on);
                }
                if let Some(limit) = loaded.max_agents {
                    self.live
                        .agent_queue
                        .set_limit(hangar_usecases::agents::clamp_limit(limit));
                    self.sync_max_agents_input(window, cx);
                }
                self.restore_agent_queue(loaded.agent_queue, window, cx);
                // Today's finished sessions, for "Done today" and the day's cost.
                self.refresh_done_today(cx);
                // One right panel, one tab: layouts saved before the merge may have both open.
                if self.layout.device.open {
                    self.layout.changes.open = false;
                }
                if self.layout.changes.open {
                    self.layout.panel_tab = layout::PanelTab::Changes;
                }
                self.sync_diffs(cx);
                self.sync_device_focus(window, cx);
                if let Some(agent) = loaded.agent.filter(|a| agents::preset(a).is_some()) {
                    self.agent_setup.agent = agent.into();
                    self.agent_setup.agent_chosen_by_user = true;
                }
                if let Some(err) = loaded.settings_error {
                    self.report(&err, window, cx);
                }
                self.offer_orphan_cleanup(window, cx);
            }
            Err(err) => {
                tracing::error!("{err:#}");
                self.work.load_error = Some(format!("{err:#}").into());
                self.offer_database_recovery(&err, window, cx);
            }
        }
        cx.notify();
    }
}
