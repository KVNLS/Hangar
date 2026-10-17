//! Window chrome: the 38 px title bar and the 24 px status bar.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::TitleBar;
use gpui_kit::component::tooltip::Tooltip;

use super::*;
use crate::ui::components::{Chip, StateGlyph, UiButton};
use crate::ui::{
    ActiveTokens as _, RADIUS_CONTROL, STATUS_BAR_HEIGHT, TITLEBAR_HEIGHT, TypeScale as _,
};

/// Title bar options matching the 38 px custom title bar (traffic lights vertically centered).
pub fn titlebar_options() -> TitlebarOptions {
    TitlebarOptions {
        title: Some("Hangar".into()),
        appears_transparent: true,
        traffic_light_position: Some(point(px(14.), px(13.))),
    }
}

/// The title of a screen that replaces the workspace (settings, overview, review).
/// The project name in the crumbs: capped and truncated, so a long name never pushes the
/// title bar buttons away.
fn project_crumb(name: impl Into<SharedString>, color: Hsla) -> Div {
    // Kept whole up to the cap; the workspace name gives way first.
    div()
        .flex_shrink_0()
        .max_w(px(240.))
        .truncate()
        .text_color(color)
        .child(name.into())
}

/// Longest branch name the title bar shows before cutting it with `…`.
const MAX_BRANCH_CHARS: usize = 32;

fn short_branch(branch: &str) -> String {
    match branch.char_indices().nth(MAX_BRANCH_CHARS - 1) {
        Some((cut, _)) if branch.chars().count() > MAX_BRANCH_CHARS => {
            format!("{}…", &branch[..cut])
        }
        _ => branch.to_owned(),
    }
}

fn screen_crumb(title: &'static str, color: Hsla) -> Div {
    div()
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(color)
        .child(title)
}

impl HangarApp {
    /// The focused pane's workspace and its project.
    pub(super) fn focused_workspace(&self) -> Option<(&Project, &Workspace)> {
        let ws = self
            .work
            .projects
            .workspace(self.layout.panes.slot(self.layout.panes.focused())?)?;
        Some((self.work.projects.project(ws.project_id)?, ws))
    }

    /// Project new workspaces go to: the focused workspace's, else the first one.
    pub(super) fn current_project(&self) -> Option<ProjectId> {
        self.focused_workspace()
            .map(|(p, _)| p.id)
            .or_else(|| self.work.projects.entries().first().map(|e| e.project.id))
    }

