//! `HANGAR_DEV_*` automation hooks (documented in `main.rs`).

mod device;
mod options;
mod review;

use super::*;
use crate::adapters::devices::SavePng as _;

use device::{
    DEV_BOOT_TIMEOUT, DEV_POLL, DevAction, dev_device, dev_pool_frames, dev_run, dev_show,
};
pub use options::DevOptions;
use review::{dev_review, run_review_hooks};

impl HangarApp {
    pub(super) fn dev_open(
        &mut self,
        repo: PathBuf,
        dev: DevOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<Vec<WorkspaceId>> {
        tracing::info!(repo = %repo.display(), "HANGAR_DEV_OPEN");
        let add = self.add_project(ops::canonical(&repo), window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let Some(project) = add.await else {
                return Vec::new();
            };
            let names = if dev.workspaces.is_empty() {
                vec![dev.workspace.clone()]
            } else {
                dev.workspaces.clone()
            };
            let agents: Vec<(String, String)> = dev
                .agent
                .split(',')
                .map(|a| (a.trim().to_owned(), a.trim().to_owned()))
                .collect();
            let mut planned = Vec::new();
            for name in names {
                let request = hangar_usecases::batch::BatchRequest {
                    name,
                    task: dev.input.clone().unwrap_or_default(),
                    agents: agents.clone(),
                    ..Default::default()
                };
                match hangar_usecases::batch::plan(&request) {
                    Ok(items) => planned.extend(items),
                    Err(err) => tracing::warn!("HANGAR_DEV_OPEN: {err}"),
                }
            }
            let mut created = Vec::new();
            for item in planned {
                let target = dev.target.clone();
                let Ok(create) = this.update_in(cx, |this, window, cx| {
                    this.create_workspace(
                        project.id,
                        item.workspace,
                        item.agent,
                        target,
                        window,
                        cx,
                    )
                }) else {
                    break;
                };
                let Some(workspace_id) = create.await else {
                    continue;
                };
                created.push(workspace_id);
            }
            created
        })
    }
}

