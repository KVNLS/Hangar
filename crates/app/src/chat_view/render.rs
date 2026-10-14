//! Transcript and composer elements, after the agent pane of `design/Hangar.dc.html`.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_agents::acp::subagents::SubagentStatus;

use super::ChatView;
use super::rows::Row;
use crate::ui::components::UiButton;
use crate::ui::{ActiveTokens as _, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TypeScale as _};

mod composer;
mod helpers;
mod items;

const MAX_WIDTH: Pixels = px(700.);
const RADIUS: Pixels = px(6.);
/// Longest tool output or diff shown in a card, in lines.
const MAX_CARD_LINES: usize = 40;

impl Render for ChatView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let subagent = self.focus.as_deref().and_then(|id| self.subagent(id));
        let working = match &subagent {
            Some(sub) => sub.status == SubagentStatus::Running,
            None => self.running && self.pending_permission().is_none(),
        };
        let mut lead = Vec::new();
        let mut trail = Vec::new();
        match &subagent {
            Some(sub) => {
                if self.launch(&sub.id).is_some_and(|c| c.prompt.is_some()) {
                    lead.push(Row::Task);
                }
                if self.subagent_result(sub).is_some() {
                    trail.push(Row::Result);
                }
            }
            None => {
                if self.done_text(cx).is_some() {
                    trail.push(Row::Done);
                }
            }
        }
        if working {
            trail.push(Row::Working);
        }
        self.sync_rows(lead, trail);

        div()
            .key_context(super::CONTEXT)
            .on_action(cx.listener(Self::on_escape))
            .size_full()
            .flex()
            .flex_col()
            .bg(t.app)
            .text_color(t.tx)
            .type_body()
            .when_some(subagent.as_ref(), |d, sub| {
                d.child(self.render_subagent_header(sub, cx))
            })
            .child(
                list(
                    self.list.clone(),
                    cx.processor(|this, ix, _, cx| this.render_row(ix, cx)),
                )
                .flex_1()
                .min_h_0()
                .py(SPACE_4),
            )
            .map(|d| match &subagent {
                Some(_) => d.child(self.render_subagent_footer(cx)),
                None => d.child(self.render_composer(cx)),
            })
    }
}

impl ChatView {
    /// Row `ix` of the transcript list, laid out alone: only rows in or near the viewport are
    /// rendered.
    fn render_row(&mut self, ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let element = match self.rows.get(ix) {
            Some(Row::Item(i)) => self.render_item(i, cx),
            Some(Row::Task) => {
                let focus = self.focus.clone();
                focus.and_then(|id| self.render_subagent_task(&id, cx))
            }
            Some(Row::Result) => {
                let sub = self.focus.as_deref().and_then(|id| self.subagent(id));
                sub.and_then(|sub| self.render_subagent_result(&sub, cx))
            }
            Some(Row::Done) => self.render_done_card(cx),
            Some(Row::Working) => Some(self.render_working(cx)),
            None => None,
        };
        // The rows' top padding is the gap between them.
        centered(
            div()
                .px(SPACE_6)
                .when(ix > 0, |d| d.pt(SPACE_3))
                .children(element),
        )
        .into_any_element()
    }

    fn render_working(&self, cx: &App) -> AnyElement {
        let t = cx.tokens();
        div()
            .flex()
            .items_center()
            .gap(SPACE_2)
            .h(px(24.))
            .type_mono()
            .text_color(t.tx3)
            .child(Spinner::new().with_size(px(10.)).color(t.acc))
            // In a subagent's view esc goes back instead.
            .child(if self.asking {
                "Waiting for your answer"
            } else if self.focus.is_some() {
                "Working…"
            } else {
                "Working · esc to interrupt"
            })
            .into_any_element()
    }

    /// The summary for the "Ready for review" card, when it shows.
    fn done_text(&self, cx: &App) -> Option<String> {
        if !self.turn_done || self.running || self.exited {
            return None;
        }
        let diff = self.diff.as_ref()?.0.read(cx);
        if !diff.needs_review() {
            return None;
        }
        let summary = diff.summary()?;
        Some(done_summary(
            summary.files,
            summary.insertions,
            summary.deletions,
        ))
    }
}

impl ChatView {
    /// "Ready for review" after a turn that ended normally, while changes are unpublished.
    fn render_done_card(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let text = self.done_text(cx)?;
        let t = cx.tokens();
        Some(
            div()
                .flex()
                .flex_col()
                .gap(SPACE_2)
                .p(SPACE_3)
                .rounded(RADIUS)
                .border_1()
                .border_color(t.line)
                .bg(t.panel)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(SPACE_2)
                        .type_body_strong()
                        .child(
                            Icon::new(Lucide::CircleCheck)
                                .size(px(13.))
                                .text_color(t.grn),
                        )
                        .child("Ready for review"),
                )
                .child(div().type_mono().text_color(t.tx2).child(text))
                .child(
                    div()
                        .flex()
                        .gap(px(6.))
                        .child(
                            UiButton::new("chat-review")
                                .small()
                                .primary()
                                .icon(Lucide::FileDiff)
                                .label("Review changes")
                                .kbd("⇧⌘R")
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(crate::shell::OpenReview), cx)
                                }),
                        )
                        .child(
                            UiButton::new("chat-ask-changes")
                                .small()
                                .label("Ask for changes")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.focus_composer(window, cx)
                                })),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// `3 files changed, +12 −4`.
fn done_summary(files: usize, insertions: u64, deletions: u64) -> String {
    let s = if files == 1 { "" } else { "s" };
    format!("{files} file{s} changed, +{insertions} −{deletions}")
}

fn centered(child: impl IntoElement) -> Div {
    div()
        .flex()
        .justify_center()
        .w_full()
        .child(div().w_full().max_w(MAX_WIDTH).child(child))
}

#[cfg(test)]
mod done_card_tests {
    #[test]
    fn summary_text() {
        assert_eq!(super::done_summary(1, 3, 0), "1 file changed, +3 −0");
        assert_eq!(super::done_summary(4, 12, 7), "4 files changed, +12 −7");
    }
}
