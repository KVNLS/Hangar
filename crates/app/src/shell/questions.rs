//! `ask_user`: an agent's question to the user. The workspace reads "Needs input" until it is
//! answered from the answer sheet (overview card, pane banner, palette or ⌘J); the agent's tool
//! call waits for it. A question is withdrawn when its agent stops or the workspace closes.

use anyhow::anyhow;
use gpui_kit::component::input::{Textarea, TextareaState};
use hangar_mcp::Answer;

use super::*;
use crate::mcp_host::Reply;
use crate::ui::components::{UiButton, card};
use crate::ui::{ActiveTokens as _, RADIUS_SHEET, TITLEBAR_HEIGHT, TypeScale as _, overlay_shadow};

/// A question an agent waits on.
pub(super) struct PendingQuestion {
    pub text: String,
    pub choices: Vec<String>,
    pub asked_at: hangar_core::Timestamp,
    reply: Reply<Answer>,
}

/// The open answer sheet.
pub(super) struct AnswerSheet {
    ws: WorkspaceId,
    input: Entity<TextareaState>,
    _subscription: Subscription,
}

/// The first line of a question, for one-line places (cards, palette, banner).
pub(super) fn question_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or_default()
}

impl HangarApp {
    /// A new question from `ws`'s agent. A question still open in that workspace is replaced.
    pub(super) fn on_question(
        &mut self,
        ws: WorkspaceId,
        text: String,
        choices: Vec<String>,
        reply: Reply<Answer>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.work.projects.is_active(ws) {
            let _ = reply.send(Err(anyhow!("the workspace is closed")));
            return;
        }
        if let Some(old) = self.live.questions.remove(&ws) {
            let _ = old.reply.send(Err(anyhow!(
                "replaced by a newer question from the same agent"
            )));
        }
        tracing::info!(workspace = %ws, "agent asks the user");
        let line = question_line(&text).to_owned();
        self.live.questions.insert(
            ws,
            PendingQuestion {
                text,
                choices,
                asked_at: hangar_core::Timestamp::now(),
                reply,
            },
        );
        if let Some(parent) = self.work.projects.workspace(ws).and_then(|w| w.parent) {
            let name = self
                .work
                .projects
                .workspace(ws)
                .map(|w| w.name.clone())
                .unwrap_or_default();
            self.tell_parent(
                parent,
                false,
                format!("“{name}” asks the user: {line}"),
                window,
                cx,
            );
        }
        self.sync_attention(cx);
        cx.notify();
    }

    pub(super) fn question(&self, ws: WorkspaceId) -> Option<&PendingQuestion> {
        self.live.questions.get(&ws)
    }

    /// Withdraws `ws`'s question (its agent stopped, the workspace closed): the tool call fails.
    pub(super) fn withdraw_question(&mut self, ws: WorkspaceId, why: &str) {
        if let Some(question) = self.live.questions.remove(&ws) {
            let _ = question
                .reply
                .send(Err(anyhow!("the question was withdrawn: {why}")));
            if self.work.answer.as_ref().is_some_and(|s| s.ws == ws) {
                self.work.answer = None;
            }
        }
    }

    pub(super) fn answer_question(
        &mut self,
        ws: WorkspaceId,
        answer: Answer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(question) = self.live.questions.remove(&ws) else {
            return;
        };
        tracing::info!(workspace = %ws, choice = ?answer.choice, "user answered");
        let _ = question.reply.send(Ok(answer));
        self.close_answer_sheet(window, cx);
        self.sync_attention(cx);
        cx.notify();
    }

