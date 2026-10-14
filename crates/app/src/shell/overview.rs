//! Overview (⌘⇧O): every active workspace as a card, grouped by agent state (or by project).

use gpui_kit::assets::IconName as Lucide;
use hangar_core::Timestamp;

use super::status::age_label;
use super::*;
use crate::chat_view::{ChatView, subagent_state};
use crate::ui::components::{
    AgentMark, Delta, Segmented, StateGlyph, UiButton, card, state_colors,
};
use gpui_kit::component::Icon;
use hangar_agents::acp::subagents::{Subagent, SubagentStatus};

use crate::ui::{ActiveTokens as _, TypeScale as _};

/// What a card's action button does for a workspace in a given state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardAction {
    /// Show the workspace.
    Open,
    /// Show the review screen.
    Review,
    /// Start the agent again (resuming its conversation when it can), then show it.
    Restart,
}

/// Button label and action of an overview card. Approval and input are answered in the agent's
/// pane, so they open it. An idle agent that is still alive (quiet terminal) is opened, not resumed.
pub(super) fn card_action(state: AgentState, alive: bool) -> (&'static str, CardAction) {
    match state {
        AgentState::Idle if alive => ("Open", CardAction::Open),
        AgentState::Approval => ("Review request", CardAction::Open),
        AgentState::Input => ("Reply", CardAction::Open),
        AgentState::Failed => ("Retry", CardAction::Restart),
        AgentState::Review => ("Review diff", CardAction::Review),
        AgentState::Working => ("Open", CardAction::Open),
        AgentState::Idle => ("Resume", CardAction::Restart),
    }
}

/// States whose action button takes the state's colors (the rest are neutral).
pub(super) fn card_action_tinted(state: AgentState) -> bool {
    matches!(
        state,
        AgentState::Approval | AgentState::Input | AgentState::Failed | AgentState::Review
    )
}

/// Subagent rows shown on a card before "+N more".
const MAX_SUBAGENT_ROWS: usize = 4;

/// "3 subagents · 1 done · 1 running · 1 waiting for approval"; zero counts are left out.
fn subagent_summary(list: &[Subagent]) -> String {
    let n = list.len();
    let mut parts = vec![format!("{n} subagent{}", if n == 1 { "" } else { "s" })];
    for (status, label) in [
        (SubagentStatus::Done, "done"),
        (SubagentStatus::Running, "running"),
        (SubagentStatus::Approval, "waiting for approval"),
        (SubagentStatus::Failed, "failed"),
    ] {
        let count = list.iter().filter(|s| s.status == status).count();
        if count > 0 {
            parts.push(format!("{count} {label}"));
        }
    }
    parts.join(" · ")
}

impl HangarApp {
    fn run_card_action(
        &mut self,
        id: WorkspaceId,
        action: CardAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let stopped = self
            .live
            .sessions
            .get(&id)
            .is_none_or(|l| l.session.status.is_terminal());
        let restart = action == CardAction::Restart && stopped && !self.work.busy.contains(&id);
        self.select_workspace(id, window, cx);
        let screen = if action == CardAction::Review {
            Screen::Review
        } else {
            Screen::Workspace
        };
        self.set_screen(screen, window, cx);
        if restart {
            let agent = self.agent_for(id);
            self.start_session(id, agent, true, window, cx).detach();
        }
    }

    fn chat_view(&self, id: WorkspaceId) -> Option<&Entity<ChatView>> {
        match &self.live.sessions.get(&id)?.surface {
            AgentSurface::Chat { view, .. } => Some(view),
            _ => None,
        }
    }

