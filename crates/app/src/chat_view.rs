//! The pane of a structured (ACP) agent: messages, tool calls, plan and permission prompts as a
//! transcript, and a composer.

mod acp_events;
mod cap;
mod input;
mod mode;
mod permissions;
mod render;
mod rows;
mod subagent;
mod transcript;
mod usage;

pub use subagent::subagent_state;
pub use usage::{
    cost_label, costs_label, stored_cost, stored_costs_label, turn_spend, usage_label,
};

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use gpui_kit::component::input::{InputEvent, TextareaState};
use gpui_kit::component::text::TextViewState;
use gpui_kit::*;
use hangar_agents::acp::approval::ApprovalLevel;
use hangar_agents::acp::protocol::{
    AvailableCommand, ConfigOption, Cost, PlanEntry, ToolCall, TurnUsage,
};
use hangar_agents::acp::subagents::{Subagent, subagents};
use hangar_agents::acp::usage::SessionUsage;
use hangar_agents::acp::{AcpEvent, AcpSession, PermissionRequest};
use hangar_core::AcpTurn;

use crate::changes::WorkspaceDiff;
use crate::terminal_view::TerminalWaker;
use rows::Rows;

const CONTEXT: &str = "ChatView";
/// Events arriving within one frame are applied together.
const REDRAW_INTERVAL: Duration = Duration::from_millis(8);
/// How long a stopped turn waits for the agent to confirm before it ends on this side.
const CANCEL_GRACE: Duration = Duration::from_secs(10);
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
        /// Trailing bytes of `text` not given to `view` (see [`cap`]).
        hidden: usize,
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

/// What a chat spent since the shell last asked ([`ChatView::take_spend`]).
#[derive(Clone, Debug, PartialEq)]
pub struct Spend {
    pub cost: Option<Cost>,
    pub turn: Option<TurnUsage>,
    pub model: Option<String>,
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
    /// Counts prompts sent, so a stop timer knows whether its turn is still the running one.
    turn_seq: u64,
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
    /// What the last turn spent, until the shell books it.
    turn_usage: Option<TurnUsage>,
    /// A turn ended since the shell last booked the spend.
    turn_ended: bool,
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
            turn_seq: 0,
            failed: false,
            exited: false,
            mode: None,
            mode_id: None,
            config: Vec::new(),
            commands: Vec::new(),
            usage: SessionUsage::default(),
            turn_usage: None,
            turn_ended: false,
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

    /// What was spent since the last call, once a turn ended or while idle (a cost reported
    /// between turns), with the model the agent is set to. Mid-turn costs wait for the turn's
    /// end so a turn is booked as one row.
    pub fn take_spend(&mut self) -> Option<Spend> {
        if self.running && !self.turn_ended && !self.exited {
            return None;
        }
        self.turn_ended = false;
        let cost = self.usage.take_cost_delta();
        let turn = self.turn_usage.take();
        if cost.is_none() && turn.is_none() {
            return None;
        }
        let model = self
            .config
            .iter()
            .find(|o| o.category.as_deref() == Some("model"))
            .map(|o| o.current.clone());
        Some(Spend { cost, turn, model })
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
}

impl Focusable for ChatView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.read(cx).focus_handle(cx)
    }
}
