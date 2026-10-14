//! Rendering the narrow panel: header, file list and the unified diff.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::changes::model::{ListEntry, file_title, status_letter};
use crate::changes::review::StageState;
use crate::ui::components::{Delta, UiButton};
use crate::ui::{ActiveTokens as _, TypeScale as _};

use super::rows::{counts, render_row, split_path, status_color};
use super::{ChangesEvent, ChangesPanel, CommitStaged, LIST_MAX_ROWS, LIST_ROW_HEIGHT};

impl ChangesPanel {
    /// `branch → base` with the totals and a refresh button; errors below it.
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = cx.tokens();
        let diff = self.source.as_ref().map(|s| s.diff.read(cx));
        let summary = diff.and_then(|d| d.summary().cloned());
        let error = diff.and_then(|d| d.error().cloned());
        let line = h_flex()
            .h(px(36.))
            .px_3()
            .gap_2()
            .flex_shrink_0()
            .items_center()
            .font_mono()
            .text_size(px(11.))
            .text_color(t.tx2)
            .map(|this| match (&self.source, &summary) {
                (None, _) => this.child("No workspace"),
                (Some(_), None) => this.child("Computing diff…"),
                (Some(source), Some(summary)) => this
                    .child(Icon::new(Lucide::GitBranch).size(px(12.)))
                    .child(
                        div().min_w_0().truncate().text_color(t.tx).child(
                            summary
                                .status
                                .branch
                                .clone()
                                .unwrap_or_else(|| source.name.to_string()),
                        ),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_color(t.tx3)
                            .child(format!("→ {}", self.base_label(&summary.merge_base))),
                    ),
            })
            .child(div().flex_1())
            .when_some(summary.as_ref(), |this, summary| {
                let n = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
                this.child(Delta::new(n(summary.insertions), n(summary.deletions)))
            })
            .when(self.source.is_some(), |this| {
                this.child(
                    UiButton::new("changes-full-review")
                        .label("Full review")
                        .kbd("⇧⌘R")
                        .ghost()
                        .small()
                        .tooltip("Review every change full width")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ChangesEvent::OpenReview))),
                )
                .child(
                    UiButton::new("changes-refresh")
                        .icon(Lucide::RefreshCw)
                        .ghost()
                        .small()
                        .tooltip("Recompute the diff")
                        .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                )
            });
        v_flex()
            .flex_shrink_0()
            .border_b_1()
            .border_color(t.line)
            .child(line)
            .when_some(error, |this, err| {
                this.child(
                    div()
                        .px_3()
                        .pb_1p5()
                        .text_size(px(11.))
                        .text_color(t.red)
                        .child(err),
                )
            })
    }

    pub(super) fn render_file_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let count = self.list.len();
        let height = px(LIST_ROW_HEIGHT * count.min(LIST_MAX_ROWS) as f32);
        div()
            .map(|this| {
                if self.wide {
                    this.flex_1().min_h_0()
                } else {
                    this.h(height + px(8.)).flex_shrink_0()
                }
            })
            .px_1p5()
            .py_1()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                uniform_list(
                    "changes-files",
                    count,
                    cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                        let list = this.list.clone();
                        range
                            .filter_map(|ix| {
                                list.get(ix).map(|e| this.render_list_entry(ix, e, cx))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full()
                .track_scroll(&self.list_scroll),
            )
    }

    fn render_list_entry(
        &self,
        ix: usize,
        entry: &ListEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let row = h_flex()
            .id(("changes-file", ix))
            .h(px(LIST_ROW_HEIGHT))
            .w_full()
            .px_1p5()
            .gap_2()
            .items_center()
            .rounded(px(4.))
            .text_size(px(12.));
        match entry {
            ListEntry::Header { group, count } => row
                .text_color(cx.tokens().tx3)
                .type_label()
                .child(format!("{} ({count})", group.label().to_uppercase()))
                .into_any_element(),
            ListEntry::File(file) => {
                let selected = self.selected.as_deref() == Some(file.path.as_path());
                let (dir, name) = split_path(&file.path);
                let path = file.path.clone();
                let change = self.stage.get(&file.path).cloned();
                let state = StageState::of(change.as_ref());
                let busy = self.current_busy().is_some();
                let toggle = change.map(|change| {
                    let tooltip = match state {
                        StageState::Staged => "Staged: click to unstage",
                        StageState::Partial => "Partly staged: click to stage the rest",
                        _ => "Click to stage",
                    };
                    Checkbox::new(("stage", ix))
                        .checked(state.checked())
                        .disabled(busy)
                        .tooltip(tooltip)
                        .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                            this.toggle_stage(&change, *checked, window, cx);
                        }))
                        .into_any_element()
                });
                row.cursor_pointer()
                    .when(selected, |this| this.bg(theme.list_active))
                    .when(!selected, |this| this.hover(|s| s.bg(theme.list_hover)))
                    .on_click(cx.listener(move |this, _, _, cx| this.select(path.clone(), cx)))
                    .child(
                        div()
                            .w_4()
                            .flex_shrink_0()
                            .when(state == StageState::Partial, |this| {
                                this.text_color(theme.warning)
                            })
                            .children(toggle),
                    )
                    .child(
                        div()
                            .w_3()
                            .flex_shrink_0()
                            .font_mono()
                            .text_size(px(10.))
                            .font_semibold()
                            .text_color(status_color(file.status, cx))
                            .child(status_letter(file.status)),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .when(!dir.is_empty(), |this| {
                                this.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(cx.tokens().tx3)
                                        .child(format!("{dir}/")),
                                )
                            })
                            .child(div().flex_shrink_0().child(name)),
                    )
                    .child(counts(file, cx))
                    .when(self.wide, |this| {
                        let viewed = self.is_viewed(&file.path);
                        // Viewed files fade, so what is left to read stands out.
                        this.when(viewed && !selected, |this| this.opacity(0.55))
                            .child(div().w(px(14.)).flex_shrink_0().when(viewed, |this| {
                                this.child(
                                    Icon::new(Lucide::Check)
                                        .size(px(12.))
                                        .text_color(cx.tokens().grn),
                                )
                            }))
                    })
                    .into_any_element()
            }
        }
    }

    fn render_diff(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let Some(shown) = &self.shown else {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(if self.list.is_empty() {
                    ""
                } else {
                    "Select a file to see its diff."
                })
                .into_any_element();
        };
        let file = &shown.file;
        let title = h_flex()
            .h(px(28.))
            .px(px(10.))
            .gap_2()
            .flex_shrink_0()
            .items_center()
            .font_mono()
            .text_size(px(11.))
            .text_color(cx.tokens().tx2)
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .font_semibold()
                    .text_color(status_color(file.status, cx))
                    .child(status_letter(file.status)),
            )
            .child(div().flex_1().min_w_0().truncate().child(file_title(file)))
            .child(counts(file, cx));
        let list = uniform_list(
            "changes-diff",
            shown.rows.len(),
            cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                let Some(shown) = &this.shown else {
                    return Vec::new();
                };
                tracing::trace!(?range, total = shown.rows.len(), "diff rows rendered");
                range
                    .filter_map(|ix| render_row(shown, ix, cx))
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(Some(shown.widest))
        .track_scroll(&self.diff_scroll);
        v_flex()
            .flex_1()
            .min_h_0()
            .child(title)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .bg(cx.tokens().app)
                    .font_mono()
                    .text_size(px(11.5))
                    .child(list),
            )
            .into_any_element()
    }
}

impl Render for ChangesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.wide {
            return self.render_wide(window, cx);
        }
        let header = self.render_header(cx);
        let review = self
            .source
            .is_some()
            .then(|| self.render_review(window, cx));
        let body = match &self.source {
            None => div()
                .flex_1()
                .p_4()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child("Focus a pane showing a workspace to see its changes.")
                .into_any_element(),
            Some(source) => {
                let diff = source.diff.read(cx);
                let empty = diff.summary().is_some() && self.list.is_empty();
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .when(empty, |this| {
                        this.child(
                            div()
                                .p_4()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("No changes since the merge-base."),
                        )
                    })
                    .when(!self.list.is_empty(), |this| {
                        this.child(self.render_file_list(cx))
                    })
                    .child(self.render_diff(cx))
                    .children(
                        review.map(|r| div().border_t_1().border_color(cx.tokens().line).child(r)),
                    )
                    .into_any_element()
            }
        };
        v_flex()
            .size_full()
            .key_context("ChangesPanel")
            .on_action(
                cx.listener(|this, _: &CommitStaged, window, cx| this.commit_shortcut(window, cx)),
            )
            .bg(cx.tokens().panel)
            .child(header)
            .child(body)
            .into_any_element()
    }
}
