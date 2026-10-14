//! Device hooks: open a device in the panel or through the pool, send input, compare frames.

use super::*;

/// The workspaces created by `HANGAR_DEV_OPEN`, else the focused one.
fn dev_targets(
    this: &WeakEntity<HangarApp>,
    created: &[WorkspaceId],
    cx: &mut AsyncWindowContext,
) -> Vec<(WorkspaceId, String)> {
    this.update(cx, |this, _| {
        let ids: Vec<WorkspaceId> = if created.is_empty() {
            this.focused_workspace()
                .map(|(_, w)| w.id)
                .into_iter()
                .collect()
        } else {
            created.to_vec()
        };
        ids.into_iter()
            .filter_map(|id| {
                this.work
                    .projects
                    .workspace(id)
                    .map(|w| (id, w.slug.clone()))
            })
            .collect()
    })
    .unwrap_or_default()
}

/// `HANGAR_DEV_RUN`: builds and runs every dev workspace at once (as ⌘R on each), on each
/// workspace's own device, and logs each outcome (`HANGAR_DEV_RUN <slug> ...`) once all ended.
pub(super) async fn dev_run(
    this: &WeakEntity<HangarApp>,
    created: &[WorkspaceId],
    cx: &mut AsyncWindowContext,
) {
    let targets = dev_targets(this, created, cx);
    if targets.is_empty() {
        tracing::error!("HANGAR_DEV_RUN: no workspace to run");
        return;
    }
    let mut runs = Vec::new();
    for (id, slug) in targets {
        tracing::info!(%slug, "HANGAR_DEV_RUN");
        if let Ok(task) = this.update_in(cx, |this, window, cx| this.run_workspace(id, window, cx))
        {
            runs.push((slug, std::time::Instant::now(), task));
        }
    }
    for (slug, started, task) in runs {
        match task.await {
            Ok(summary) => tracing::info!(
                %slug,
                %summary,
                secs = started.elapsed().as_secs(),
                "HANGAR_DEV_RUN done"
            ),
            Err(err) => {
                tracing::error!(%slug, secs = started.elapsed().as_secs(), "HANGAR_DEV_RUN failed: {err:#}")
            }
        }
    }
}

/// `HANGAR_DEV_POOL_FRAMES=<dir>`: through the device API, logs each dev workspace's device
/// status, saves its latest frame as `<dir>/<slug>.png`, taps the screen centre and saves the
/// next frame as `<slug>-tap.png`.
pub(super) async fn dev_pool_frames(
    this: &WeakEntity<HangarApp>,
    created: &[WorkspaceId],
    dir: &std::path::Path,
    cx: &mut AsyncWindowContext,
) {
    if let Err(err) = std::fs::create_dir_all(dir) {
        tracing::error!(%err, "HANGAR_DEV_POOL_FRAMES: cannot create {}", dir.display());
        return;
    }
    for (id, slug) in dev_targets(this, created, cx) {
        let Ok(status) = this.update(cx, |this, cx| this.device_status(id, cx)) else {
            return;
        };
        match status.await {
            Ok(status) => tracing::info!(%slug, ?status, "HANGAR_DEV_POOL_FRAMES status"),
            Err(err) => tracing::error!(%slug, "HANGAR_DEV_POOL_FRAMES status: {err:#}"),
        }
        let Ok(frame) = this.update(cx, |this, cx| this.latest_frame(id, cx)) else {
            return;
        };
        let shot = match frame.await {
            Ok(shot) => shot,
            Err(err) => {
                tracing::error!(%slug, "HANGAR_DEV_POOL_FRAMES frame: {err:#}");
                continue;
            }
        };
        let path = dir.join(format!("{slug}.png"));
        match shot.frame.save_png(&path) {
            Ok(()) => tracing::info!(
                %slug,
                path = %path.display(),
                width = shot.frame.width,
                height = shot.frame.height,
                width_pt = shot.width_pt,
                height_pt = shot.height_pt,
                "HANGAR_DEV_POOL_FRAMES saved"
            ),
            Err(err) => tracing::error!(%slug, "HANGAR_DEV_POOL_FRAMES save: {err}"),
        }
        let tap = hangar_devices::Input::Tap {
            x: shot.width_pt / 2.0,
            y: shot.height_pt / 2.0,
        };
        let Ok(sent) = this.update(cx, |this, cx| this.send_input(id, tap, cx)) else {
            return;
        };
        match sent.await {
            Ok(()) => tracing::info!(%slug, ?tap, "HANGAR_DEV_POOL_FRAMES tap sent"),
            Err(err) => tracing::error!(%slug, "HANGAR_DEV_POOL_FRAMES tap: {err:#}"),
        }
        cx.background_executor().timer(Duration::from_secs(1)).await;
        if let Ok(frame) = this.update(cx, |this, cx| this.latest_frame(id, cx))
            && let Ok(after) = frame.await
        {
            let path = dir.join(format!("{slug}-tap.png"));
            let diff = after.frame.diff_ratio(&shot.frame);
            let _ = after.frame.save_png(&path);
            tracing::info!(%slug, diff_percent = diff * 100.0, path = %path.display(), "HANGAR_DEV_POOL_FRAMES after tap");
        }
        let Ok(logs) = this.update(cx, |this, cx| this.recent_logs(id, 5, None, None, cx)) else {
            return;
        };
        match logs.await {
            Ok(text) => {
                tracing::info!(%slug, lines = text.lines().count(), "HANGAR_DEV_POOL_FRAMES logs")
            }
            Err(err) => tracing::error!(%slug, "HANGAR_DEV_POOL_FRAMES logs: {err:#}"),
        }
    }
}

