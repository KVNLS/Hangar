//! Rendering of the main window.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Sizable as _, v_resizable};

use super::run::PaneTab;
use super::*;
use crate::ui::components::{CountBadge, EmptyState, StateGlyph, TabStyle, UiButton, UiTab, card};
use crate::ui::{
    ActiveTokens as _, PANE_TAB_HEIGHT, PANEL_TAB_HEIGHT, SPACE_2, SPACE_3, TypeScale as _,
};

impl Render for HangarApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panes = h_resizable("panes").with_state(&self.layout.pane_groups.columns);
        for (column, range) in self.layout.panes.columns().into_iter().enumerate() {
            let content = match self.layout.pane_groups.rows.get(column) {
                Some(rows) if range.len() > 1 => {
                    let mut group = v_resizable(("pane-column", column)).with_state(rows);
                    for pane in range {
                        group = group.child(resizable_panel().child(self.render_pane(pane, cx)));
                    }
                    group.into_any_element()
                }
                _ => {
                    let mut stack = v_flex().size_full();
                    for pane in range {
                        stack = stack.child(self.render_pane(pane, cx));
                    }
                    stack.into_any_element()
                }
            };
            panes = panes.child(resizable_panel().child(content));
        }
        let main = match self.layout.screen {
            Screen::Review => div().flex_1().min_h_0().child(self.changes_panel.clone()),
            Screen::Files => div().flex_1().min_h_0().child(self.files_panel.clone()),
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
                    this.child(self.render_resizable_sidebar(cx))
                })
                .child(self.render_overview(cx)),
            Screen::Workspace => {
                let right_panel = self.render_right_panel(cx);
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.layout.sidebar_open, |this| {
                        this.child(self.render_resizable_sidebar(cx))
                    })
                    .map(|this| {
                        if self.showing_welcome() {
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
            .on_action(
                cx.listener(|this, _: &SplitPaneDown, window, cx| this.split_pane_down(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &FocusPane1, window, cx| {
                    this.focus_pane_number(0, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &FocusPane2, window, cx| {
                    this.focus_pane_number(1, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &FocusPane3, window, cx| {
                    this.focus_pane_number(2, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &FocusPane4, window, cx| {
                    this.focus_pane_number(3, window, cx)
                }),
            )
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
            .on_action(cx.listener(|this, _: &OpenFiles, window, cx| {
                this.set_screen(Screen::Files, window, cx)
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
                cx.listener(|this, event: &DragMoveEvent<SidebarResize>, window, cx| {
                    let width = event.event.position.x - event.bounds.left();
                    this.resize_sidebar(width.as_f32(), window, cx);
                }),
            )
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
            .children(self.render_compare(cx))
            .children(self.render_answer_sheet(cx))
            .children(self.render_palette(cx))
            .children(self.render_toast(cx))
    }
}

impl HangarApp {
    pub(super) fn render_pane(&self, pane: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let focused = self.layout.panes.focused() == pane;
        let workspace_id = self.layout.panes.slot(pane);
        let workspace = workspace_id.and_then(|id| self.work.projects.workspace(id));
        let live = workspace_id.and_then(|id| self.live.sessions.get(&id));
        let panel_open = self.open_panel_tab().is_some() && focused;
        let full = self.layout.panes.count() >= MAX_PANES;

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
        let queued = workspace_id.and_then(|id| Some((id, self.queued_ahead(id)?)));
        let start = workspace.and_then(|ws| {
            // Without a session the pane's empty state offers Start/Resume instead.
            let can_start = !self.work.busy.contains(&ws.id)
                && queued.is_none()
                && live.is_some_and(|l| l.session.status.is_terminal());
            let ws_id = ws.id;
            let agent = self.agent_for(ws_id);
            let resumable = self.resumable_chat(ws_id, &agent).is_some();
            let label = match (resumable, live.is_some()) {
                (true, _) => "Resume",
                (false, true) => "Restart",
                (false, false) => "Start",
            };
            can_start.then(|| {
                h_flex()
                    .items_center()
                    .gap_1()
                    .child(self.agent_picker(("agent-picker", pane), ws_id, agent, cx))
                    .child(
                        UiButton::new(("start", pane))
                            .icon(Lucide::Play)
                            .label(label)
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_agent_in(ws_id, true, window, cx)
                            })),
                    )
            })
        });
        let new_conversation = workspace.map(|ws| ws.id).filter(|id| {
            !self.work.busy.contains(id)
                && live.is_none_or(|l| l.session.status.is_terminal())
                && self.resumable_chat(*id, &self.agent_for(*id)).is_some()
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
                    .disabled(full)
                    .tooltip("Split right ⌘\\")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.layout.panes.focus(pane);
                        this.split_pane(window, cx)
                    })),
            )
            .child(
                UiButton::new(("split-down", pane))
                    .icon(Lucide::Rows2)
                    .ghost()
                    .small()
                    .disabled(full)
                    .tooltip("Split down ⌘|")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.layout.panes.focus(pane);
                        this.split_pane_down(window, cx)
                    })),
            )
            .child(div().pl(px(2.)).pr_1p5().child(self.pane_menu(
                pane,
                panel_open,
                new_conversation,
                cx,
            )));

        let body = div().flex_1().min_h_0().w_full();
        let run_view = workspace_id.and_then(|id| self.run_view(id, shown));
        let body = match (run_view, live) {
            (Some(view), _) => body.child(view),
            (None, _) if let Some((id, ahead)) = queued => body
                .flex()
                .items_center()
                .justify_center()
                .bg(t.term)
                .child(self.render_queued_pane(id, ahead, cx)),
            (None, Some(live)) => body.child(live.view()),
            (None, None) => body
                .flex()
                .items_center()
                .justify_center()
                .bg(t.term)
                .child(match workspace {
                    Some(ws) => self.render_no_agent(pane, ws.id, cx),
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
            .children(workspace_id.and_then(|id| self.question_banner(id, cx)))
            .child(body)
    }

    /// "<agent> asks: …  Answer" above a pane whose agent waits on a question.
    fn question_banner(&self, id: WorkspaceId, cx: &mut Context<Self>) -> Option<AnyElement> {
        let question = self.question(id)?;
        let t = cx.tokens();
        let (fg, bg) = crate::ui::components::state_colors(AgentState::Input, t);
        let agent = self
            .live
            .sessions
            .get(&id)
            .map(|l| l.agent_name.to_string())
            .unwrap_or_else(|| "The agent".to_owned());
        Some(
            h_flex()
                .flex_none()
                .px_3()
                .py_1p5()
                .gap_2()
                .items_center()
                .bg(bg)
                .border_b_1()
                .border_color(t.line)
                .child(StateGlyph::new(AgentState::Input))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(12.5))
                        .text_color(fg)
                        .child(format!(
                            "{agent} asks: {}",
                            super::questions::question_line(&question.text)
                        )),
                )
                .child(
                    UiButton::new(SharedString::from(format!("pane-answer-{id}")))
                        .label("Answer")
                        .small()
                        .primary()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_answer_sheet(id, window, cx)
                        })),
                )
                .into_any_element(),
        )
    }

    /// The sidebar with a drag handle on its right edge.
    fn render_resizable_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .flex_none()
            .h_full()
            .child(self.render_sidebar(cx))
            .child(
                div()
                    .id("sidebar-resize")
                    .absolute()
                    .right(px(-3.))
                    .top_0()
                    .h_full()
                    .w(px(6.))
                    .cursor_col_resize()
                    .on_drag(SidebarResize, |_, _, _, cx| cx.new(|_| SidebarResize)),
            )
    }

    /// Secondary pane actions: new conversation, right panel, close pane.
    fn pane_menu(
        &self,
        pane: usize,
        panel_open: bool,
        new_conversation: Option<WorkspaceId>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let app = cx.entity().downgrade();
        let can_close = self.layout.panes.count() > 1;
        Button::new(("pane-menu", pane))
            .ghost()
            .small()
            .icon(Lucide::Ellipsis)
            .tooltip("More")
            .dropdown_menu_with_anchor(Anchor::TopRight, move |mut menu, _, _| {
                if let Some(ws_id) = new_conversation {
                    let app = app.clone();
                    menu = menu.item(
                        PopupMenuItem::new("New conversation")
                            .icon(Lucide::Plus)
                            .on_click(move |_, window, cx| {
                                let _ = app.update(cx, |this, cx| {
                                    this.start_agent_in(ws_id, false, window, cx)
                                });
                            }),
                    );
                }
                let (toggle_app, close_app) = (app.clone(), app.clone());
                menu.item(
                    PopupMenuItem::new(if panel_open {
                        "Hide right panel"
                    } else {
                        "Show right panel"
                    })
                    .icon(Lucide::PanelRight)
                    .action(Box::new(ToggleChanges))
                    .on_click(move |_, window, cx| {
                        let _ = toggle_app
                            .update(cx, |this, cx| this.toggle_panel_for(pane, window, cx));
                    }),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Close pane")
                        .icon(Lucide::X)
                        .action(Box::new(ClosePane))
                        .disabled(!can_close)
                        .on_click(move |_, window, cx| {
                            let _ =
                                close_app.update(cx, |this, cx| this.close_pane(pane, window, cx));
                        }),
                )
            })
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
                    .tooltip("Hide panel ⇧⌘D")
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

    /// The empty agent pane: resume the saved chat with the agent that ran it, or start a new one
    /// with the agent picked here.
    fn render_no_agent(
        &self,
        pane: usize,
        ws_id: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> EmptyState {
        let state = EmptyState::new(Lucide::Terminal, "No agent running");
        if self.work.busy.contains(&ws_id) {
            return state;
        }
        let t = cx.tokens();
        let saved_agent = self
            .live
            .saved_chats
            .get(&ws_id)
            .map(|chat| chat.agent.clone())
            .filter(|id| agents::preset(id).is_some());
        let resume = saved_agent.as_ref().map(|agent| {
            let (agent, name) = (agent.clone(), agent_display_name(agent));
            empty_choice(
                "Last conversation",
                div()
                    .min_w_0()
                    .truncate()
                    .type_secondary()
                    .text_color(t.tx2)
                    .child(name),
                UiButton::new(("empty-resume", pane))
                    .icon(Lucide::Play)
                    .label("Resume")
                    .primary()
                    .small()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_agent_with_resume(ws_id, agent.clone(), true, window, cx)
                    })),
                cx,
            )
        });
        let start = UiButton::new(("empty-new-chat", pane))
            .icon(Lucide::Plus)
            .label("Start")
            .small()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.start_agent_in(ws_id, false, window, cx)
            }));
        let start = if saved_agent.is_some() {
            start
        } else {
            start.primary()
        };
        let picker = self.agent_picker(
            ("empty-agent-picker", pane),
            ws_id,
            self.agent_for(ws_id),
            cx,
        );
        let new_chat = empty_choice("New conversation", picker, start, cx);
        state.action(
            card(cx)
                .w_full()
                .text_left()
                .when_some(resume, |this, resume| {
                    this.child(resume).child(div().h(px(1.)).bg(t.line))
                })
                .child(new_chat),
        )
    }

    /// The workspace's agent with a menu to choose another; choosing does not start it.
    fn agent_picker(
        &self,
        id: impl Into<ElementId>,
        ws_id: WorkspaceId,
        current: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let app = cx.entity().downgrade();
        let choices: Vec<(String, String, bool)> = self
            .agent_setup
            .agent_choices
            .iter()
            .map(|c| (c.id.clone(), c.name.clone(), c.installed))
            .collect();
        Button::new(id)
            .outline()
            .small()
            .label(agent_display_name(&current))
            .child(gpui_kit::component::Icon::new(Lucide::ChevronDown).xsmall())
            .tooltip("Choose the agent")
            .dropdown_menu_with_anchor(Anchor::TopCenter, move |mut menu, _, _| {
                for (id, name, installed) in &choices {
                    let label = if *installed {
                        name.clone()
                    } else {
                        format!("{name} (not on PATH)")
                    };
                    let (app, id) = (app.clone(), id.clone());
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(id == current)
                            .disabled(!installed)
                            .on_click(move |_, window, cx| {
                                let _ = app.update(cx, |this, cx| {
                                    this.remember_workspace_agent(ws_id, &id, window, cx);
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    /// Starts the workspace's agent (see [`Self::agent_for`]) from the pane header and shows it.
    fn start_agent_in(
        &mut self,
        ws_id: WorkspaceId,
        resume: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = self.agent_for(ws_id);
        self.start_agent_with_resume(ws_id, agent, resume, window, cx);
    }

    fn start_agent_with_resume(
        &mut self,
        ws_id: WorkspaceId,
        agent: String,
        resume: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout.panes.show(ws_id);
        self.panes_changed(window, cx);
        self.start_session(ws_id, agent, resume, window, cx)
            .detach();
    }
}

/// One choice of the empty agent pane: a title over a detail, its button on the right.
fn empty_choice(
    title: &'static str,
    detail: impl IntoElement,
    button: impl IntoElement,
    cx: &App,
) -> Div {
    let t = cx.tokens();
    v_flex()
        .gap(SPACE_2)
        .p(SPACE_3)
        .child(div().type_body_strong().text_color(t.tx).child(title))
        .child(
            h_flex()
                .gap(SPACE_2)
                .items_center()
                .justify_between()
                .child(detail)
                .child(button),
        )
}

/// The preset's display name, else its id.
fn agent_display_name(agent: &str) -> String {
    agents::preset(agent)
        .map(|a| a.display_name().to_owned())
        .unwrap_or_else(|| agent.to_owned())
}
