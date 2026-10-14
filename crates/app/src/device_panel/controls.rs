//! Logs, stats and hardware input of the open device.

use gpui_kit::*;
use hangar_devices::Input;

use crate::device_view::DeviceView;

use super::logs::{LogsEvent, LogsPane, LogsSettings};
use super::platform::Platform;
use super::{DevicePanel, DevicePanelEvent};

impl DevicePanel {
    /// Applies saved logs pane settings; call before the panel is first shown.
    pub fn restore_logs(&mut self, settings: LogsSettings) {
        self.logs_settings = settings;
    }

    /// Opens or closes the logs pane (now, or when the device opens).
    pub fn set_logs_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.logs_settings.open = open;
        if let Some(logs) = self.logs().cloned() {
            logs.update(cx, |logs, cx| logs.set_open(open, cx));
        }
    }

    pub(super) fn new_logs(
        &self,
        platform: Platform,
        view: &Entity<DeviceView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<LogsPane>, Subscription) {
        let source = view.read(cx).handle();
        let history = platform == Platform::Android;
        let settings = self.logs_settings;
        let logs = cx.new(|cx| LogsPane::new(source, history, settings, window, cx));
        let sub = cx.subscribe(&logs, |this, _, event: &LogsEvent, cx| {
            let LogsEvent::Settings(settings) = event;
            this.logs_settings = *settings;
            cx.emit(DevicePanelEvent::Logs(*settings));
        });
        (logs, sub)
    }

    pub fn stats(&self) -> bool {
        self.stats
    }

    pub fn set_stats(&mut self, stats: bool, cx: &mut Context<Self>) {
        if stats != self.stats {
            self.set_stats_inner(stats, cx);
            cx.emit(DevicePanelEvent::Stats(stats));
        }
    }

    pub(super) fn set_stats_inner(&mut self, stats: bool, cx: &mut Context<Self>) {
        self.stats = stats;
        if let Some(view) = self.view().cloned() {
            view.update(cx, |view, cx| view.set_stats(stats, cx));
        }
        cx.notify();
    }

    /// Sends input to the open device. Returns `false` when no device is open.
    pub fn send(&mut self, input: Input, cx: &mut Context<Self>) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
        view.read(cx).send(input);
        self.input_status = None;
        cx.notify();
        true
    }

    /// Types text into the open device. Returns `false` when no device is open.
    pub fn send_text(&mut self, text: String, cx: &mut Context<Self>) -> bool {
        let Some(view) = self.view() else {
            return false;
        };
        view.read(cx).send_text(text);
        self.input_status = None;
        cx.notify();
        true
    }

    /// A workspace device is on screen: its screenshot can go to the workspace's agent.
    pub(super) fn can_screenshot(&self) -> bool {
        self.workspace.is_some() && self.attached.is_some()
    }

    /// Asks the shell to save a screenshot of the workspace's device for its agent.
    pub fn screenshot(&mut self, cx: &mut Context<Self>) {
        if let Some(ws) = self.workspace.filter(|_| self.can_screenshot()) {
            cx.emit(DevicePanelEvent::Screenshot(ws));
        }
    }

    /// The Home button (⇧⌘H), when the shown device takes it.
    pub(super) fn press_home(&mut self, cx: &mut Context<Self>) {
        let accepts = self
            .view()
            .is_some_and(|v| v.read(cx).session().capabilities().accepts(&Input::Home));
        if accepts {
            self.send(Input::Home, cx);
        }
    }

    /// The error of the last input, if it failed.
    pub fn input_error(&self) -> Option<SharedString> {
        self.input_status
            .as_ref()
            .filter(|s| s.error)
            .map(|s| s.text.clone())
    }
}