    fn subagent_block(
        &self,
        id: WorkspaceId,
        list: &[Subagent],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = cx.tokens();
        let rows = list.iter().take(MAX_SUBAGENT_ROWS).map(|sub| {
            let sub_id = sub.id.clone();
            h_flex()
                .id(SharedString::from(format!("overview-sub-{id}-{}", sub.id)))
                .gap_2()
                .px_2()
                .py(px(6.))
                .items_start()
                .border_t_1()
                .border_color(t.line)
                .hover(|s| s.bg(t.hover))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.select_workspace(id, window, cx);
                    this.set_screen(Screen::Workspace, window, cx);
                    if let Some(view) = this.chat_view(id).cloned() {
                        view.update(cx, |view, cx| view.focus_subagent(Some(&sub_id), cx));
                    }
                }))
                .child(
                    div()
                        .flex()
                        .h(px(18.))
                        .w(px(14.))
                        .items_center()
                        .justify_center()
                        .child(StateGlyph::new(subagent_state(sub.status))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().type_body().truncate().child(sub.name.clone()))
                        .children(sub.activity.clone().map(|a| {
                            div()
                                .type_mono_small()
                                .text_color(t.tx3)
                                .truncate()
                                .child(a)
                        })),
                )
                .child(
                    div().flex().h(px(18.)).items_center().child(
                        Icon::new(Lucide::ChevronRight)
                            .size(px(12.))
                            .text_color(t.tx3),
                    ),
                )
        });
        let more = list.len().saturating_sub(MAX_SUBAGENT_ROWS);
        v_flex()
            .rounded(px(4.))
            .border_1()
            .border_color(t.line)
            .overflow_hidden()
            .child(
                h_flex()
                    .gap_2()
                    .px_2()
                    .py(px(5.))
                    .items_center()
                    .bg(t.panel)
                    .child(Icon::new(Lucide::GitFork).size(px(12.)).text_color(t.tx3))
                    .child(
                        div()
                            .type_secondary()
                            .text_color(t.tx2)
                            .truncate()
                            .child(subagent_summary(list)),
                    ),
            )
            .children(rows)
            .when(more > 0, |this| {
                this.child(
                    div()
                        .px_2()
                        .py(px(5.))
                        .border_t_1()
                        .border_color(t.line)
                        .type_mono_small()
                        .text_color(t.tx3)
                        .child(format!("+{more} more")),
                )
            })
            .into_any_element()
    }

    fn overview_card(&self, ws: &Workspace, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.tokens();
        let id = ws.id;
        let state = self.agent_state(id, cx);
        let live = self.live.sessions.get(&id);
        let project = self
            .work
            .projects
            .project(ws.project_id)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let meta = format!(
            "{project} · {} · {}",
            ws.branch,
            age_label(ws.created_at.as_millis(), Timestamp::now().as_millis())
        );
        let queued = self.queued_ahead(id);
        // A request that can be allowed once is answered right on the card.
        let request = (state == AgentState::Approval)
            .then(|| self.pending_request(id, cx))
            .flatten()
            .filter(|r| r.can_allow);
        let last = queued
            .map(|ahead| super::queue::queued_label(ahead).into())
            .or_else(|| {
                request
                    .as_ref()
                    .map(|r| format!("Permission: {}", r.title).into())
            })
            .or_else(|| live.and_then(|l| l.activity.last_line.clone()))
            .unwrap_or_else(|| {
                if live.is_some() {
                    "No output yet.".into()
                } else {
                    "No agent running.".into()
                }
            });
        let counts = self.diff_counts(id, cx);
        let subagents = self
            .chat_view(id)
            .map(|v| v.read(cx).subagents())
            .unwrap_or_default();
        let device = self.device_pool.read(cx).device_name(id).map(str::to_owned);
        let attempts = self.attempts_of(id);
        let allow_inline = request.is_some();
        let (action_label, action) =
            card_action(state, live.is_some_and(|l| !l.session.status.is_terminal()));
        let (action_fg, action_bg, action_border) = if card_action_tinted(state) {
            let (fg, bg) = state_colors(state, t);
            (fg, bg, bg)
        } else {
            (t.tx2, t.elev, t.line2)
        };
        card(cx)
            .id(SharedString::from(format!("overview-{id}")))
            .gap_2()
            .px_3()
            .py(px(10.))
            .cursor_pointer()
            .hover(|s| s.border_color(t.line2))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_workspace(id, window, cx);
                this.set_screen(Screen::Workspace, window, cx);
            }))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .w(px(14.))
                            .justify_center()
                            .child(StateGlyph::new(state)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .type_body_strong()
                            .child(ws.name.clone()),
                    )
                    .children(live.map(|l| AgentMark::new(l.session.agent.monogram()))),
            )
            .when(ws.title() != ws.name, |this| {
                this.child(
                    div()
                        .type_secondary()
                        .text_color(t.tx2)
                        .truncate()
                        .child(ws.title().to_owned()),
                )
            })
            .child(
                div()
                    .type_mono_small()
                    .text_color(t.tx3)
                    .truncate()
                    .child(meta),
            )
            .child(
                div()
                    .h(px(34.))
                    .px_2()
                    .py(px(6.))
                    .overflow_hidden()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.term)
                    .font_mono()
                    .text_size(px(11.5))
                    .line_height(px(17.))
                    .text_color(t.tx2)
                    .truncate()
                    .child(last),
            )
            .when(!subagents.is_empty(), |this| {
                this.child(self.subagent_block(id, &subagents, cx))
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .children(counts.map(|c| Delta::new(c.insertions, c.deletions)))
                    .when_some(device, |this, device| {
                        this.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .type_mono_small()
                                .text_color(t.tx3)
                                .child(format!("· {device}")),
                        )
                    })
                    .child(div().flex_1())
                    .when_some(attempts, |this, (group, count)| {
                        this.child(
                            UiButton::new(SharedString::from(format!("overview-compare-{id}")))
                                .label(format!("Compare {count}"))
                                .ghost()
                                .small()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_compare(group, window, cx)
                                })),
                        )
                    })
                    .when(request.is_some(), |this| {
                        this.child(
                            UiButton::new(SharedString::from(format!("overview-deny-{id}")))
                                .label("Deny")
                                .ghost()
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.answer_request(id, false, cx);
                                })),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("overview-action-{id}")))
                            .flex()
                            .flex_none()
                            .h(px(24.))
                            .px_2()
                            .items_center()
                            .rounded(px(5.))
                            .border_1()
                            .border_color(action_border)
                            .bg(action_bg)
                            .text_color(action_fg)
                            .text_size(px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.85))
                            .child(if request.is_some() {
                                "Allow once"
                            } else {
                                action_label
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                if allow_inline {
                                    this.answer_request(id, true, cx);
                                } else {
                                    this.run_card_action(id, action, window, cx);
                                }
                            })),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_overview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let workspaces: Vec<&Workspace> = self
            .work
            .projects
            .entries()
            .iter()
            .flat_map(|e| e.active())
            .collect();
        let states: Vec<AgentState> = workspaces
            .iter()
            .map(|ws| self.agent_state(ws.id, cx))
            .collect();
        let needs_you = states.iter().filter(|s| s.needs_you()).count();
        let to_review = states.iter().filter(|s| **s == AgentState::Review).count();
        let subagents_running: usize = workspaces
            .iter()
            .filter_map(|ws| self.chat_view(ws.id))
            .map(|v| {
                v.read(cx)
                    .subagents()
                    .iter()
                    .filter(|s| {
                        matches!(s.status, SubagentStatus::Running | SubagentStatus::Approval)
                    })
                    .count()
            })
            .sum();
        let subagents = if subagents_running > 0 {
            format!(" · {subagents_running} subagents running")
        } else {
            String::new()
        };
        let queued = match self.queued_count() {
            0 => String::new(),
            n => format!(" · {n} queued"),
        };
        let summary = format!(
            "{} agent{}{subagents}{queued} · {needs_you} need you · {to_review} to review",
            workspaces.len(),
            if workspaces.len() == 1 { "" } else { "s" }
        );
        let groups: Vec<(AnyElement, String, Vec<&Workspace>)> = if self.layout.overview_by_project
        {
            self.work
                .projects
                .entries()
                .iter()
                .map(|e| {
                    let icon = Icon::new(Lucide::Folder)
                        .size(px(12.))
                        .text_color(t.tx3)
                        .into_any_element();
                    (icon, e.project.name.clone(), e.active().collect())
                })
                .filter(|(_, _, items): &(AnyElement, String, Vec<&Workspace>)| !items.is_empty())
                .collect()
        } else {
            AgentState::ORDER
                .into_iter()
                .filter_map(|state| {
                    let items: Vec<&Workspace> = workspaces
                        .iter()
                        .zip(&states)
                        .filter(|(_, s)| **s == state)
                        .map(|(ws, _)| *ws)
                        .collect();
                    (!items.is_empty()).then(|| {
                        (
                            StateGlyph::new(state).into_any_element(),
                            state.label().to_owned(),
                            items,
                        )
                    })
                })
                .collect()
        };
        let body: Vec<Div> = groups
            .into_iter()
            .map(|(glyph, label, items)| {
                let n = items.len();
                let cards: Vec<AnyElement> = items
                    .into_iter()
                    .map(|ws| self.overview_card(ws, cx))
                    .collect();
                v_flex()
                    .gap_2()
                    .child(
                        h_flex()
                            .h(px(24.))
                            .gap_2()
                            .items_center()
                            .child(div().flex().w(px(14.)).justify_center().child(glyph))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(label),
                            )
                            .child(
                                div()
                                    .type_mono_small()
                                    .text_color(t.tx3)
                                    .child(n.to_string()),
                            )
                            .child(div().flex_1().ml_1p5().h(px(1.)).bg(t.line)),
                    )
                    .child(div().grid().grid_cols(3).gap_2().children(cards))
            })
            .collect();
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .bg(t.app)
            .child(
                h_flex()
                    .flex_none()
                    .h(px(48.))
                    .px_5()
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(t.line)
                    .child(div().type_title().child("Overview"))
                    .child(
                        div()
                            .font_mono()
                            .text_size(px(12.))
                            .text_color(t.tx3)
                            .child(summary),
                    )
                    .child(div().flex_1())
                    .child(
                        Segmented::new("overview-group", ["By status", "By project"])
                            .selected(usize::from(self.layout.overview_by_project))
                            .on_select(cx.listener(|this, i: &usize, _, cx| {
                                this.layout.overview_by_project = *i == 1;
                                cx.notify();
                            })),
                    )
                    .child(
                        UiButton::new("overview-next")
                            .label("Next needing you")
                            .kbd("⌘J")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.focus_next_needing_you(window, cx)
                            })),
                    ),
            )
            .child(
                div()
                    .id("overview-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_5()
                    .pt_4()
                    .pb_6()
                    .child(v_flex().gap_5().children(body))
                    .when(workspaces.is_empty(), |this| {
                        this.child(
                            div().pt_10().flex().justify_center().child(
                                crate::ui::components::EmptyState::new(
                                    Lucide::LayoutGrid,
                                    "No workspaces yet",
                                )
                                .hint("Create one with ⌘N to see it here."),
                            ),
                        )
                    }),
            )
            .text_color(t.tx)
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: the shell glob-imports gpui, whose `test` macro would shadow `#[test]`.
    use super::{CardAction, card_action, card_action_tinted};
    use hangar_core::AgentState::{self, *};

    #[test]
    fn action_labels_follow_the_state() {
        let labels: Vec<&str> = AgentState::ORDER
            .into_iter()
            .map(|s| card_action(s, false).0)
            .collect();
        assert_eq!(
            labels,
            [
                "Review request",
                "Reply",
                "Retry",
                "Review diff",
                "Open",
                "Resume"
            ]
        );
        assert_eq!(card_action(Review, false).1, CardAction::Review);
        assert_eq!(card_action(Failed, false).1, CardAction::Restart);
        assert_eq!(card_action(Idle, false).1, CardAction::Restart);
        assert_eq!(card_action(Idle, true), ("Open", CardAction::Open));
        assert_eq!(card_action(Approval, false).1, CardAction::Open);
    }

    #[test]
    fn only_states_waiting_on_the_user_are_tinted() {
        let tinted: Vec<AgentState> = AgentState::ORDER
            .into_iter()
            .filter(|s| card_action_tinted(*s))
            .collect();
        assert_eq!(tinted, [Approval, Input, Failed, Review]);
    }
}
