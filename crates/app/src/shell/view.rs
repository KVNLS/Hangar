//! Rendering of the main window.

use gpui_kit::assets::IconName as Lucide;

use super::*;
use crate::ui::components::{CountBadge, EmptyState, StateGlyph, TabStyle, UiButton, UiTab};
use crate::ui::{ActiveTokens as _, FONT_MONO, PANE_TAB_HEIGHT, PANEL_TAB_HEIGHT};

impl Render for HangarApp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut panes = h_resizable("panes").with_state(&self.pane_sizes);
        for pane in 0..PANE_COUNT {
            panes = panes.child(resizable_panel().child(self.render_pane(pane, cx)));
        }
        let main = match self.screen {
            Screen::Review => div().flex_1().min_h_0().child(self.changes_panel.clone()),
            Screen::Settings => div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(self.render_settings(cx)),
            Screen::Overview => div()
                .flex()
                .flex_1()
                .min_h_0()
                .when(self.sidebar_open, |this| {
                    this.child(self.render_sidebar(cx))
                })
                .child(self.render_overview(cx)),
            Screen::Workspace => {
                let right_panel = self.render_right_panel(cx);
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar_open, |this| {
                        this.child(self.render_sidebar(cx))
                    })
                    .map(|this| {
                        if self.projects.is_empty() && !self.loading && self.load_error.is_none() {
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
                let screen = if this.screen == Screen::Settings {
                    Screen::Workspace
                } else {
                    Screen::Settings
                };
                this.set_screen(screen, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleOverview, window, cx| {
                    this.toggle_overview(window, cx)
                }),
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
                let label = match live {
                    Some(l) => format!("{} · {}", l.agent_name, ws.name),
                    None => ws.name.clone(),
                };
                UiTab::new(("pane-tab", pane), TabStyle::Pane, label)
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

    /// Device and changes as tabs of one right panel; each tab keeps its own width.
    fn render_right_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let device_tab = self.device.open;
        let changes_tab = !device_tab && self.changes.open;
        let files_tab = !device_tab && !changes_tab && self.files_open;
        if !device_tab && !changes_tab && !files_tab {
            return None;
        }
        let t = cx.tokens();
        let width = if device_tab {
            self.device.width
        } else {
            self.changes.width
        };
        let files = self
            .panes
            .slot(self.panes.focused())
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
                    .tooltip("Hide panel")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.set_device_open(false, window, cx);
                        this.set_changes_open(false, window, cx);
                        this.set_files_open(false, window, cx);
                    })),
            );
        let handle = div()
            .id("right-panel-resize")
            .absolute()
            .left(px(-3.))
            .top_0()
            .h_full()
            .w(px(6.))
            .cursor_col_resize();
        let handle = if device_tab {
            handle.on_drag(DeviceResize, |_, _, _, cx| cx.new(|_| DeviceResize))
        } else {
            handle.on_drag(ChangesResize, |_, _, _, cx| cx.new(|_| ChangesResize))
        };
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

    pub(super) fn agent_display_name(&self) -> String {
        agents::preset(&self.agent)
            .map(|a| a.display_name)
            .unwrap_or_else(|| self.agent.to_string())
    }
}
