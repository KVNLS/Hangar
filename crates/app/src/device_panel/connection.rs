//! Listing, picking, booting, opening and shutting down the panel's device.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use gpui_kit::*;

use crate::device_view::{DeviceSession, DeviceView, DeviceViewEvent};

use super::picker::{DeviceEntry, DeviceKey, Effect};
use super::platform::{BackendReason, Listing};
use super::{
    BOOT_TICK, Booting, Connection, DeviceOption, DevicePanel, DevicePanelEvent, InputStatus,
    entry_title, input_status, plural_chars,
};

impl DevicePanel {
    pub(super) fn on_listed(
        &mut self,
        listing: Listing,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.listing = None;
        for (platform, err) in &listing.errors {
            tracing::warn!(platform = platform.label(), %err, "device listing failed");
        }
        let entries: Vec<DeviceEntry> = listing
            .devices
            .into_iter()
            .map(|(platform, info)| DeviceEntry::new(platform, info))
            .collect();
        tracing::info!(devices = entries.len(), "devices listed");
        let before = self.picker.selected().cloned();
        let effect = self.picker.list_done(entries, listing.errors);
        self.sync_select(window, cx);
        if self.picker.selected() != before.as_ref() {
            cx.emit(DevicePanelEvent::Selected(self.picker.selected().cloned()));
        }
        self.apply(effect, window, cx);
        cx.notify();
    }

    pub(super) fn sync_select(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut seen = HashSet::new();
        let options: Vec<DeviceOption> = self
            .picker
            .entries()
            .iter()
            // The pool's own devices are not bases; several instances of one AVD are one choice.
            .filter(|e| !e.info.name.starts_with(hangar_core::device::CLONE_PREFIX))
            .filter(|e| seen.insert(e.key.value()))
            .map(|e| DeviceOption {
                value: e.key.value().into(),
                title: entry_title(e).into(),
                ready: e.ready,
            })
            .collect();
        let selected: Option<SharedString> = match self.workspace {
            Some(ws) => self.pool.read(cx).assignment(ws).map(|a| {
                DeviceKey {
                    platform: a.platform,
                    id: a.base.clone(),
                }
                .value()
                .into()
            }),
            None => self.picker.selected().map(|k| k.value().into()),
        };
        self.select.update(cx, |select, cx| {
            select.set_items(options, window, cx);
            match selected {
                Some(value) => select.set_selected_value(&value, window, cx),
                None => select.set_selected_index(None, window, cx),
            }
        });
    }

    /// Selects a device (picker or dev hook).
    pub fn select_device(&mut self, key: DeviceKey, window: &mut Window, cx: &mut Context<Self>) {
        let changed = self.picker.selected() != Some(&key);
        let effect = self.picker.select(key);
        if changed {
            self.sync_select(window, cx);
            cx.emit(DevicePanelEvent::Selected(self.picker.selected().cloned()));
        }
        self.apply(effect, window, cx);
        cx.notify();
    }

