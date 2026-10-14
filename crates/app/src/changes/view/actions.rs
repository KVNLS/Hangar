//! Review actions of the changes panel: staging, the commit box, push, pull request and
//! merge. Git and gh run on the background executor; one operation per workspace at a time.

use anyhow::anyhow;
use gpui_kit::component::button::ButtonVariants as _;
use gpui_kit::component::input::{InputEvent, Textarea, TextareaState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::{IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_worktree::{ChangedPath, Gh, MergeOutcome, MergeStrategy, PullOutcome, PushOutcome};

use super::{ChangesPanel, CommitBox};
use crate::changes::dialogs::{MergeDialog, PrDialog};
use crate::changes::model::short_sha;
use crate::changes::review::{self, PrRequest, ReviewTarget};
use crate::ui::components::UiButton;
use crate::ui::{ActiveTokens as _, TypeScale as _};

/// The operation running in a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Busy {
    Staging,
    Committing,
    Pushing,
    Pulling,
    CreatingPr,
    Merging,
}

impl Busy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Staging => "Staging…",
            Self::Committing => "Committing…",
            Self::Pushing => "Pushing…",
            Self::Pulling => "Pulling…",
            Self::CreatingPr => "Opening pull request…",
            Self::Merging => "Merging…",
        }
    }
}

impl ChangesPanel {
    /// How `gh` is run (the login PATH, or an explicit program).
    pub fn set_gh(&mut self, gh: Gh, cx: &mut Context<Self>) {
        self.gh = Some(gh);
        self.info.clear();
        self.reload_info(cx);
    }

    pub(super) fn current_busy(&self) -> Option<Busy> {
        let source = self.source.as_ref()?;
        self.busy.get(&source.workspace).copied()
    }

    fn target(&self, workspace: WorkspaceId) -> anyhow::Result<ReviewTarget> {
        self.source
            .as_ref()
            .filter(|s| s.workspace == workspace)
            .map(|s| s.target.clone())
            .ok_or_else(|| anyhow!("the workspace is no longer shown in the changes panel"))
    }

    /// Re-reads gh status, the PR and the last commit subject of the shown workspace.
    pub fn reload_info(&mut self, cx: &mut Context<Self>) {
        let (Some(source), Some(gh)) = (&self.source, &self.gh) else {
            return;
        };
        let (workspace, target, gh) = (source.workspace, source.target.clone(), gh.clone());
        let task = cx.spawn(async move |this, cx| {
            let info = cx
                .background_executor()
                .spawn(async move { review::load_info(&gh, &target) })
                .await;
            tracing::debug!(%workspace, gh = ?info.gh, pr = ?info.pr, "review info loaded");
            let _ = this.update(cx, |this, cx| {
                this.info.insert(workspace, info);
                this.info_tasks.remove(&workspace);
                cx.notify();
            });
        });
        self.info_tasks.insert(workspace, task);
    }

