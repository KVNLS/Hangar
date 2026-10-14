//! Review hooks: stage, commit, push and open a pull request from the changes panel.

use super::*;

/// `HANGAR_DEV_COMMIT`, `_PUSH`, `_PR`, `_MERGE`, in that order, on the workspace shown in the
/// changes panel (the `HANGAR_DEV_OPEN` one). Each goes through the panel's own operations and
/// logs its outcome.
pub(super) async fn run_review_hooks(
    this: &WeakEntity<HangarApp>,
    dev: &DevOptions,
    cx: &mut AsyncWindowContext,
) {
    if dev.commit.is_none() && !dev.push && dev.pr.is_none() && dev.merge.is_none() {
        return;
    }
    let Ok(panel) = this.update(cx, |this, _| this.changes_panel.clone()) else {
        return;
    };
    let worktree = this
        .update(cx, |this, cx| {
            let id = panel.read(cx).workspace()?;
            this.work
                .projects
                .workspace(id)
                .map(|ws| ws.worktree_path.clone())
        })
        .ok()
        .flatten();
    let Some(worktree) = worktree else {
        tracing::error!("review hooks: no workspace in the changes panel");
        return;
    };
    let executor = cx.background_executor().clone();

    if let Some(message) = dev.commit.clone() {
        // The agent writes files asynchronously: stage until something is staged.
        let mut staged = 0;
        for _ in 0..50 {
            let task = panel.update(cx, |panel, cx| panel.stage_all(cx));
            if let Err(err) = task.await {
                tracing::error!("HANGAR_DEV_COMMIT stage all: {err:#}");
            }
            let path = worktree.clone();
            staged = executor
                .spawn(async move {
                    crate::adapters::changes()
                        .status(&path, &[])
                        .map(|s| s.staged)
                })
                .await
                .unwrap_or(0);
            if staged > 0 {
                break;
            }
            executor.timer(Duration::from_millis(300)).await;
        }
        tracing::info!(staged, "HANGAR_DEV_COMMIT staged");
        let task = panel.update(cx, |panel, cx| panel.commit(message, cx));
        match task.await {
            Ok(oid) => tracing::info!(%oid, "HANGAR_DEV_COMMIT committed"),
            Err(err) => tracing::error!("HANGAR_DEV_COMMIT: {err:#}"),
        }
    }
    if dev.push {
        let task = panel.update(cx, |panel, cx| panel.push(cx));
        match task.await {
            Ok(out) => tracing::info!(?out, "HANGAR_DEV_PUSH pushed"),
            Err(err) => tracing::error!("HANGAR_DEV_PUSH: {err:#}"),
        }
    }
    if let Some(title) = dev.pr.clone() {
        // The default title comes from the review info, loaded once gh is known.
        let mut defaults = None;
        for _ in 0..50 {
            defaults = panel.update(cx, |panel, _| {
                panel.pr_defaults().filter(|_| panel.info_loaded())
            });
            if defaults.is_some() {
                break;
            }
            executor.timer(Duration::from_millis(200)).await;
        }
        match defaults {
            None => tracing::error!("HANGAR_DEV_PR: review info never loaded"),
            Some((workspace, default_title)) => {
                let request = crate::changes::review::PrRequest {
                    title: if title == "1" { default_title } else { title },
                    body: "Opened by HANGAR_DEV_PR.".to_owned(),
                    draft: true,
                };
                let task = panel.update(cx, |panel, cx| panel.create_pr(workspace, request, cx));
                match task.await {
                    Ok(url) => tracing::info!(%url, "HANGAR_DEV_PR opened"),
                    Err(err) => tracing::error!("HANGAR_DEV_PR: {err:#}"),
                }
            }
        }
    }
    if let Some(strategy) = dev.merge {
        let task = panel.update(cx, |panel, cx| {
            let (workspace, message) = panel.merge_defaults(strategy)?;
            Some(panel.merge(workspace, strategy, message, cx))
        });
        match task {
            Some(task) => match task.await {
                Ok(out) => tracing::info!(?strategy, ?out, "HANGAR_DEV_MERGE merged"),
                Err(err) => tracing::error!(?strategy, "HANGAR_DEV_MERGE: {err:#}"),
            },
            None => tracing::error!("HANGAR_DEV_MERGE: no workspace in the changes panel"),
        }
    }
}

/// `HANGAR_DEV_REVIEW`: waits (up to 15 s) for the changes panel to list files, opens the review
/// screen, then logs what it shows in split and inline mode.
pub(super) async fn dev_review(this: &WeakEntity<HangarApp>, cx: &mut AsyncWindowContext) {
    let stats = |this: &WeakEntity<HangarApp>, cx: &mut AsyncWindowContext| {
        this.update(cx, |this, cx| this.changes_panel.read(cx).review_stats())
            .ok()
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while stats(this, cx).is_some_and(|s| s.files == 0) && std::time::Instant::now() < deadline {
        cx.background_executor().timer(DEV_POLL).await;
    }
    let _ = this.update_in(cx, |this, window, cx| {
        this.set_screen(Screen::Review, window, cx)
    });
    cx.background_executor()
        .timer(Duration::from_millis(500))
        .await;
    match stats(this, cx) {
        Some(s) => tracing::info!(
            files = s.files,
            unified_rows = s.unified_rows,
            split_rows = s.split_rows,
            wide = s.wide,
            "HANGAR_DEV_REVIEW"
        ),
        None => tracing::error!("HANGAR_DEV_REVIEW: app gone"),
    }
    let _ = this.update(cx, |this, cx| {
        this.changes_panel
            .update(cx, |panel, cx| panel.set_split_view(false, cx))
    });
    cx.background_executor()
        .timer(Duration::from_millis(300))
        .await;
    let _ = this.update_in(cx, |this, window, cx| {
        this.set_screen(Screen::Workspace, window, cx)
    });
    tracing::info!("HANGAR_DEV_REVIEW inline rendered, back to workspace");
}