    pub(super) fn open_answer_sheet(
        &mut self,
        ws: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.live.questions.contains_key(&ws) {
            return;
        }
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Type an answer")
                .auto_grow(2, 6)
        });
        let subscription = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
        input.update(cx, |state, cx| state.focus(window, cx));
        self.work.answer = Some(AnswerSheet {
            ws,
            input,
            _subscription: subscription,
        });
        cx.notify();
    }

    pub(super) fn close_answer_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.work.answer.take().is_some() {
            window.focus(&self.focus, cx);
            self.focus_terminal(window, cx);
            cx.notify();
        }
    }

    fn submit_typed_answer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = &self.work.answer else {
            return;
        };
        let text = sheet.input.read(cx).value().trim().to_owned();
        if text.is_empty() {
            return;
        }
        let ws = sheet.ws;
        self.answer_question(ws, Answer { text, choice: None }, window, cx);
    }

    pub(super) fn render_answer_sheet(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.work.answer.as_ref()?;
        let question = self.live.questions.get(&sheet.ws)?;
        let t = cx.tokens();
        let ws = sheet.ws;
        let workspace = self
            .work
            .projects
            .workspace(ws)
            .map(|w| w.name.clone())
            .unwrap_or_default();
        let agent = self
            .live
            .sessions
            .get(&ws)
            .map(|l| l.agent_name.to_string())
            .unwrap_or_else(|| "The agent".to_owned());
        let typed = !sheet.input.read(cx).value().trim().is_empty();
        let asked = match super::status::age_label(
            question.asked_at.as_millis(),
            hangar_core::Timestamp::now().as_millis(),
        ) {
            age if age == "now" => "just now".to_owned(),
            age => format!("{age} ago"),
        };
        let choices = question.choices.iter().enumerate().map(|(i, choice)| {
            let text = choice.clone();
            UiButton::new(SharedString::from(format!("answer-choice-{i}")))
                .label(choice.clone())
                .on_click(cx.listener(move |this, _, window, cx| {
                    let answer = Answer {
                        text: text.clone(),
                        choice: Some(i),
                    };
                    this.answer_question(ws, answer, window, cx)
                }))
        });
        let panel =
            v_flex()
                .id("answer-sheet")
                .key_context("AnswerSheet")
                .w(px(640.))
                .bg(t.elev)
                .border_1()
                .border_t_0()
                .border_color(t.line2)
                .rounded_b(RADIUS_SHEET)
                .shadow(overlay_shadow())
                .text_color(t.tx)
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_action(cx.listener(|this, _: &CloseSheet, window, cx| {
                    this.close_answer_sheet(window, cx)
                }))
                .on_action(cx.listener(|this, _: &SubmitSheet, window, cx| {
                    this.submit_typed_answer(window, cx)
                }))
                .child(
                    h_flex()
                        .h(px(44.))
                        .px_4()
                        .gap_2()
                        .items_center()
                        .border_b_1()
                        .border_color(t.line)
                        .child(
                            div()
                                .text_size(px(14.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("{agent} asks")),
                        )
                        .child(
                            div()
                                .type_secondary()
                                .text_color(t.tx3)
                                .child(format!("{workspace} · asked {asked}")),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .font_mono()
                                .text_size(px(10.))
                                .text_color(t.tx3)
                                .child("esc"),
                        ),
                )
                .child(
                    v_flex()
                        .p_4()
                        .gap_3()
                        .child(
                            card(cx)
                                .px_3()
                                .py(px(10.))
                                .type_body()
                                .child(question.text.clone()),
                        )
                        .when(!question.choices.is_empty(), |this| {
                            this.child(h_flex().flex_wrap().gap_2().children(choices))
                        })
                        .child(Textarea::new(&sheet.input)),
                )
                .child(
                    h_flex()
                        .h(px(52.))
                        .px_4()
                        .gap_2()
                        .items_center()
                        .border_t_1()
                        .border_color(t.line)
                        .child(
                            div()
                                .flex_1()
                                .type_secondary()
                                .text_color(t.tx3)
                                .child("The agent waits for your answer."),
                        )
                        .child(
                            UiButton::new("answer-later")
                                .label("Later")
                                .ghost()
                                .large()
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_answer_sheet(window, cx)
                                })),
                        )
                        .child(
                            UiButton::new("answer-send")
                                .label("Send answer")
                                .kbd("⌘⏎")
                                .primary()
                                .large()
                                .disabled(!typed)
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.submit_typed_answer(window, cx)
                                })),
                        ),
                );
        Some(
            div()
                .id("answer-scrim")
                .absolute()
                .top(TITLEBAR_HEIGHT)
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .justify_center()
                .items_start()
                .bg(t.scrim)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_answer_sheet(window, cx)),
                )
                .child(panel)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::question_line;

    #[test]
    fn question_line_is_the_first_non_empty_line() {
        assert_eq!(question_line("\n  Which API?  \nmore"), "Which API?");
        assert_eq!(question_line(" "), "");
    }
}
