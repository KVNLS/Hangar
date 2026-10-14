//! The empty agent pane and the agent picker.

use super::*;

impl HangarApp {
    /// The empty agent pane: resume the saved chat with the agent that ran it, or start a new one
    /// with the agent picked here.
    pub(super) fn render_no_agent(
        &self,
        pane: usize,
        ws_id: WorkspaceId,
        cx: &mut Context<Self>,
    ) -> EmptyState {
        let state = EmptyState::new(Lucide::Terminal, "No agent running");
        if self.work.busy.contains(&ws_id) {
            return state;
        }
        let t = cx.tokens();
        let saved_agent = self
            .live
            .saved_chats
            .get(&ws_id)
            .map(|chat| chat.agent.clone())
            .filter(|id| agents::preset(id).is_some());
        let resume = saved_agent.as_ref().map(|agent| {
            let (agent, name) = (agent.clone(), agent_display_name(agent));
            empty_choice(
                "Last conversation",
                div()
                    .min_w_0()
                    .truncate()
                    .type_secondary()
                    .text_color(t.tx2)
                    .child(name),
                UiButton::new(("empty-resume", pane))
                    .icon(Lucide::Play)
                    .label("Resume")
                    .primary()
                    .small()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_agent_with_resume(ws_id, agent.clone(), true, window, cx)
                    })),
                cx,
            )
        });
        let start = UiButton::new(("empty-new-chat", pane))
            .icon(Lucide::Plus)
            .label("Start")
            .small()
            .on_click(cx.listener(move |this, _, window, cx| {
                this.start_agent_in(ws_id, false, window, cx)
            }));
        let start = if saved_agent.is_some() {
            start
        } else {
            start.primary()
        };
        let picker = self.agent_picker(
            ("empty-agent-picker", pane),
            ws_id,
            self.agent_for(ws_id),
            cx,
        );
        let new_chat = empty_choice("New conversation", picker, start, cx);
        state.action(
            card(cx)
                .w_full()
                .text_left()
                .when_some(resume, |this, resume| {
                    this.child(resume).child(div().h(px(1.)).bg(t.line))
                })
                .child(new_chat),
        )
    }

    /// The workspace's agent with a menu to choose another; choosing does not start it.
    pub(super) fn agent_picker(
        &self,
        id: impl Into<ElementId>,
        ws_id: WorkspaceId,
        current: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let app = cx.entity().downgrade();
        let choices: Vec<(String, String, bool)> = self
            .agent_setup
            .agent_choices
            .iter()
            .map(|c| (c.id.clone(), c.name.clone(), c.installed))
            .collect();
        Button::new(id)
            .outline()
            .small()
            .label(agent_display_name(&current))
            .child(gpui_kit::component::Icon::new(Lucide::ChevronDown).xsmall())
            .tooltip("Choose the agent")
            .dropdown_menu_with_anchor(Anchor::TopCenter, move |mut menu, _, _| {
                for (id, name, installed) in &choices {
                    let label = if *installed {
                        name.clone()
                    } else {
                        format!("{name} (not on PATH)")
                    };
                    let (app, id) = (app.clone(), id.clone());
                    menu = menu.item(
                        PopupMenuItem::new(label)
                            .checked(id == current)
                            .disabled(!installed)
                            .on_click(move |_, window, cx| {
                                let _ = app.update(cx, |this, cx| {
                                    this.remember_workspace_agent(ws_id, &id, window, cx);
                                    cx.notify();
                                });
                            }),
                    );
                }
                menu
            })
            .into_any_element()
    }

    /// Starts the workspace's agent (see [`Self::agent_for`]) from the pane header and shows it.
    pub(super) fn start_agent_in(
        &mut self,
        ws_id: WorkspaceId,
        resume: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let agent = self.agent_for(ws_id);
        self.start_agent_with_resume(ws_id, agent, resume, window, cx);
    }

    pub(super) fn start_agent_with_resume(
        &mut self,
        ws_id: WorkspaceId,
        agent: String,
        resume: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout.panes.show(ws_id);
        self.panes_changed(window, cx);
        self.start_session(ws_id, agent, resume, window, cx)
            .detach();
    }
}

/// One choice of the empty agent pane: a title over a detail, its button on the right.
fn empty_choice(
    title: &'static str,
    detail: impl IntoElement,
    button: impl IntoElement,
    cx: &App,
) -> Div {
    let t = cx.tokens();
    v_flex()
        .gap(SPACE_2)
        .p(SPACE_3)
        .child(div().type_body_strong().text_color(t.tx).child(title))
        .child(
            h_flex()
                .gap(SPACE_2)
                .items_center()
                .justify_between()
                .child(detail)
                .child(button),
        )
}

/// The preset's display name, else its id.
fn agent_display_name(agent: &str) -> String {
    agents::preset(agent)
        .map(|a| a.display_name().to_owned())
        .unwrap_or_else(|| agent.to_owned())
}
