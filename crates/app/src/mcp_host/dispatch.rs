//! Maps each tool call to the request the GPUI side answers, and the answer to the tool's output.

use std::time::Duration;

use crate::adapters::devices::LogEntry;
use hangar_core::WorkspaceId;
use hangar_core::device::Input;
use hangar_mcp::{MAX_SIDE, Sharing, Tool, ToolError, ToolOutput, encode_screenshot};

use super::{McpHost, McpRequest};

/// Interval between finger moves of a swipe.
const SWIPE_STEP_MS: u64 = 16;
const MAX_SWIPE_MOVES: u64 = 120;

/// A swipe as touch events, each with the delay to wait before sending it: down at `from`,
/// evenly spaced moves over `duration_ms` ending at `to`, then up at `to`.
fn swipe_inputs(from: (f64, f64), to: (f64, f64), duration_ms: u64) -> Vec<(Input, Duration)> {
    let moves = (duration_ms / SWIPE_STEP_MS).clamp(1, MAX_SWIPE_MOVES);
    let delay = Duration::from_millis(duration_ms) / moves as u32;
    let mut out = Vec::with_capacity(moves as usize + 2);
    out.push((
        Input::TouchDown {
            x: from.0,
            y: from.1,
        },
        Duration::ZERO,
    ));
    for i in 1..=moves {
        let t = i as f64 / moves as f64;
        out.push((
            Input::TouchMove {
                x: from.0 + (to.0 - from.0) * t,
                y: from.1 + (to.1 - from.1) * t,
            },
            delay,
        ));
    }
    out.push((Input::TouchUp { x: to.0, y: to.1 }, Duration::ZERO));
    out
}

/// Log entries as plain lines for the model.
fn format_logs(entries: &[LogEntry]) -> String {
    if entries.is_empty() {
        return "No log lines matched.".to_owned();
    }
    let mut out = String::new();
    for e in entries {
        if !e.timestamp.is_empty() {
            out.push_str(&e.timestamp);
            out.push(' ');
        }
        out.push_str(e.level.as_str());
        let mut source = e.tag.clone();
        if !e.category.is_empty() {
            source = format!("{source}/{}", e.category);
        }
        if !e.process.is_empty() {
            source = if source.is_empty() {
                e.process.clone()
            } else {
                format!("{} {source}", e.process)
            };
        }
        if let Some(pid) = e.pid {
            source = format!("{source}[{pid}]");
        }
        if !source.is_empty() {
            out.push(' ');
            out.push_str(&source);
        }
        out.push_str(": ");
        out.push_str(&e.message);
        out.push('\n');
    }
    out
}

impl McpHost {
    fn input(&self, ws: WorkspaceId, input: Input) -> Result<(), ToolError> {
        self.ask("input", self.timeouts.call, |reply| McpRequest::Input {
            ws,
            input,
            reply,
        })
    }

    fn swipe(
        &self,
        ws: WorkspaceId,
        from: (f64, f64),
        to: (f64, f64),
        duration_ms: u64,
    ) -> Result<(), ToolError> {
        let mut last = from;
        for (input, delay) in swipe_inputs(from, to, duration_ms) {
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            if let Err(err) = self.input(ws, input) {
                if !matches!(input, Input::TouchDown { .. } | Input::TouchUp { .. }) {
                    // Don't leave a finger on the screen.
                    let _ = self.input(
                        ws,
                        Input::TouchUp {
                            x: last.0,
                            y: last.1,
                        },
                    );
                }
                return Err(err);
            }
            if let Input::TouchMove { x, y } = input {
                last = (x, y);
            }
        }
        Ok(())
    }

    /// Puts the workspace's pending notices in `sharing` (`device_info`, `device_acquire`).
    fn sharing_notice(&self, ws: WorkspaceId, sharing: Option<&mut Sharing>) {
        if let Some(sharing) = sharing {
            sharing.notice = self.notices.take(ws);
        }
    }

