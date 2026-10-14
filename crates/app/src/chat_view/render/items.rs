use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_agents::acp::protocol::{PlanStatus, ToolStatus};
use hangar_core::AgentState;

use super::RADIUS;
use super::helpers::{diff_stat, tool_body, tool_kind, without_verb};
use crate::chat_view::{ChatView, Item, Outcome};
use crate::chat_view::{cap, rows};
use crate::ui::components::{StateGlyph, UiButton};
use crate::ui::{ActiveTokens as _, SPACE_2, SPACE_3, TypeScale as _};

impl ChatView {
    pub(super) fn render_item(&self, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = cx.tokens();
        Some(match self.items.get(index)? {
            Item::User(text) => div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(SPACE_3)
                .py(SPACE_2)
                .rounded(RADIUS)
                .border_1()
                .border_color(t.line)
                .bg(t.elev)
                .child(div().type_secondary().text_color(t.tx3).child("You"))
                .child(text.clone())
                .into_any_element(),
            Item::Agent { view, hidden, .. } => div()
                .px(px(2.))
                // Inline code and code blocks use the theme's mono family: no ligatures there.
                .font_features(crate::ui::mono_features())
                .child(TextView::new(view).selectable(true))
                .when(*hidden > 0, |this| {
                    this.child(
                        div()
                            .type_secondary()
                            .text_color(t.tx3)
                            .child(cap::hidden_notice(*hidden)),
                    )
                })
                .into_any_element(),
            Item::Thought { text, open, .. } => {
                let open = *open;
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .child(
                        div()
                            .id(("thought", index))
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .type_secondary()
                            .text_color(t.tx3)
                            .cursor_pointer()
                            .hover(|s| s.text_color(t.tx2))
                            .child(
                                Icon::new(if open {
                                    Lucide::ChevronDown
                                } else {
                                    Lucide::ChevronRight
                                })
                                .with_size(px(12.)),
                            )
                            .child("Thinking")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(Item::Thought { open, .. }) = this.items.get_mut(index)
                                {
                                    *open = !*open;
                                    rows::touch(&mut this.dirty, index);
                                    cx.notify();
                                }
                            })),
                    )
                    .when(open, |this| {
                        let shown = text.floor_char_boundary(cap::MAX_SHOWN_BYTES);
                        this.child(
                            div()
                                .pl(px(18.))
                                .type_secondary()
                                .text_color(t.tx2)
                                .child(text[..shown].to_owned())
                                .when(shown < text.len(), |this| {
                                    this.child(cap::hidden_notice(text.len() - shown))
                                }),
                        )
                    })
                    .into_any_element()
            }
            Item::Tool(call) => self.render_tool(index, call, cx),
            Item::Plan(entries) => div()
                .flex()
                .flex_col()
                .gap(px(4.))
                .px(SPACE_3)
                .py(SPACE_2)
                .rounded(RADIUS)
                .border_1()
                .border_color(t.line)
                .bg(t.elev)
                .child(div().type_label().text_color(t.tx3).child("PLAN"))
                .children(entries.iter().map(|entry| {
                    let (icon, color) = match entry.status {
                        PlanStatus::Completed => (Lucide::CircleCheck, t.grn),
                        PlanStatus::InProgress => (Lucide::CircleDot, t.acc),
                        PlanStatus::Pending => (Lucide::Circle, t.tx3),
                    };
                    div()
                        .flex()
                        .items_start()
                        .gap(SPACE_2)
                        .child(
                            div()
                                .pt(px(3.))
                                .child(Icon::new(icon).with_size(px(12.)).text_color(color)),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(if entry.status == PlanStatus::Completed {
                                    t.tx2
                                } else {
                                    t.tx
                                })
                                .when(entry.status == PlanStatus::Completed, |d| d.line_through())
                                .child(entry.content.clone()),
                        )
                }))
                .into_any_element(),
            Item::Permission {
                request,
                title,
                outcome,
            } => match outcome {
                None => self.render_permission(index, request, title, cx),
                Some(outcome) => resolved_permission(title, outcome, cx),
            },
            Item::Notice { error, text } => div()
                .flex()
                .items_center()
                .gap(px(6.))
                .type_secondary()
                .text_color(if *error { t.red } else { t.tx3 })
                .when(*error, |d| {
                    d.child(Icon::new(Lucide::CircleX).with_size(px(12.)))
                })
                .child(text.clone())
                .into_any_element(),
        })
    }

    pub(super) fn render_tool(
        &self,
        index: usize,
        call: &hangar_agents::acp::protocol::ToolCall,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = cx.tokens();
        let (icon, verb) = tool_kind(&call.kind);
        let (meta, meta_color): (SharedString, Hsla) = match call.status {
            ToolStatus::Pending => ("pending".into(), t.tx3),
            ToolStatus::InProgress => ("running".into(), t.acc),
            ToolStatus::Completed => (diff_stat(&call.content).unwrap_or_default().into(), t.tx2),
            ToolStatus::Failed => ("failed".into(), t.red),
        };
        let body = tool_body(&call.content, &self.root, cx);
        let locations: Vec<AnyElement> = call
            .locations
            .iter()
            .enumerate()
            .map(|(n, location)| {
                let path = location.path.clone();
                let label = match location.line {
                    Some(line) => format!("{}:{line}", self.display_path(&location.path)),
                    None => self.display_path(&location.path).to_owned(),
                };
                div()
                    .id(("tool-loc", index * 64 + n))
                    .type_mono_small()
                    .text_color(t.acc)
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.open_location(&path, cx)))
                    .into_any_element()
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .rounded(RADIUS)
            .border_1()
            .border_color(if call.status == ToolStatus::Failed {
                t.red
            } else {
                t.line
            })
            .bg(t.elev)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(SPACE_2)
                    .h(px(28.))
                    .px(px(10.))
                    .font_mono()
                    .text_size(px(12.))
                    .child(Icon::new(icon).with_size(px(12.)).text_color(t.tx3))
                    .child(div().font_weight(FontWeight::SEMIBOLD).child(verb))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(t.tx2)
                            .child(without_verb(&call.title, verb).to_owned()),
                    )
                    .when(call.status == ToolStatus::InProgress, |d| {
                        d.child(Spinner::new().with_size(px(10.)).color(t.acc))
                    })
                    .child(div().text_size(px(11.)).text_color(meta_color).child(meta))
                    .when(call.is_subagent() && self.focus.is_none(), |d| {
                        let id = call.id.clone();
                        d.child(
                            UiButton::new(("open-subagent", index))
                                .small()
                                .label("Open")
                                .icon(Lucide::ChevronRight)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.focus_subagent(Some(&id), cx)
                                })),
                        )
                    }),
            )
            .when(!locations.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap(SPACE_2)
                        .px(px(10.))
                        .pb(px(6.))
                        .children(locations),
                )
            })
            .children(body)
            .into_any_element()
    }

    pub(super) fn render_permission(
        &self,
        index: usize,
        request: &hangar_agents::acp::PermissionRequest,
        title: &SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = cx.tokens();
        let first_allow = request.options.iter().position(|o| o.kind.is_allow());
        let first_reject = request.options.iter().position(|o| !o.kind.is_allow());
        let buttons: Vec<AnyElement> = request
            .options
            .iter()
            .enumerate()
            .map(|(n, option)| {
                let id = option.id.clone();
                let mut button = UiButton::new(("perm", index * 16 + n)).label(option.name.clone());
                button = if Some(n) == first_allow {
                    button.primary().kbd("⏎")
                } else if option.kind.is_allow() {
                    button
                } else if Some(n) == first_reject {
                    button.ghost().kbd("esc")
                } else {
                    button.ghost()
                };
                button
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.answer(index, Some(id.clone()), cx)),
                    )
                    .into_any_element()
            })
            .collect();
        let detail = request
            .tool_call
            .content
            .as_ref()
            .and_then(|content| tool_body(content, &self.root, cx));

        div()
            .flex()
            .flex_col()
            .gap(SPACE_2)
            .px(SPACE_3)
            .py(px(10.))
            .rounded(RADIUS)
            .border_1()
            .border_color(t.amb)
            .bg(t.ambbg)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(SPACE_2)
                    .type_body_strong()
                    .text_size(px(12.))
                    .child(StateGlyph::new(AgentState::Approval).size(px(9.)))
                    .child(if request.tool_call.kind.as_deref() == Some("execute") {
                        format!("{} wants to run a command", self.agent_name)
                    } else {
                        format!("{} asks for permission", self.agent_name)
                    }),
            )
            .child(
                div()
                    .px(px(9.))
                    .py(px(7.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.term)
                    .type_mono()
                    .child(title.clone()),
            )
            .children(detail)
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(px(6.))
                    .children(buttons),
            )
            .into_any_element()
    }
}

pub(super) fn resolved_permission(title: &SharedString, outcome: &Outcome, cx: &App) -> AnyElement {
    let t = cx.tokens();
    let (icon, color) = if outcome.allowed {
        (Lucide::Check, t.grn)
    } else {
        (Lucide::X, t.red)
    };
    div()
        .flex()
        .items_center()
        .gap(SPACE_2)
        .h(px(28.))
        .px(px(10.))
        .rounded(RADIUS)
        .border_1()
        .border_color(t.line)
        .type_mono()
        .text_color(t.tx2)
        .child(Icon::new(icon).with_size(px(12.)).text_color(color))
        .child(outcome.label.clone())
        .child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(t.tx3)
                .child(title.clone()),
        )
        .into_any_element()
}
