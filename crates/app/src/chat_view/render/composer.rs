use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::Textarea;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_agents::acp::protocol::ConfigOption;

use super::{RADIUS, centered};
use crate::chat_view::{ChatView, usage_label};
use crate::ui::components::UiButton;
use crate::ui::{ActiveTokens as _, SPACE_2, SPACE_3, SPACE_6, TypeScale as _};

impl ChatView {
    /// The session mode as a chip; with several modes it opens a menu to switch.
    pub(super) fn render_mode_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
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
    pub(super) fn render_config_chips(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
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

    pub(super) fn render_commands(
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

    pub(super) fn render_composer(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
                        .when_some(usage_label(&self.usage), |d, label| {
                            d.child(div().px(px(6.)).text_color(t.tx3).child(label))
                        })
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

/// Model and effort values speak for themselves ("Opus 5.5", "High"); other options need their
/// name ("Fast mode: Off").
pub(super) fn config_label(option: &ConfigOption) -> String {
    match option.category.as_deref() {
        Some("model" | "thought_level") => option.current_name().to_owned(),
        _ => format!("{}: {}", option.name, option.current_name()),
    }
}
