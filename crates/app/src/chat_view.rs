//! The pane of a structured (ACP) agent: messages, tool calls, plan and permission prompts as a
//! transcript, and a composer.

mod render;
mod rows;
mod subagent;
mod usage;

pub use subagent::subagent_state;
pub use usage::{costs_label, stored_cost, stored_costs_label, usage_label};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use gpui_kit::component::input::{InputEvent, Position, TextareaState};
use gpui_kit::component::text::TextViewState;
use gpui_kit::*;
use hangar_agents::acp::approval::{self, ApprovalLevel};
use hangar_agents::acp::protocol::{
    AvailableCommand, ConfigOption, PermissionKind, PlanEntry, SessionUpdate, StopReason, ToolCall,
};
use hangar_agents::acp::subagents::{PendingPermission, Subagent, owner, subagents};
use hangar_agents::acp::usage::SessionUsage;
use hangar_agents::acp::{AcpEvent, AcpSession, PermissionRequest};
use hangar_core::AcpTurn;

use crate::changes::WorkspaceDiff;
use crate::terminal_view::TerminalWaker;
use rows::{Rows, touch};

const CONTEXT: &str = "ChatView";
/// Events arriving within one frame are applied together.
const REDRAW_INTERVAL: Duration = Duration::from_millis(8);
/// Transcript rows measured beyond the viewport, so scrolling does not pop items in.
const OVERDRAW: Pixels = px(800.);

actions!(chat_view, [ChatEscape]);

