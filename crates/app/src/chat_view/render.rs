//! Transcript and composer elements, after the agent pane of `design/Hangar.dc.html`.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_agents::acp::protocol::{ConfigOption, PlanStatus, ToolContent, ToolStatus};
use hangar_core::AgentState;

use super::{ChatView, Item, Outcome};
use crate::ui::components::{StateGlyph, UiButton};
use crate::ui::{ActiveTokens as _, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TypeScale as _};

const MAX_WIDTH: Pixels = px(700.);
const RADIUS: Pixels = px(6.);
/// Longest tool output or diff shown in a card, in lines.
const MAX_CARD_LINES: usize = 40;

impl Render for ChatView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        if self.follow {
            self.scroll.scroll_to_bottom();
        }
        let items: Vec<AnyElement> = (0..self.items.len())
            .filter_map(|i| self.render_item(i, cx))
            .collect();
        let working = self.running && self.pending_permission().is_none();

        div()
            .key_context(super::CONTEXT)
            .on_action(cx.listener(Self::on_escape))
            .size_full()
            .flex()
            .flex_col()
            .bg(t.app)
            .text_color(t.tx)
            .type_body()
            .child(
                div()
                    .id("chat-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, _| {
                        let dy = event.delta.pixel_delta(window.line_height()).y;
                        if dy > px(0.) {
                            this.follow = false;
                        } else {
                            let offset = this.scroll.offset().y;
                            let max = this.scroll.max_offset().y;
                            this.follow = -offset + px(24.) >= max;
                        }
                    }))
                    .child(centered(
                        div()
                            .flex()
                            .flex_col()
                            .gap(SPACE_3)
                            .px(SPACE_6)
                            .py(SPACE_4)
                            .children(items)
                            .when(working, |this| {
                                this.child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(SPACE_2)
                                        .h(px(24.))
                                        .type_mono()
                                        .text_color(t.tx3)
                                        .child(Spinner::new().with_size(px(10.)).color(t.acc))
                                        .child("Working…"),
                                )
                            }),
                    )),
            )
            .child(self.render_composer(cx))
    }
}

fn centered(child: impl IntoElement) -> Div {
    div()
        .flex()
        .justify_center()
        .w_full()
        .child(div().w_full().max_w(MAX_WIDTH).child(child))
}

