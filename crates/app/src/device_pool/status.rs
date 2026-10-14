//! What the panel, Settings and the status bar show about the pool.

use std::time::Instant;

use hangar_core::WorkspaceId;

use crate::device_panel::DeviceKey;
use crate::device_view::DeviceSession;

use super::{DevicePool, DeviceRow, DeviceStatus};

impl DevicePool {
    /// What the panel shows for the workspace.
    pub fn status(&self, ws: WorkspaceId) -> DeviceStatus<'_> {
        self.machine.status(ws, Instant::now())
    }

    /// Another workspace holding the lease on `ws`'s device (a run or tool call): its id and
    /// name.
    pub fn held_by_other(&self, ws: WorkspaceId) -> Option<(WorkspaceId, &str)> {
        self.machine.held_by_other(ws)
    }

    /// Whether the lease on the base device `key` is held by a workspace other than `ws`.
    pub fn base_held_by_other(&self, key: &DeviceKey, ws: WorkspaceId) -> bool {
        self.machine.base_held_by_other(key, ws)
    }

    /// The open device of the workspace, if any (no boot).
    pub fn session(&self, ws: WorkspaceId) -> Option<&DeviceSession> {
        self.machine.session(ws)
    }

    /// Every device the pool knows, for Settings › Devices.
    pub fn rows(&self) -> Vec<DeviceRow> {
        self.machine.rows()
    }

    /// Counts of booted pool devices, for the status bar and Settings.
    pub fn booted_count(&self) -> usize {
        self.machine.booted_count()
    }
}
