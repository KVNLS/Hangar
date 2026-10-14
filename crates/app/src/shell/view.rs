//! Rendering of the main window.

use gpui_kit::assets::IconName as Lucide;

use super::*;
use crate::ui::components::{EmptyState, StateGlyph, TabStyle, UiButton, UiTab};
use crate::ui::{ActiveTokens as _, FONT_MONO, PANE_TAB_HEIGHT};

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
        let main = h_flex()
            .flex_1()
            .min_h_0()
            .when(self.sidebar_open, |this| {
                this.child(self.render_sidebar(cx))
            })
            .child(div().flex_1().min_w_0().h_full().child(panes))
            .children(changes)
            .children(device);
        v_flex()
            .id("hangar")
            .key_context("Hangar")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::toggle_changes))
            .on_action(cx.listener(Self::toggle_device))
            .on_action(cx.listener(Self::toggle_device_stats))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &FilterWorkspaces, window, cx| {
                this.sidebar_open = true;
                this.sidebar_filter
                    .update(cx, |state, cx| state.focus(window, cx));
                cx.notify();
            }))
            .on_action(
                cx.listener(|this, _: &AddProject, window, cx| this.pick_project(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NewWorkspace, window, cx| {
                if let Some(project) = this.current_project() {
                    this.sidebar_open = true;
                    this.open_new_workspace(project, window, cx);
                }
            }))
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
            .font_family(crate::ui::FONT_UI)
            .child(self.render_titlebar(cx))
            .child(main)
            .child(self.render_status_bar(cx))
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
        let focused = self.panes.focused() == pane;
        let workspace_id = self.panes.slot(pane);
        let workspace = workspace_id.and_then(|id| self.projects.workspace(id));
        let live = workspace_id.and_then(|id| self.sessions.get(&id));
        let changes_open = self.changes.open && focused;

        let tab = match workspace {
            None => UiTab::new(("pane-tab", pane), TabStyle::Pane, "Empty pane")
                .icon(Lucide::Terminal)
                .active(focused),
            Some(ws) => {
                let agent = live.map_or_else(|| "No agent".into(), |l| l.agent_name.clone());
                UiTab::new(
                    ("pane-tab", pane),
                    TabStyle::Pane,
                    format!("{agent} · {}", ws.name),
                )
                .leading(StateGlyph::new(self.agent_state(ws.id, cx)))
                .active(focused)
            }
        };
        let start = workspace.and_then(|ws| {
            let can_start =
                !self.busy.contains(&ws.id) && live.is_none_or(|l| l.session.status.is_terminal());
            let ws_id = ws.id;
            let label = if live.is_some() { "Restart" } else { "Start" };
            can_start.then(|| {
                UiButton::new(("start", pane))
                    .icon(Lucide::Play)
                    .label(format!("{label} {}", self.agent_display_name()))
                    .ghost()
                    .small()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let agent = this.agent.to_string();
                        this.panes.show(ws_id);
                        this.panes_changed(window, cx);
                        this.start_session(ws_id, agent, window, cx).detach();
                    }))
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
            .child(div().flex_1())
            .when_some(status, |this, status| {
                this.child(
                    div()
                        .px_2()
                        .font_family(FONT_MONO)
                        .text_size(px(11.))
                        .text_color(t.tx3)
                        .child(status),
                )
            })
            .children(start)
            .child(
                div().px_1p5().child(
                    UiButton::new(("changes-toggle", pane))
                        .icon(Lucide::FileDiff)
                        .ghost()
                        .small()
                        .selected(changes_open)
                        .tooltip(if changes_open {
                            "Hide changes ⌘⇧D"
                        } else {
                            "Show changes ⌘⇧D"
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.toggle_changes_for(pane, window, cx)
                        })),
                ),
            );

        let body = div().flex_1().min_h_0().w_full();
        let body = match live {
            Some(live) => body.child(live.view.clone()),
            None => body
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

    pub(super) fn agent_display_name(&self) -> String {
        agents::preset(&self.agent)
            .map(|a| a.display_name)
            .unwrap_or_else(|| self.agent.to_string())
    }
}
