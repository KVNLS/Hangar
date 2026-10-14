//! "Compare attempts": the workspaces made from one task for several agents (best of N), side by
//! side with their state and diff, to open, review or keep one and archive the others.

use hangar_core::WorkspaceGroupId;
use hangar_usecases::batch;

use super::*;
use crate::ui::components::{AgentMark, Delta, StateGlyph, UiButton, card};
use crate::ui::{ActiveTokens as _, RADIUS_SHEET, TITLEBAR_HEIGHT, TypeScale as _, overlay_shadow};

/// The open comparison.
pub(super) struct CompareAttempts {
    pub group: WorkspaceGroupId,
    focus: FocusHandle,
}

impl HangarApp {
    /// The active attempts in `id`'s group, when it has more than one.
    pub(super) fn attempts_of(&self, id: WorkspaceId) -> Option<(WorkspaceGroupId, usize)> {
        let group = self.work.projects.workspace(id)?.group?;
        let count = batch::attempts(self.all_workspaces(), group).len();
        (count > 1).then_some((group, count))
    }

    fn all_workspaces(&self) -> impl Iterator<Item = &Workspace> {
        self.work.projects.entries().iter().flat_map(|e| e.active())
    }

    pub(super) fn open_compare(
        &mut self,
        group: WorkspaceGroupId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.work.compare = Some(CompareAttempts { group, focus });
        // Diffs only run for workspaces shown in a pane; the comparison needs every attempt's.
        self.sync_diffs(cx);
        cx.notify();
    }

