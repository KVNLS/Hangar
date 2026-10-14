//! The pane of a structured (ACP) agent: messages, tool calls, plan and permission prompts as a
//! transcript, and a composer.

mod render;
mod subagent;

pub use subagent::subagent_state;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use gpui_kit::component::input::{InputEvent, TextareaState};
use gpui_kit::component::text::TextViewState;
use gpui_kit::*;
use hangar_agents::acp::protocol::{
    AvailableCommand, ConfigOption, PermissionKind, PlanEntry, SessionUpdate, StopReason, ToolCall,
};
use hangar_agents::acp::subagents::{PendingPermission, Subagent, owner, subagents};
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
    /// `parent`, here and on tool calls: the subagent launch it came from (`None`: the agent).
    Agent {
        text: String,
        view: Entity<TextViewState>,
        parent: Option<String>,
    },
    Thought {
        text: String,
        open: bool,
        parent: Option<String>,
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
    /// The user picked a value for a non-mode config option (model, effort, …) and the agent
    /// took it.
    ConfigChosen { config_id: String, value: String },
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
    /// Display name of the session mode (Claude Code: Manual, Accept edits, Plan, …).
    mode: Option<SharedString>,
    mode_id: Option<String>,
    /// The agent's config options (model, effort, mode, …) as it last reported them.
    config: Vec<ConfigOption>,
    /// Slash commands the agent offers (`/name` at the start of a prompt).
    commands: Vec<AvailableCommand>,
    composer: Entity<TextareaState>,
    scroll: ScrollHandle,
    /// Keep the newest item in view; turned off when the user scrolls up.
    follow: bool,
    /// The first item of the current (or last) prompt turn.
    turn_start: usize,
    /// The subagent shown instead of the agent's own transcript (its launch call id).
    focus: Option<String>,
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
                match event {
                    InputEvent::PressEnter { .. } => this.on_enter(window, cx),
                    // The slash-command list follows the text.
                    InputEvent::Change => cx.notify(),
                    _ => {}
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
        let mut view = Self {
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
            mode_id: None,
            config: Vec::new(),
            commands: Vec::new(),
            composer,
            scroll: ScrollHandle::new(),
            follow: true,
            turn_start: 0,
            focus: None,
            _tasks: vec![pump],
            _subscriptions: subscriptions,
        };
        let mode = view.session.initial_mode().map(str::to_owned);
        view.set_mode(mode);
        view.config = view.session.config_options().to_vec();
        view
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
            Item::Agent {
                text, parent: None, ..
            } => text
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| SharedString::from(l.to_owned())),
            Item::Tool(call) if call.parent.is_none() => Some(call.title.clone().into()),
            Item::Permission {
                title,
                outcome: None,
                ..
            } => Some(format!("Wants to: {title}").into()),
            _ => None,
        })
    }

    /// The subagents launched in the current (or last) turn.
    pub fn subagents(&self) -> Vec<Subagent> {
        let pending = self.pending();
        let calls = self.items[self.turn_start.min(self.items.len())..]
            .iter()
            .filter_map(|item| match item {
                Item::Tool(call) => Some(call),
                _ => None,
            });
        subagents(calls, pending)
    }

    /// Shows subagent `id`'s transcript (its launch call id), or the agent's own with `None`.
    pub fn focus_subagent(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        let id = id.filter(|id| self.launch(id).is_some_and(ToolCall::is_subagent));
        self.focus = id.map(str::to_owned);
        self.follow = true;
        cx.notify();
    }

    /// The tool call `id`, if reported.
    fn launch(&self, id: &str) -> Option<&ToolCall> {
        match self.items.get(*self.tools.get(id)?)? {
            Item::Tool(call) => Some(call),
            _ => None,
        }
    }

    /// The subagent `id`, wherever it was launched in the session.
    fn subagent(&self, id: &str) -> Option<Subagent> {
        let pending = self.pending();
        let calls = self.items.iter().filter_map(|item| match item {
            Item::Tool(call) => Some(call),
            _ => None,
        });
        subagents(calls, pending).into_iter().find(|s| s.id == id)
    }

    fn pending(&self) -> Option<PendingPermission<'_>> {
        self.pending_permission()
            .and_then(|i| match &self.items[i] {
                Item::Permission { request, title, .. } => Some(PendingPermission {
                    call: &request.tool_call,
                    title: title.as_ref(),
                }),
                _ => None,
            })
    }

    /// The top-level subagent item `index` belongs to; `None` for the agent's own items.
    fn owner(&self, index: usize) -> Option<&str> {
        let parent = match self.items.get(index)? {
            Item::Agent { parent, .. } | Item::Thought { parent, .. } => parent.as_deref(),
            Item::Tool(call) => call.parent.as_deref(),
            Item::Permission { request, .. } => request.tool_call.parent.as_deref().or_else(|| {
                self.launch(&request.tool_call.id)
                    .and_then(|c| c.parent.as_deref())
            }),
            Item::User(_) | Item::Plan(_) | Item::Notice { .. } => None,
        };
        owner(parent, |id| self.launch(id))
    }

    /// Shown in the current view. Permission prompts also show in the agent's own transcript:
    /// the whole agent waits on them.
    fn visible(&self, index: usize) -> bool {
        let owner = self.owner(index);
        match &self.focus {
            None => owner.is_none() || matches!(self.items[index], Item::Permission { .. }),
            Some(focus) => owner == Some(focus.as_str()),
        }
    }

    /// The index of the latest item from `parent` if it is text of the same kind, else `None`.
    fn open_text(&self, parent: Option<&str>, thought: bool) -> Option<usize> {
        let (i, item) = self.items.iter().enumerate().rev().find(|(_, item)| {
            let from = match item {
                Item::Agent { parent, .. } | Item::Thought { parent, .. } => parent.as_deref(),
                Item::Tool(call) => call.parent.as_deref(),
                Item::Permission { request, .. } => request.tool_call.parent.as_deref(),
                Item::User(_) | Item::Plan(_) | Item::Notice { .. } => None,
            };
            from == parent
        })?;
        match item {
            Item::Agent { .. } if !thought => Some(i),
            Item::Thought { .. } if thought => Some(i),
            _ => None,
        }
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
        self.turn_start = self.items.len();
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

    /// Adds a one-line note to the transcript (resumed, could not resume, …).
    pub fn add_notice(&mut self, error: bool, text: impl Into<SharedString>) {
        self.notice(error, text);
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
            // Parallel subagents interleave: a chunk continues the latest text of its sender.
            SessionUpdate::AgentMessage {
                text: chunk,
                parent,
            } => match self.open_text(parent.as_deref(), false) {
                Some(i) => {
                    if let Some(Item::Agent { text, view, .. }) = self.items.get_mut(i) {
                        text.push_str(&chunk);
                        view.update(cx, |state, cx| state.push_str(&chunk, cx));
                    }
                }
                None => {
                    let view = cx.new(|cx| TextViewState::markdown(&chunk, cx));
                    self.items.push(Item::Agent {
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
            SessionUpdate::CurrentMode(mode) => self.set_mode(Some(mode)),
            SessionUpdate::ConfigOptions(list) => self.set_config(list),
            SessionUpdate::AvailableCommands(commands) => self.commands = commands,
            SessionUpdate::Other { .. } => {}
        }
    }

    fn set_mode(&mut self, id: Option<String>) {
        self.mode = id.as_ref().map(|id| {
            let name = self.session.modes().iter().find(|m| &m.id == id);
            SharedString::from(name.map_or(id.clone(), |m| m.name.clone()))
        });
        self.mode_id = id;
    }

    fn set_config(&mut self, list: Vec<ConfigOption>) {
        if let Some(mode) = list.iter().find(|o| o.is_mode()) {
            self.set_mode(Some(mode.current.clone()));
        }
        self.config = list;
    }

    /// Sets a config option off the main thread; the chips follow the list the agent answers
    /// with (picking a model can add or drop the effort option).
    fn choose_config(&mut self, config_id: String, value: String, cx: &mut Context<Self>) {
        let unchanged = self
            .config
            .iter()
            .any(|o| o.id == config_id && o.current == value);
        if unchanged || self.exited {
            return;
        }
        let session = Arc::clone(&self.session);
        cx.spawn(async move |this, cx| {
            let (id, requested) = (config_id.clone(), value.clone());
            let result = cx
                .background_executor()
                .spawn(async move { session.set_config_option(&id, &requested) })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(list) => {
                        let mut list = list;
                        if list.is_empty() {
                            list = view.config.clone();
                            if let Some(o) = list.iter_mut().find(|o| o.id == config_id) {
                                o.current = value.clone();
                            }
                        }
                        let is_mode = list.iter().any(|o| o.id == config_id && o.is_mode());
                        view.set_config(list);
                        if !is_mode {
                            cx.emit(ChatEvent::ConfigChosen { config_id, value });
                        }
                    }
                    Err(err) => view.notice(true, format!("Could not change the setting: {err}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Switches the session mode off the main thread; the chip changes once the agent agrees.
    fn choose_mode(&mut self, id: String, cx: &mut Context<Self>) {
        if self.mode_id.as_deref() == Some(id.as_str()) || self.exited {
            return;
        }
        let session = Arc::clone(&self.session);
        cx.spawn(async move |this, cx| {
            let requested = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move { session.set_mode(&requested) })
                .await;
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(()) => view.set_mode(Some(id)),
                    Err(err) => view.notice(true, format!("Could not switch mode: {err}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Commands matching the composer while it holds only `/prefix`.
    fn command_matches(&self, cx: &App) -> Vec<AvailableCommand> {
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

    fn pick_command(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.composer.update(cx, |state, cx| {
            state.set_value(format!("/{name} "), window, cx);
            state.focus(window, cx);
        });
        cx.notify();
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
