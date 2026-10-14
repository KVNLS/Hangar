//! Opening the database and recovering from one that cannot be read.

use super::*;

/// Opens the database at `db` and reads what the app restores. Blocks.
pub(super) fn open_and_load(db: Option<PathBuf>) -> anyhow::Result<(Arc<dyn Store>, Loaded)> {
    let db =
        db.ok_or_else(|| anyhow!("cannot locate the database: HOME is not set (set HANGAR_DB)"))?;
    let store = crate::adapters::open_store(&db)?;
    let loaded = ops::load(&store)?;
    tracing::info!(
        db = %db.display(),
        projects = loaded.entries.len(),
        layout = ?loaded.layout,
        agent = ?loaded.agent,
        appearance = ?loaded.appearance,
        "loaded store"
    );
    Ok((store, loaded))
}

/// Renames `db` and its WAL files to `<db>.unreadable-<unix time>`; returns the new name.
fn move_aside(db: &std::path::Path) -> anyhow::Result<PathBuf> {
    let stamp = hangar_core::Timestamp::now().0;
    let mut to = db.as_os_str().to_owned();
    to.push(format!(".unreadable-{stamp}"));
    let to = PathBuf::from(to);
    std::fs::rename(db, &to).map_err(|e| anyhow!("cannot move {} aside: {e}", db.display()))?;
    for suffix in ["-wal", "-shm"] {
        let mut from = db.as_os_str().to_owned();
        from.push(suffix);
        let mut dest = to.as_os_str().to_owned();
        dest.push(suffix);
        if let Err(err) = std::fs::rename(&from, &dest)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("cannot move {} aside: {err}", PathBuf::from(from).display());
        }
    }
    Ok(to)
}

impl HangarApp {
    /// The database cannot be opened or read: offers to move it aside and start with an empty
    /// one, to go on without saving (the sidebar keeps the error), or to quit.
    pub(super) fn offer_database_recovery(
        &mut self,
        err: &anyhow::Error,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(db) = paths::db_path() else {
            self.report(err, window, cx);
            return;
        };
        let detail = format!(
            "{err:#}\n\nMoving it aside renames {} and starts with an empty database; your \
             worktrees and branches stay where they are.",
            db.display()
        );
        let answer = window.prompt(
            PromptLevel::Critical,
            "Hangar cannot read its database",
            Some(&detail),
            &[
                "Move Aside and Start Fresh",
                "Continue Without Saving",
                "Quit",
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| match answer.await {
            Ok(0) => {
                let moved = cx
                    .background_executor()
                    .spawn(async move { move_aside(&db).map(|to| (to, open_and_load(Some(db)))) })
                    .await;
                let _ = this.update_in(cx, |this, window, cx| match moved {
                    Ok((to, loaded)) => {
                        tracing::warn!(to = %to.display(), "moved the unreadable database aside");
                        this.work.load_error = None;
                        this.on_loaded(loaded, window, cx);
                    }
                    Err(err) => this.report(&err, window, cx),
                });
            }
            Ok(2) => {
                let _ = cx.update(|_, cx| cx.quit());
            }
            _ => {}
        })
        .detach();
    }
}
