//! Viewed marks and the wide layout: file list beside a unified or split diff.

use std::path::{Path, PathBuf};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::changes::model::file_title;
use crate::ui::components::{Chip, Delta, EmptyState, Segmented, UiButton, section_label};
use crate::ui::{ActiveTokens as _, TypeScale as _};

use super::rows::{counts, render_row, render_split_row};
use super::{ChangesEvent, ChangesPanel, CommitStaged, ExitReview};

impl ChangesPanel {
    fn viewed_count(&self) -> usize {
        let Some(source) = &self.source else { return 0 };
        self.viewed
            .get(&source.workspace)
            .map_or(0, |v| v.iter().filter(|p| self.paths.contains(p)).count())
    }

    pub(super) fn is_viewed(&self, path: &Path) -> bool {
        self.source
            .as_ref()
            .and_then(|s| self.viewed.get(&s.workspace))
            .is_some_and(|v| v.contains(path))
    }

    fn toggle_viewed(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let Some(source) = &self.source else { return };
        let viewed = self.viewed.entry(source.workspace).or_default();
        if !viewed.remove(&path) {
            viewed.insert(path);
        }
        cx.notify();
    }

    /// A button that starts a message to the workspace's agent with `text`; disabled, with the
    /// reason, when no agent runs.
    fn ask_agent_button(
        &self,
        id: &'static str,
        label: &'static str,
        icon: Lucide,
        text: String,
        cx: &mut Context<Self>,
    ) -> UiButton {
        let agent = self.workspace_agent(cx);
        let tooltip = match &agent {
            None => "No agent runs in this workspace".to_owned(),
            Some(agent) if !agent.live => format!("{} is no longer running", agent.name),
            Some(agent) => format!("Write to {}", agent.name),
        };
        let workspace = self.workspace();
        UiButton::new(id)
            .icon(icon)
            .label(label)
            .small()
            .tooltip(tooltip)
            .disabled(!agent.is_some_and(|a| a.live))
            .on_click(cx.listener(move |_, _, _, cx| {
                if let Some(workspace) = workspace {
                    cx.emit(ChangesEvent::AskAgent {
                        workspace,
                        text: text.clone(),
                    });
                }
            }))
    }

    /// The full-width review screen: top bar, file column with commit/PR actions, diff.
    pub(super) fn render_wide(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = cx.tokens();
        let back = UiButton::new("review-back")
            .icon(Lucide::ArrowLeft)
            .label("Workspace")
            .kbd("esc")
            .ghost()
            .on_click(cx.listener(|_, _, _, cx| cx.emit(ChangesEvent::ExitReview)));
        let Some(source) = &self.source else {
            return v_flex()
                .size_full()
                .bg(t.app)
                .child(h_flex().h(px(48.)).px(px(14.)).items_center().child(back))
                .child(
                    div().flex_1().flex().items_center().justify_center().child(
                        EmptyState::new(Lucide::FileDiff, "Nothing to review")
                            .hint("Focus a pane that shows a workspace, then review its changes."),
                    ),
                )
                .into_any_element();
        };
        let summary = source.diff.read(cx).summary().cloned();
        let name = source.name.clone();
        let files = self.paths.len();
        let viewed = self.viewed_count();
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
            .child(div().type_body_strong().truncate().child(name))
            .when_some(summary.as_ref(), |this, summary| {
                let branch = summary
                    .status
                    .branch
                    .clone()
                    .unwrap_or_else(|| "detached".into());
                let n = |v: u64| usize::try_from(v).unwrap_or(usize::MAX);
                this.child(
                    Chip::new(format!(
                        "{branch} → {}",
                        self.base_label(&summary.merge_base)
                    ))
                    .icon(Lucide::GitBranch),
                )
                .child(
                    div()
                        .font_mono()
                        .text_size(px(11.))
                        .text_color(t.tx3)
                        .child(format!("{files} file{}", if files == 1 { "" } else { "s" })),
                )
                .child(Delta::new(n(summary.insertions), n(summary.deletions)))
            })
            .child(div().flex_1())
            .child(self.ask_agent_button(
                "review-request-changes",
                "Request changes",
                Lucide::Pencil,
                "Please change: ".to_owned(),
                cx,
            ))
            .child(
                Segmented::new("review-mode", ["Split", "Inline"])
                    .selected(if self.split_view { 0 } else { 1 })
                    .on_select(cx.listener(|this, i: &usize, _, cx| {
                        this.split_view = *i == 0;
                        cx.notify();
                    })),
            );
        let review = self.render_review(window, cx);
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
                    .justify_between()
                    .child(section_label("Files", cx))
                    .child(
                        div()
                            .font_mono()
                            .text_size(px(11.))
                            .text_color(t.tx3)
                            .child(format!("{viewed}/{files} viewed")),
                    ),
            )
            .child(self.render_file_list(cx))
            .child(div().border_t_1().border_color(t.line).child(review));
        v_flex()
            .size_full()
            .key_context("ReviewScreen")
            .track_focus(&self.focus)
            .on_action(cx.listener(|_, _: &ExitReview, _, cx| cx.emit(ChangesEvent::ExitReview)))
            .on_action(
                cx.listener(|this, _: &CommitStaged, window, cx| this.commit_shortcut(window, cx)),
            )
            .bg(t.app)
            .child(top)
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .child(column)
                    .child(self.render_wide_diff(cx)),
            )
            .into_any_element()
    }

    fn render_wide_diff(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.tokens();
        let Some(shown) = &self.shown else {
            return div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(t.tx3)
                .child(if self.list.is_empty() {
                    "No changes since the merge-base."
                } else {
                    "Select a file to see its diff."
                })
                .into_any_element();
        };
        let file = &shown.file;
        let path = file.path.clone();
        let viewed = self.is_viewed(&path);
        let header = h_flex()
            .flex_none()
            .h(px(36.))
            .px(px(14.))
            .gap(px(10.))
            .items_center()
            .bg(t.panel)
            .border_b_1()
            .border_color(t.line)
            .font_mono()
            .text_size(px(12.))
            .child(Icon::new(Lucide::File).size(px(13.)).text_color(t.tx3))
            .child(div().min_w_0().truncate().child(file_title(file)))
            .child(counts(file, cx))
            .child(div().flex_1())
            .child(self.ask_agent_button(
                "review-ask-file",
                "Ask agent about this file",
                Lucide::MessageSquare,
                format!("About {}: ", path.display()),
                cx,
            ))
            .child(
                UiButton::new("review-viewed")
                    .icon(Lucide::Check)
                    .label("Viewed")
                    .small()
                    .selected(viewed)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.toggle_viewed(path.clone(), cx)),
                    ),
            );
        let list = if self.split_view {
            uniform_list(
                "review-split",
                shown.split.len(),
                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                    let Some(shown) = &this.shown else {
                        return Vec::new();
                    };
                    range
                        .filter_map(|ix| render_split_row(shown, ix, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .size_full()
            .track_scroll(&self.diff_scroll)
            .into_any_element()
        } else {
            uniform_list(
                "review-inline",
                shown.rows.len(),
                cx.processor(|this, range: std::ops::Range<usize>, _, cx| {
                    let Some(shown) = &this.shown else {
                        return Vec::new();
                    };
                    range
                        .filter_map(|ix| render_row(shown, ix, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .size_full()
            .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
            .with_width_from_item(Some(shown.widest))
            .track_scroll(&self.diff_scroll)
            .into_any_element()
        };
        v_flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .font_mono()
                    .text_size(px(12.))
                    .child(list),
            )
            .into_any_element()
    }
}
