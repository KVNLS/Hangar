//! What an agent should hear about its device (its reservation ends soon or was revoked,
//! another workspace's app is installed), kept until the workspace's next device tool result
//! carries it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hangar_core::WorkspaceId;
use hangar_mcp::{Content, Tool, ToolError, ToolOutput};

/// Pending notices of a workspace beyond which the oldest is dropped (nobody reads them while
/// its agent makes no device call).
const MAX_PENDING: usize = 4;

/// Pending notices per workspace, shared by the host and the GPUI side.
#[derive(Clone, Default)]
pub struct Notices(Arc<Mutex<HashMap<WorkspaceId, Vec<String>>>>);

impl Notices {
    fn lock(&self) -> MutexGuard<'_, HashMap<WorkspaceId, Vec<String>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn push(&self, ws: WorkspaceId, message: String) {
        let mut map = self.lock();
        let pending = map.entry(ws).or_default();
        if pending.contains(&message) {
            return;
        }
        pending.push(message);
        let extra = pending.len().saturating_sub(MAX_PENDING);
        pending.drain(..extra);
    }

    /// The workspace's pending notices as one text, cleared.
    pub fn take(&self, ws: WorkspaceId) -> Option<String> {
        let pending = self.lock().remove(&ws)?;
        (!pending.is_empty()).then(|| pending.join("\n"))
    }

    pub fn forget(&self, ws: WorkspaceId) {
        self.lock().remove(&ws);
    }

    pub(super) fn clear(&self) {
        self.lock().clear();
    }
}

/// Tools whose results carry the workspace's pending notices.
pub(super) fn carries_notices(tool: &Tool) -> bool {
    matches!(
        tool,
        Tool::DeviceInfo
            | Tool::Screenshot
            | Tool::Tap { .. }
            | Tool::Swipe { .. }
            | Tool::TypeText { .. }
            | Tool::Press(_)
            | Tool::Logs { .. }
            | Tool::BuildAndRun
            | Tool::LaunchApp { .. }
            | Tool::DeviceAcquire { .. }
            | Tool::DeviceRelease
    )
}

/// `result` with `notice` appended: a text block of a result, a paragraph of an error.
pub(super) fn deliver(
    result: Result<ToolOutput, ToolError>,
    notice: Option<String>,
) -> Result<ToolOutput, ToolError> {
    let Some(notice) = notice else {
        return result;
    };
    let notice = format!("Notice: {notice}");
    match result {
        Ok(mut out) => {
            out.content.push(Content::Text(notice));
            Ok(out)
        }
        Err(ToolError::Failed(message)) => Err(ToolError::Failed(format!("{message}\n\n{notice}"))),
        Err(ToolError::Unsupported(message)) => {
            Err(ToolError::Unsupported(format!("{message}\n\n{notice}")))
        }
        Err(err @ ToolError::UnknownSession) => Err(err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notices_are_delivered_once_deduplicated_and_bounded() {
        let notices = Notices::default();
        let (a, b) = (WorkspaceId::new(), WorkspaceId::new());
        assert_eq!(notices.take(a), None);
        notices.push(a, "warned".into());
        notices.push(a, "warned".into());
        notices.push(a, "revoked".into());
        assert_eq!(notices.take(b), None, "per workspace");
        assert_eq!(notices.take(a).as_deref(), Some("warned\nrevoked"));
        assert_eq!(notices.take(a), None, "cleared once delivered");
        for i in 0..6 {
            notices.push(b, format!("n{i}"));
        }
        assert_eq!(notices.take(b).as_deref(), Some("n2\nn3\nn4\nn5"));
        notices.push(b, "late".into());
        notices.forget(b);
        assert_eq!(notices.take(b), None);
    }

    #[test]
    fn a_notice_rides_on_results_and_errors() {
        let out = deliver(Ok(ToolOutput::text("Tapped.")), Some("revoked".into())).unwrap();
        assert_eq!(
            out.content,
            vec![
                Content::Text("Tapped.".into()),
                Content::Text("Notice: revoked".into())
            ]
        );
        assert_eq!(
            deliver(
                Err(ToolError::Failed("queued".into())),
                Some("warned".into())
            ),
            Err(ToolError::Failed("queued\n\nNotice: warned".into()))
        );
        assert_eq!(
            deliver(Err(ToolError::UnknownSession), Some("x".into())),
            Err(ToolError::UnknownSession)
        );
        assert_eq!(
            deliver(Ok(ToolOutput::text("ok")), None),
            Ok(ToolOutput::text("ok"))
        );
        assert!(carries_notices(&Tool::Screenshot));
        assert!(!carries_notices(&Tool::WorkspacesList));
    }
}
