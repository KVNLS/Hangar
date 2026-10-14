//! Overview (⌘⇧O): every active workspace as a card, grouped by agent state (or by project).

use gpui_kit::assets::IconName as Lucide;
use hangar_core::Timestamp;

use super::status::age_label;
use super::*;
use crate::ui::components::{AgentMark, Delta, Segmented, StateGlyph, UiButton, card};
use gpui_kit::component::Icon;

use crate::ui::{ActiveTokens as _, FONT_MONO, TypeScale as _};

impl HangarApp {
    fn overview_card(&self, ws: &Workspace, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.tokens();
        let id = ws.id;
        let state = self.agent_state(id, cx);
        let live = self.sessions.get(&id);
        let project = self
            .projects
            .project(ws.project_id)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let meta = format!(
            "{project} · {} · {}",
            ws.branch,
            age_label(ws.created_at.as_millis(), Timestamp::now().as_millis())
        );
        let last = live
            .and_then(|l| l.activity.last_line.clone())
            .unwrap_or_else(|| {
                if live.is_some() {
                    "No output yet.".into()
                } else {
                    "No agent running.".into()
                }
            });
        let counts = self.diff_counts(id, cx);
        let review = state == AgentState::Review;
        let (action, action_icon) = if review {
            ("Review diff", Lucide::FileDiff)
        } else {
            ("Open", Lucide::ArrowRight)
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
                    .font_family(FONT_MONO)
                    .text_size(px(11.5))
                    .line_height(px(17.))
                    .text_color(t.tx2)
                    .truncate()
                    .child(last),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .children(counts.map(|c| Delta::new(c.insertions, c.deletions)))
                    .child(div().flex_1())
                    .child(
                        UiButton::new(SharedString::from(format!("overview-action-{id}")))
                            .icon(action_icon)
                            .label(action)
                            .small()
                            .when(review, |b| b.primary())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.select_workspace(id, window, cx);
                                let screen = if review {
                                    Screen::Review
                                } else {
                                    Screen::Workspace
                                };
                                this.set_screen(screen, window, cx);
                            })),
                    ),
            )
            .into_any_element()
    }

    pub(super) fn render_overview(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let workspaces: Vec<&Workspace> = self
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
        let summary = format!(
            "{} workspace{} · {needs_you} need you · {to_review} to review",
            workspaces.len(),
            if workspaces.len() == 1 { "" } else { "s" }
        );
        let groups: Vec<(AnyElement, String, Vec<&Workspace>)> = if self.overview_by_project {
            self.projects
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
                            .font_family(FONT_MONO)
                            .text_size(px(12.))
                            .text_color(t.tx3)
                            .child(summary),
                    )
                    .child(div().flex_1())
                    .child(
                        UiButton::new("overview-next")
                            .label("Next needing you")
                            .kbd("⌘J")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.focus_next_needing_you(window, cx)
                            })),
                    )
                    .child(
                        Segmented::new("overview-group", ["By status", "By project"])
                            .selected(usize::from(self.overview_by_project))
                            .on_select(cx.listener(|this, i: &usize, _, cx| {
                                this.overview_by_project = *i == 1;
                                cx.notify();
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