impl ChatView {
    fn render_item(&self, index: usize, cx: &mut Context<Self>) -> Option<AnyElement> {
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
            Item::Agent { view, .. } => div()
                .px(px(2.))
                .child(TextView::new(view).selectable(true))
                .into_any_element(),
            Item::Thought { text, open } => {
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
                                    cx.notify();
                                }
                            })),
                    )
                    .when(open, |this| {
                        this.child(
                            div()
                                .pl(px(18.))
                                .type_secondary()
                                .text_color(t.tx2)
                                .child(text.clone()),
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

    fn render_tool(
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
                    .child(div().text_size(px(11.)).text_color(meta_color).child(meta)),
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

    fn render_permission(
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
                    .child(format!("{} asks for permission", self.agent_name)),
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

    /// The session mode as a chip; with several modes it opens a menu to switch.
    fn render_mode_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = cx.tokens();
        let mode = self.mode.clone()?;
        let modes = self.session.modes().to_vec();
        if modes.len() < 2 {
            return Some(
                div()
                    .h(px(20.))
                    .px(px(6.))
                    .rounded(px(4.))
                    .bg(t.hover)
                    .flex()
                    .items_center()
                    .child(mode)
                    .into_any_element(),
            );
        }
        let current = self.mode_id.clone();
        let view = cx.entity().downgrade();
        Some(
            Button::new("chat-mode")
                .ghost()
                .xsmall()
                .label(mode)
                .icon(Lucide::ChevronDown)
                .tooltip("Session mode")
                .dropdown_menu(move |mut menu, _, _| {
                    for mode in &modes {
                        let view = view.clone();
                        let id = mode.id.clone();
                        menu = menu.item(
                            PopupMenuItem::new(mode.name.clone())
                                .checked(current.as_deref() == Some(mode.id.as_str()))
                                .on_click(move |_, _, cx| {
                                    let _ = view
                                        .update(cx, |chat, cx| chat.choose_mode(id.clone(), cx));
                                }),
                        );
                    }
                    menu
                })
                .into_any_element(),
        )
    }

    /// A menu chip per non-mode config option with a choice (model, effort, fast mode, …).
    fn render_config_chips(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let view = cx.entity().downgrade();
        self.config
            .iter()
            .filter(|o| !o.is_mode() && o.values.len() > 1)
            .map(|option| {
                let view = view.clone();
                let option = option.clone();
                Button::new(SharedString::from(format!("chat-config-{}", option.id)))
                    .ghost()
                    .xsmall()
                    .label(config_label(&option))
                    .icon(Lucide::ChevronDown)
                    .tooltip(option.name.clone())
                    .dropdown_menu(move |mut menu, _, _| {
                        for value in &option.values {
                            let view = view.clone();
                            let (id, picked) = (option.id.clone(), value.value.clone());
                            menu = menu.item(
                                PopupMenuItem::new(value.name.clone())
                                    .checked(value.value == option.current)
                                    .on_click(move |_, _, cx| {
                                        let _ = view.update(cx, |chat, cx| {
                                            chat.choose_config(id.clone(), picked.clone(), cx)
                                        });
                                    }),
                            );
                        }
                        menu
                    })
                    .into_any_element()
            })
            .collect()
    }

    fn render_commands(
        &self,
        commands: Vec<hangar_agents::acp::protocol::AvailableCommand>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = cx.tokens();
        div()
            .flex()
            .flex_col()
            .py(px(4.))
            .rounded(RADIUS)
            .border_1()
            .border_color(t.line2)
            .bg(t.elev)
            .children(commands.into_iter().enumerate().map(|(n, command)| {
                let name = command.name.clone();
                div()
                    .id(("slash", n))
                    .flex()
                    .items_center()
                    .gap(SPACE_2)
                    .h(px(26.))
                    .px(px(10.))
                    .cursor_pointer()
                    .hover(|s| s.bg(t.hover))
                    .child(
                        div()
                            .flex_none()
                            .type_mono()
                            .text_color(t.tx)
                            .child(format!("/{}", command.name)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .type_secondary()
                            .text_color(t.tx3)
                            .child(command.description),
                    )
                    .on_click(
                        cx.listener(move |this, _, window, cx| {
                            this.pick_command(&name, window, cx)
                        }),
                    )
            }))
    }

    fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let inner = if self.exited {
            div().type_secondary().text_color(t.tx3).child(format!(
                "{} is no longer running. Restart it from the pane header.",
                self.agent_name
            ))
        } else {
            let action = if self.running {
                UiButton::new("chat-stop")
                    .small()
                    .icon(Lucide::Square)
                    .label("Stop")
                    .kbd("esc")
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx)))
                    .into_any_element()
            } else {
                UiButton::new("chat-send")
                    .small()
                    .primary()
                    .icon(Lucide::ArrowUp)
                    .tooltip("Send")
                    .on_click(cx.listener(|this, _, window, cx| this.on_enter(window, cx)))
                    .into_any_element()
            };
            div()
                .flex()
                .flex_col()
                .gap(SPACE_2)
                .child(Textarea::new(&self.composer).appearance(false))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(6.))
                        .type_mono_small()
                        .text_color(t.tx2)
                        .when_some(self.render_mode_chip(cx), |d, chip| d.child(chip))
                        .children(self.render_config_chips(cx))
                        .child(div().flex_1())
                        .child(div().text_color(t.tx3).child("⏎ send · ⇧⏎ newline"))
                        .child(action),
                )
        };
        let commands = self.command_matches(cx);
        div()
            .flex_none()
            .px(SPACE_6)
            .pt(SPACE_2)
            .pb(SPACE_3)
            .when(!commands.is_empty() && !self.exited, |d| {
                d.child(centered(self.render_commands(commands, cx)).mb(px(6.)))
            })
            .child(centered(
                div()
                    .px(px(10.))
                    .py(SPACE_2)
                    .rounded(RADIUS)
                    .border_1()
                    .border_color(t.line2)
                    .bg(t.elev)
                    .child(inner),
            ))
    }
}

fn resolved_permission(title: &SharedString, outcome: &Outcome, cx: &App) -> AnyElement {
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

/// Icon and short verb for an ACP tool kind.
fn tool_kind(kind: &str) -> (Lucide, &'static str) {
    match kind {
        "read" => (Lucide::FileText, "Read"),
        "edit" => (Lucide::Pencil, "Edit"),
        "delete" => (Lucide::Trash, "Delete"),
        "move" => (Lucide::Move, "Move"),
        "search" => (Lucide::Search, "Search"),
        "execute" => (Lucide::SquareTerminal, "Run"),
        "think" => (Lucide::Brain, "Think"),
        "fetch" => (Lucide::Globe, "Fetch"),
        "switch_mode" => (Lucide::ArrowRightLeft, "Mode"),
        _ => (Lucide::Wrench, "Tool"),
    }
}

/// `title` without a leading `verb` the card header already shows ("Read main.rs" → "main.rs").
fn without_verb<'a>(title: &'a str, verb: &str) -> &'a str {
    match title.get(..verb.len()) {
        Some(head) if head.eq_ignore_ascii_case(verb) && title[verb.len()..].starts_with(' ') => {
            title[verb.len()..].trim_start()
        }
        _ => title,
    }
}