/// `HANGAR_DEV_SHOW`: leaves the window on one screen or overlay.
pub(super) async fn dev_show(
    this: &WeakEntity<HangarApp>,
    name: &str,
    cx: &mut AsyncWindowContext,
) {
    use super::settings::SettingsTab;
    if name.starts_with("review") {
        // The review screen only lists files once the changes panel has loaded them.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while this
            .update(cx, |this, cx| {
                this.changes_panel.read(cx).review_stats().files == 0
            })
            .unwrap_or(false)
            && std::time::Instant::now() < deadline
        {
            cx.background_executor().timer(DEV_POLL).await;
        }
    }
    let shown = this.update_in(cx, |this, window, cx| {
        let settings = |tab| (Screen::Settings, Some(tab));
        let (screen, tab) = match name {
            "workspace" | "device" => (Screen::Workspace, None),
            "review" | "review-inline" => (Screen::Review, None),
            "overview" | "compare" => (Screen::Overview, None),
            "settings" | "settings-agents" | "settings-permissions" => {
                settings(SettingsTab::Agents)
            }
            "settings-devices" => settings(SettingsTab::Devices),
            "settings-appearance" => settings(SettingsTab::Appearance),
            "settings-keybindings" => settings(SettingsTab::Keybindings),
            "palette" | "sheet" => (Screen::Workspace, None),
            _ => return false,
        };
        this.set_screen(screen, window, cx);
        if let Some(tab) = tab {
            this.open_settings_tab(tab, window, cx);
        }
        if name.starts_with("review") {
            let split = name == "review";
            this.changes_panel
                .update(cx, |panel, cx| panel.set_split_view(split, cx));
        }
        match name {
            "device" => this.set_device_open(true, window, cx),
            "palette" => this.open_palette(window, cx),
            "sheet" => match this.current_project() {
                Some(project) => this.open_new_workspace(project, window, cx),
                None => tracing::error!("HANGAR_DEV_SHOW sheet: no project"),
            },
            // Permissions sit at the bottom of the Agents page; the page is laid out next frame.
            "settings-permissions" => {
                let scroll = this.settings_ui.scroll.clone();
                cx.on_next_frame(window, move |_, _, cx| {
                    scroll.scroll_to_bottom();
                    cx.notify();
                });
            }
            "compare" => match this.focused_workspace().and_then(|(_, ws)| ws.group) {
                Some(group) => this.open_compare(group, window, cx),
                None => tracing::error!("HANGAR_DEV_SHOW compare: the workspace has no attempts"),
            },
            _ => {}
        }
        true
    });
    match shown {
        Ok(true) => tracing::info!(%name, "HANGAR_DEV_SHOW"),
        Ok(false) => tracing::error!(
            %name,
            "HANGAR_DEV_SHOW: expected workspace, review, review-inline, overview, settings-<tab>, palette or sheet"
        ),
        Err(_) => {}
    }
}

/// How long the device hooks wait for the device list and for frames.
const DEV_DEVICE_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) const DEV_BOOT_TIMEOUT: Duration = Duration::from_secs(300);

pub(super) const DEV_POLL: Duration = Duration::from_millis(100);

pub(super) enum DevAction {
    Input(hangar_devices::Input),
    Text(String),
}

