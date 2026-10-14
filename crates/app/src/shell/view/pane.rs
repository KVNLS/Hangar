//! One pane: tab bar, question banner, pane menu and body.

use super::*;

impl HangarApp {
    pub(in crate::shell) fn render_pane(
        &self,
        pane: usize,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
        let waiting = workspace_id.is_some_and(|id| self.devices.pool.read(cx).is_waiting(id));
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
    pub(super) fn question_banner(
        &self,
        id: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
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

    /// Secondary pane actions: new conversation, right panel, close pane.
    pub(super) fn pane_menu(
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
}
