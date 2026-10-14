use gpui_kit::component::input::Position;
use gpui_kit::*;
use hangar_agents::acp::protocol::{AvailableCommand, PermissionKind};

use super::rows::touch;
use super::{CANCEL_GRACE, ChatEscape, ChatEvent, ChatView, Item, Outcome, WorkspaceDiff};

impl ChatView {
    /// Sends `text` as the next prompt. Fails while a turn runs or after the agent exited.
    pub fn send(&mut self, text: &str, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(());
        }
        if self.exited {
            anyhow::bail!("{} is no longer running", self.agent_name);
        }
        if self.running {
            anyhow::bail!(
                "{} is still working on the previous message",
                self.agent_name
            );
        }
        self.session.prompt(text)?;
        self.turn_start = self.items.len();
        self.items.push(Item::User(text.to_owned().into()));
        self.running = true;
        self.turn_seq += 1;
        self.turn_done = false;
        self.failed = false;
        self.plan = None;
        self.follow_tail();
        cx.emit(ChatEvent::StateChanged);
        cx.notify();
        Ok(())
    }

    /// Puts `text` in the composer after any draft and focuses it, without sending.
    pub fn prefill(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |state, cx| {
            let draft = state.value();
            let value = match draft.trim_end() {
                "" => text.to_owned(),
                draft => format!("{draft}\n{text}"),
            };
            let last_line = value.split('\n').count().saturating_sub(1);
            state.set_value(value, window, cx);
            state.set_cursor_position(
                Position::new(u32::try_from(last_line).unwrap_or(u32::MAX), u32::MAX),
                window,
                cx,
            );
        });
    }

    /// The workspace's live diff, summarized in the card after a finished turn.
    pub fn set_diff(&mut self, diff: Entity<WorkspaceDiff>, cx: &mut Context<Self>) {
        if self.diff.as_ref().is_some_and(|(d, _)| *d == diff) {
            return;
        }
        let observe = cx.observe(&diff, |_, _, cx| cx.notify());
        self.diff = Some((diff, observe));
        cx.notify();
    }

    pub(super) fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |state, cx| state.focus(window, cx));
    }

    /// Asks the agent to stop the running turn; open permission requests are cancelled.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.running || self.exited {
            return;
        }
        if let Err(err) = self.session.cancel() {
            self.notice(true, format!("Could not stop the agent: {err}"));
        }
        // An agent that ignores the cancel would keep the turn running for good.
        let turn = self.turn_seq;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(CANCEL_GRACE).await;
            let _ = this.update(cx, |this, cx| {
                if this.running && !this.exited && this.turn_seq == turn {
                    this.session.abandon_turn();
                    this.running = false;
                    this.notice(true, "Stopped: the agent did not answer the cancel");
                    cx.emit(ChatEvent::StateChanged);
                    cx.notify();
                }
            });
        })
        .detach();
        for (i, item) in self.items.iter_mut().enumerate() {
            if let Item::Permission {
                outcome: outcome @ None,
                ..
            } = item
            {
                *outcome = Some(Outcome {
                    allowed: false,
                    label: "Cancelled".into(),
                });
                touch(&mut self.dirty, i);
            }
        }
        cx.emit(ChatEvent::StateChanged);
        cx.notify();
    }

    pub(super) fn on_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() {
            // Enter on an empty composer approves the pending request (Allow once).
            if let Some(index) = self.pending_permission() {
                let option = self.first_option(index, |k| k == PermissionKind::AllowOnce);
                if let Some(option) = option {
                    self.answer(index, Some(option), cx);
                }
            }
            return;
        }
        match self.send(&text, cx) {
            Ok(()) => self
                .composer
                .update(cx, |state, cx| state.set_value("", window, cx)),
            Err(err) => {
                self.notice(true, format!("{err:#}"));
                cx.notify();
            }
        }
    }

    pub(super) fn on_escape(&mut self, _: &ChatEscape, _: &mut Window, cx: &mut Context<Self>) {
        if self.focus.is_some() {
            self.focus_subagent(None, cx);
        } else if let Some(index) = self.pending_permission() {
            let option = self.first_option(index, |k| k == PermissionKind::RejectOnce);
            self.answer(index, option, cx);
        } else if self.running {
            self.stop(cx);
        } else {
            cx.propagate();
        }
    }

    /// Commands matching the composer while it holds only `/prefix`.
    pub(super) fn command_matches(&self, cx: &App) -> Vec<AvailableCommand> {
        let text = self.composer.read(cx).value();
        let Some(prefix) = text.strip_prefix('/') else {
            return Vec::new();
        };
        if prefix.contains(char::is_whitespace) {
            return Vec::new();
        }
        self.commands
            .iter()
            .filter(|c| c.name.starts_with(prefix))
            .take(8)
            .cloned()
            .collect()
    }

    pub(super) fn pick_command(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |state, cx| {
            state.set_value(format!("/{name} "), window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }
}
