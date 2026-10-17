//! Workspace and session lifecycle: create, start, attach, exit, archive, restore, delete.

use std::sync::{Mutex, PoisonError};

use super::*;

/// Held while a worktree and its workspace row are created.
static CREATE_LOCK: Mutex<()> = Mutex::new(());

/// A workspace just created, and its agent's start.
pub(super) struct SpawnedWorkspace {
    pub workspace: Workspace,
    /// How many starts wait before its agent's, when it was queued.
    pub queued: Option<usize>,
    /// Resolves to whether the agent started now.
    pub start: Task<bool>,
}

mod archive;
mod chats;
mod delete;
mod focus;
mod sessions;

impl HangarApp {
    /// Creates the worktree and workspace off the main thread, assigns it `target` as its base
    /// device (else the default on first use), shows it in the focused pane and starts
    /// `agent_id` in it. Resolves once the agent has started, failed or been queued. The task
    /// of `new` is typed into the agent once it runs, which may be later if the start is queued.
    pub(super) fn create_workspace(
        &mut self,
        project_id: ProjectId,
        new: ops::NewWorkspace,
        agent_id: String,
        target: Option<crate::device_panel::DeviceKey>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<WorkspaceId>> {
        let created = self.spawn_workspace(project_id, new, agent_id, target, true, window, cx);
        cx.spawn_in(window, async move |this, cx| match created.await {
            Ok(spawned) => {
                spawned.start.await;
                Some(spawned.workspace.id)
            }
            Err(err) => {
                let _ = this.update_in(cx, |this, window, cx| this.report(&err, window, cx));
                None
            }
        })
    }

    /// [`Self::create_workspace`] without reporting errors, and showing the new workspace in
    /// the focused pane only with `show`. Resolves once the workspace exists and its agent was
    /// asked to start.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn spawn_workspace(
        &mut self,
        project_id: ProjectId,
        new: ops::NewWorkspace,
        agent_id: String,
        target: Option<crate::device_panel::DeviceKey>,
        show: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<SpawnedWorkspace>> {
        let prepared = (|| {
            let store = self
                .store
                .clone()
                .ok_or_else(|| anyhow!("the database is not open"))?;
            let project = self
                .work
                .projects
                .project(project_id)
                .cloned()
                .ok_or_else(|| anyhow!("unknown project"))?;
            let root = paths::worktrees_root()
                .ok_or_else(|| anyhow!("cannot locate the worktree directory: HOME is not set"))?;
            anyhow::Ok((store, project, root))
        })();
        let (store, project, root) = match prepared {
            Ok(prepared) => prepared,
            Err(err) => return Task::ready(Err(err)),
        };
        self.work.creating.insert(project_id);
        cx.notify();

        let input = new.task.clone().filter(|t| !t.trim().is_empty());
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    // Agents may create several workspaces at once; git worktree commands on one
                    // repository must not race.
                    let _serial = CREATE_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
                    ops::create_workspace_from(&store, &project, &root, &new)
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.work.creating.remove(&project_id);
                cx.notify();
                let workspace = result?;
                tracing::info!(
                    name = %workspace.name,
                    branch = %workspace.branch,
                    path = %workspace.worktree_path.display(),
                    "workspace created"
                );
                let id = workspace.id;
                this.work.projects.upsert_workspace(workspace.clone());
                this.track_workspace(id, cx);
                if let Some(target) = target {
                    let assigned = this.devices.pool.update(cx, |pool, cx| {
                        pool.assign(id, target.platform, target.id, cx)
                    });
                    if let Err(err) = assigned {
                        tracing::warn!(workspace = %id, "device not assigned: {err:#}");
                    }
                }
                if show {
                    this.layout.panes.show(id);
                    this.panes_changed(window, cx);
                }
                let request = queue::StartRequest {
                    agent: agent_id,
                    resume: false,
                    input,
                };
                let start = this.request_agent(id, request, window, cx);
                Ok(SpawnedWorkspace {
                    queued: this.queued_ahead(id),
                    workspace,
                    start,
                })
            })
            .map_err(|_| anyhow!("the window closed"))?
        })
    }
}
