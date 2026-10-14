//! Window chrome: the 38 px title bar and the 24 px status bar.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::TitleBar;

use super::*;
use crate::ui::components::{Chip, StateGlyph, UiButton};
use crate::ui::{ActiveTokens as _, FONT_MONO, RADIUS_CONTROL, STATUS_BAR_HEIGHT, TITLEBAR_HEIGHT};

/// Title bar options matching the 38 px custom title bar (traffic lights vertically centered).
pub fn titlebar_options() -> TitlebarOptions {
    TitlebarOptions {
        title: Some("Hangar".into()),
        appears_transparent: true,
        traffic_light_position: Some(point(px(14.), px(13.))),
    }
}

impl HangarApp {
    /// The focused pane's workspace and its project.
    pub(super) fn focused_workspace(&self) -> Option<(&Project, &Workspace)> {
        let ws = self
            .projects
            .workspace(self.panes.slot(self.panes.focused())?)?;
        Some((self.projects.project(ws.project_id)?, ws))
    }

    /// Project new workspaces go to: the focused workspace's, else the first one.
    pub(super) fn current_project(&self) -> Option<ProjectId> {
        self.focused_workspace()
            .map(|(p, _)| p.id)
            .or_else(|| self.projects.entries().first().map(|e| e.project.id))
    }

    pub(super) fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let sidebar_open = self.sidebar_open;
        let device_open = self.device.open;
        let crumb = self.focused_workspace();
        let new_target = self.current_project();
        let crumbs = h_flex()
            .flex_1()
            .min_w_0()
            .gap_1p5()
            .items_center()
            .text_size(px(13.))
            .map(|this| match crumb {
                None => this.child(div().text_color(t.tx2).child("Hangar")),
                Some((project, ws)) => this
                    .child(div().text_color(t.tx2).child(project.name.clone()))
                    .child(div().text_color(t.tx3).child("/"))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.tx)
                            .truncate()
                            .child(ws.name.clone()),
                    )
                    .child(Chip::new(ws.branch.clone()).icon(Lucide::GitBranch)),
            });
        TitleBar::new()
            .h(TITLEBAR_HEIGHT)
            .bg(t.panel)
            .border_color(t.line)
            .child(
                h_flex()
                    .w_full()
                    .h_full()
                    .gap(px(10.))
                    .pr(px(10.))
                    .items_center()
                    .child(
                        UiButton::new("sidebar-toggle")
                            .icon(if sidebar_open {
                                Lucide::PanelLeftClose
                            } else {
                                Lucide::PanelLeftOpen
                            })
                            .ghost()
                            .tooltip(if sidebar_open {
                                "Hide sidebar ⌘B"
                            } else {
                                "Show sidebar ⌘B"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.sidebar_open = !this.sidebar_open;
                                cx.notify();
                            })),
                    )
                    .child(crumbs)
                    .child(
                        h_flex()
                            .id("palette-search")
                            .flex_none()
                            .w(px(300.))
                            .h(px(26.))
                            .px(px(8.))
                            .gap(px(8.))
                            .items_center()
                            .rounded(RADIUS_CONTROL)
                            .border_1()
                            .border_color(t.line)
                            .bg(t.app)
                            .text_size(px(12.))
                            .text_color(t.tx3)
                            .cursor_pointer()
                            .hover(|this| this.border_color(t.line2))
                            .child(gpui_kit::component::Icon::new(Lucide::Search).size(px(12.)))
                            .child(
                                div()
                                    .flex_1()
                                    .truncate()
                                    .child("Search workspaces and commands"),
                            )
                            .child(
                                div()
                                    .font_family(FONT_MONO)
                                    .text_size(px(10.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("⌘K"),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_palette(window, cx)),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .justify_end()
                            .gap_1()
                            .items_center()
                            .child(
                                UiButton::new("overview-toggle")
                                    .icon(Lucide::LayoutGrid)
                                    .label("Overview")
                                    .kbd("⌘⇧O")
                                    .ghost()
                                    .selected(self.screen == Screen::Overview)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.toggle_overview(window, cx)
                                    })),
                            )
                            .child(
                                UiButton::new("new-workspace")
                                    .icon(Lucide::Plus)
                                    .label("New workspace")
                                    .primary()
                                    .disabled(new_target.is_none())
                                    .tooltip("New workspace ⌘N")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if let Some(project) = new_target {
                                            this.sidebar_open = true;
                                            this.open_new_workspace(project, window, cx);
                                        }
                                    })),
                            )
                            .child(
                                UiButton::new("device-toggle")
                                    .icon(Lucide::Smartphone)
                                    .ghost()
                                    .selected(device_open)
                                    .tooltip(if device_open {
                                        "Hide device ⌘⇧P"
                                    } else {
                                        "Show device ⌘⇧P"
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.set_device_open(!device_open, window, cx)
                                    })),
                            )
                            .child(
                                UiButton::new("settings-toggle")
                                    .icon(Lucide::Settings)
                                    .ghost()
                                    .selected(self.screen == Screen::Settings)
                                    .tooltip("Settings ⌘,")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let screen = if this.screen == Screen::Settings {
                                            Screen::Workspace
                                        } else {
                                            Screen::Settings
                                        };
                                        this.set_screen(screen, window, cx)
                                    })),
                            ),
                    ),
            )
    }

    pub(super) fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let counts = self.state_counts(cx);
        // Idle workspaces are only counted when nothing else is going on.
        let all_idle = counts.iter().all(|(s, _)| *s == AgentState::Idle);
        let agents = self
            .sessions
            .values()
            .filter(|l| !l.session.status.is_terminal())
            .count();
        let device = self
            .device_panel
            .read(cx)
            .picker()
            .selected_entry()
            .map(|entry| format!("{} · {}", entry.info.name, entry.info.state.to_lowercase()));
        let run = self
            .panes
            .slot(self.panes.focused())
            .and_then(|id| self.runs.get(&id))
            .map(|run| run.summary());
        h_flex()
            .flex_none()
            .h(STATUS_BAR_HEIGHT)
            .px(px(10.))
            .gap(px(14.))
            .items_center()
            .bg(t.panel)
            .border_t_1()
            .border_color(t.line)
            .font_family(FONT_MONO)
            .text_size(px(11.))
            .text_color(t.tx2)
            .whitespace_nowrap()
            .children(
                counts
                    .into_iter()
                    .filter(|(s, _)| all_idle || *s != AgentState::Idle)
                    .map(|(state, n)| {
                        h_flex()
                            .gap(px(5.))
                            .items_center()
                            .child(StateGlyph::new(state).size(px(8.)))
                            .child(format!("{n} {}", state.short_label()))
                    }),
            )
            .when_some(device, |this, device| {
                this.child(div().w(px(1.)).h(px(12.)).bg(t.line2)).child(
                    h_flex()
                        .gap(px(5.))
                        .items_center()
                        .child(gpui_kit::component::Icon::new(Lucide::Smartphone).size(px(11.)))
                        .child(device),
                )
            })
            .when_some(run, |this, (text, state)| {
                let color = match state {
                    AgentState::Failed => t.red,
                    AgentState::Working => t.acc,
                    _ => t.tx2,
                };
                this.child(div().w(px(1.)).h(px(12.)).bg(t.line2)).child(
                    h_flex()
                        .gap(px(5.))
                        .items_center()
                        .text_color(color)
                        .child(gpui_kit::component::Icon::new(Lucide::Hammer).size(px(11.)))
                        .child(text),
                )
            })
            .child(div().flex_1())
            .child(format!(
                "{agents} agent{} running",
                if agents == 1 { "" } else { "s" }
            ))
    }
}