    pub(super) fn run(&self, ws: WorkspaceId, tool: Tool) -> Result<ToolOutput, ToolError> {
        let call = self.timeouts.call;
        match tool {
            Tool::DeviceInfo => {
                let mut status = self.ask("device_info", call, |reply| McpRequest::Status {
                    ws,
                    reply,
                })?;
                self.sharing_notice(ws, status.sharing.as_mut());
                Ok(ToolOutput::device_status(&status))
            }
            Tool::Screenshot => {
                let screen =
                    self.ask("screenshot", call, |reply| McpRequest::Frame { ws, reply })?;
                let shot = encode_screenshot(&screen.frame, MAX_SIDE)
                    .map_err(|e| ToolError::Failed(format!("screenshot: {e}")))?;
                Ok(ToolOutput::screenshot(
                    shot,
                    screen.width_pt,
                    screen.height_pt,
                ))
            }
            Tool::Tap { x, y } => {
                self.input(ws, Input::Tap { x, y })?;
                Ok(ToolOutput::text(format!("Tapped at ({x}, {y}).")))
            }
            Tool::Swipe {
                from_x,
                from_y,
                to_x,
                to_y,
                duration_ms,
            } => {
                self.swipe(ws, (from_x, from_y), (to_x, to_y), duration_ms)?;
                Ok(ToolOutput::text(format!(
                    "Swiped from ({from_x}, {from_y}) to ({to_x}, {to_y}) in {duration_ms} ms."
                )))
            }
            Tool::TypeText { text } => {
                let count = text.chars().count();
                self.ask("type_text", call, |reply| McpRequest::Text {
                    ws,
                    text,
                    reply,
                })?;
                Ok(ToolOutput::text(format!("Typed {count} characters.")))
            }
            Tool::Press(press) => {
                self.input(ws, press.input())?;
                Ok(ToolOutput::text("Pressed."))
            }
            Tool::Logs {
                lines,
                min_level,
                process,
            } => {
                let entries = self.ask("logs", call, |reply| McpRequest::Logs {
                    ws,
                    lines,
                    min_level,
                    process,
                    reply,
                })?;
                Ok(ToolOutput::text(format_logs(&entries)))
            }
            Tool::BuildAndRun => {
                let summary = self.ask("build_and_run", self.timeouts.build_and_run, |reply| {
                    McpRequest::BuildAndRun { ws, reply }
                })?;
                Ok(ToolOutput::text(summary))
            }
            Tool::DeviceAcquire { wait_s, platform } => {
                let wait = Duration::from_secs(wait_s);
                let mut got =
                    self.ask("device_acquire", call + wait, |reply| McpRequest::Acquire {
                        ws,
                        wait,
                        platform,
                        reply,
                    })?;
                self.sharing_notice(ws, Some(&mut got.sharing));
                Ok(ToolOutput::device_acquired(got.granted, &got.sharing))
            }
            Tool::DeviceRelease => {
                let released = self.ask("device_release", call, |reply| McpRequest::Release {
                    ws,
                    reply,
                })?;
                Ok(ToolOutput::device_released(
                    released.had_waiters,
                    released.next.as_deref(),
                    released.left_line,
                ))
            }
            Tool::LaunchApp { app_id } => {
                let label = app_id.clone();
                self.ask("launch_app", call, |reply| McpRequest::Launch {
                    ws,
                    app_id,
                    reply,
                })?;
                Ok(ToolOutput::text(format!("Launched {label}.")))
            }
            Tool::AskUser { question, choices } => {
                let answer = self.ask("ask_user", self.timeouts.ask, |reply| McpRequest::Ask {
                    ws,
                    question,
                    choices,
                    reply,
                })?;
                Ok(ToolOutput::answer(&answer))
            }
            Tool::WorkspacesList => {
                let children = self.ask("workspaces_list", call, |reply| McpRequest::Children {
                    ws,
                    reply,
                })?;
                Ok(ToolOutput::workspaces(&children))
            }
            Tool::WorkspaceCreate { task, name, agent } => {
                let created = self.ask("workspace_create", self.timeouts.create, |reply| {
                    McpRequest::CreateChild {
                        ws,
                        task,
                        name,
                        agent,
                        reply,
                    }
                })?;
                Ok(ToolOutput::created(&created))
            }
            Tool::AgentPrompt { workspace, text } => {
                let name = self.ask("agent_prompt", call, |reply| McpRequest::PromptChild {
                    ws,
                    child: workspace,
                    text,
                    reply,
                })?;
                Ok(ToolOutput::text(format!("Sent to the agent of “{name}”.")))
            }
            Tool::AgentTranscript { workspace, last } => {
                let (name, entries) =
                    self.ask("agent_transcript", call, |reply| McpRequest::Transcript {
                        ws,
                        child: workspace,
                        last,
                        reply,
                    })?;
                Ok(ToolOutput::transcript(&name, &entries))
            }
        }
    }
}

#[cfg(test)]
mod tests;
