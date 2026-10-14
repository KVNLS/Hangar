//! The pool's blocking device I/O that is more than one port call. Call it on the background
//! executor only.

use std::sync::Arc;

use hangar_core::device::{BackendReason, DeviceInfo, Platform, RunningSet};

use crate::adapters;
use crate::device_view::DeviceSession;

/// Opens a running device on its own input thread.
pub fn open(
    platform: Platform,
    info: DeviceInfo,
    forced: Option<&str>,
) -> anyhow::Result<(DeviceSession, Option<BackendReason>)> {
    let opened = adapters::device_catalog(platform).open(info, forced)?;
    Ok((
        DeviceSession::start(Arc::from(opened.device))?,
        opened.reason,
    ))
}

/// What runs right now on every platform; a platform that cannot be asked counts as none.
pub fn running() -> RunningSet {
    let mut set = RunningSet::default();
    for platform in Platform::ALL {
        match adapters::device_catalog(platform).running() {
            Ok(instances) => set.add(platform, instances),
            Err(err) => {
                tracing::debug!(target: "device_pool", platform = platform.label(), "running devices not listed: {:#}", anyhow::Error::from(err));
            }
        }
    }
    set
}

/// Platforms the apps in `root` build for, in detection order.
pub fn stack_platforms(root: &std::path::Path) -> Vec<Platform> {
    let stacks = hangar_runner::detect(root);
    Platform::ALL
        .into_iter()
        .filter(|p| stacks.iter().any(|s| s.targets(*p)))
        .collect()
}
