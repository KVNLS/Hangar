//! At startup: simulator copies Hangar made for workspaces that no longer exist (see
//! `device_pool::orphans`). The user decides; nothing is deleted without the button.

use crate::device_pool::{backend, orphans};
use crate::shell::toast::ToastKind;

use super::*;

impl HangarApp {
    /// Looks for leftover copies off the UI thread and offers to delete them.
    pub(super) fn offer_orphan_cleanup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let existing: Vec<WorkspaceId> = self
            .work
            .projects
            .entries()
            .iter()
            .flat_map(|e| e.workspaces.iter().map(|w| w.id))
            .collect();
        cx.spawn_in(window, async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move {
                    let on_mac = backend::list_clones()?
                        .into_iter()
                        .map(|d| d.udid)
                        .collect();
                    let found = orphans::find(store.as_ref(), existing, &on_mac)?;
                    anyhow::Ok((store, found))
                })
                .await;
            let (store, found) = match found {
                Ok((_, found)) if found.is_empty() => return,
                Ok(found) => found,
                Err(err) => {
                    tracing::debug!("no leftover simulator check: {err:#}");
                    return;
                }
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.show_orphans(store, found, window, cx)
            });
        })
        .detach();
    }

    fn show_orphans(
        &mut self,
        store: Arc<dyn Store>,
        found: Vec<orphans::Orphan>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = found.len();
        tracing::info!(count, "leftover simulator copies found");
        let shell = cx.entity().downgrade();
        let notification = Notification::warning(format!(
            "{count} simulator{} Hangar made for deleted workspaces {} still on this Mac.",
            if count == 1 { "" } else { "s" },
            if count == 1 { "is" } else { "are" },
        ))
        .autohide(false)
        .action(move |_, _, _| {
            let (shell, store, found) = (shell.clone(), store.clone(), found.clone());
            gpui_kit::component::button::Button::new("delete-orphans")
                .label("Delete")
                .on_click(move |_, window, cx| {
                    let (store, found) = (store.clone(), found.clone());
                    let _ =
                        shell.update(cx, |this, cx| this.delete_orphans(store, found, window, cx));
                })
        });
        window.push_notification(notification, cx);
    }

    fn delete_orphans(
        &mut self,
        store: Arc<dyn Store>,
        found: Vec<orphans::Orphan>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            let deleted = cx
                .background_executor()
                .spawn(async move { orphans::delete(store.as_ref(), &found) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| match deleted {
                Ok(n) => this.show_toast(
                    ToastKind::Info,
                    format!(
                        "Deleted {n} leftover simulator{}.",
                        if n == 1 { "" } else { "s" }
                    ),
                    cx,
                ),
                Err(err) => this.report(
                    &err.context("could not delete leftover simulators"),
                    window,
                    cx,
                ),
            });
        })
        .detach();
    }
}
