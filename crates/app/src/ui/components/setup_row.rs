//! One row of the setup checklist: status dot, title, what was found or what is wrong, and the
//! command that fixes it with a copy button.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_core::setup::SetupCheck;

use super::UiButton;
use crate::ui::{ActiveTokens as _, RADIUS_BADGE, TypeScale as _};

/// `id` keeps the copy button's element id unique where several lists show the same check.
pub fn setup_row(id: &str, check: &SetupCheck, cx: &App) -> Div {
    let t = cx.tokens();
    let dot = match (check.is_ok(), check.required) {
        (true, _) => t.grn,
        (false, true) => t.red,
        (false, false) => t.amb,
    };
    let fix = check.fix.clone().map(|command| {
        let copied = command.clone();
        h_flex()
            .mt_1()
            .gap_2()
            .items_center()
            .px_2()
            .py_1()
            .rounded(RADIUS_BADGE)
            .border_1()
            .border_color(t.line)
            .bg(t.term)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .type_mono_small()
                    .text_color(t.tx)
                    .child(command),
            )
            .child(
                UiButton::new(SharedString::from(format!("setup-copy-{id}-{}", check.id)))
                    .icon(Lucide::Copy)
                    .ghost()
                    .small()
                    .tooltip("Copy the command, then run it in a terminal")
                    .on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(copied.clone()))
                    }),
            )
    });
    h_flex()
        .items_start()
        .gap_3()
        .px(px(14.))
        .py(px(10.))
        .child(
            div()
                .flex_none()
                .mt(px(5.))
                .size(px(8.))
                .rounded_full()
                .bg(dot),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(div().type_body_strong().child(check.title.clone()))
                        .when(check.required && !check.is_ok(), |this| {
                            this.child(div().type_secondary().text_color(t.red).child("Required"))
                        }),
                )
                .child(if check.is_ok() {
                    div()
                        .type_mono_small()
                        .text_color(t.tx3)
                        .truncate()
                        .child(check.detail.clone())
                } else {
                    div()
                        .type_secondary()
                        .text_color(t.tx2)
                        .child(check.detail.clone())
                })
                .children(fix),
        )
}