    /// Runs `job` off the main thread as the workspace's one operation, then refreshes the
    /// diff and the review info.
    fn run_op<T: Send + 'static>(
        &mut self,
        workspace: WorkspaceId,
        busy: Busy,
        job: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<T>> {
        if let Some(running) = self.busy.get(&workspace) {
            return Task::ready(Err(anyhow!("wait: {}", running.label())));
        }
        self.busy.insert(workspace, busy);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx.background_executor().spawn(async move { job() }).await;
            let _ = this.update(cx, |this, cx| {
                this.busy.remove(&workspace);
                if let Some(source) = this.source.as_ref().filter(|s| s.workspace == workspace) {
                    source.diff.read(cx).refresh();
                    this.reload_info(cx);
                }
                cx.notify();
            });
            result
        })
    }

    /// Shows a failed operation as a notification; successes go to `on_ok`.
    fn notify<T: 'static>(
        task: Task<anyhow::Result<T>>,
        title: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
        on_ok: impl FnOnce(T, &mut Window, &mut Context<Self>) + 'static,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |_, window, cx| match result {
                Ok(value) => on_ok(value, window, cx),
                Err(err) => {
                    tracing::error!("{title}: {err:#}");
                    window.push_notification(
                        Notification::error(review::describe(&err)).title(title),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    pub(super) fn toggle_stage(
        &mut self,
        change: &ChangedPath,
        stage: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source) = &self.source else {
            return;
        };
        let worktree = source.target.workspace.worktree_path.clone();
        let paths = review::stage_paths(change);
        let task = self.run_op(
            source.workspace,
            Busy::Staging,
            move || review::run_stage(&worktree, &paths, stage),
            cx,
        );
        Self::notify(task, "Stage", window, cx, |_, _, _| {});
    }

    pub fn stage_all(&mut self, cx: &mut Context<Self>) -> Task<anyhow::Result<()>> {
        let Some(source) = &self.source else {
            return Task::ready(Err(anyhow!("no workspace shown")));
        };
        let worktree = source.target.workspace.worktree_path.clone();
        self.run_op(
            source.workspace,
            Busy::Staging,
            move || review::run_stage_all(&worktree),
            cx,
        )
    }

    /// Commits what is staged in the shown workspace; resolves to the new HEAD.
    pub fn commit(
        &mut self,
        message: String,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<String>> {
        let Some(source) = &self.source else {
            return Task::ready(Err(anyhow!("no workspace shown")));
        };
        let worktree = source.target.workspace.worktree_path.clone();
        self.run_op(
            source.workspace,
            Busy::Committing,
            move || review::run_commit(&worktree, &message),
            cx,
        )
    }

    pub fn push(&mut self, cx: &mut Context<Self>) -> Task<anyhow::Result<PushOutcome>> {
        let Some(source) = &self.source else {
            return Task::ready(Err(anyhow!("no workspace shown")));
        };
        let worktree = source.target.workspace.worktree_path.clone();
        self.run_op(
            source.workspace,
            Busy::Pushing,
            move || review::run_push(&worktree),
            cx,
        )
    }

    /// Fetches the upstream and rebases the branch onto it (after a rejected push).
    pub fn pull_rebase(&mut self, cx: &mut Context<Self>) -> Task<anyhow::Result<PullOutcome>> {
        let Some(source) = &self.source else {
            return Task::ready(Err(anyhow!("no workspace shown")));
        };
        let worktree = source.target.workspace.worktree_path.clone();
        self.run_op(
            source.workspace,
            Busy::Pulling,
            move || Ok(hangar_worktree::pull_rebase(&worktree)?),
            cx,
        )
    }

    /// Pushes if needed and opens a PR; resolves to its URL.
    pub fn create_pr(
        &mut self,
        workspace: WorkspaceId,
        request: PrRequest,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<String>> {
        let target = match self.target(workspace) {
            Ok(target) => target,
            Err(err) => return Task::ready(Err(err)),
        };
        let Some(gh) = self.gh.clone() else {
            return Task::ready(Err(anyhow!("still reading the login PATH; try again")));
        };
        self.run_op(
            workspace,
            Busy::CreatingPr,
            move || review::run_create_pr(&gh, &target, &request),
            cx,
        )
    }

    pub fn merge(
        &mut self,
        workspace: WorkspaceId,
        strategy: MergeStrategy,
        message: String,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<MergeOutcome>> {
        let target = match self.target(workspace) {
            Ok(target) => target,
            Err(err) => return Task::ready(Err(err)),
        };
        self.run_op(
            workspace,
            Busy::Merging,
            move || review::run_merge(&target, strategy, &message),
            cx,
        )
    }

    /// The shown workspace's id and default PR title (for dev hooks).
    pub fn pr_defaults(&self) -> Option<(WorkspaceId, String)> {
        let source = self.source.as_ref()?;
        let subject = self
            .info
            .get(&source.workspace)
            .and_then(|i| i.last_subject.as_deref());
        Some((
            source.workspace,
            review::default_pr_title(subject, &source.target.workspace.name),
        ))
    }

    /// The shown workspace's id and default merge message for `strategy`.
    pub fn merge_defaults(&self, strategy: MergeStrategy) -> Option<(WorkspaceId, String)> {
        let (workspace, title) = self.pr_defaults()?;
        let target = &self.source.as_ref()?.target;
        let message = review::default_merge_message(
            strategy,
            &target.workspace.branch,
            &target.base_branch(),
            &title,
        );
        Some((workspace, message))
    }

    /// Whether gh status and the PR of the shown workspace have been read.
    pub fn info_loaded(&self) -> bool {
        self.source
            .as_ref()
            .is_some_and(|s| self.info.contains_key(&s.workspace))
    }

    fn commit_box(
        &mut self,
        workspace: WorkspaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextareaState> {
        if let Some(existing) = self.commit_boxes.get(&workspace) {
            return existing.input.clone();
        }
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Commit message")
                .auto_grow(2, 8)
        });
        let change = cx.subscribe(&input, |_, _, _: &InputEvent, cx| cx.notify());
        self.commit_boxes.insert(
            workspace,
            CommitBox {
                input: input.clone(),
                _change: change,
            },
        );
        input
    }

    fn commit_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace() else {
            return;
        };
        let input = self.commit_box(workspace, window, cx);
        let message = input.read(cx).value().to_string();
        let task = self.commit(message, cx);
        Self::notify(task, "Commit", window, cx, move |oid, window, cx| {
            input.update(cx, |state, cx| state.set_value("", window, cx));
            window.push_notification(
                Notification::success(format!("Committed {}", short_sha(&oid))),
                cx,
            );
        });
    }

    fn push_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.push(cx);
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |_, window, cx| match result {
                Ok(out) => {
                    let text = if out.up_to_date {
                        format!("{} is up to date on {}", out.branch, out.remote)
                    } else if out.set_upstream {
                        format!("Published {} to {}", out.branch, out.remote)
                    } else {
                        format!("Pushed {} to {}", out.branch, out.remote)
                    };
                    window.push_notification(Notification::success(text), cx);
                }
                Err(err) => {
                    tracing::error!("Push: {err:#}");
                    let behind = matches!(
                        err.downcast_ref::<hangar_worktree::Error>(),
                        Some(hangar_worktree::Error::PushRejected {
                            non_fast_forward: true,
                            ..
                        })
                    );
                    let mut note = Notification::error(review::describe(&err)).title("Push");
                    if behind {
                        let panel = cx.entity().downgrade();
                        note = note.action(move |_, _, cx| {
                            let panel = panel.clone();
                            gpui_kit::component::button::Button::new("pull-rebase")
                                .primary()
                                .label("Pull and rebase")
                                .on_click(cx.listener(move |note, _, window, cx| {
                                    let _ = panel.update(cx, |panel, cx| {
                                        panel.pull_rebase_clicked(window, cx)
                                    });
                                    note.dismiss(window, cx);
                                }))
                        });
                    }
                    window.push_notification(note, cx);
                }
            });
        })
        .detach();
    }

    /// After a merge: pushes the base branch from the checkout the merge was made in.
    pub(crate) fn push_merged_base(
        &mut self,
        workspace: WorkspaceId,
        checkout: std::path::PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self.run_op(
            workspace,
            Busy::Pushing,
            move || review::run_push(&checkout),
            cx,
        );
        Self::notify(task, "Push base", window, cx, |out, window, cx| {
            let text = if out.up_to_date {
                format!("{} is up to date on {}", out.branch, out.remote)
            } else {
                format!("Pushed {} to {}", out.branch, out.remote)
            };
            window.push_notification(Notification::success(text), cx);
        });
    }

    fn pull_rebase_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.pull_rebase(cx);
        Self::notify(task, "Pull and rebase", window, cx, |out, window, cx| {
            let text = match out.pulled {
                0 => format!("Already up to date with {}", out.upstream),
                n => format!(
                    "Rebased onto {} ({n} new commit{}); push again",
                    out.upstream,
                    if n == 1 { "" } else { "s" }
                ),
            };
            window.push_notification(Notification::success(text), cx);
        });
    }

    fn stage_all_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let task = self.stage_all(cx);
        Self::notify(task, "Stage all", window, cx, |_, _, _| {});
    }

    fn open_pr_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, title)) = self.pr_defaults() else {
            return;
        };
        let Some(target) = self.source.as_ref().map(|s| s.target.clone()) else {
            return;
        };
        let panel = cx.entity().downgrade();
        let body = cx.new(|cx| PrDialog::new(panel, target, title, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title("Create pull request")
                .w(px(520.))
                .child(body.clone())
        });
    }

    fn open_merge_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, title)) = self.pr_defaults() else {
            return;
        };
        let Some(target) = self.source.as_ref().map(|s| s.target.clone()) else {
            return;
        };
        let base = target.base_branch();
        let panel = cx.entity().downgrade();
        let uncommitted = self.uncommitted;
        let body = cx.new(|cx| MergeDialog::new(panel, target, title, uncommitted, window, cx));
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(format!("Merge into {base}"))
                .w(px(520.))
                .child(body.clone())
        });
    }

    /// Toolbar (push, PR, merge) and the commit box.
    pub(super) fn render_review(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(source) = &self.source else {
            return Empty.into_any_element();
        };
        let workspace = source.workspace;
        let base = source.target.base_branch();
        let status = source
            .diff
            .read(cx)
            .summary()
            .map(|s| s.status.clone())
            .unwrap_or_default();
        let busy = self.busy.get(&workspace).copied();
        let info = self.info.get(&workspace).cloned();
        let input = self.commit_box(workspace, window, cx);
        let message = input.read(cx).value().to_string();
        let commit_blocker = review::commit_blocker(&message, self.staged);
        let can_stage_all = self.stage.values().any(|c| c.unstaged || !c.staged);
        let t = cx.tokens();

        let detached = status.branch.is_none();
        let nothing_to_push = review::own_upstream(&status).is_some() && status.ahead == 0;
        let no_remote = info.as_ref().is_some_and(|i| !i.remote);
        let push = UiButton::new("review-push")
            .label(review::push_label(&status))
            .tooltip(if no_remote {
                review::NO_REMOTE.to_owned()
            } else {
                review::push_tooltip(&status)
            })
            .disabled(busy.is_some() || detached || nothing_to_push || no_remote)
            .on_click(cx.listener(|this, _, window, cx| this.push_clicked(window, cx)));

        let gh_blocker = info.as_ref().and_then(|i| review::gh_blocker(&i.gh));
        let pr_blocker = if no_remote {
            Some(review::NO_REMOTE.to_owned())
        } else {
            gh_blocker.clone()
        };
        let pr = match info.as_ref().and_then(|i| i.pr.clone()) {
            Some(pr) => {
                let url = pr.url.clone();
                UiButton::new("review-pr-link")
                    .icon(IconName::ExternalLink)
                    .label(review::pr_label(&pr))
                    .tooltip(format!("{} — open in browser", pr.title))
                    .on_click(move |_, _, cx| cx.open_url(&url))
            }
            None => UiButton::new("review-pr")
                .primary()
                .icon(gpui_kit::assets::IconName::GitPullRequest)
                .label("Create PR")
                .tooltip(
                    pr_blocker
                        .clone()
                        .unwrap_or_else(|| format!("Open a pull request into {base}")),
                )
                .disabled(busy.is_some() || info.is_none() || pr_blocker.is_some() || detached)
                .on_click(cx.listener(|this, _, window, cx| this.open_pr_dialog(window, cx))),
        };
        let merge = UiButton::new("review-merge")
            .label(format!("Merge into {base}"))
            .tooltip("Merge this branch where the base branch is checked out")
            .disabled(busy.is_some() || detached)
            .on_click(cx.listener(|this, _, window, cx| this.open_merge_dialog(window, cx)));

        let notes: Vec<SharedString> = [
            busy.map(|b| b.label().to_owned()),
            info.as_ref()
                .and_then(|i| i.pr_error.clone())
                .map(|e| format!("Could not look up the pull request: {e}")),
        ]
        .into_iter()
        .flatten()
        .map(SharedString::from)
        .collect();

        let commit_row = h_flex()
            .gap(px(6.))
            .items_center()
            .child(
                UiButton::new("review-commit")
                    .icon(IconName::Check)
                    .label("Commit")
                    .tooltip(commit_blocker.unwrap_or("Commit the staged files"))
                    .disabled(busy.is_some() || commit_blocker.is_some())
                    .on_click(cx.listener(|this, _, window, cx| this.commit_clicked(window, cx))),
            )
            .child(
                UiButton::new("review-stage-all")
                    .ghost()
                    .label("Stage all")
                    .tooltip("Stage every change, untracked files included")
                    .disabled(busy.is_some() || !can_stage_all)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.stage_all_clicked(window, cx)),
                    ),
            )
            .child(div().flex_1())
            .child(
                div()
                    .font_mono()
                    .text_size(px(11.))
                    .text_color(t.tx3)
                    .child(format!("{} staged", self.staged)),
            );
        // Wraps in the 280 px column of the review screen.
        let share_row = h_flex()
            .flex_wrap()
            .gap(px(6.))
            .items_center()
            .child(push)
            .child(merge)
            .child(div().flex_1())
            .child(pr);

        v_flex()
            .flex_shrink_0()
            .px(px(12.))
            .py(px(10.))
            .gap(px(8.))
            .children(
                notes
                    .into_iter()
                    .map(|note| div().text_size(px(11.)).text_color(t.tx3).child(note)),
            )
            .child(Textarea::new(&input).small())
            .child(commit_row)
            .child(share_row)
            .into_any_element()
    }
}
