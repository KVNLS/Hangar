//! A subagent's transcript: header, the task it was given, its result, and a footer in place
//! of the composer (subagents take no messages).

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_agents::acp::protocol::ToolContent;
use hangar_agents::acp::subagents::{Subagent, SubagentStatus};
use hangar_core::AgentState;

use super::ChatView;
use crate::ui::components::{StateGlyph, UiButton};
use crate::ui::{ActiveTokens as _, SPACE_2, SPACE_3, SPACE_6, TypeScale as _};

/// The glyph a subagent shows, in the agents' state colours.
pub fn subagent_state(status: SubagentStatus) -> AgentState {
    match status {
        SubagentStatus::Running => AgentState::Working,
        SubagentStatus::Approval => AgentState::Approval,
        SubagentStatus::Done => AgentState::Review,
        SubagentStatus::Failed => AgentState::Failed,
    }
}

fn bubble(label: &'static str, text: String, cx: &App) -> Div {
    let t = cx.tokens();
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .px(SPACE_3)
        .py(SPACE_2)
        .rounded(px(6.))
        .border_1()
        .border_color(t.line)
        .bg(t.elev)
        .child(div().type_secondary().text_color(t.tx3).child(label))
        .child(text)
}

impl ChatView {
    pub(super) fn render_subagent_header(&self, sub: &Subagent, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let kind = self.launch(&sub.id).and_then(|c| c.subagent_type.clone());
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_2)
            .h(px(36.))
            .px(SPACE_3)
            .border_b_1()
            .border_color(t.line)
            .bg(t.panel)
            .child(
                UiButton::new("subagent-back")
                    .small()
                    .icon(Lucide::ArrowLeft)
                    .label(self.agent_name.clone())
                    .kbd("esc")
                    .on_click(cx.listener(|this, _, _, cx| this.focus_subagent(None, cx))),
            )
            .child(div().text_color(t.tx3).child("/"))
            .child(StateGlyph::new(subagent_state(sub.status)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .type_body_strong()
                    .child(sub.name.clone()),
            )
            .when_some(kind, |d, kind| {
                d.child(div().type_mono_small().text_color(t.tx3).child(kind))
            })
            .child(div().type_mono_small().text_color(t.tx3).child(format!(
                "{} tool call{}",
                sub.tool_calls,
                if sub.tool_calls == 1 { "" } else { "s" }
            )))
    }

    /// The prompt the agent gave the subagent.
    pub(super) fn render_subagent_task(&self, id: &str, cx: &App) -> Option<AnyElement> {
        let prompt = self.launch(id)?.prompt.clone()?;
        Some(bubble("Task", prompt, cx).into_any_element())
    }

    /// What the subagent returned to the agent, once it ended.
    pub(super) fn render_subagent_result(&self, sub: &Subagent, cx: &App) -> Option<AnyElement> {
        let label = match sub.status {
            SubagentStatus::Done => "Returned",
            SubagentStatus::Failed => "Failed",
            SubagentStatus::Running | SubagentStatus::Approval => return None,
        };
        let text = self
            .launch(&sub.id)?
            .content
            .iter()
            .filter_map(|c| match c {
                ToolContent::Text(text) => Some(text.trim()),
                _ => None,
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n");
        (!text.is_empty()).then(|| bubble(label, text, cx).into_any_element())
    }

    pub(super) fn render_subagent_footer(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(SPACE_2)
            .px(SPACE_6)
            .py(SPACE_3)
            .border_t_1()
            .border_color(t.line)
            .child(
                div()
                    .flex_1()
                    .type_secondary()
                    .text_color(t.tx3)
                    .child(format!(
                        "Subagents take no messages; {} reads what they return.",
                        self.agent_name
                    )),
            )
            .child(
                UiButton::new("subagent-footer-back")
                    .small()
                    .icon(Lucide::ArrowLeft)
                    .label(format!("Back to {}", self.agent_name))
                    .on_click(cx.listener(|this, _, _, cx| this.focus_subagent(None, cx))),
            )
    }
}
