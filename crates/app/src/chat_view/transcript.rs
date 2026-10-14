use std::path::Path;

use gpui_kit::*;
use hangar_agents::acp::protocol::ToolCall;
use hangar_agents::acp::subagents::{PendingPermission, Subagent, owner, subagents};

use super::rows::{self};
use super::{ChatEvent, ChatView, Item};

impl ChatView {
    /// Scrolls to the newest item and keeps it in view until the user scrolls up.
    pub(super) fn follow_tail(&self) {
        self.list.set_follow_mode(FollowMode::Tail);
    }

    /// Brings the list up to date with the items and the rows around them; only changed and new
    /// items are looked at, unless the view changed.
    pub(super) fn sync_rows(&mut self, lead: Vec<rows::Row>, trail: Vec<rows::Row>) {
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
    pub(super) fn launch(&self, id: &str) -> Option<&ToolCall> {
        match self.items.get(*self.tools.get(id)?)? {
            Item::Tool(call) => Some(call),
            _ => None,
        }
    }

    /// The subagent `id`, wherever it was launched in the session.
    pub(super) fn subagent(&self, id: &str) -> Option<Subagent> {
        let pending = self.pending();
        let calls = self.items.iter().filter_map(|item| match item {
            Item::Tool(call) => Some(call),
            _ => None,
        });
        subagents(calls, pending).into_iter().find(|s| s.id == id)
    }

    pub(super) fn pending(&self) -> Option<PendingPermission<'_>> {
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
    pub(super) fn owner(&self, index: usize) -> Option<&str> {
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
    pub(super) fn visible(&self, index: usize) -> bool {
        let owner = self.owner(index);
        match &self.focus {
            None => owner.is_none() || matches!(self.items[index], Item::Permission { .. }),
            Some(focus) => owner == Some(focus.as_str()),
        }
    }

    /// The index of the latest item from `parent` if it is text of the same kind, else `None`.
    pub(super) fn open_text(&self, parent: Option<&str>, thought: bool) -> Option<usize> {
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

    pub(super) fn open_location(&mut self, path: &str, cx: &mut Context<Self>) {
        let path = Path::new(path);
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            self.root.join(path)
        };
        cx.emit(ChatEvent::OpenFile(path));
    }

    /// `path` relative to the worktree when it is inside it.
    pub(super) fn display_path<'a>(&self, path: &'a str) -> &'a str {
        Path::new(path)
            .strip_prefix(&self.root)
            .ok()
            .and_then(Path::to_str)
            .unwrap_or(path)
    }
}
