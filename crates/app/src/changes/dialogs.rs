//! Bodies of the "Create pull request" and "Merge into base" dialogs. Each runs its operation
//! through the changes panel, shows progress and refusals inline, and closes on success.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::radio::{Radio, RadioGroup};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_worktree::MergeStrategy;

use super::model::short_sha;
use super::review::{self, PrRequest, ReviewTarget};
use super::view::ChangesPanel;

fn field_label(text: &'static str, cx: &App) -> Div {
    div()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(text)
}

fn error_line(error: &Option<SharedString>, cx: &App) -> Option<Div> {
    error
        .clone()
        .map(|err| div().text_sm().text_color(cx.theme().danger).child(err))
}

pub(super) struct PrDialog {
    panel: WeakEntity<ChangesPanel>,
    target: ReviewTarget,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    draft: bool,
    running: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl PrDialog {
    pub fn new(
        panel: WeakEntity<ChangesPanel>,
        target: ReviewTarget,
        title: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Title")
                .default_value(title)
        });
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Description (optional)")
                .auto_grow(4, 12)
        });
        let subscriptions = vec![
            cx.subscribe(&title, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(&body, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        title.update(cx, |state, cx| state.focus(window, cx));
        Self {
            panel,
            target,
            title,
            body,
            draft: false,
            running: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let request = PrRequest {
            title: self.title.read(cx).value().to_string(),
            body: self.body.read(cx).value().to_string(),
            draft: self.draft,
        };
        let workspace = self.target.workspace.id;
        let Ok(task) = self
            .panel
            .update(cx, |panel, cx| panel.create_pr(workspace, request, cx))
        else {
            return;
        };
        self.running = true;
        self.error = None;
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.running = false;
                match result {
                    Ok(url) => {
                        window.close_dialog(cx);
                        window.push_notification(
                            Notification::success(format!("Opened {url}")).title("Pull request"),
                            cx,
                        );
                    }
                    Err(err) => {
                        tracing::error!("create pull request: {err:#}");
                        this.error = Some(review::describe(&err).into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for PrDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ws = &self.target.workspace;
        let empty = self.title.read(cx).value().trim().is_empty();
        let info = format!("{} → {}", ws.branch, self.target.base_branch());
        v_flex()
            .gap_3()
            .text_sm()
            .child(
                field_label("Branch", cx)
                    .child(div().text_color(cx.theme().foreground).child(info)),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(field_label("Title", cx))
                    .child(Input::new(&self.title)),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(field_label("Description", cx))
                    .child(Textarea::new(&self.body)),
            )
            .child(
                Checkbox::new("pr-draft")
                    .label("Open as draft")
                    .checked(self.draft)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.draft = *checked;
                        cx.notify();
                    })),
            )
            .child(field_label(
                "The branch is pushed first if the remote is missing commits.",
                cx,
            ))
            .children(error_line(&self.error, cx))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("pr-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("pr-create")
                            .primary()
                            .small()
                            .label(if self.running {
                                "Creating…"
                            } else {
                                "Create pull request"
                            })
                            .loading(self.running)
                            .disabled(self.running || empty)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}

pub(super) struct MergeDialog {
    panel: WeakEntity<ChangesPanel>,
    target: ReviewTarget,
    strategy: MergeStrategy,
    message: Entity<TextareaState>,
    /// The prefilled message per strategy; replaced on switch unless the user edited it.
    title: String,
    /// Uncommitted files in the workspace (not part of the merge).
    uncommitted: usize,
    /// Push the base branch once merged (off by default: it publishes to a shared branch).
    push_base: bool,
    running: bool,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl MergeDialog {
    pub fn new(
        panel: WeakEntity<ChangesPanel>,
        target: ReviewTarget,
        title: String,
        uncommitted: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let strategy = MergeStrategy::NoFf;
        let default = review::default_merge_message(
            strategy,
            &target.workspace.branch,
            &target.base_branch(),
            &title,
        );
        let message = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 8)
                .default_value(default)
        });
        let subscriptions = vec![cx.subscribe(&message, |_, _, _: &InputEvent, cx| cx.notify())];
        Self {
            panel,
            target,
            strategy,
            message,
            title,
            uncommitted,
            push_base: false,
            running: false,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn default_message(&self, strategy: MergeStrategy) -> String {
        review::default_merge_message(
            strategy,
            &self.target.workspace.branch,
            &self.target.base_branch(),
            &self.title,
        )
    }

    fn set_strategy(
        &mut self,
        strategy: MergeStrategy,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if strategy == self.strategy {
            return;
        }
        let current = self.message.read(cx).value().to_string();
        if current.trim() == self.default_message(self.strategy).trim() {
            let next = self.default_message(strategy);
            self.message
                .update(cx, |state, cx| state.set_value(next, window, cx));
        }
        self.strategy = strategy;
        self.error = None;
        cx.notify();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        let message = self.message.read(cx).value().to_string();
        let (workspace, strategy) = (self.target.workspace.id, self.strategy);
        let Ok(task) = self.panel.update(cx, |panel, cx| {
            panel.merge(workspace, strategy, message, cx)
        }) else {
            return;
        };
        self.running = true;
        self.error = None;
        cx.notify();
        let base = self.target.base_branch();
        let push_base = self.push_base;
        let panel = self.panel.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.running = false;
                match result {
                    Ok(outcome) => {
                        window.close_dialog(cx);
                        window.push_notification(
                            Notification::success(format!(
                                "Merged into {base} at {} ({})",
                                outcome.worktree.display(),
                                short_sha(&outcome.head)
                            ))
                            .title("Merge"),
                            cx,
                        );
                        if push_base {
                            let _ = panel.update(cx, |panel, cx| {
                                panel.push_merged_base(workspace, outcome.worktree, window, cx)
                            });
                        }
                    }
                    Err(err) => {
                        if review::is_merge_refusal(&err) {
                            tracing::warn!("merge refused: {err:#}");
                        } else {
                            tracing::error!("merge failed: {err:#}");
                            window.push_notification(
                                Notification::error(review::describe(&err)).title("Merge failed"),
                                cx,
                            );
                        }
                        this.error = Some(review::describe(&err).into());
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

impl Render for MergeDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = self.message.read(cx).value().trim().is_empty();
        let base = self.target.base_branch();
        let selected = match self.strategy {
            MergeStrategy::NoFf => 0,
            MergeStrategy::Squash => 1,
        };
        v_flex()
            .gap_3()
            .text_sm()
            .child(
                field_label("Branch", cx).child(
                    div()
                        .text_color(cx.theme().foreground)
                        .child(format!("{} → {base}", self.target.workspace.branch)),
                ),
            )
            .child(
                RadioGroup::horizontal("merge-strategy")
                    .selected_index(Some(selected))
                    .disabled(self.running)
                    .child(Radio::new("merge-noff").label("Merge commit"))
                    .child(Radio::new("merge-squash").label("Squash"))
                    .on_change(cx.listener(|this, ix: &usize, window, cx| {
                        let strategy = if *ix == 1 {
                            MergeStrategy::Squash
                        } else {
                            MergeStrategy::NoFf
                        };
                        this.set_strategy(strategy, window, cx);
                    })),
            )
            .child(
                v_flex()
                    .gap_1()
                    .child(field_label("Commit message", cx))
                    .child(Textarea::new(&self.message)),
            )
            .child(field_label(
                "Runs where the base branch is checked out; it must be clean. Nothing is \
                 checked out for you.",
                cx,
            ))
            .child(
                Checkbox::new("merge-push-base")
                    .label(format!("Push {base} afterwards"))
                    .checked(self.push_base)
                    .disabled(self.running)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.push_base = *checked;
                        cx.notify();
                    })),
            )
            .when(self.uncommitted > 0, |this| {
                this.child(div().text_color(cx.theme().warning).child(format!(
                    "{} uncommitted file(s) in this workspace will not be merged.",
                    self.uncommitted
                )))
            })
            .children(error_line(&self.error, cx))
            .child(
                h_flex()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("merge-cancel")
                            .ghost()
                            .small()
                            .label("Cancel")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("merge-run")
                            .primary()
                            .small()
                            .label(if self.running {
                                "Merging…".to_owned()
                            } else {
                                format!("Merge into {base}")
                            })
                            .loading(self.running)
                            .disabled(self.running || empty)
                            .on_click(cx.listener(|this, _, window, cx| this.submit(window, cx))),
                    ),
            )
    }
}
