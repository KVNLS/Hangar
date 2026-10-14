use gpui_kit::*;
use hangar_agents::acp::protocol::PermissionKind;

use super::rows::touch;
use super::{ChatEvent, ChatView, Item, Outcome, PendingRequest};

impl ChatView {
    pub(super) fn pending_permission(&self) -> Option<usize> {
        self.items
            .iter()
            .position(|item| matches!(item, Item::Permission { outcome: None, .. }))
    }

    pub(super) fn first_option(
        &self,
        index: usize,
        want: impl Fn(PermissionKind) -> bool,
    ) -> Option<String> {
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
    pub(super) fn answer(
        &mut self,
        index: usize,
        option_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
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

    pub(super) fn notice(&mut self, error: bool, text: impl Into<SharedString>) {
        self.items.push(Item::Notice {
            error,
            text: text.into(),
        });
    }
}