/// Runs the `HANGAR_DEV_*` hooks in order: open, archive, restore, delete, remove project.
pub(super) async fn run_dev_hooks(
    this: WeakEntity<HangarApp>,
    dev: DevOptions,
    cx: &mut AsyncWindowContext,
) {
    if let Some(host) = dev.ollama.clone()
        && let Ok(listed) = this.update_in(cx, |this, window, cx| {
            this.use_ollama_for_run(host, window, cx)
        })
    {
        listed.await;
    }
    if let Some(changes) = dev.changes.clone() {
        tracing::info!(%changes, "HANGAR_DEV_CHANGES");
        let _ = this.update_in(cx, |this, window, cx| {
            if changes != "1" {
                this.changes_panel
                    .update(cx, |panel, _| panel.prefer(PathBuf::from(changes)));
            }
            this.set_changes_open(true, window, cx)
        });
    }
    if let Some(id) = dev.device.clone() {
        let mut inputs = Vec::new();
        if dev.device_home {
            inputs.push((
                "HANGAR_DEV_DEVICE_HOME",
                DevAction::Input(hangar_core::device::Input::Home),
            ));
        }
        if let Some((x, y)) = dev.device_tap {
            inputs.push((
                "HANGAR_DEV_DEVICE_TAP",
                DevAction::Input(hangar_core::device::Input::Tap { x, y }),
            ));
        }
        if let Some(text) = dev.device_text.clone() {
            inputs.push(("HANGAR_DEV_DEVICE_TEXT", DevAction::Text(text)));
        }
        for input in dev.device_buttons.iter().chain(&dev.device_keys) {
            inputs.push(("HANGAR_DEV_DEVICE_INPUT", DevAction::Input(*input)));
        }
        let boot = dev.device_boot;
        let shutdown = dev.device_shutdown;
        let logs = dev.device_logs;
        let this = this.clone();
        // Runs alongside the other hooks: waiting for frames must not hold them up.
        cx.spawn(async move |cx| dev_device(this, id, boot, logs, shutdown, inputs, cx).await)
            .detach();
    }
    let mut created = Vec::new();
    if let Some(repo) = dev.open.clone() {
        let open = dev.clone();
        if let Ok(task) =
            this.update_in(cx, |this, window, cx| this.dev_open(repo, open, window, cx))
        {
            created = task.await;
        }
    }
    if let Some(run) = dev.run.as_deref() {
        let mut targets = created.clone();
        if run != "1" {
            targets = this
                .update(cx, |this, _| {
                    run.split(',')
                        .filter_map(|slug| {
                            let found = this.workspace_by_slug(slug.trim());
                            if found.is_none() {
                                tracing::error!(%slug, "HANGAR_DEV_RUN: no such workspace");
                            }
                            found
                        })
                        .collect()
                })
                .unwrap_or_default();
        }
        dev_run(&this, &targets, cx).await;
    }
    if let Some(dir) = dev.pool_frames.clone() {
        dev_pool_frames(&this, &created, &dir, cx).await;
    }
    if let Some(slug) = dev.select.clone() {
        let _ = this.update_in(cx, |this, window, cx| match this.workspace_by_slug(&slug) {
            Some(id) => {
                tracing::info!(%slug, "HANGAR_DEV_SELECT");
                this.select_workspace(id, window, cx);
            }
            None => tracing::error!(%slug, "HANGAR_DEV_SELECT: no such workspace"),
        });
    }
    run_review_hooks(&this, &dev, cx).await;
    if dev.review {
        dev_review(&this, cx).await;
    }
    if let Some(name) = dev.appearance.clone() {
        dev_settings(&this, &name, cx).await;
    }
    if let Some(path) = dev.files.clone() {
        let _ = this.update_in(cx, |this, window, cx| this.set_files_open(true, window, cx));
        cx.background_executor()
            .timer(Duration::from_millis(800))
            .await;
        let _ = this.update(cx, |this, cx| {
            this.files_panel
                .update(cx, |panel, cx| panel.open(PathBuf::from(path), cx))
        });
        cx.background_executor()
            .timer(Duration::from_millis(500))
            .await;
        let _ = this.update(cx, |this, cx| {
            let (files, rows, lines) = this.files_panel.read(cx).stats();
            tracing::info!(
                files,
                rows,
                ?lines,
                open = this.layout.files_open,
                "HANGAR_DEV_FILES"
            );
        });
    }
    if dev.overview {
        let _ = this.update_in(cx, |this, window, cx| {
            this.set_screen(Screen::Overview, window, cx)
        });
        cx.background_executor()
            .timer(Duration::from_millis(500))
            .await;
        let _ = this.update_in(cx, |this, window, cx| {
            let counts = this.state_counts(cx);
            tracing::info!(?counts, screen = ?this.layout.screen, "HANGAR_DEV_OVERVIEW");
            this.focus_next_needing_you(window, cx);
            let focused = this.focused_workspace().map(|(_, ws)| ws.slug.clone());
            tracing::info!(?focused, screen = ?this.layout.screen, "HANGAR_DEV_OVERVIEW next needing you");
            this.set_screen(Screen::Workspace, window, cx);
        });
    }
    let find = |this: &mut HangarApp, hook: &str, slug: &str| {
        let found = this.workspace_by_slug(slug);
        if found.is_none() {
            tracing::error!(%slug, "{hook}: no such workspace");
        }
        found
    };
    if let Some(slug) = dev.archive.clone() {
        tracing::info!(%slug, "HANGAR_DEV_ARCHIVE");
        let task = this.update_in(cx, |this, window, cx| {
            find(this, "HANGAR_DEV_ARCHIVE", &slug).map(|id| this.archive_workspace(id, window, cx))
        });
        if let Ok(Some(task)) = task {
            task.await;
        }
    }
    if let Some(slug) = dev.restore.clone() {
        tracing::info!(%slug, "HANGAR_DEV_RESTORE");
        let task = this.update_in(cx, |this, window, cx| {
            find(this, "HANGAR_DEV_RESTORE", &slug).map(|id| this.restore_workspace(id, window, cx))
        });
        if let Ok(Some(task)) = task {
            task.await;
        }
    }
    if let Some(slug) = dev.delete.clone() {
        let options = DeleteOptions {
            force: dev.delete_force,
            delete_branch: dev.delete_branch,
        };
        tracing::info!(%slug, ?options, "HANGAR_DEV_DELETE");
        let task = this.update_in(cx, |this, window, cx| {
            find(this, "HANGAR_DEV_DELETE", &slug)
                .map(|id| this.delete_workspace(id, options, window, cx))
        });
        if let Ok(Some(task)) = task {
            task.await;
        }
    }
    if let Some(repo) = dev.remove_project.clone() {
        tracing::info!(repo = %repo.display(), "HANGAR_DEV_REMOVE_PROJECT");
        let repo = ops::canonical(&repo);
        let task = this.update_in(cx, |this, window, cx| {
            let id = this
                .work.projects
                .entries()
                .iter()
                .find(|e| ops::canonical(&e.project.repo_path) == repo)
                .map(|e| e.project.id);
            match id {
                Some(id) => Some(this.remove_project(id, window, cx)),
                None => {
                    tracing::error!(repo = %repo.display(), "HANGAR_DEV_REMOVE_PROJECT: no such project");
                    None
                }
            }
        });
        if let Ok(Some(task)) = task {
            task.await;
        }
    }
    if let Some(name) = dev.show.clone() {
        dev_show(&this, &name, cx).await;
    }
    if dev.screenshot {
        dev_screenshot(&this, cx).await;
    }
    for (delay, text) in dev.keys.clone() {
        cx.background_executor()
            .timer(Duration::from_secs(delay))
            .await;
        let typed = this.update_in(cx, |this, window, cx| {
            let id = this.layout.panes.slot(this.layout.panes.focused())?;
            let live = this.live.sessions.get(&id)?;
            Some(live.prefill(&text, window, cx))
        });
        tracing::info!(keys = %text.escape_debug(), ok = matches!(typed, Ok(Some(Ok(())))), "HANGAR_DEV_KEYS");
    }
    if let Some(delay) = dev.approve {
        dev_approve(&this, Duration::from_secs(delay), cx).await;
    }
    if let Some((delay, text)) = dev.answer.clone() {
        dev_answer(&this, Duration::from_secs(delay), text, cx).await;
    }
}

