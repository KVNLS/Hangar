//! Adding projects.

use super::*;

impl HangarApp {
    pub(super) fn pick_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add Project".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let path = match picked.await {
                Ok(Ok(Some(paths))) => paths.into_iter().next(),
                Ok(Ok(None)) | Err(_) => None,
                Ok(Err(err)) => {
                    let _ = this.update_in(cx, |this, window, cx| this.report(&err, window, cx));
                    None
                }
            };
            if let Some(path) = path {
                let _ = this.update_in(cx, |this, window, cx| {
                    this.add_project(path, window, cx).detach();
                });
            }
        })
        .detach();
    }

    pub(super) fn add_project(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Option<Project>> {
        let Some(store) = self.store.clone() else {
            self.report(&anyhow!("the database is not open"), window, cx);
            return Task::ready(None);
        };
        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { ops::add_project(&store, &path) })
                .await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(project) => {
                    tracing::info!(name = %project.name, repo = %project.repo_path.display(), "project added");
                    this.work.projects.add_project(project.clone());
                    cx.notify();
                    Some(project)
                }
                Err(err) => {
                    this.report(&err, window, cx);
                    None
                }
            })
            .ok()
            .flatten()
        })
    }
}