/// A permission request a structured agent waits on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingRequest {
    /// What the agent wants to do (the tool call's title).
    pub title: SharedString,
    /// The agent offers "allow once" (some only offer always / reject).
    pub can_allow: bool,
}

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
    /// The turn started or ended, a permission request arrived or was answered, or the usage
    /// changed.
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
    /// The user switched the session mode and the agent took it.
    ModeChosen(String),
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
    usage: SessionUsage,
    composer: Entity<TextareaState>,
    /// The last turn ended normally: a "Ready for review" card follows it while the workspace
    /// has changes.
    turn_done: bool,
    /// The workspace's live diff, for the review card (set by the shell).
    diff: Option<(Entity<WorkspaceDiff>, Subscription)>,
    /// The virtualized transcript. It follows the tail until the user scrolls up, and again
    /// once they scroll back to the bottom.
    list: ListState,
    /// What `list` holds, kept in step by `sync_rows` on each render.
    rows: Rows,
    /// Items changed in place since the last sync.
    dirty: Vec<usize>,
    /// A tool call arrived or learned its parent: in a subagent's view, which items show may
    /// have changed beyond the dirty ones.
    relink: bool,
    /// The view `rows` was built for (`focus` at the time); `None` before the first render.
    rows_view: Option<Option<String>>,
    /// The first item of the current (or last) prompt turn.
    turn_start: usize,
    /// The subagent shown instead of the agent's own transcript (its launch call id).
    focus: Option<String>,
    /// Read-kind permission requests are answered without asking at this level.
    approval: ApprovalLevel,
    /// The agent waits on a question it asked the user (`ask_user`).
    asking: bool,
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
            usage: SessionUsage::default(),
            composer,
            turn_done: false,
            diff: None,
            list: ListState::new(0, ListAlignment::Top, OVERDRAW),
            rows: Rows::default(),
            dirty: Vec::new(),
            relink: false,
            rows_view: None,
            turn_start: 0,
            focus: None,
            approval: ApprovalLevel::default(),
            asking: false,
            _tasks: vec![pump],
            _subscriptions: subscriptions,
        };
        view.list.set_follow_mode(FollowMode::Tail);
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

    pub fn set_approval(&mut self, level: ApprovalLevel) {
        self.approval = level;
    }

    /// Whether the agent waits on a question it asked the user; the working line says so.
    pub fn set_asking(&mut self, asking: bool, cx: &mut Context<Self>) {
        if self.asking != asking {
            self.asking = asking;
            cx.notify();
        }
    }

    /// Context use and cost as the agent last reported them.
    pub fn usage(&self) -> &SessionUsage {
        &self.usage
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

    /// The agent's own last `last` messages, oldest first, for another agent to read
    /// (`agent_transcript`). Thoughts, plans and subagent items are left out.
    pub fn transcript(&self, last: usize) -> Vec<hangar_mcp::TranscriptEntry> {
        use hangar_agents::acp::protocol::ToolStatus;
        use hangar_mcp::TranscriptEntry;
        let mut entries: Vec<TranscriptEntry> = self
            .items
            .iter()
            .enumerate()
            .filter(|(i, _)| self.owner(*i).is_none())
            .filter_map(|(_, item)| match item {
                Item::User(text) => Some(TranscriptEntry::User(text.to_string())),
                Item::Agent { text, .. } => Some(TranscriptEntry::Agent(text.clone())),
                Item::Tool(call) => Some(TranscriptEntry::Tool {
                    title: call.title.clone(),
                    status: match call.status {
                        ToolStatus::Pending => "pending",
                        ToolStatus::InProgress => "in_progress",
                        ToolStatus::Completed => "completed",
                        ToolStatus::Failed => "failed",
                    }
                    .to_owned(),
                }),
                Item::Permission { title, outcome, .. } => Some(TranscriptEntry::Permission {
                    title: title.to_string(),
                    answer: outcome.as_ref().map(|o| o.label.to_string()),
                }),
                Item::Notice { text, .. } => Some(TranscriptEntry::Notice(text.to_string())),
                Item::Thought { .. } | Item::Plan(_) => None,
            })
            .collect();
        let skip = entries.len().saturating_sub(last);
        entries.drain(..skip);
        entries
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
        self.follow_tail();
        cx.notify();
    }

    /// Scrolls to the newest item and keeps it in view until the user scrolls up.
    fn follow_tail(&self) {
        self.list.set_follow_mode(FollowMode::Tail);
    }

    /// Brings the list up to date with the items and the rows around them; only changed and new
    /// items are looked at, unless the view changed.
    fn sync_rows(&mut self, lead: Vec<rows::Row>, trail: Vec<rows::Row>) {
        let dirty = std::mem::take(&mut self.dirty);
        let relink = std::mem::take(&mut self.relink) && self.focus.is_some();
        let mut rows = std::mem::take(&mut self.rows);
        if self.rows_view.as_ref() != Some(&self.focus) {
            let count = rows.rebuild(lead, self.items.len(), |i| self.visible(i), trail);
            self.list.reset(count);
            self.rows_view = Some(self.focus.clone());
        } else {
            let ops = rows.update(
                lead,
                self.items.len(),
                &dirty,
                relink,
                |i| self.visible(i),
                trail,
            );
            for op in ops {
                match op {
                    rows::ListOp::Splice(range, count) => self.list.splice(range, count),
                    rows::ListOp::Remeasure(range) => self.list.remeasure_items(range),
                }
            }
        }
        self.rows = rows;
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

    fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        touch(&mut self.dirty, index);
        if let Err(err) = result {
            self.notice(true, format!("Could not answer the agent: {err}"));
        }
        cx.emit(ChatEvent::StateChanged);
        cx.notify();
    }

    /// The request the agent waits on, for answering it outside the pane (overview, palette).
    pub fn pending_request(&self) -> Option<PendingRequest> {
        let index = self.pending_permission()?;
        let Some(Item::Permission { title, .. }) = self.items.get(index) else {
            return None;
        };
        Some(PendingRequest {
            title: title.clone(),
            can_allow: self
                .first_option(index, |k| k == PermissionKind::AllowOnce)
                .is_some(),
        })
    }

    /// Answers the pending request: allow once, or reject (once when offered, else outright).
    /// Returns whether a request was answered.
    pub fn answer_pending(&mut self, allow: bool, cx: &mut Context<Self>) -> bool {
        let Some(index) = self.pending_permission() else {
            return false;
        };
        let option = if allow {
            match self.first_option(index, |k| k == PermissionKind::AllowOnce) {
                Some(option) => Some(option),
                None => return false,
            }
        } else {
            self.first_option(index, |k| k == PermissionKind::RejectOnce)
        };
        self.answer(index, option, cx);
        true
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

    fn apply(&mut self, event: AcpEvent, cx: &mut Context<Self>) {
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
            AcpEvent::TurnEnded { reason, .. } => {
                self.running = false;
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
                        touch(&mut self.dirty, i);
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
                        if is_mode {
                            cx.emit(ChatEvent::ModeChosen(value));
                        } else {
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
                    Ok(()) => {
                        view.set_mode(Some(id.clone()));
                        cx.emit(ChatEvent::ModeChosen(id));
                    }
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

/// What decides which view a tool call's items show in.
fn linkage(call: &ToolCall) -> (Option<String>, bool) {
    (call.parent.clone(), call.is_subagent())
}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }
}