    pub(super) fn render_titlebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let sidebar_open = self.layout.sidebar_open;
        let device_open = self.layout.device.open;
        let crumb = self.focused_workspace();
        let new_target = self.current_project();
        // A zero basis, not just `min_w_0`: the title bar sizes itself to its children's content,
        // and long crumbs would push the buttons out of the window.
        let crumbs = h_flex()
            .flex_1()
            .w_0()
            .min_w_0()
            .overflow_hidden()
            .gap_1p5()
            .items_center()
            .text_size(px(13.))
            .child(img(crate::assets::MARK_PATH).flex_none().size(px(16.)))
            .map(|this| match (self.layout.screen, crumb) {
                (Screen::Settings, _) => this.child(screen_crumb("Settings", t.tx)),
                (Screen::Overview, _) => this.child(screen_crumb("Overview", t.tx)),
                (Screen::Review, Some((project, ws))) => this
                    .child(project_crumb(project.name.clone(), t.tx2))
                    .child(div().text_color(t.tx3).child("/"))
                    .child(screen_crumb("Review", t.tx))
                    .child(div().text_color(t.tx3).child("·"))
                    .child(
                        div()
                            .min_w_0()
                            .text_color(t.tx2)
                            .truncate()
                            .child(ws.title().to_owned()),
                    ),
                (Screen::Review, None) => this.child(screen_crumb("Review", t.tx)),
                (Screen::Files, Some((project, ws))) => this
                    .child(project_crumb(project.name.clone(), t.tx2))
                    .child(div().text_color(t.tx3).child("/"))
                    .child(screen_crumb("Files", t.tx))
                    .child(div().text_color(t.tx3).child("·"))
                    .child(
                        div()
                            .min_w_0()
                            .text_color(t.tx2)
                            .truncate()
                            .child(ws.title().to_owned()),
                    ),
                (Screen::Files, None) => this.child(screen_crumb("Files", t.tx)),
                (Screen::Workspace, None) => this.child(div().text_color(t.tx2).child("Hangar")),
                (Screen::Workspace, Some((project, ws))) => this
                    .child(project_crumb(project.name.clone(), t.tx2))
                    .child(div().text_color(t.tx3).child("/"))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(t.tx)
                            .truncate()
                            .child(ws.name.clone()),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .overflow_hidden()
                            .child(Chip::new(short_branch(&ws.branch)).icon(Lucide::GitBranch)),
                    ),
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
                                this.layout.sidebar_open = !this.layout.sidebar_open;
                                cx.notify();
                            })),
                    )
                    .child(crumbs)
                    .child(
                        h_flex()
                            .id("palette-search")
                            // Gives way before the buttons on its right when space runs out.
                            .w(px(300.))
                            .min_w(px(160.))
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
                                    .font_mono()
                                    .text_size(px(10.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("⌘K"),
                            )
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_palette(window, cx)),
                            ),
                    )
                    // Keeps the search box near the middle; the buttons never shrink.
                    .child(div().flex_1().min_w_0())
                    .child(
                        h_flex()
                            .flex_none()
                            .justify_end()
                            .gap_1()
                            .items_center()
                            .child(
                                UiButton::new("overview-toggle")
                                    .icon(Lucide::LayoutGrid)
                                    .label("Overview")
                                    .kbd("⇧⌘O")
                                    .ghost()
                                    .selected(self.layout.screen == Screen::Overview)
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
                                            this.layout.sidebar_open = true;
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
                                        "Hide device ⇧⌘P"
                                    } else {
                                        "Show device ⇧⌘P"
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.set_device_open(!device_open, window, cx)
                                    })),
                            )
                            .child(
                                UiButton::new("settings-toggle")
                                    .icon(Lucide::Settings)
                                    .ghost()
                                    .selected(self.layout.screen == Screen::Settings)
                                    .tooltip("Settings ⌘,")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        let screen = if this.layout.screen == Screen::Settings {
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
            .live
            .sessions
            .values()
            .filter(|l| !l.session.status.is_terminal())
            .count();
        // Cost of the structured agents still running, as far as they report it.
        let costs: Vec<_> = self
            .live
            .sessions
            .values()
            .filter(|l| !l.session.status.is_terminal())
            .filter_map(|l| match &l.surface {
                AgentSurface::Chat { view, .. } => view.read(cx).usage().cost(),
                AgentSurface::Terminal { .. } => None,
            })
            .collect();
        let live_cost =
            crate::chat_view::costs_label(&hangar_agents::acp::usage::total_costs(&costs));
        // Today: sessions that ended today as saved, plus every session of this run (live wins).
        let session_costs: Vec<(hangar_core::SessionId, hangar_core::changes::Cost)> = self
            .live
            .sessions
            .values()
            .filter_map(|l| match &l.surface {
                AgentSurface::Chat { view, .. } => view
                    .read(cx)
                    .usage()
                    .cost()
                    .map(|c| (l.session.id, crate::chat_view::stored_cost(&c))),
                AgentSurface::Terminal { .. } => None,
            })
            .collect();
        let today_cost = crate::chat_view::stored_costs_label(
            &hangar_usecases::history::cost_today(&self.live.done_today.sessions, &session_costs),
        );
        let focused = self.layout.panes.slot(self.layout.panes.focused());
        let pool = self.devices.pool.read(cx);
        // Every device the pool runs, booted or on its way: one count, names in the tooltip.
        let mut devices: Vec<String> = Vec::new();
        let mut transitioning = 0;
        for row in pool.rows() {
            match row.state {
                "open" | "running" => devices.push(row.name),
                "starting" | "shutting down" => {
                    transitioning += 1;
                    devices.push(format!("{} · {}", row.name, row.state));
                }
                _ => {}
            }
        }
        devices.sort();
        let pressure = pool.pressure();
        // The focused workspace's run when it is busy or failed, else any run in progress.
        let focused_run = match focused {
            Some(id) if pool.is_waiting(id) => {
                let label = crate::device_pool::copy::waiting_label(
                    pool.sharing(id).and_then(|s| s.position),
                );
                Some((label, AgentState::Working))
            }
            Some(id) => self
                .live
                .runs
                .get(&id)
                .map(|run| run.summary())
                .filter(|(_, state)| matches!(state, AgentState::Working | AgentState::Failed)),
            None => None,
        };
        let run = focused_run.or_else(|| {
            self.live
                .runs
                .values()
                .map(|run| run.summary())
                .find(|(_, state)| *state == AgentState::Working)
        });
        h_flex()
            .flex_none()
            .h(STATUS_BAR_HEIGHT)
            .px(px(10.))
            .gap(px(14.))
            .items_center()
            .bg(t.panel)
            .border_t_1()
            .border_color(t.line)
            .font_mono()
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
            .when_some(run, |this, (text, state)| {
                let color = match state {
                    AgentState::Failed => t.red,
                    _ => t.acc,
                };
                this.child(
                    h_flex()
                        .gap(px(5.))
                        .items_center()
                        .text_color(color)
                        .child(gpui_kit::component::Icon::new(Lucide::Hammer).size(px(11.)))
                        .child(text),
                )
            })
            .when(!devices.is_empty(), |this| {
                let n = devices.len();
                let mut label = format!("{n} device{}", if n == 1 { "" } else { "s" });
                if transitioning > 0 {
                    label.push_str(&format!(" · {transitioning} changing"));
                }
                let list = SharedString::from(devices.join("\n"));
                this.child(
                    h_flex()
                        .id("status-devices")
                        .gap(px(5.))
                        .items_center()
                        .tooltip(move |window, cx| Tooltip::new(list.clone()).build(window, cx))
                        .child(gpui_kit::component::Icon::new(Lucide::Smartphone).size(px(11.)))
                        .child(label),
                )
            })
            .child(div().flex_1())
            .child(format!(
                "{agents} agent{} running",
                if agents == 1 { "" } else { "s" }
            ))
            .when(self.queued_count() > 0, |this| {
                this.child(
                    div()
                        .text_color(t.amb)
                        .child(format!("{} queued", self.queued_count())),
                )
            })
            .when_some(live_cost, |this, cost| this.child(format!("{cost} live")))
            .when_some(today_cost, |this, cost| {
                this.child(div().text_color(t.tx3).child(format!("{cost} today")))
            })
            // Memory pressure is the only live host figure the pool tracks; RAM totals are a
            // launch-time snapshot and per-agent memory is not measured.
            // Shown only when it calls for action.
            .when_some(pressure, |this, pressure| {
                let color = match pressure {
                    hangar_core::device::Pressure::Normal => return this,
                    hangar_core::device::Pressure::Warn => t.amb,
                    hangar_core::device::Pressure::Critical => t.red,
                };
                this.child(
                    h_flex()
                        .gap(px(5.))
                        .items_center()
                        .text_color(color)
                        .child(gpui_kit::component::Icon::new(Lucide::MemoryStick).size(px(11.)))
                        .child(format!("memory {pressure}")),
                )
            })
    }
}