/// `HANGAR_DEV_ANSWER=<seconds>|<text>`: opens the answer sheet of the first question, then
/// answers it `seconds` later.
async fn dev_answer(
    this: &WeakEntity<HangarApp>,
    delay: Duration,
    text: String,
    cx: &mut AsyncWindowContext,
) {
    let deadline = std::time::Instant::now() + DEV_BOOT_TIMEOUT;
    let asked = loop {
        let first = this
            .update(cx, |this, _| {
                this.live
                    .questions
                    .iter()
                    .next()
                    .map(|(id, q)| (*id, q.text.clone(), q.choices.clone()))
            })
            .ok()
            .flatten();
        if let Some(first) = first {
            break first;
        }
        if std::time::Instant::now() >= deadline {
            tracing::error!("HANGAR_DEV_ANSWER: no question");
            return;
        }
        cx.background_executor().timer(DEV_POLL).await;
    };
    let (id, question, choices) = asked;
    tracing::info!(workspace = %id, %question, ?choices, "HANGAR_DEV_ANSWER question");
    let _ = this.update_in(cx, |this, window, cx| {
        this.select_workspace(id, window, cx);
        this.open_answer_sheet(id, window, cx)
    });
    cx.background_executor().timer(delay).await;
    let choice = text
        .trim()
        .parse::<usize>()
        .ok()
        .filter(|i| *i < choices.len());
    let answer = hangar_mcp::Answer {
        text: choice.map_or(text, |i| choices[i].clone()),
        choice,
    };
    let _ = this.update_in(cx, |this, window, cx| {
        this.answer_question(id, answer, window, cx)
    });
    tracing::info!(workspace = %id, ?choice, "HANGAR_DEV_ANSWER answered");
}

