//! The pane of a structured (ACP) agent: messages, tool calls, plan and permission prompts as a
//! transcript, and a composer.

mod render;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use gpui_kit::component::input::{InputEvent, TextareaState};
use gpui_kit::component::text::TextViewState;
use gpui_kit::*;
use hangar_agents::acp::protocol::{
    PermissionKind, PlanEntry, SessionUpdate, StopReason, ToolCall,
};
use hangar_agents::acp::{AcpEvent, AcpSession, PermissionRequest};
use hangar_core::AcpTurn;

use crate::terminal_view::TerminalWaker;

const CONTEXT: &str = "ChatView";
/// Events arriving within one frame are applied together.
const REDRAW_INTERVAL: Duration = Duration::from_millis(8);

actions!(chat_view, [ChatEscape]);

pub fn init(cx: &mut App) {
    cx.bind_keys([KeyBinding::new("escape", ChatEscape, Some(CONTEXT))]);
}

enum Item {
    User(SharedString),
    Agent {
        text: String,
        view: Entity<TextViewState>,
    },
    Thought {
        text: String,
        open: bool,
    },
    Tool(ToolCall),
    Plan(Vec<PlanEntry>),
    Permission {
        request: PermissionRequest,
        title: SharedString,
        /// How it was answered; `None` while the agent waits.
        outcome: Option<Outcome>,
    },
    Notice {
        error: bool,
        text: SharedString,
    },
}

#[derive(Clone, Debug)]
struct Outcome {
    allowed: bool,
    label: SharedString,
}

/// Emitted by [`ChatView`].
#[derive(Clone, Debug)]
pub enum ChatEvent {
    /// The turn started or ended, or a permission request arrived or was answered.
    StateChanged,
    /// The agent process ended. Always the last event.
    Exited {
        code: Option<i32>,
        stderr_tail: String,
    },
    /// A tool call location was clicked (absolute path inside the worktree).
    OpenFile(PathBuf),
}

impl EventEmitter<ChatEvent> for ChatView {}

