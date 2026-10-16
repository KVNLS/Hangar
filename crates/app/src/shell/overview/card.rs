use super::*;

/// Subagent rows shown on a card before "+N more".
const MAX_SUBAGENT_ROWS: usize = 4;

/// "3 subagents · 1 done · 1 running · 1 waiting for approval"; zero counts are left out.
fn subagent_summary(list: &[Subagent]) -> String {
    let n = list.len();
    let mut parts = vec![format!("{n} subagent{}", if n == 1 { "" } else { "s" })];
    for (status, label) in [
        (SubagentStatus::Done, "done"),
        (SubagentStatus::Running, "running"),
        (SubagentStatus::Approval, "waiting for approval"),
        (SubagentStatus::Failed, "failed"),
    ] {
        let count = list.iter().filter(|s| s.status == status).count();
        if count > 0 {
            parts.push(format!("{count} {label}"));
        }
    }
    parts.join(" · ")
}

impl HangarApp {
    pub(super) fn run_card_action(
        &mut self,
        id: WorkspaceId,
        action: CardAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if action == CardAction::Unqueue {
            self.unqueue_agent(id, window, cx);
            return;
        }
        let stopped = self
            .live
            .sessions
            .get(&id)
            .is_none_or(|l| l.session.status.is_terminal());
        let restart = action == CardAction::Restart && stopped && !self.work.busy.contains(&id);
        self.select_workspace(id, window, cx);
        let screen = if action == CardAction::Review {
            Screen::Review
        } else {
            Screen::Workspace
        };
        self.set_screen(screen, window, cx);
        if restart {
            let agent = self.agent_for(id);
            self.start_session(id, agent, true, window, cx).detach();
        }
    }

    pub(super) fn chat_view(&self, id: WorkspaceId) -> Option<&Entity<ChatView>> {
        match &self.live.sessions.get(&id)?.surface {
            AgentSurface::Chat { view, .. } => Some(view),
            _ => None,
        }
    }