/// `HANGAR_DEV_APPROVE=<seconds>`: allows the first pending permission request once.
async fn dev_approve(this: &WeakEntity<HangarApp>, delay: Duration, cx: &mut AsyncWindowContext) {
    let deadline = std::time::Instant::now() + DEV_BOOT_TIMEOUT;
    let pending = loop {
        let first = this
            .update(cx, |this, cx| {
                this.pending_approvals(cx)
                    .into_iter()
                    .next()
                    .map(|(id, _, request)| (id, request.title))
            })
            .ok()
            .flatten();
        if let Some(first) = first {
            break first;
        }
        if std::time::Instant::now() >= deadline {
            tracing::error!("HANGAR_DEV_APPROVE: no permission request");
            return;
        }
        cx.background_executor().timer(DEV_POLL).await;
    };
    let (id, title) = pending;
    tracing::info!(workspace = %id, %title, "HANGAR_DEV_APPROVE waiting");
    cx.background_executor().timer(delay).await;
    let _ = this.update(cx, |this, cx| this.answer_request(id, true, cx));
    let left = this
        .update(cx, |this, cx| this.pending_request(id, cx).is_some())
        .unwrap_or(true);
    tracing::info!(workspace = %id, still_pending = left, "HANGAR_DEV_APPROVE answered");
}

/// `HANGAR_DEV_SCREENSHOT=1`: the device panel's ⌘S once the workspace device paints a frame.
async fn dev_screenshot(this: &WeakEntity<HangarApp>, cx: &mut AsyncWindowContext) {
    let deadline = std::time::Instant::now() + DEV_BOOT_TIMEOUT;
    loop {
        let ready = this
            .update(cx, |this, cx| {
                let panel = this.devices.panel.read(cx);
                let ws = panel.workspace()?;
                panel
                    .view()
                    .and_then(|v| v.read(cx).painted_frame().map(|_| ws))
            })
            .ok()
            .flatten();
        if let Some(ws) = ready {
            tracing::info!(workspace = %ws, "HANGAR_DEV_SCREENSHOT");
            let _ = this.update_in(cx, |this, window, cx| {
                this.screenshot_to_agent(ws, window, cx)
            });
            return;
        }
        if std::time::Instant::now() >= deadline {
            tracing::error!("HANGAR_DEV_SCREENSHOT: no workspace device frame");
            return;
        }
        cx.background_executor().timer(DEV_POLL).await;
    }
}

/// `HANGAR_DEV_APPEARANCE`: renders each settings page, then applies and saves the appearance.
async fn dev_settings(this: &WeakEntity<HangarApp>, name: &str, cx: &mut AsyncWindowContext) {
    use crate::ui::theme::Appearance;
    let appearance = match name {
        "dark" => Appearance::Dark,
        "light" => Appearance::Light,
        "system" => Appearance::System,
        other => {
            tracing::error!(%other, "HANGAR_DEV_APPEARANCE: expected dark, light or system");
            return;
        }
    };
    let _ = this.update_in(cx, |this, window, cx| {
        this.set_screen(Screen::Settings, window, cx)
    });
    for tab in super::settings::SettingsTab::ALL {
        let _ = this.update_in(cx, |this, window, cx| {
            this.open_settings_tab(tab, window, cx)
        });
        cx.background_executor()
            .timer(Duration::from_millis(200))
            .await;
    }
    let _ = this.update_in(cx, |this, window, cx| {
        this.set_appearance(appearance, window, cx);
        tracing::info!(appearance = ?this.layout.appearance, dark = cx.theme().is_dark(), "HANGAR_DEV_APPEARANCE");
        this.set_screen(Screen::Workspace, window, cx);
    });
}
