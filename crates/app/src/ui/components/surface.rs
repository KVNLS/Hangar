//! Containers: cards, section labels, empty states, toasts.

use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::ui::{ActiveTokens as _, RADIUS_CARD, TypeScale as _, overlay_shadow};

/// Bordered panel-colored card, 8 px radius.
pub fn card(cx: &App) -> Div {
    let t = cx.tokens();
    div()
        .flex()
        .flex_col()
        .rounded(RADIUS_CARD)
        .border_1()
        .border_color(t.line)
        .bg(t.panel)
}

/// Upper-case section label (`PROJECTS`, `FILES`).
pub fn section_label(label: impl Into<SharedString>, cx: &App) -> Div {
    let label: SharedString = label.into();
    div()
        .type_label()
        .text_color(cx.tokens().tx3)
        .child(label.to_uppercase())
}

/// Centered icon, title, hint and optional actions.
#[derive(IntoElement)]
pub struct EmptyState {
    icon: Icon,
    title: SharedString,
    hint: Option<SharedString>,
    actions: Vec<AnyElement>,
}

impl EmptyState {
    pub fn new(icon: impl Into<Icon>, title: impl Into<SharedString>) -> Self {
        Self {
            icon: icon.into(),
            title: title.into(),
            hint: None,
            actions: Vec::new(),
        }
    }

    pub fn hint(mut self, hint: impl Into<SharedString>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = cx.tokens();
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(14.))
            .w(px(300.))
            .text_center()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(44.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(t.line2)
                    .text_color(t.tx3)
                    .child(self.icon.with_size(px(20.))),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().type_body_strong().text_color(t.tx).child(self.title))
                    .when_some(self.hint, |this, hint| {
                        this.child(div().type_secondary().text_color(t.tx2).child(hint))
                    }),
            )
            .children(self.actions)
    }
}

/// Bottom-right notification body (the caller positions and dismisses it).
pub fn toast(
    icon: impl Into<Icon>,
    icon_color: Hsla,
    message: impl Into<SharedString>,
    cx: &App,
) -> Div {
    let t = cx.tokens();
    let message: SharedString = message.into();
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(12.))
        .py(px(9.))
        .rounded(RADIUS_CARD)
        .border_1()
        .border_color(t.line2)
        .bg(t.elev)
        .shadow(overlay_shadow())
        .text_size(px(12.))
        .text_color(t.tx)
        .child(icon.into().with_size(px(14.)).text_color(icon_color))
        .child(message)
}