    pub(super) fn subagent_block(
        &self,
        id: WorkspaceId,
        list: &[Subagent],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = cx.tokens();
        let rows = list.iter().take(MAX_SUBAGENT_ROWS).map(|sub| {
            let sub_id = sub.id.clone();
            h_flex()
                .id(SharedString::from(format!("overview-sub-{id}-{}", sub.id)))
                .gap_2()
                .px_2()
                .py(px(6.))
                .items_start()
                .border_t_1()
                .border_color(t.line)
                .hover(|s| s.bg(t.hover))
                .on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.select_workspace(id, window, cx);
                    this.set_screen(Screen::Workspace, window, cx);
                    if let Some(view) = this.chat_view(id).cloned() {
                        view.update(cx, |view, cx| view.focus_subagent(Some(&sub_id), cx));
                    }
                }))
                .child(
                    div()
                        .flex()
                        .h(px(18.))
                        .w(px(14.))
                        .items_center()
                        .justify_center()
                        .child(StateGlyph::new(subagent_state(sub.status))),
                )
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .child(div().type_body().truncate().child(sub.name.clone()))
                        .children(sub.activity.clone().map(|a| {
                            div()
                                .type_mono_small()
                                .text_color(t.tx3)
                                .truncate()
                                .child(a)
                        })),
                )
                .child(
                    div().flex().h(px(18.)).items_center().child(
                        Icon::new(Lucide::ChevronRight)
                            .size(px(12.))
                            .text_color(t.tx3),
                    ),
                )
        });
        let more = list.len().saturating_sub(MAX_SUBAGENT_ROWS);
        v_flex()
            .rounded(px(4.))
            .border_1()
            .border_color(t.line)
            .overflow_hidden()
            .child(
                h_flex()
                    .gap_2()
                    .px_2()
                    .py(px(5.))
                    .items_center()
                    .bg(t.panel)
                    .child(Icon::new(Lucide::GitFork).size(px(12.)).text_color(t.tx3))
                    .child(
                        div()
                            .type_secondary()
                            .text_color(t.tx2)
                            .truncate()
                            .child(subagent_summary(list)),
                    ),
            )
            .children(rows)
            .when(more > 0, |this| {
                this.child(
                    div()
                        .px_2()
                        .py(px(5.))
                        .border_t_1()
                        .border_color(t.line)
                        .type_mono_small()
                        .text_color(t.tx3)
                        .child(format!("+{more} more")),
                )
            })
            .into_any_element()
    }

    pub(super) fn overview_card(&self, ws: &Workspace, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.tokens();
        let id = ws.id;
        let state = self.agent_state(id, cx);
        let live = self.live.sessions.get(&id);
        let project = self
            .work
            .projects
            .project(ws.project_id)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let mut meta = format!(
            "{project} · {} · {}",
            ws.branch,
            age_label(ws.created_at.as_millis(), Timestamp::now().as_millis())
        );
        if let Some(parent) = self.parent_name(ws) {
            meta.push_str(&format!(" · ↳ {parent}"));
        }
        let queued = self.queued_ahead(id);
        // A request that can be allowed once is answered right on the card.
        let request = (state == AgentState::Approval)
            .then(|| self.pending_request(id, cx))
            .flatten()
            .filter(|r| r.can_allow);
        let last = queued
            .map(|ahead| super::queue::queued_label(ahead).into())
            .or_else(|| {
                let label = self.device_wait_label(id, cx)?;
                Some(format!("{}{}", label[..1].to_uppercase(), &label[1..]).into())
            })
            .or_else(|| {
                request
                    .as_ref()
                    .map(|r| format!("Permission: {}", r.title).into())
            })
            .or_else(|| {
                self.question(id).map(|q| {
                    format!("Question: {}", super::questions::question_line(&q.text)).into()
                })
            })
            .or_else(|| live.and_then(|l| l.activity.last_line.clone()))
            .unwrap_or_else(|| {
                if live.is_some() {
                    "No output yet.".into()
                } else {
                    "No agent running.".into()
                }
            });
        let counts = self.diff_counts(id, cx);
        let (subagents, usage) = self
            .chat_view(id)
            .map(|v| {
                let chat = v.read(cx);
                (chat.subagents(), usage_label(chat.usage()))
            })
            .unwrap_or_default();
        let device = self
            .devices
            .pool
            .read(cx)
            .device_name(id)
            .map(str::to_owned);
        let attempts = self.attempts_of(id);
        let allow_inline = request.is_some();
        let asks = self.question(id).is_some();
        let (action_label, action) = card_action(
            state,
            live.is_some_and(|l| !l.session.status.is_terminal()),
            self.queued_ahead(id).is_some(),
        );
        let (action_fg, action_bg, action_border) = if card_action_tinted(state) {
            let (fg, bg) = state_colors(state, t);
            (fg, bg, bg)
        } else {
            (t.tx2, t.elev, t.line2)
        };
        card(cx)
            .id(SharedString::from(format!("overview-{id}")))
            .gap_2()
            .px_3()
            .py(px(10.))
            .cursor_pointer()
            .hover(|s| s.border_color(t.line2))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.select_workspace(id, window, cx);
                this.set_screen(Screen::Workspace, window, cx);
            }))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div()
                            .flex()
                            .w(px(14.))
                            .justify_center()
                            .child(StateGlyph::new(state)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .type_body_strong()
                            .child(ws.name.clone()),
                    )
                    .children(live.map(|l| AgentMark::new(l.session.agent.monogram()))),
            )
            .when(ws.title() != ws.name, |this| {
                this.child(
                    div()
                        .type_secondary()
                        .text_color(t.tx2)
                        .truncate()
                        .child(ws.title().to_owned()),
                )
            })
            .child(
                div()
                    .type_mono_small()
                    .text_color(t.tx3)
                    .truncate()
                    .child(meta),
            )
            .child(
                div()
                    .h(px(34.))
                    .px_2()
                    .py(px(6.))
                    .overflow_hidden()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(t.line)
                    .bg(t.term)
                    .font_mono()
                    .text_size(px(11.5))
                    .line_height(px(17.))
                    .text_color(t.tx2)
                    .truncate()
                    .child(last),
            )
            .when(!subagents.is_empty(), |this| {
                this.child(self.subagent_block(id, &subagents, cx))
            })
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .children(counts.map(|c| Delta::new(c.insertions, c.deletions)))
                    .when_some(device, |this, device| {
                        this.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .type_mono_small()
                                .text_color(t.tx3)
                                .child(format!("· {device}")),
                        )
                    })
                    .child(div().flex_1())
                    .when_some(usage, |this, usage| {
                        this.child(
                            div()
                                .flex_none()
                                .type_mono_small()
                                .text_color(t.tx3)
                                .child(usage),
                        )
                    })
                    .when_some(attempts, |this, (group, count)| {
                        this.child(
                            UiButton::new(SharedString::from(format!("overview-compare-{id}")))
                                .label(format!("Compare {count}"))
                                .ghost()
                                .small()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    cx.stop_propagation();
                                    this.open_compare(group, window, cx)
                                })),
                        )
                    })
                    .when(request.is_some(), |this| {
                        this.child(
                            UiButton::new(SharedString::from(format!("overview-deny-{id}")))
                                .label("Deny")
                                .ghost()
                                .small()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.answer_request(id, false, cx);
                                })),
                        )
                    })
                    .child(
                        div()
                            .id(SharedString::from(format!("overview-action-{id}")))
                            .flex()
                            .flex_none()
                            .h(px(24.))
                            .px_2()
                            .items_center()
                            .rounded(px(5.))
                            .border_1()
                            .border_color(action_border)
                            .bg(action_bg)
                            .text_color(action_fg)
                            .text_size(px(11.5))
                            .font_weight(FontWeight::MEDIUM)
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.85))
                            .child(if request.is_some() {
                                "Allow once"
                            } else if asks {
                                "Answer"
                            } else {
                                action_label
                            })
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                if allow_inline {
                                    this.answer_request(id, true, cx);
                                } else if asks {
                                    this.open_answer_sheet(id, window, cx);
                                } else {
                                    this.run_card_action(id, action, window, cx);
                                }
                            })),
                    ),
            )
            .into_any_element()
    }
}
