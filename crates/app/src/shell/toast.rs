//! Transient confirmations: one toast at a time above the status bar, gone after a few seconds.
//! Warnings, errors and anything with an action stay gpui-component notifications.

use super::*;
use crate::ui::components::toast;
use crate::ui::{ActiveTokens as _, STATUS_BAR_HEIGHT};

const TOAST_DURATION: Duration = Duration::from_millis(2800);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ToastKind {
    Info,
    Success,
}

pub(super) struct Toast {
    kind: ToastKind,
    message: SharedString,
    /// Dropping it (a newer toast) cancels the dismissal.
    _dismiss: Task<()>,
}

impl HangarApp {
    /// Shows `message` as the toast, replacing the current one.
    pub(super) fn show_toast(
        &mut self,
        kind: ToastKind,
        message: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let dismiss = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(TOAST_DURATION).await;
            let _ = this.update(cx, |this, cx| {
                this.layout.toast = None;
                cx.notify();
            });
        });
        self.layout.toast = Some(Toast {
            kind,
            message: message.into(),
            _dismiss: dismiss,
        });
        cx.notify();
    }

    pub(super) fn render_toast(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let current = self.layout.toast.as_ref()?;
        let t = cx.tokens();
        let (icon, color) = match current.kind {
            ToastKind::Info => (IconName::Info, t.acc),
            ToastKind::Success => (IconName::Check, t.grn),
        };
        Some(
            div()
                .absolute()
                .right(px(16.))
                .bottom(STATUS_BAR_HEIGHT + px(16.))
                .max_w(px(420.))
                .child(toast(icon, color, current.message.clone(), cx)),
        )
    }
}
