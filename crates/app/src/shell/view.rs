//! Rendering of the main window.

use gpui_kit::assets::IconName as Lucide;

use super::run::PaneTab;
use super::*;
use crate::ui::components::{CountBadge, EmptyState, StateGlyph, TabStyle, UiButton, UiTab};
use crate::ui::{ActiveTokens as _, PANE_TAB_HEIGHT, PANEL_TAB_HEIGHT, TypeScale as _};

impl Render for HangarApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panes = h_resizable("panes").with_state(&self.layout.pane_sizes);
        for pane in 0..self.layout.panes.count() {
            panes = panes.child(resizable_panel().child(self.render_pane(pane, cx)));
        }
        let main = match self.layout.screen {
            Screen::Review => div().flex_1().min_h_0().child(self.changes_panel.clone()),
            Screen::Settings => div()
                .key_context("SettingsScreen")
                .track_focus(&self.layout.screen_focus)
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.render_settings(cx)),
            Screen::Overview => div()
                .key_context("OverviewScreen")
                .track_focus(&self.layout.screen_focus)
                .flex()
                .flex_1()
                .min_h_0()
                .when(self.layout.sidebar_open, |this| {
                    this.child(self.render_sidebar(cx))
                })
                .child(self.render_overview(cx)),
            Screen::Workspace => {
                let right_panel = self.render_right_panel(cx);
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.layout.sidebar_open, |this| {
                        this.child(self.render_sidebar(cx))
                    })
                    .map(|this| {
                        if self.work.projects.is_empty()
                            && !self.work.loading
                            && self.work.load_error.is_none()
                        {
                            this.child(self.render_welcome(cx))
                        } else {
                            this.child(div().flex_1().min_w_0().h_full().child(panes))
                        }
                    })
                    .children(right_panel)
            }
        };
        v_flex()
            .id("hangar")
            .key_context("Hangar")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::toggle_changes))
            .on_action(cx.listener(Self::toggle_device))
            .on_action(cx.listener(Self::toggle_device_stats))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.layout.sidebar_open = !this.layout.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FilterWorkspaces, window, cx| {
                this.layout.sidebar_open = true;
                this.layout
                    .sidebar_filter
                    .update(cx, |state, cx| state.focus(window, cx));
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &AddProject, window, cx| this.pick_project(window, cx)),
            )
            .on_action(cx.listener(|this, _: &Run, window, cx| this.run_focused(window, cx)))
            .on_action(cx.listener(|this, _: &StopRun, _, cx| this.stop_run(cx)))
            .on_action(cx.listener(|this, _: &SplitPane, window, cx| this.split_pane(window, cx)))
            .on_action(cx.listener(|this, _: &ClosePane, window, cx| {
                this.close_pane(this.layout.panes.focused(), window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &RestartServer, window, cx| this.restart_server(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowDeviceTab, window, cx| {
                this.set_device_open(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowChangesTab, window, cx| {
                this.set_changes_open(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NextNeedingYou, window, cx| {
                this.focus_next_needing_you(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowFilesTab, window, cx| {
                this.set_files_open(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenReview, window, cx| {
                this.set_screen(Screen::Review, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &OpenPalette, window, cx| this.open_palette(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleTheme, window, cx| {
                let next = if cx.theme().is_dark() {
                    crate::ui::theme::Appearance::Light
                } else {
                    crate::ui::theme::Appearance::Dark
                };
                this.set_appearance(next, window, cx)
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                let screen = if this.layout.screen == Screen::Settings {
                    Screen::Workspace
                } else {
                    Screen::Settings
                };
                this.set_screen(screen, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseScreen, window, cx| {
                this.set_screen(Screen::Workspace, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleOverview, window, cx| {
                    this.toggle_overview(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &NewWorkspace, window, cx| {
                if let Some(project) = this.current_project() {
                    this.layout.sidebar_open = true;
                    this.open_new_workspace(project, window, cx);
                }
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<PanelResize>, window, cx| {
                    let width = event.bounds.right() - event.event.position.x;
                    this.resize_panel(width.as_f32(), window, cx);
                }),
            )
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(crate::ui::FONT_UI)
            .relative()
            .child(self.render_titlebar(cx))
            .child(main)
            .child(self.render_status_bar(cx))
            .children(self.render_new_workspace_sheet(cx))
            .children(self.render_palette(cx))
    }
}

pub(super) fn agent_options(choices: &[AgentChoice]) -> Vec<AgentOption> {
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

impl HangarApp {
    pub(super) fn render_pane(&self, pane: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let focused = self.layout.panes.focused() == pane;
        let workspace_id = self.layout.panes.slot(pane);
        let workspace = workspace_id.and_then(|id| self.work.projects.workspace(id));
        let live = workspace_id.and_then(|id| self.live.sessions.get(&id));
        let panel_open = self.open_panel_tab().is_some() && focused;

        let shown = workspace_id.map(|id| self.pane_tab(id)).unwrap_or_default();
        let tab = match workspace {
            None => UiTab::new(("pane-tab", pane), TabStyle::Pane, "Empty pane")
                .icon(Lucide::Terminal)
                .active(focused),
            Some(ws) => {
                let label = match live {
                    Some(l) => format!("{} · {}", l.agent_name, ws.name),
                    None => ws.name.clone(),
                };
                let ws_id = ws.id;
                UiTab::new(("pane-tab", pane), TabStyle::Pane, label)
                    .leading(StateGlyph::new(self.agent_state(ws.id, cx)))
                    .active(focused && shown == PaneTab::Agent)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_pane_tab(ws_id, PaneTab::Agent, window, cx)
                    }))
            }
        };
        let run = workspace_id.and_then(|id| self.live.runs.get(&id));
        let run_tabs: Vec<UiTab> = run
            .into_iter()
            .flat_map(|run| {
                [(PaneTab::Server, &run.server), (PaneTab::Build, &run.build)]
                    .into_iter()
                    .filter_map(|(kind, tab)| Some((kind, tab.as_ref()?)))
            })
            .filter_map(|(kind, tab)| {
                let ws_id = workspace_id?;
                let id = match kind {
                    PaneTab::Server => "server",
                    _ => "build",
                };
                Some(
                    UiTab::new((id, pane), TabStyle::Pane, tab.title.clone())
                        .leading(StateGlyph::new(tab.state()))
                        .active(focused && shown == kind)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.show_pane_tab(ws_id, kind, window, cx)
                        })),
                )
            })
            .collect();
        let waiting = workspace_id.is_some_and(|id| self.device_pool.read(cx).is_waiting(id));
        let run_button = workspace.map(|ws| {
            let ws_id = ws.id;
            UiButton::new(("run", pane))
                .icon(Lucide::Hammer)
                .label("Run")
                .kbd("⌘R")
                .ghost()
                .small()
                .disabled(self.work.busy.contains(&ws_id))
                .tooltip("Build and run on this workspace's device")
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.layout.panes.show(ws_id);
                    this.panes_changed(window, cx);
                    this.run_focused(window, cx);
                }))
        });
        let start = workspace.and_then(|ws| {
            let can_start = !self.work.busy.contains(&ws.id)
                && live.is_none_or(|l| l.session.status.is_terminal());
            let ws_id = ws.id;
            let resumable = self
                .resumable_chat(ws_id, &self.agent_setup.agent)
                .is_some();
            let label = match (resumable, live.is_some()) {
                (true, _) => "Resume",
                (false, true) => "Restart",
                (false, false) => "Start",
            };
            can_start.then(|| {
                h_flex()
                    .gap_1()
                    .child(
                        UiButton::new(("start", pane))
                            .icon(Lucide::Play)
                            .label(format!("{label} {}", self.agent_display_name()))
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_agent_in(ws_id, true, window, cx)
                            })),
                    )
                    .when(resumable, |row| {
                        row.child(
                            UiButton::new(("start-new", pane))
                                .label("New conversation")
                                .ghost()
                                .small()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.start_agent_in(ws_id, false, window, cx)
                                })),
                        )
                    })
            })
        });
        let status = live
            .filter(|l| l.session.status.is_terminal())
            .map(|l| status_label(&l.session.status));
        let tab_bar = h_flex()
            .flex_none()
            .h(PANE_TAB_HEIGHT)
            .items_center()
            .bg(t.panel)
            .border_b_1()
            .border_color(t.line)
            .child(tab)
            .children(run_tabs)
            .when(waiting, |this| {
                this.child(
                    div()
                        .px_2()
                        .font_mono()
                        .text_size(px(11.))
                        .text_color(t.amb)
                        .child("Waiting for the device"),
                )
            })
            .child(div().flex_1())
            .when_some(status, |this, status| {
                this.child(
                    div()
                        .px_2()
                        .font_mono()
                        .text_size(px(11.))
                        .text_color(t.tx3)
                        .child(status),
                )
            })
            .children(start)
            .children(run_button)
            .child(
                UiButton::new(("split", pane))
                    .icon(Lucide::Columns2)
                    .ghost()
                    .small()
                    .disabled(self.layout.panes.count() >= MAX_PANES)
                    .tooltip("Split right ⌘D")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.layout.panes.focus(pane);
                        this.split_pane(window, cx)
                    })),
            )
            .when(self.layout.panes.count() > 1, |this| {
                this.child(
                    UiButton::new(("close-pane", pane))
                        .icon(Lucide::X)
                        .ghost()
                        .small()
                        .tooltip("Close pane ⌘W (the agent keeps running)")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.close_pane(pane, window, cx)
                        })),
                )
            })
            .child(
                div().px_1p5().child(
                    UiButton::new(("panel-toggle", pane))
                        .icon(Lucide::PanelRight)
                        .ghost()
                        .small()
                        .selected(panel_open)
                        .tooltip(if panel_open {
                            "Hide panel ⌘⇧D"
                        } else {
                            "Show panel ⌘⇧D"
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle_panel_for(pane, window, cx)
                        })),
                ),
            );

        let body = div().flex_1().min_h_0().w_full();
        let run_view = workspace_id.and_then(|id| self.run_view(id, shown));
        let body = match (run_view, live) {
            (Some(view), _) => body.child(view),
            (None, Some(live)) => body.child(live.view()),
            (None, None) => body
                .flex()
                .items_center()
                .justify_center()
                .bg(t.term)
                .child(match workspace {
                    Some(_) => EmptyState::new(Lucide::Terminal, "No agent running")
                        .hint("Start an agent to work in this workspace."),
                    None => EmptyState::new(Lucide::Terminal, "Empty pane")
                        .hint("Select a workspace in the sidebar to show it here."),
                }),
        };

        v_flex()
            .id(("pane", pane))
            .size_full()
            .bg(t.term)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseDownEvent, window, cx| {
                    this.focus_pane(pane, window, cx);
                }),
            )
            .child(tab_bar)
            .child(body)
    }

    /// Device, changes and files as tabs of one right panel of one width.
    fn render_right_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let device_tab = self.layout.device.open;
        let changes_tab = !device_tab && self.layout.changes.open;
        let files_tab = !device_tab && !changes_tab && self.layout.files_open;
        if !device_tab && !changes_tab && !files_tab {
            return None;
        }
        let t = cx.tokens();
        let width = self.layout.panel_width;
        let files = self
            .layout
            .panes
            .slot(self.layout.panes.focused())
            .and_then(|id| self.diff_counts(id, cx))
            .map(|c| c.files)
            .filter(|n| *n > 0);
        let tabs = h_flex()
            .flex_none()
            .h(PANEL_TAB_HEIGHT)
            .px_1p5()
            .gap(px(2.))
            .items_center()
            .border_b_1()
            .border_color(t.line)
            .child(
                UiTab::new("tab-device", TabStyle::Panel, "Device")
                    .icon(Lucide::Smartphone)
                    .kbd("⌘1")
                    .active(device_tab)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.set_device_open(true, window, cx)),
                    ),
            )
            .child(
                UiTab::new("tab-changes", TabStyle::Panel, "Changes")
                    .icon(Lucide::GitCommitHorizontal)
                    .when_some(files, |this, n| {
                        this.trailing(CountBadge::new(n.to_string()))
                    })
                    .kbd("⌘2")
                    .active(changes_tab)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.set_changes_open(true, window, cx)),
                    ),
            )
            .child(
                UiTab::new("tab-files", TabStyle::Panel, "Files")
                    .icon(Lucide::Folder)
                    .kbd("⌘3")
                    .active(files_tab)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.set_files_open(true, window, cx)),
                    ),
            )
            .child(div().flex_1())
            .child(
                UiButton::new("right-panel-close")
                    .icon(Lucide::PanelRightClose)
                    .ghost()
                    .small()
                    .tooltip("Hide panel ⌘⇧D")
                    .on_click(cx.listener(|this, _, window, cx| this.hide_panel(window, cx))),
            );
        let handle = div()
            .id("right-panel-resize")
            .absolute()
            .left(px(-3.))
            .top_0()
            .h_full()
            .w(px(6.))
            .cursor_col_resize()
            .on_drag(PanelResize, |_, _, _, cx| cx.new(|_| PanelResize));
        Some(
            v_flex()
                .relative()
                .h_full()
                .w(px(width))
                .flex_none()
                .bg(t.panel)
                .border_l_1()
                .border_color(t.line)
                .child(tabs)
                .child(div().flex_1().min_h_0().map(|this| {
                    if device_tab {
                        this.child(self.device_panel.clone())
                    } else if changes_tab {
                        this.child(self.changes_panel.clone())
                    } else {
                        this.child(self.files_panel.clone())
                    }
                }))
                .child(handle)
                .into_any_element(),
        )
    }

    /// Starts the selected agent in `ws_id` from the pane header and shows it.
    fn start_agent_in(
        &mut self,
        ws_id: WorkspaceId,
        resume: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = self.agent_setup.agent.to_string();
        self.layout.panes.show(ws_id);
        self.panes_changed(window, cx);
        self.start_session(ws_id, agent, resume, window, cx)
            .detach();
    }

    pub(super) fn agent_display_name(&self) -> String {
        agents::preset(&self.agent_setup.agent)
            .map(|a| a.display_name().to_owned())
            .unwrap_or_else(|| self.agent_setup.agent.to_string())
    }
}
