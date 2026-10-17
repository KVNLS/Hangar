//! One `LiveDiff` per workspace, as a GPUI entity the sidebar and the changes panel observe.

use std::path::PathBuf;
use std::time::Instant;

use gpui_kit::*;
use hangar_core::WorkspaceId;
use hangar_files::{DiffSummary, DiffUpdate, LiveDiff, LiveDiffConfig};

use super::model::{FileMap, apply_update};

pub struct WorkspaceDiff {
    workspace: WorkspaceId,
    /// Dropping it stops the watcher and the worker thread.
    live: Option<LiveDiff>,
    files: FileMap,
    summary: Option<DiffSummary>,
    error: Option<SharedString>,
    _task: Task<()>,
}

impl Drop for WorkspaceDiff {
    fn drop(&mut self) {
        tracing::info!(workspace = %self.workspace, "live diff stopped");
    }
}

impl WorkspaceDiff {
    /// Starts watching off the main thread (the watcher setup runs git) and applies every
    /// update as it arrives.
    pub fn start(
        workspace: WorkspaceId,
        worktree: PathBuf,
        base: String,
        cx: &mut Context<Self>,
    ) -> Self {
        let task = cx.spawn(async move |this, cx| {
            let started_at = Instant::now();
            let started = cx
                .background_executor()
                .spawn(async move {
                    LiveDiff::start(
                        crate::adapters::changes(),
                        &worktree,
                        base,
                        LiveDiffConfig::default(),
                    )
                })
                .await;
            let mut updates = match started {
                Ok((live, updates)) => {
                    if this.update(cx, |this, _| this.live = Some(live)).is_err() {
                        return;
                    }
                    updates
                }
                Err(err) => {
                    tracing::error!(workspace = %workspace, %err, "failed to start live diff");
                    let _ = this.update(cx, |this, cx| {
                        this.error = Some(format!("Cannot watch the worktree: {err}").into());
                        cx.notify();
                    });
                    return;
                }
            };
            tracing::info!(
                workspace = %workspace,
                ms = started_at.elapsed().as_millis() as u64,
                "live diff started"
            );
            while let Some(update) = updates.recv().await {
                if this.update(cx, |this, cx| this.apply(update, cx)).is_err() {
                    return;
                }
            }
        });
        Self {
            workspace,
            live: None,
            files: FileMap::new(),
            summary: None,
            error: None,
            _task: task,
        }
    }

    fn apply(&mut self, update: hangar_files::Result<DiffUpdate>, cx: &mut Context<Self>) {
        match update {
            Ok(update) => {
                let (changed, removed) = (update.changed.len(), update.removed.len());
                apply_update(&mut self.files, update.changed, &update.removed);
                tracing::info!(
                    workspace = %self.workspace,
                    files = update.summary.files,
                    insertions = update.summary.insertions,
                    deletions = update.summary.deletions,
                    changed,
                    removed,
                    full = update.full,
                    latency_ms = update.trigger.map(|t| t.elapsed().as_millis() as u64),
                    "diff update"
                );
                self.summary = Some(update.summary);
                self.error = None;
            }
            Err(err) => {
                tracing::warn!(workspace = %self.workspace, %err, "diff update failed");
                self.error = Some(format!("{err}").into());
            }
        }
        cx.notify();
    }

    /// Requests a full recompute.
    pub fn refresh(&self) {
        if let Some(live) = &self.live
            && let Err(err) = live.refresh()
        {
            tracing::warn!(workspace = %self.workspace, %err, "live diff refresh");
        }
    }

    pub fn files(&self) -> &FileMap {
        &self.files
    }

    /// `None` until the first update arrived.
    pub fn summary(&self) -> Option<&DiffSummary> {
        self.summary.as_ref()
    }

    /// Differs from its base and is not yet published: once committed and pushed, nothing is
    /// left to review here.
    pub fn needs_review(&self) -> bool {
        self.summary
            .as_ref()
            .is_some_and(|s| s.files > 0 && !s.status.is_published())
    }

    pub fn error(&self) -> Option<&SharedString> {
        self.error.as_ref()
    }
}
