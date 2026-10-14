//! Device operations on any workspace's device, for the MCP host (and dev hooks). Call them on
//! the GPUI thread; each returns a task that resolves off it. Every call that touches the device
//! holds its lease while it runs (in single-device mode it waits for its turn, up to
//! [`crate::device_pool::policy::LEASE_TIMEOUT`]) and boots/opens the device when needed.
//! [`HangarApp::device_status`] only reports and never boots.

use std::time::Duration;

use hangar_devices::{Frame, Input, LogEntry, LogFilter, LogLevel, Orientation};

use super::*;
use crate::device_panel::Platform;

/// How long [`HangarApp::latest_frame`] waits for a first frame of a device that just opened.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(15);
/// How long [`HangarApp::recent_logs`] collects entries.
const LOG_WINDOW: Duration = Duration::from_millis(1500);

/// What [`HangarApp::device_status`] reports.
#[derive(Clone, Debug, PartialEq)]
pub struct DeviceStatusInfo {
    pub platform: Platform,
    pub name: String,
    /// Simulator UDID or adb serial when open; else the base device's id.
    pub id: String,
    /// Open in the pool (booted and connected).
    pub booted: bool,
    /// Width and height in points (dp on Android), as currently oriented.
    pub screen_pt: Option<(f64, f64)>,
    pub scale: Option<f64>,
    pub orientation: Option<Orientation>,
}

/// One frame and the point size of the screen it shows (as oriented).
#[derive(Clone, Debug)]
pub struct DeviceFrame {
    pub frame: Frame,
    pub width_pt: f64,
    pub height_pt: f64,
}