    /// Boots the shut-down device `key` in the background, then lists devices and opens it.
    pub fn boot_device(&mut self, key: DeviceKey, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.picker.entry_by_value(&key.value()).cloned() else {
            return;
        };
        if self.booting.is_some() || !key.platform.can_boot(&entry.info) {
            return;
        }
        tracing::info!(device = %key.id, name = %entry.info.name, "booting device");
        self.boot_error = None;
        let name: SharedString = entry.info.name.clone().into();
        let platform = key.platform;
        let stage = Arc::new(Mutex::new("booting".to_owned()));
        let reported = stage.clone();
        let task = cx.background_executor().spawn(async move {
            crate::adapters::device_catalog(platform)
                .boot_listed(&entry.info, &|s| {
                    if let Ok(mut stage) = reported.lock() {
                        *stage = s;
                    }
                })
                .map_err(anyhow::Error::from)
        });
        let task_key = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.on_booted(task_key, result, window, cx)
            });
        });
        // Redraws the stage and elapsed time; dropped with `Booting` when the boot ends.
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(BOOT_TICK).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        self.booting = Some(Booting {
            key,
            name,
            stage,
            started: Instant::now(),
            _task: task,
            _tick: tick,
        });
        cx.notify();
    }

    fn on_booted(
        &mut self,
        key: DeviceKey,
        result: anyhow::Result<hangar_devices::DeviceInfo>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.booting = None;
        match result {
            // The booted device keeps its key, so the listing below opens it if it is selected.
            Ok(info) => {
                tracing::info!(device = %info.udid, name = %info.name, "device booted");
            }
            Err(err) => {
                tracing::error!(device = %key.id, "failed to boot device: {err:#}");
                self.boot_error = Some((key, format!("{err:#}").into()));
            }
        }
        self.refresh(window, cx);
    }

    /// Shuts the open emulator down in the background; the panel closes it once it is off.
    pub fn shutdown_device(&mut self, cx: &mut Context<Self>) {
        if let Some(ws) = self.workspace {
            if self.shutting_down {
                return;
            }
            let Some(key) = self.pool.read(cx).key_for(ws) else {
                return;
            };
            self.shutting_down = true;
            self.input_status = None;
            let task = self.pool.update(cx, |pool, cx| pool.shutdown(&key, cx));
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |this, cx| {
                    this.shutting_down = false;
                    if let Err(err) = result {
                        this.input_status = Some(InputStatus {
                            text: format!("Shutdown failed: {err:#}").into(),
                            error: true,
                        });
                    }
                    cx.notify();
                });
            })
            .detach();
            cx.notify();
            return;
        }
        let Connection::Open { key, view, .. } = &self.connection else {
            return;
        };
        if self.shutting_down || !view.read(cx).session().capabilities().shutdown {
            return;
        }
        tracing::info!(device = %key.id, "shutting device down");
        self.shutting_down = true;
        self.input_status = None;
        view.read(cx).shutdown();
        cx.notify();
    }

    fn on_shutdown(
        &mut self,
        result: &Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.shutting_down = false;
        match result {
            Ok(()) => {
                if let Connection::Open { key, .. } = &self.connection {
                    let key = key.clone();
                    self.picker.closed(&key);
                }
                self.connection = Connection::None;
                self.input_status = None;
                self.refresh(window, cx);
            }
            Err(err) => {
                self.input_status = Some(InputStatus {
                    text: format!("Shutdown failed: {err}").into(),
                    error: true,
                });
            }
        }
        cx.notify();
    }

    pub(super) fn apply(&mut self, effect: Effect, window: &mut Window, cx: &mut Context<Self>) {
        if effect.close {
            if let Connection::Open { key, .. } | Connection::Opening { key, .. } = &self.connection
            {
                tracing::info!(device = %key.id, "closing device");
            }
            // Dropping the session only closes a channel: the device is released on its
            // own input thread.
            self.connection = Connection::None;
            self.input_status = None;
            self.shutting_down = false;
        }
        if let Some(entry) = effect.open {
            self.open(entry, window, cx);
        }
        cx.notify();
    }

    fn open(&mut self, entry: DeviceEntry, window: &mut Window, cx: &mut Context<Self>) {
        let DeviceEntry { key, info, .. } = entry;
        let name: SharedString = info.name.clone().into();
        tracing::info!(device = %key.id, name = %info.name, "opening device");
        let forced = self.forced_backend.clone();
        let platform = key.platform;
        let task = cx.background_executor().spawn(async move {
            let opened = crate::adapters::device_catalog(platform).open(info, forced.as_deref())?;
            let session = DeviceSession::start(Arc::from(opened.device))?;
            anyhow::Ok((session, opened.reason))
        });
        let task_key = key.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.on_opened(task_key, result, window, cx)
            });
        });
        self.connection = Connection::Opening {
            key,
            name,
            _task: task,
        };
        self.input_status = None;
    }

    fn on_opened(
        &mut self,
        key: DeviceKey,
        result: anyhow::Result<(DeviceSession, Option<BackendReason>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = match &self.connection {
            Connection::Opening { key: k, name, .. } if *k == key => name.clone(),
            // Superseded (closed or switched): the stale session is dropped here.
            _ => return,
        };
        match result {
            Ok((session, reason)) => {
                tracing::info!(
                    device = %key.id,
                    backend = session.backend(),
                    reason = ?reason,
                    "device opened"
                );
                let stats = self.stats;
                let view = cx.new(|cx| DeviceView::new(session, stats, cx));
                let events = self.subscribe_view(&view, window, cx);
                let (logs, logs_events) = self.new_logs(key.platform, &view, window, cx);
                self.connection = Connection::Open {
                    key,
                    view,
                    reason,
                    logs,
                    _events: events,
                    _logs_events: logs_events,
                };
            }
            Err(err) => {
                tracing::error!(device = %key.id, "failed to open device: {err:#}");
                self.picker.open_failed(&key);
                self.connection = Connection::Failed {
                    name,
                    error: format!("{err:#}").into(),
                };
            }
        }
        cx.notify();
    }

    pub(super) fn subscribe_view(
        &mut self,
        view: &Entity<DeviceView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            view,
            window,
            |this, _, event: &DeviceViewEvent, window, cx| {
                match event {
                    DeviceViewEvent::Input(result) => {
                        this.input_status = Some(input_status(result));
                    }
                    DeviceViewEvent::Text(result) => {
                        this.input_status = Some(InputStatus {
                            text: format!(
                                "Typing {} failed: {}",
                                plural_chars(result.chars),
                                result.error
                            )
                            .into(),
                            error: true,
                        });
                    }
                    // The pool owns workspace devices and reports their shutdown itself.
                    DeviceViewEvent::Shutdown(result) if this.workspace.is_some() => {
                        this.shutting_down = false;
                        if let Err(err) = result {
                            this.input_status = Some(InputStatus {
                                text: format!("Shutdown failed: {err}").into(),
                                error: true,
                            });
                        }
                    }
                    DeviceViewEvent::Shutdown(result) => {
                        this.on_shutdown(result, window, cx);
                    }
                    DeviceViewEvent::Rotate(orientation, result) => {
                        this.input_status = Some(InputStatus {
                            text: match result {
                                Ok(()) => format!("Rotated to {orientation:?}"),
                                Err(e) => format!("Rotate failed: {e}"),
                            }
                            .into(),
                            error: result.is_err(),
                        });
                    }
                }
                cx.notify();
            },
        )
    }
}
