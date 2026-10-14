use gpui_kit::component::text::TextViewState;
use gpui_kit::*;
use hangar_agents::acp::AcpEvent;
use hangar_agents::acp::approval::{self};
use hangar_agents::acp::protocol::{SessionUpdate, StopReason, ToolCall};

use super::rows::touch;
use super::{ChatEvent, ChatView, Item, Outcome, cap};

impl ChatView {
    pub(super) fn drain(&mut self, cx: &mut Context<Self>) {
        let before = (self.turn(), self.usage.clone());
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            changed = true;
            self.apply(event, cx);
        }
        if changed {
            if before.0 != self.turn() || before.1 != self.usage {
                cx.emit(ChatEvent::StateChanged);
            }
            cx.notify();
        }
    }

    pub(super) fn apply(&mut self, event: AcpEvent, cx: &mut Context<Self>) {
        match event {
            AcpEvent::Update(update) => self.apply_update(update, cx),
            AcpEvent::PermissionRequest(request) => {
                let (title, kind) = match self.tools.get(&request.tool_call.id) {
                    Some(&i) => {
                        if let Some(Item::Tool(call)) = self.items.get_mut(i) {
                            let link = linkage(call);
                            call.apply(&request.tool_call);
                            self.relink |= linkage(call) != link;
                            touch(&mut self.dirty, i);
                            (Some(call.title.clone()), Some(call.kind.clone()))
                        } else {
                            (None, None)
                        }
                    }
                    None => (None, None),
                };
                let kind = request.tool_call.kind.clone().or(kind);
                let auto = approval::auto_allow(self.approval, kind.as_deref(), &request.options)
                    .map(str::to_owned);
                let title = title
                    .or_else(|| request.tool_call.title.clone())
                    .unwrap_or_else(|| "use a tool".to_owned());
                self.items.push(Item::Permission {
                    request,
                    title: title.into(),
                    outcome: None,
                });
                self.follow_tail();
                if let Some(option) = auto {
                    let index = self.items.len() - 1;
                    self.answer(index, Some(option), cx);
                    if let Some(Item::Permission {
                        outcome: Some(outcome),
                        ..
                    }) = self.items.get_mut(index)
                    {
                        outcome.label = "Allowed automatically (reads)".into();
                        touch(&mut self.dirty, index);
                    }
                }
            }
            AcpEvent::TurnEnded { reason, usage } => {
                self.running = false;
                self.turn_ended = true;
                self.turn_usage = usage;
                self.turn_done = reason == StopReason::EndTurn;
                let note = match reason {
                    StopReason::EndTurn => None,
                    StopReason::Cancelled => Some("Stopped"),
                    StopReason::Refusal => Some("The agent declined to continue"),
                    StopReason::MaxTokens => Some("Stopped: the reply hit the token limit"),
                    StopReason::MaxTurnRequests => Some("Stopped: too many steps in one turn"),
                };
                if let Some(note) = note {
                    self.notice(false, note);
                }
            }
            AcpEvent::TurnFailed(message) => {
                self.running = false;
                self.failed = true;
                self.notice(true, message);
            }
            AcpEvent::ProtocolError(message) => {
                tracing::warn!(agent = %self.agent_name, "acp: {message}");
                self.notice(true, format!("Protocol error: {message}"));
            }
            AcpEvent::Exited { code, stderr_tail } => {
                self.running = false;
                self.exited = true;
                for (i, item) in self.items.iter_mut().enumerate() {
                    if let Item::Permission {
                        outcome: outcome @ None,
                        ..
                    } = item
                    {
                        *outcome = Some(Outcome {
                            allowed: false,
                            label: "Agent exited".into(),
                        });
                        touch(&mut self.dirty, i);
                    }
                }
                let text = match code {
                    Some(0) => format!("{} exited", self.agent_name),
                    Some(code) => format!("{} exited with code {code}", self.agent_name),
                    None => format!("{} was stopped", self.agent_name),
                };
                self.notice(code.is_some_and(|c| c != 0), text);
                cx.emit(ChatEvent::Exited { code, stderr_tail });
            }
        }
    }

    pub(super) fn apply_update(&mut self, update: SessionUpdate, cx: &mut Context<Self>) {
        match update {
            // Parallel subagents interleave: a chunk continues the latest text of its sender.
            SessionUpdate::AgentMessage {
                text: chunk,
                parent,
            } => match self.open_text(parent.as_deref(), false) {
                Some(i) => {
                    if let Some(Item::Agent {
                        text, view, hidden, ..
                    }) = self.items.get_mut(i)
                    {
                        let shown = cap::shown_part(text.len() - *hidden, &chunk);
                        if shown > 0 {
                            view.update(cx, |state, cx| state.push_str(&chunk[..shown], cx));
                        }
                        *hidden += chunk.len() - shown;
                        text.push_str(&chunk);
                        touch(&mut self.dirty, i);
                    }
                }
                None => {
                    let shown = cap::shown_part(0, &chunk);
                    let view = cx.new(|cx| TextViewState::markdown(&chunk[..shown], cx));
                    self.items.push(Item::Agent {
                        hidden: chunk.len() - shown,
                        text: chunk,
                        view,
                        parent,
                    });
                }
            },
            SessionUpdate::AgentThought {
                text: chunk,
                parent,
            } => match self.open_text(parent.as_deref(), true) {
                Some(i) => {
                    if let Some(Item::Thought { text, .. }) = self.items.get_mut(i) {
                        text.push_str(&chunk);
                        touch(&mut self.dirty, i);
                    }
                }
                None => self.items.push(Item::Thought {
                    text: chunk,
                    open: false,
                    parent,
                }),
            },
            SessionUpdate::UserMessage(chunk) => {
                if let Some(Item::User(text)) = self.items.last_mut() {
                    *text = format!("{text}{chunk}").into();
                    touch(&mut self.dirty, self.items.len() - 1);
                } else {
                    self.items.push(Item::User(chunk.into()));
                }
            }
            SessionUpdate::ToolCall(call) => match self.tools.get(&call.id) {
                Some(&i) => {
                    if let Some(Item::Tool(old)) = self.items.get(i) {
                        self.relink |= linkage(old) != linkage(&call);
                    }
                    self.items[i] = Item::Tool(call);
                    touch(&mut self.dirty, i);
                }
                None => {
                    self.tools.insert(call.id.clone(), self.items.len());
                    self.items.push(Item::Tool(call));
                    self.relink = true;
                }
            },
            SessionUpdate::ToolCallUpdate(update) => match self.tools.get(&update.id) {
                Some(&i) => {
                    if let Some(Item::Tool(call)) = self.items.get_mut(i) {
                        let link = linkage(call);
                        call.apply(&update);
                        self.relink |= linkage(call) != link;
                        touch(&mut self.dirty, i);
                    }
                }
                None => {
                    self.tools.insert(update.id.clone(), self.items.len());
                    self.items.push(Item::Tool(ToolCall::from_update(&update)));
                    self.relink = true;
                }
            },
            SessionUpdate::Plan(entries) => match self.plan {
                Some(i) => {
                    self.items[i] = Item::Plan(entries);
                    touch(&mut self.dirty, i);
                }
                None => {
                    self.plan = Some(self.items.len());
                    self.items.push(Item::Plan(entries));
                }
            },
            SessionUpdate::CurrentMode(mode) => self.set_mode(Some(mode)),
            SessionUpdate::ConfigOptions(list) => self.set_config(list),
            SessionUpdate::AvailableCommands(commands) => self.commands = commands,
            SessionUpdate::Usage(usage) => self.usage.apply(&usage),
            SessionUpdate::Other { .. } => {}
        }
    }
}

/// What decides which view a tool call's items show in.
pub(super) fn linkage(call: &ToolCall) -> (Option<String>, bool) {
    (call.parent.clone(), call.is_subagent())
}
