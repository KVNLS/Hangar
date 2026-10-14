//! Answering structured agents' permission requests without opening their pane: inline on
//! overview cards and from the palette ("Approve").

use super::*;
use crate::chat_view::PendingRequest;

impl HangarApp {
    /// The permission request `id`'s agent waits on, if it is a structured agent.
    pub(super) fn pending_request(&self, id: WorkspaceId, cx: &App) -> Option<PendingRequest> {
        match &self.live.sessions.get(&id)?.surface {
            AgentSurface::Chat { view, .. } => view.read(cx).pending_request(),
            AgentSurface::Terminal { .. } => None,
        }
    }

    /// Every waiting request that can be allowed once, in sidebar order.
    pub(super) fn pending_approvals(&self, cx: &App) -> Vec<(WorkspaceId, String, PendingRequest)> {
        self.work
            .projects
            .entries()
            .iter()
            .flat_map(|e| e.active())
            .filter_map(|ws| {
                let request = self.pending_request(ws.id, cx)?;
                request.can_allow.then(|| (ws.id, ws.name.clone(), request))
            })
            .collect()
    }

    /// Allows the request once, or rejects it.
    pub(super) fn answer_request(&mut self, id: WorkspaceId, allow: bool, cx: &mut Context<Self>) {
        let Some(LiveSession {
            surface: AgentSurface::Chat { view, .. },
            ..
        }) = self.live.sessions.get(&id)
        else {
            return;
        };
        let answered = view.update(cx, |chat, cx| chat.answer_pending(allow, cx));
        if answered {
            tracing::info!(workspace = %id, allow, "permission answered outside the pane");
        }
        cx.notify();
    }
}

/// "Allow <workspace>: <request>" for the palette.
pub(super) fn approve_label(workspace: &str, request: &PendingRequest) -> String {
    format!("Allow {workspace}: {}", request.title)
}