/// `HANGAR_DEV_DEVICE` (+ `_BOOT`, `_LOGS`, `_HOME`, `_TAP`, `_TEXT`): opens the device panel on the device, waits for a
/// painted frame, then sends each input and logs whether a later painted frame differs.
pub(super) async fn dev_device(
    this: WeakEntity<HangarApp>,
    id: String,
    boot: bool,
    logs: bool,
    shutdown: bool,
    inputs: Vec<(&'static str, DevAction)>,
    cx: &mut AsyncWindowContext,
) {
    tracing::info!(%id, "HANGAR_DEV_DEVICE");
    let Ok(panel) = this.update_in(cx, |this, window, cx| {
        this.set_device_open(true, window, cx);
        this.device_panel.clone()
    }) else {
        return;
    };
    if logs {
        panel.update(cx, |panel, cx| panel.set_logs_open(true, cx));
    }
    let deadline = std::time::Instant::now() + DEV_DEVICE_TIMEOUT;
    let wait = |cx: &AsyncWindowContext| cx.background_executor().timer(DEV_POLL);
    // Wait for a listing that knows the device.
    let key = loop {
        let found = panel.update(cx, |panel, _| {
            let picker = panel.picker();
            (picker.listed() && !picker.is_listing()).then(|| picker.find_id(&id))
        });
        match found {
            Some(Some(key)) => break key,
            Some(None) => {
                tracing::error!(%id, "HANGAR_DEV_DEVICE: device not listed (is it booted?)");
                return;
            }
            None if std::time::Instant::now() < deadline => wait(cx).await,
            None => {
                tracing::error!(%id, "HANGAR_DEV_DEVICE: device list not ready");
                return;
            }
        }
    };
    let mut deadline = deadline;
    if boot {
        // Booting takes minutes; the panel selects and opens the device once it is up.
        deadline = std::time::Instant::now() + DEV_BOOT_TIMEOUT;
        let _ = panel.update_in(cx, |panel, window, cx| {
            panel.select_device(key.clone(), window, cx);
            panel.boot_device(key, window, cx);
        });
    } else {
        let _ = panel.update_in(cx, |panel, window, cx| panel.select_device(key, window, cx));
    }
    let painted = |cx: &mut AsyncWindowContext| {
        panel.update(cx, |panel, cx| {
            panel
                .view()
                .and_then(|view| view.read(cx).painted_frame().cloned())
        })
    };
    let baseline = loop {
        if let Some(frame) = painted(cx) {
            break frame;
        }
        if std::time::Instant::now() >= deadline {
            tracing::error!(%id, "HANGAR_DEV_DEVICE: no frame painted");
            return;
        }
        wait(cx).await;
    };
    tracing::info!(
        seq = baseline.seq,
        width = baseline.width,
        height = baseline.height,
        "HANGAR_DEV_DEVICE frame painted"
    );
    'inputs: for (hook, action) in inputs {
        // Let animations settle so the baseline is what the input acts on.
        cx.background_executor().timer(Duration::from_secs(2)).await;
        let Some(baseline) = painted(cx) else {
            return;
        };
        let sent_at = std::time::Instant::now();
        let sent = match &action {
            DevAction::Input(input) => panel.update(cx, |panel, cx| panel.send(*input, cx)),
            DevAction::Text(text) => {
                panel.update(cx, |panel, cx| panel.send_text(text.clone(), cx))
            }
        };
        if !sent {
            tracing::error!("{hook}: device closed");
            return;
        }
        match &action {
            DevAction::Input(input) => tracing::info!(?input, seq = baseline.seq, "{hook} sent"),
            DevAction::Text(text) => {
                tracing::info!(%text, seq = baseline.seq, "{hook} sent");
            }
        }
        let deadline = sent_at + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            wait(cx).await;
            if let Some(err) = panel.read_with(cx, |panel, _| panel.input_error()) {
                tracing::error!(%err, "{hook}: input failed");
                continue 'inputs;
            }
            let Some(frame) = painted(cx) else {
                continue;
            };
            let diff = frame.diff_ratio(&baseline);
            if frame.seq != baseline.seq && diff > 0.002 {
                tracing::info!(
                    seq = frame.seq,
                    diff_percent = diff * 100.0,
                    after_ms = sent_at.elapsed().as_millis() as u64,
                    "{hook} painted frame changed"
                );
                continue 'inputs;
            }
        }
        tracing::error!("{hook}: painted frame did not change within 5 s");
    }
    if shutdown {
        tracing::info!(%id, "HANGAR_DEV_DEVICE_SHUTDOWN");
        panel.update(cx, |panel, cx| panel.shutdown_device(cx));
        let deadline = std::time::Instant::now() + DEV_BOOT_TIMEOUT;
        while std::time::Instant::now() < deadline {
            wait(cx).await;
            if panel.read_with(cx, |panel, _| panel.view().is_none()) {
                tracing::info!(%id, "HANGAR_DEV_DEVICE_SHUTDOWN done");
                return;
            }
        }
        tracing::error!(%id, "HANGAR_DEV_DEVICE_SHUTDOWN: device still open");
    }
}
