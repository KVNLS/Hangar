//! Overview (⇧⌘O): every active workspace as a card, grouped by agent state (or by project).

use gpui_kit::assets::IconName as Lucide;
use hangar_core::{Session, Timestamp};
use hangar_usecases::history::{self, Outcome};

use super::status::age_label;
use super::*;
use crate::chat_view::{ChatView, subagent_state, usage_label};
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
    /// Take a queued start out of the queue.
    Unqueue,
}

/// Button label and action of an overview card. Approval and input are answered in the agent's
/// pane, so they open it. An idle agent that is still alive (quiet terminal) is opened, not resumed.
/// A queued start already waits for a slot: it can only be cancelled.
pub(super) fn card_action(
    state: AgentState,
    alive: bool,
    queued: bool,
) -> (&'static str, CardAction) {
    if queued {
        return ("Cancel start", CardAction::Unqueue);
    }
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

mod card;
mod done_today;

pub(super) use done_today::{DoneToday, local_utc_offset};

impl HangarApp {
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
                    .child(
                        v_flex()
                            .gap_5()
                            .children(body)
                            .children(self.done_today_section(cx)),
                    )
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
            .map(|s| card_action(s, false, false).0)
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
        assert_eq!(card_action(Review, false, false).1, CardAction::Review);
        assert_eq!(card_action(Failed, false, false).1, CardAction::Restart);
        assert_eq!(card_action(Idle, false, false).1, CardAction::Restart);
        assert_eq!(card_action(Idle, true, false), ("Open", CardAction::Open));
        assert_eq!(card_action(Approval, false, false).1, CardAction::Open);
        // Queued: never Resume, which would only ask for the start it already waits for.
        assert_eq!(
            card_action(Idle, false, true),
            ("Cancel start", CardAction::Unqueue)
        );
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