    pub(super) fn close_compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.work.compare.take().is_some() {
            window.focus(&self.focus, cx);
            cx.notify();
        }
    }

    /// Workspaces whose diff the open comparison shows.
    pub(super) fn compared_workspaces(&self) -> Vec<WorkspaceId> {
        self.work
            .compare
            .as_ref()
            .map(|c| {
                batch::attempts(self.all_workspaces(), c.group)
                    .iter()
                    .map(|ws| ws.id)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn open_attempt(
        &mut self,
        id: WorkspaceId,
        screen: Screen,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.close_compare(window, cx);
        self.select_workspace(id, window, cx);
        self.set_screen(screen, window, cx);
    }

    /// Asks before archiving the other attempts; their branches and worktrees stay.
    fn confirm_keep(&mut self, keep: WorkspaceId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ws) = self.work.projects.workspace(keep) else {
            return;
        };
        let others = batch::others_to_archive(self.all_workspaces(), ws);
        if others.is_empty() {
            return;
        }
        let n = others.len();
        let title: SharedString = format!("Keep “{}”?", ws.name).into();
        let others_label = if n == 1 {
            "The other attempt is".to_owned()
        } else {
            format!("The other {n} attempts are")
        };
        let body = format!(
            "{others_label} archived and {} agent stopped. Branches and worktrees stay: restore \
             them from the sidebar.",
            if n == 1 { "its" } else { "their" },
        );
        let app = cx.entity().downgrade();
        window.open_alert_dialog(cx, move |alert, _, _| {
            let app = app.clone();
            let others = others.clone();
            alert
                .confirm()
                .title(title.clone())
                .child(body.clone())
                .ok_text(if n == 1 {
                    "Archive the other"
                } else {
                    "Archive the others"
                })
                .cancel_text("Cancel")
                .on_ok(move |_, window, cx| {
                    let others = others.clone();
                    let _ = app.update(cx, |this, cx| this.keep_attempt(keep, others, window, cx));
                    true
                })
        });
    }

    fn keep_attempt(
        &mut self,
        keep: WorkspaceId,
        others: Vec<WorkspaceId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::info!(workspace = %keep, archived = others.len(), "kept one attempt");
        for id in others {
            self.archive_workspace(id, window, cx).detach();
        }
        self.open_attempt(keep, Screen::Workspace, window, cx);
    }

    pub(super) fn render_compare(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let compare = self.work.compare.as_ref()?;
        let t = cx.tokens();
        let attempts = batch::attempts(self.all_workspaces(), compare.group);
        let task = attempts
            .first()
            .and_then(|ws| ws.task.clone())
            .unwrap_or_default();
        let columns = attempts.len().clamp(1, 4) as u16;
        let cards: Vec<AnyElement> = attempts
            .iter()
            .map(|ws| self.attempt_card(ws, cx))
            .collect();
        let panel = v_flex()
            .id("compare-attempts")
            .key_context("CompareAttempts")
            .track_focus(&compare.focus)
            .w(px(960.))
            .max_h(px(640.))
            .bg(t.elev)
            .border_1()
            .border_t_0()
            .border_color(t.line2)
            .rounded_b(RADIUS_SHEET)
            .shadow(overlay_shadow())
            .text_color(t.tx)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_action(
                cx.listener(|this, _: &CloseSheet, window, cx| this.close_compare(window, cx)),
            )
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
                            .child(format!("Compare {} attempts", attempts.len())),
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
                    .id("compare-body")
                    .overflow_y_scroll()
                    .p_4()
                    .gap_3()
                    .when(!task.is_empty(), |this| {
                        this.child(
                            div()
                                .type_secondary()
                                .text_color(t.tx2)
                                .line_clamp(3)
                                .child(task),
                        )
                    })
                    .child(div().grid().grid_cols(columns).gap_3().children(cards)),
            );
        Some(
            div()
                .id("compare-scrim")
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
                    cx.listener(|this, _, window, cx| this.close_compare(window, cx)),
                )
                .child(panel)
                .into_any_element(),
        )
    }

    fn attempt_card(&self, ws: &Workspace, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.tokens();
        let id = ws.id;
        let state = self.agent_state(id, cx);
        let live = self.live.sessions.get(&id);
        let counts = self.diff_counts(id, cx);
        let changes = match &counts {
            None => "Reading the diff…".to_owned(),
            Some(c) if c.files == 0 => "No changes yet".to_owned(),
            Some(c) => format!(
                "{} file{} changed",
                c.files,
                if c.files == 1 { "" } else { "s" }
            ),
        };
        let last = live
            .and_then(|l| l.activity.last_line.clone())
            .unwrap_or_else(|| "No output yet.".into());
        card(cx)
            .id(SharedString::from(format!("attempt-{id}")))
            .gap_2()
            .px_3()
            .py(px(10.))
            .min_w_0()
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(StateGlyph::new(state))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .type_body_strong()
                            .child(ws.name.clone()),
                    )
                    .children(live.map(|l| AgentMark::new(l.session.agent.monogram()))),
            )
            .child(
                div()
                    .type_mono_small()
                    .text_color(t.tx3)
                    .truncate()
                    .child(format!("{} · {}", state.label(), ws.branch)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .children(counts.map(|c| Delta::new(c.insertions, c.deletions)))
                    .child(div().type_secondary().text_color(t.tx2).child(changes)),
            )
            .child(
                div()
                    .h(px(34.))
                    .px_2()
                    .py(px(6.))
                    .overflow_hidden()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.term)
                    .font_mono()
                    .text_size(px(11.5))
                    .line_height(px(17.))
                    .text_color(t.tx2)
                    .truncate()
                    .child(last),
            )
            .child(
                h_flex()
                    .gap_1p5()
                    .child(
                        UiButton::new(SharedString::from(format!("attempt-open-{id}")))
                            .label("Open")
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_attempt(id, Screen::Workspace, window, cx)
                            })),
                    )
                    .child(
                        UiButton::new(SharedString::from(format!("attempt-review-{id}")))
                            .label("Review")
                            .ghost()
                            .small()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_attempt(id, Screen::Review, window, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        UiButton::new(SharedString::from(format!("attempt-keep-{id}")))
                            .label("Keep this one")
                            .primary()
                            .small()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.confirm_keep(id, window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }
}
