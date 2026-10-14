//! The full-width files screen: tree column beside the viewer.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::ui::components::{EmptyState, UiButton, section_label};
use crate::ui::{ActiveTokens as _, TypeScale as _};

use super::{ExitFiles, FilesEvent, FilesPanel};

impl FilesPanel {
    pub(super) fn render_wide(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.tokens();
        let back = UiButton::new("files-back")
            .icon(Lucide::ArrowLeft)
            .label("Workspace")
            .kbd("esc")
            .ghost()
            .on_click(cx.listener(|_, _, _, cx| cx.emit(FilesEvent::ExitWide)));
        let screen = v_flex()
            .size_full()
            .key_context("FilesScreen")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &ExitFiles, _, cx| cx.emit(FilesEvent::ExitWide)))
            .bg(t.app);
        if self.source.is_none() {
            return screen
                .child(h_flex().h(px(48.)).px(px(14.)).items_center().child(back))
                .child(
                    div().flex_1().flex().items_center().justify_center().child(
                        EmptyState::new(Lucide::Folder, "No workspace")
                            .hint("Focus a pane that shows a workspace to browse its files."),
                    ),
                )
                .into_any_element();
        }
        let top = h_flex()
            .flex_none()
            .h(px(48.))
            .px(px(14.))
            .gap(px(10.))
            .items_center()
            .bg(t.panel)
            .border_b_1()
            .border_color(t.line)
            .child(back)
            .child(div().w(px(1.)).h(px(18.)).bg(t.line))
            .child(div().type_body_strong().child("Files"))
            .child(
                div()
                    .font_mono()
                    .text_size(px(11.))
                    .text_color(t.tx3)
                    .child(self.summary(cx)),
            );
        let column = v_flex()
            .w(px(280.))
            .flex_none()
            .h_full()
            .bg(t.panel)
            .border_r_1()
            .border_color(t.line)
            .child(
                h_flex()
                    .px_3()
                    .pt(px(10.))
                    .pb_1()
                    .items_center()
                    .justify_between()
                    .child(section_label("Tree", cx))
                    .child(h_flex().gap_1().children(self.tree_buttons(cx))),
            )
            .when_some(self.error.clone(), |this, err| {
                this.child(
                    div()
                        .px_3()
                        .py_1p5()
                        .text_size(px(11.))
                        .text_color(t.red)
                        .child(err),
                )
            })
            .child(div().flex_1().min_h_0().p_1p5().child(self.render_tree(cx)));
        screen
            .child(top)
            .child(
                h_flex().flex_1().min_h_0().child(column).child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(self.render_viewer(cx)),
                ),
            )
            .into_any_element()
    }
}
