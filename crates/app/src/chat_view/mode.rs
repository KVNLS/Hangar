use std::sync::Arc;

use gpui_kit::*;
use hangar_agents::acp::protocol::ConfigOption;

use super::{ChatEvent, ChatView};

impl ChatView {
    pub(super) fn set_mode(&mut self, id: Option<String>) {
        self.mode = id.as_ref().map(|id| {
            let name = self.session.modes().iter().find(|m| &m.id == id);
            SharedString::from(name.map_or(id.clone(), |m| m.name.clone()))
        });
        self.mode_id = id;
    }

    pub(super) fn set_config(&mut self, list: Vec<ConfigOption>) {
        if let Some(mode) = list.iter().find(|o| o.is_mode()) {
            self.set_mode(Some(mode.current.clone()));
        }
        self.config = list;
    }

    /// Sets a config option off the main thread; the chips follow the list the agent answers
    /// with (picking a model can add or drop the effort option).
    pub(super) fn choose_config(
        &mut self,
        config_id: String,
        value: String,
        cx: &mut Context<Self>,
    ) {
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
    pub(super) fn choose_mode(&mut self, id: String, cx: &mut Context<Self>) {
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
}
