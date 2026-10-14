//! Rendering of the main window.

use super::*;

impl Render for HangarApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panes = h_resizable("panes").with_state(&self.pane_sizes);
        for pane in 0..PANE_COUNT {
            panes = panes.child(resizable_panel().child(self.render_pane(pane, cx)));
        }
        let changes = self.changes.open.then(|| {
            div()
                .relative()
                .h_full()
                .w(px(self.changes.width))
                .flex_shrink_0()
                .border_l_1()
                .border_color(cx.theme().border)
                .child(self.changes_panel.clone())
                .child(
                    div()
                        .id("changes-resize")
                        .absolute()
                        .left(px(-3.))
                        .top_0()
                        .h_full()
                        .w(px(6.))
                        .cursor_col_resize()
                        .on_drag(ChangesResize, |_, _, _, cx| cx.new(|_| ChangesResize)),
                )
        });
        let device = self.device.open.then(|| {
            div()
                .relative()
                .h_full()
                .w(px(self.device.width))
                .flex_shrink_0()
                .border_l_1()
                .border_color(cx.theme().border)
                .child(self.device_panel.clone())
                .child(
                    div()
                        .id("device-resize")
                        .absolute()
                        .left(px(-3.))
                        .top_0()
                        .h_full()
                        .w(px(6.))
                        .cursor_col_resize()
                        .on_drag(DeviceResize, |_, _, _, cx| cx.new(|_| DeviceResize)),
                )
        });
        h_flex()
            .id("hangar")
            .key_context("Hangar")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::toggle_changes))
            .on_action(cx.listener(Self::toggle_device))
            .on_action(cx.listener(Self::toggle_device_stats))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<ChangesResize>, window, cx| {
                    // The device panel, when open, is right of the changes panel.
                    let right = event.bounds.right() - px(this.device_width_on_screen());
                    let width = right - event.event.position.x;
                    this.resize_changes(width.as_f32(), window, cx);
                }),
            )
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DeviceResize>, window, cx| {
                    let width = event.bounds.right() - event.event.position.x;
                    this.resize_device(width.as_f32(), window, cx);
                }),
            )
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(self.render_sidebar(cx))
            .child(div().flex_1().min_w_0().h_full().child(panes))
            .children(changes)
            .children(device)
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

fn short_status(status: &SessionStatus) -> String {
    match status {
        SessionStatus::Failed { .. } => "failed".to_owned(),
        other => status_label(other),
    }
}

impl HangarApp {
    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let app = cx.entity().downgrade();
        let focused_workspace = self.panes.slot(self.panes.focused());
        let row = |ws: &Workspace| WorkspaceRow {
            id: ws.id,
            name: ws.name.clone().into(),
            status: if self.busy.contains(&ws.id) {
                Some("…".into())
            } else {
                self.sessions
                    .get(&ws.id)
                    .map(|live| short_status(&live.session.status).into())
            },
            changes: self
                .diffs
                .get(&ws.id)
                .and_then(|entry| entry.diff.read(cx).badge())
                .map(SharedString::from),
            active: focused_workspace == Some(ws.id),
            busy: self.busy.contains(&ws.id),
        };
        let sections = self.projects.entries().iter().map(|entry| {
            let project_id = entry.project.id;
            ProjectSection {
                app: app.clone(),
                project_id,
                name: entry.project.name.clone().into(),
                workspaces: entry.active().map(row).collect(),
                archived: entry.archived().map(row).collect(),
                input: self
                    .new_workspace
                    .as_ref()
                    .filter(|n| n.project_id == project_id)
                    .map(|n| n.input.clone()),
                creating: self.creating.contains(&project_id),
                collapsed: false,
            }
        });

        let device_open = self.device.open;
        let header = h_flex()
            .w_full()
            .gap_1()
            .items_center()
            .child(div().font_semibold().child("Hangar"))
            .child(div().flex_1())
            .child(
                Button::new("device-toggle")
                    .ghost()
                    .small()
                    .icon(if device_open {
                        IconName::PanelRightClose
                    } else {
                        IconName::PanelRightOpen
                    })
                    .selected(device_open)
                    .tooltip(if device_open {
                        "Hide device (⌘⇧P)"
                    } else {
                        "Show device (⌘⇧P)"
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.set_device_open(!device_open, window, cx)
                    })),
            )
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
            .w(px(SIDEBAR_WIDTH))
            .header(header)
            .children(sections)
            .footer(footer)
    }

    pub(super) fn render_pane(&self, pane: usize, cx: &mut Context<Self>) -> impl IntoElement {
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
        let changes_open = self.changes.open && focused;
        let toggle = Button::new(("changes-toggle", pane))
            .ghost()
            .xsmall()
            .icon(if changes_open {
                IconName::PanelRightClose
            } else {
                IconName::PanelRightOpen
            })
            .tooltip(if changes_open {
                "Hide changes (⌘⇧D)"
            } else {
                "Show changes (⌘⇧D)"
            })
            .on_click(
                cx.listener(move |this, _, window, cx| this.toggle_changes_for(pane, window, cx)),
            );
        let header = match workspace {
            None => header.child("Empty pane").child(div().flex_1()),
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
                let can_start = !self.busy.contains(&ws.id)
                    && live.is_none_or(|l| l.session.status.is_terminal());
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
                                    this.panes_changed(window, cx);
                                    this.start_session(ws_id, agent, window, cx).detach();
                                })),
                        )
                    })
            }
        };
        let header = header.child(toggle);

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

    pub(super) fn agent_display_name(&self) -> String {
        agents::preset(&self.agent)
            .map(|a| a.display_name)
            .unwrap_or_else(|| self.agent.to_string())
    }
}
