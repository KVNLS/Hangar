//! Body of the "delete workspace" confirmation: what would be lost, and the branch option.

use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::*;
use hangar_core::Workspace;

use crate::ops::{DeleteOptions, DeleteSummary};

pub(super) struct DeleteWorkspaceDialog {
    workspace: Workspace,
    /// `None` while the status is read in the background.
    summary: Option<Result<DeleteSummary, SharedString>>,
    delete_branch: bool,
}

impl DeleteWorkspaceDialog {
    pub fn new(workspace: Workspace) -> Self {
        Self {
            workspace,
            summary: None,
            delete_branch: false,
        }
    }

    pub fn set_summary(&mut self, summary: Result<DeleteSummary, SharedString>) {
        self.summary = Some(summary);
    }

    /// The options to delete with, or `None` while the status is still loading. Force is
    /// only set when what it loses (files, submodules) was shown to the user.
    pub fn options(&self) -> Option<DeleteOptions> {
        let summary = self.summary.as_ref()?;
        Some(DeleteOptions {
            force: summary.as_ref().is_ok_and(DeleteSummary::needs_force),
            delete_branch: self.delete_branch,
        })
    }
}

impl Render for DeleteWorkspaceDialog {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let (lines, color): (Vec<String>, Hsla) = match &self.summary {
            None => (vec!["Checking the worktree…".to_owned()], muted),
            Some(Ok(summary)) => (
                summary.describe(&self.workspace.base),
                if summary.needs_force() {
                    theme.danger
                } else {
                    theme.foreground
                },
            ),
            Some(Err(err)) => (
                vec![
                    format!("Could not read the worktree status: {err}"),
                    "Deleting will fail if it has uncommitted changes.".to_owned(),
                ],
                theme.warning,
            ),
        };
        let branch = self.workspace.branch.clone();
        v_flex()
            .gap_2()
            .text_sm()
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child(self.workspace.worktree_path.display().to_string()),
            )
            .children(
                lines
                    .into_iter()
                    .map(|line| div().text_color(color).child(line)),
            )
            .child(
                Checkbox::new("delete-branch")
                    .label(format!("Also delete branch {branch}"))
                    .checked(self.delete_branch)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.delete_branch = *checked;
                        cx.notify();
                    })),
            )
            .child(div().text_xs().text_color(muted).child(
                "An unmerged branch is kept and reported; the repository is otherwise untouched.",
            ))
    }
}