/// `+a −r` over the diffs of a tool call, if it has any.
fn diff_stat(content: &[ToolContent]) -> Option<String> {
    let mut added = 0;
    let mut removed = 0;
    let mut any = false;
    for item in content {
        if let ToolContent::Diff {
            old_text, new_text, ..
        } = item
        {
            any = true;
            for line in diff_lines(old_text.as_deref().unwrap_or_default(), new_text) {
                match line.0 {
                    '+' => added += 1,
                    '-' => removed += 1,
                    _ => {}
                }
            }
        }
    }
    any.then(|| format!("+{added} −{removed}"))
}

/// The changed middle of `old` → `new` (common leading and trailing lines dropped), as removed
/// lines then added lines.
fn diff_lines<'a>(old: &'a str, new: &'a str) -> Vec<(char, &'a str)> {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    old[prefix..old.len() - suffix]
        .iter()
        .map(|l| ('-', *l))
        .chain(new[prefix..new.len() - suffix].iter().map(|l| ('+', *l)))
        .collect()
}

/// The content area of a tool card: output text, diffs, or nothing.
fn tool_body(content: &[ToolContent], root: &std::path::Path, cx: &App) -> Option<AnyElement> {
    let t = cx.tokens();
    let mut lines: Vec<AnyElement> = Vec::new();
    let mut hidden = 0usize;
    let mut push = |line: AnyElement, lines: &mut Vec<AnyElement>| {
        if lines.len() < MAX_CARD_LINES {
            lines.push(line);
        } else {
            hidden += 1;
        }
    };
    for item in content {
        match item {
            ToolContent::Text(text) => {
                // Agents wrap command output in Markdown fences; the card is already code.
                for line in text.lines().filter(|l| !l.trim_start().starts_with("```")) {
                    push(
                        div()
                            .px(px(10.))
                            .whitespace_nowrap()
                            .text_color(t.tx2)
                            .child(line.to_owned())
                            .into_any_element(),
                        &mut lines,
                    );
                }
            }
            ToolContent::Diff {
                path,
                old_text,
                new_text,
            } => {
                push(
                    div()
                        .px(px(10.))
                        .text_color(t.tx3)
                        .child(
                            std::path::Path::new(path)
                                .strip_prefix(root)
                                .unwrap_or(std::path::Path::new(path))
                                .display()
                                .to_string(),
                        )
                        .into_any_element(),
                    &mut lines,
                );
                for (sign, line) in diff_lines(old_text.as_deref().unwrap_or_default(), new_text) {
                    let (bg, fg) = if sign == '+' {
                        (t.addbg, t.grn)
                    } else {
                        (t.delbg, t.red)
                    };
                    push(
                        div()
                            .flex()
                            .px(px(10.))
                            .bg(bg)
                            .whitespace_nowrap()
                            .child(div().w(px(14.)).text_color(fg).child(sign.to_string()))
                            .child(line.to_owned())
                            .into_any_element(),
                        &mut lines,
                    );
                }
            }
            ToolContent::Terminal(id) => push(
                div()
                    .px(px(10.))
                    .text_color(t.tx3)
                    .child(format!("[terminal {id}: output not shown]"))
                    .into_any_element(),
                &mut lines,
            ),
        }
    }
    if lines.is_empty() {
        return None;
    }
    Some(
        div()
            .border_t_1()
            .border_color(t.line)
            .py(px(4.))
            .font_mono()
            .text_size(px(11.5))
            .line_height(px(18.))
            .overflow_hidden()
            .children(lines)
            .when(hidden > 0, |d| {
                d.child(
                    div()
                        .px(px(10.))
                        .text_color(t.tx3)
                        .child(format!("… {hidden} more lines")),
                )
            })
            .into_any_element(),
    )
}

/// Model and effort values speak for themselves ("Opus 5.5", "High"); other options need their
/// name ("Fast mode: Off").
fn config_label(option: &ConfigOption) -> String {
    match option.category.as_deref() {
        Some("model" | "thought_level") => option.current_name().to_owned(),
        _ => format!("{}: {}", option.name, option.current_name()),
    }
}

#[cfg(test)]
mod tests {
    use super::{diff_lines, without_verb};

    #[test]
    fn verb_is_not_repeated() {
        assert_eq!(without_verb("Read main.rs", "Read"), "main.rs");
        assert_eq!(without_verb("edit a.rs", "Edit"), "a.rs");
        assert_eq!(without_verb("Readme", "Read"), "Readme");
        assert_eq!(without_verb("git status", "Run"), "git status");
    }

    #[test]
    fn diff_drops_common_lines() {
        assert_eq!(
            diff_lines("a\nb\nc\n", "a\nB\nc\n"),
            vec![('-', "b"), ('+', "B")]
        );
        assert_eq!(diff_lines("", "x\ny"), vec![('+', "x"), ('+', "y")]);
        assert!(diff_lines("same", "same").is_empty());
    }
}