pub struct ChatView {
    session: Arc<AcpSession>,
    events: Receiver<AcpEvent>,
    agent_name: SharedString,
    /// The worktree; tool paths under it are shown relative.
    root: PathBuf,
    items: Vec<Item>,
    tools: HashMap<String, usize>,
    /// The plan item of the current turn, updated in place.
    plan: Option<usize>,
    running: bool,
    failed: bool,
    exited: bool,
    mode: Option<SharedString>,
    composer: Entity<TextareaState>,
    scroll: ScrollHandle,
    /// Keep the newest item in view; turned off when the user scrolls up.
    follow: bool,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ChatView {
    pub fn new(
        session: Arc<AcpSession>,
        events: Receiver<AcpEvent>,
        waker: TerminalWaker,
        agent_name: SharedString,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(format!("Reply to {agent_name}…"))
                .submit_on_enter(true)
                .auto_grow(1, 10)
        });
        let subscriptions = vec![cx.subscribe_in(
            &composer,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.on_enter(window, cx);
                }
            },
        )];
        // Events sent during the handshake left a pending wake-up, so the first drain happens
        // after the owner has subscribed (an early `Exited` is not lost).
        let pump = cx.spawn(async move |this, cx| {
            loop {
                waker.notified().await;
                if this.update(cx, |view, cx| view.drain(cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(REDRAW_INTERVAL).await;
            }
        });
        Self {
            session,
            events,
            agent_name,
            root,
            items: Vec::new(),
            tools: HashMap::new(),
            plan: None,
            running: false,
            failed: false,
            exited: false,
            mode: None,
            composer,
            scroll: ScrollHandle::new(),
            follow: true,
            _tasks: vec![pump],
            _subscriptions: subscriptions,
        }
    }

    pub fn turn(&self) -> AcpTurn {
        AcpTurn {
            running: self.running,
            permission_pending: self.pending_permission().is_some(),
            failed: self.failed,
        }
    }

    /// The latest thing worth showing in a one-line summary (overview cards).
    pub fn last_line(&self) -> Option<SharedString> {
        self.items.iter().rev().find_map(|item| match item {
            Item::Agent { text, .. } => text
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| SharedString::from(l.to_owned())),
            Item::Tool(call) => Some(call.title.clone().into()),
            Item::Permission {
                title,
                outcome: None,
                ..
            } => Some(format!("Wants to: {title}").into()),
            _ => None,
        })
    }

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
        self.items.push(Item::User(text.to_owned().into()));
        self.running = true;
        self.failed = false;
        self.plan = None;
        self.follow = true;
        cx.emit(ChatEvent::StateChanged);
        cx.notify();
        Ok(())
    }

    /// Asks the agent to stop the running turn; open permission requests are cancelled.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if !self.running || self.exited {
            return;
        }
        if let Err(err) = self.session.cancel() {
            self.notice(true, format!("Could not stop the agent: {err}"));
        }
        for item in &mut self.items {
            if let Item::Permission {
                outcome: outcome @ None,
                ..
            } = item
            {
                *outcome = Some(Outcome {
                    allowed: false,
                    label: "Cancelled".into(),
                });
            }
        }
        cx.emit(ChatEvent::StateChanged);
        cx.notify();
    }

    fn on_enter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    fn on_escape(&mut self, _: &ChatEscape, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.pending_permission() {
            let option = self.first_option(index, |k| k == PermissionKind::RejectOnce);
            self.answer(index, option, cx);
        } else if self.running {
            self.stop(cx);
        } else {
            cx.propagate();
        }
    }

    fn pending_permission(&self) -> Option<usize> {
        self.items
            .iter()
            .position(|item| matches!(item, Item::Permission { outcome: None, .. }))
    }

    fn first_option(&self, index: usize, want: impl Fn(PermissionKind) -> bool) -> Option<String> {
        match self.items.get(index) {
            Some(Item::Permission { request, .. }) => request
                .options
                .iter()
                .find(|o| want(o.kind))
                .map(|o| o.id.clone()),
            _ => None,
        }
    }

    /// Answers the permission request at `index` with `option_id` (`None` rejects it outright).
    fn answer(&mut self, index: usize, option_id: Option<String>, cx: &mut Context<Self>) {
        let Some(Item::Permission {
            request, outcome, ..
        }) = self.items.get_mut(index)
        else {
            return;
        };
        if outcome.is_some() {
            return;
        }
        let chosen = option_id
            .as_deref()
            .and_then(|id| request.options.iter().find(|o| o.id == id));
        let result = self
            .session
            .answer_permission(&request.request_id, option_id.as_deref());
        *outcome = Some(match chosen {
            Some(option) => Outcome {
                allowed: option.kind.is_allow(),
                label: option.name.clone().into(),
            },
            None => Outcome {
                allowed: false,
                label: "Denied".into(),
            },
        });
        if let Err(err) = result {
            self.notice(true, format!("Could not answer the agent: {err}"));
        }
        cx.emit(ChatEvent::StateChanged);
        cx.notify();
    }

    fn notice(&mut self, error: bool, text: impl Into<SharedString>) {
        self.items.push(Item::Notice {
            error,
            text: text.into(),
        });
    }

    fn drain(&mut self, cx: &mut Context<Self>) {
        let before = self.turn();
        let mut changed = false;
        while let Ok(event) = self.events.try_recv() {
            changed = true;
            self.apply(event, cx);
        }
        if changed {
            if self.turn() != before {
                cx.emit(ChatEvent::StateChanged);
            }
            cx.notify();
        }
    }

    fn apply(&mut self, event: AcpEvent, cx: &mut Context<Self>) {
        match event {
            AcpEvent::Update(update) => self.apply_update(update, cx),
            AcpEvent::PermissionRequest(request) => {
                let title = match self.tools.get(&request.tool_call.id) {
                    Some(&i) => {
                        if let Some(Item::Tool(call)) = self.items.get_mut(i) {
                            call.apply(&request.tool_call);
                            Some(call.title.clone())
                        } else {
                            None
                        }
                    }
                    None => None,
                };
                let title = title
                    .or_else(|| request.tool_call.title.clone())
                    .unwrap_or_else(|| "use a tool".to_owned());
                self.items.push(Item::Permission {
                    request,
                    title: title.into(),
                    outcome: None,
                });
                self.follow = true;
            }
            AcpEvent::TurnEnded(reason) => {
                self.running = false;
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
                for item in &mut self.items {
                    if let Item::Permission {
                        outcome: outcome @ None,
                        ..
                    } = item
                    {
                        *outcome = Some(Outcome {
                            allowed: false,
                            label: "Agent exited".into(),
                        });
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

    fn apply_update(&mut self, update: SessionUpdate, cx: &mut Context<Self>) {
        match update {
            SessionUpdate::AgentMessage(chunk) => {
                if let Some(Item::Agent { text, view }) = self.items.last_mut() {
                    text.push_str(&chunk);
                    view.update(cx, |state, cx| state.push_str(&chunk, cx));
                } else {
                    let view = cx.new(|cx| TextViewState::markdown(&chunk, cx));
                    self.items.push(Item::Agent { text: chunk, view });
                }
            }
            SessionUpdate::AgentThought(chunk) => {
                if let Some(Item::Thought { text, .. }) = self.items.last_mut() {
                    text.push_str(&chunk);
                } else {
                    self.items.push(Item::Thought {
                        text: chunk,
                        open: false,
                    });
                }
            }
            SessionUpdate::UserMessage(chunk) => {
                if let Some(Item::User(text)) = self.items.last_mut() {
                    *text = format!("{text}{chunk}").into();
                } else {
                    self.items.push(Item::User(chunk.into()));
                }
            }
            SessionUpdate::ToolCall(call) => match self.tools.get(&call.id) {
                Some(&i) => self.items[i] = Item::Tool(call),
                None => {
                    self.tools.insert(call.id.clone(), self.items.len());
                    self.items.push(Item::Tool(call));
                }
            },
            SessionUpdate::ToolCallUpdate(update) => match self.tools.get(&update.id) {
                Some(&i) => {
                    if let Some(Item::Tool(call)) = self.items.get_mut(i) {
                        call.apply(&update);
                    }
                }
                None => {
                    self.tools.insert(update.id.clone(), self.items.len());
                    self.items.push(Item::Tool(ToolCall::from_update(&update)));
                }
            },
            SessionUpdate::Plan(entries) => match self.plan {
                Some(i) => self.items[i] = Item::Plan(entries),
                None => {
                    self.plan = Some(self.items.len());
                    self.items.push(Item::Plan(entries));
                }
            },
            SessionUpdate::CurrentMode(mode) => self.mode = Some(mode.into()),
            // Slash commands are not offered in the composer yet.
            SessionUpdate::AvailableCommands(_) | SessionUpdate::Other { .. } => {}
        }
    }

    fn open_location(&mut self, path: &str, cx: &mut Context<Self>) {
        let path = Path::new(path);
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            self.root.join(path)
        };
        cx.emit(ChatEvent::OpenFile(path));
    }

    /// `path` relative to the worktree when it is inside it.
    fn display_path<'a>(&self, path: &'a str) -> &'a str {
        Path::new(path)
            .strip_prefix(&self.root)
            .ok()
            .and_then(Path::to_str)
            .unwrap_or(path)
    }
}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }
}