impl HangarApp {
    fn acquire_device(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<crate::device_pool::Acquired>> {
        if !self.work.projects.is_active(ws) {
            return Task::ready(Err(anyhow!("unknown or archived workspace")));
        }
        self.track_workspace(ws, cx);
        self.device_pool.update(cx, |pool, cx| pool.acquire(ws, cx))
    }

    /// The workspace's device: platform, name, id, whether it is open, screen size in points
    /// (oriented), scale and orientation. Does not boot it and takes no lease.
    pub fn device_status(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<DeviceStatusInfo>> {
        if !self.work.projects.is_active(ws) {
            return Task::ready(Err(anyhow!("unknown or archived workspace")));
        }
        self.track_workspace(ws, cx);
        let assignment = self
            .device_pool
            .update(cx, |pool, cx| pool.assignment_for(ws, cx));
        let pool = self.device_pool.downgrade();
        cx.spawn(async move |_, cx| {
            let a = assignment.await?;
            let (session, name) = pool.read_with(cx, |pool, _| {
                (
                    pool.session(ws).cloned(),
                    pool.device_name(ws).map(str::to_owned),
                )
            })?;
            let Some(session) = session else {
                return Ok(DeviceStatusInfo {
                    platform: a.platform,
                    name: name.unwrap_or_else(|| a.base.clone()),
                    id: a.own.clone().unwrap_or(a.base),
                    booted: false,
                    screen_pt: None,
                    scale: None,
                    orientation: None,
                });
            };
            let info = session.info().clone();
            let frame_shape = session
                .frames()
                .borrow()
                .as_ref()
                .map(|f| (f.width, f.height));
            let handle = session.handle();
            let reported = cx
                .background_executor()
                .spawn(async move { handle.orientation() })
                .await;
            let orientation = match reported {
                Ok(o) => Some(o),
                // iOS reports no orientation: the frame's shape tells portrait from landscape.
                Err(_) => frame_shape.map(|(w, h)| {
                    if w > h {
                        Orientation::LandscapeLeft
                    } else {
                        Orientation::Portrait
                    }
                }),
            };
            let landscape = orientation.is_some_and(Orientation::is_landscape);
            let screen = info.screen.map(|s| s.oriented(landscape));
            Ok(DeviceStatusInfo {
                platform: a.platform,
                name: info.name,
                id: info.udid,
                booted: true,
                screen_pt: screen
                    .map(|s| (s.width_pt(), s.height_pt()))
                    .or_else(|| crate::device_view::geometry::screen_points(None, frame_shape)),
                scale: screen.map(|s| s.scale),
                orientation,
            })
        })
    }

    /// The latest frame of the workspace's device (opening it first if needed) with the point
    /// size of the screen as the frame shows it.
    pub fn latest_frame(
        &mut self,
        ws: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<DeviceFrame>> {
        let acquire = self.acquire_device(ws, cx);
        cx.spawn(async move |_, cx| {
            let acquired = acquire.await?;
            let mut frames = acquired.session.frames();
            let deadline = std::time::Instant::now() + FIRST_FRAME_TIMEOUT;
            let frame = loop {
                if let Some(frame) = frames.borrow_and_update().clone() {
                    break frame;
                }
                if std::time::Instant::now() >= deadline {
                    return Err(anyhow!(
                        "{} sent no frame within {} s",
                        acquired.session.info().name,
                        FIRST_FRAME_TIMEOUT.as_secs()
                    ));
                }
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
            };
            let (width_pt, height_pt) = crate::device_view::geometry::screen_points(
                acquired.session.info().screen,
                Some((frame.width, frame.height)),
            )
            .ok_or_else(|| anyhow!("the frame is empty"))?;
            Ok(DeviceFrame {
                frame,
                width_pt,
                height_pt,
            })
        })
    }

    /// Sends one input (device points, as currently oriented) and resolves once the device
    /// took it.
    pub fn send_input(
        &mut self,
        ws: WorkspaceId,
        input: Input,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        let acquire = self.acquire_device(ws, cx);
        cx.spawn(async move |_, _| {
            let acquired = acquire.await?;
            if let Some(missing) = acquired.session.capabilities().missing_for(&input) {
                return Err(hangar_devices::Error::Unsupported(missing).into());
            }
            acquired
                .session
                .send_acked(input)
                .await
                .map_err(|_| anyhow!("the device closed"))?
                .map_err(anyhow::Error::msg)
        })
    }

    /// Types text into the focused field of the workspace's device.
    pub fn send_text(
        &mut self,
        ws: WorkspaceId,
        text: String,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        let acquire = self.acquire_device(ws, cx);
        cx.spawn(async move |_, _| {
            let acquired = acquire.await?;
            if !acquired.session.capabilities().text {
                return Err(hangar_devices::Error::Unsupported("text input on this device").into());
            }
            acquired
                .session
                .send_text_acked(text)
                .await
                .map_err(|_| anyhow!("the device closed"))?
                .map_err(anyhow::Error::msg)
        })
    }

    /// The device's most recent log lines (at most `lines`), at `min_level` or above, of
    /// `process` (Android: package name of a running app; iOS: process name or subsystem
    /// prefix). Android includes the device's recent backlog; iOS has none, so only what is
    /// logged during the ~1.5 s capture comes back.
    pub fn recent_logs(
        &mut self,
        ws: WorkspaceId,
        lines: usize,
        min_level: Option<LogLevel>,
        process: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<String>> {
        let entries = self.recent_log_entries(ws, lines, min_level, process, cx);
        cx.background_executor().spawn(async move {
            Ok(entries
                .await?
                .iter()
                .map(format_log)
                .collect::<Vec<_>>()
                .join("\n"))
        })
    }

    /// [`Self::recent_logs`] as entries, oldest first.
    pub fn recent_log_entries(
        &mut self,
        ws: WorkspaceId,
        lines: usize,
        min_level: Option<LogLevel>,
        process: Option<String>,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<Vec<LogEntry>>> {
        let acquire = self.acquire_device(ws, cx);
        cx.spawn(async move |_, cx| {
            let acquired = acquire.await?;
            let filter = LogFilter {
                min_level,
                process,
                history: acquired.platform == Platform::Android,
                capacity: lines.max(1) * 4,
            };
            let handle = acquired.session.handle();
            let mut entries = cx
                .background_executor()
                .spawn(async move {
                    let stream = handle.logs(&filter)?;
                    let started = std::time::Instant::now();
                    let mut entries: Vec<LogEntry> = Vec::new();
                    while started.elapsed() < LOG_WINDOW && !stream.is_closed() {
                        if let Some(entry) = stream.recv_timeout(Duration::from_millis(100)) {
                            entries.push(entry);
                        }
                    }
                    entries.extend(stream.drain());
                    let failed = stream.error().filter(|_| entries.is_empty());
                    // Dropping joins the reader, which can wait on a helper process that
                    // outlives the killed child (`simctl spawn`): never on this task.
                    let _ = std::thread::Builder::new()
                        .name("device-logs-release".into())
                        .spawn(move || drop(stream));
                    match failed {
                        Some(err) => Err(anyhow!("log stream failed: {err}")),
                        None => anyhow::Ok(entries),
                    }
                })
                .await?;
            drop(acquired);
            let start = entries.len().saturating_sub(lines);
            Ok(entries.split_off(start))
        })
    }

    /// Launches an installed app by bundle id (iOS) or package name (Android).
    pub fn launch_app(
        &mut self,
        ws: WorkspaceId,
        app_id: String,
        cx: &mut Context<Self>,
    ) -> Task<anyhow::Result<()>> {
        let acquire = self.acquire_device(ws, cx);
        cx.spawn(async move |_, cx| {
            let acquired = acquire.await?;
            let handle = acquired.session.handle();
            cx.background_executor()
                .spawn(async move { handle.launch(&app_id) })
                .await?;
            drop(acquired);
            Ok(())
        })
    }
}

/// `12:00:01.123 E MyApp: message`.
fn format_log(e: &LogEntry) -> String {
    let tag = if e.process.is_empty() || e.process == e.tag {
        e.tag.clone()
    } else {
        format!("{}/{}", e.process, e.tag)
    };
    let level = e
        .level
        .as_str()
        .chars()
        .next()
        .unwrap_or('?')
        .to_ascii_uppercase();
    format!("{} {level} {tag}: {}", e.timestamp, e.message)
}

#[cfg(test)]
mod tests {
    use super::format_log;
    use hangar_devices::{LogEntry, LogLevel};

    #[test]
    fn log_lines_show_level_tag_and_process() {
        let mut e = LogEntry::new(LogLevel::Error, "boom");
        e.timestamp = "10-05 12:00:01.123".into();
        e.tag = "MyApp".into();
        assert_eq!(format_log(&e), "10-05 12:00:01.123 E MyApp: boom");
        e.process = "Runner".into();
        assert_eq!(format_log(&e), "10-05 12:00:01.123 E Runner/MyApp: boom");
    }
}
