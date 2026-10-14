//! Settings › Agents: installed agents and their versions, permissions, approval mode.

use super::*;

impl HangarApp {
    /// Reads every found agent's version once, in the background.
    fn load_agent_versions(&self, cx: &mut Context<Self>) {
        if self.agent_setup.path.is_none() || self.settings_ui.versions_started.replace(true) {
            return;
        }
        let path = self.agent_setup.path.as_ref().map(|p| p.value.clone());
        let programs: Vec<(String, std::path::PathBuf)> = self
            .agent_setup
            .agent_choices
            .iter()
            .filter_map(|c| Some((c.id.clone(), c.path.clone()?)))
            .collect();
        cx.spawn(async move |this, cx| {
            let tasks: Vec<_> = programs
                .into_iter()
                .map(|(id, program)| {
                    let path = path.clone();
                    cx.background_executor()
                        .spawn(async move { (id, program_version(&program, path.as_deref())) })
                })
                .collect();
            for task in tasks {
                let (id, version) = task.await;
                let Some(version) = version else { continue };
                let _ = this.update(cx, |this, cx| {
                    this.settings_ui.versions.insert(id, version);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(super) fn render_settings_agents(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        self.load_agent_versions(cx);
        let detected = self.agent_setup.path.is_some();
        let rows = self.agent_setup.agent_choices.iter().map(|choice| {
            let id = SharedString::from(choice.id.clone());
            let is_default = self.agent_setup.agent.as_ref() == choice.id;
            let program = match agents::preset(&choice.id) {
                Some(agents::AgentPreset::Acp(acp))
                    if detected && !choice.installed && !acp.install_hint.is_empty() =>
                {
                    format!("{} · {}", acp.program, acp.install_hint)
                }
                _ if choice.path.is_some() => {
                    let path = choice.path.as_deref().map(tilde).unwrap_or_default();
                    match self.settings_ui.versions.get(&choice.id) {
                        Some(version) => format!("{path} · {version}"),
                        None => path,
                    }
                }
                Some(preset) => preset.program().to_owned(),
                None => String::new(),
            };
            h_flex()
                .gap_3()
                .px(px(14.))
                .py(px(10.))
                .items_center()
                .border_b_1()
                .border_color(t.line)
                .child(AgentMark::new(agents::kind(&choice.id).monogram()).large())
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_center()
                                .child(div().type_body_strong().child(choice.name.clone()))
                                .when(is_default, |this| this.child(CountBadge::new("Default"))),
                        )
                        .child(
                            div()
                                .type_mono_small()
                                .text_color(t.tx3)
                                .truncate()
                                .child(program),
                        ),
                )
                .child(
                    div()
                        .w(px(110.))
                        .font_mono()
                        .text_size(px(11.))
                        .text_color(if detected && choice.installed {
                            t.grn
                        } else {
                            t.tx3
                        })
                        .child(match (detected, choice.installed) {
                            (false, _) => "checking…",
                            (true, true) => "found",
                            (true, false) => "not on PATH",
                        }),
                )
                .child(if is_default {
                    // The badge next to the name already says so.
                    div().into_any_element()
                } else {
                    UiButton::new(SharedString::from(format!("default-{}", choice.id)))
                        .label("Make default")
                        .small()
                        .disabled(!choice.installed)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.agent_setup.agent = id.clone();
                            this.agent_setup.agent_chosen_by_user = true;
                            let agent = this.agent_setup.agent.clone();
                            this.agent_setup.agent_select.update(cx, |select, cx| {
                                select.set_selected_value(&agent, window, cx)
                            });
                            this.save_agent(window, cx);
                            cx.notify();
                        }))
                        .into_any_element()
                })
        });
        v_flex()
            .gap_4()
            .child(self.settings_heading(
                "Agents & providers",
                "CLIs Hangar can launch in a workspace. Each runs in its own worktree and PTY.",
                cx,
            ))
            .child(card(cx).children(rows))
            .child(self.render_ollama_card(cx))
            .child(self.render_permissions_card(cx))
    }

    /// "Permissions": what agents may do and when Hangar calls the user back. Each row is its
    /// own card, as in the design.
    fn render_permissions_card(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let approval = self.render_approval_mode_row(cx);
        let max_agents = self.render_max_agents_row(cx);
        let row = |title: &'static str, hint: &'static str| {
            card(cx)
                .flex_row()
                .px(px(14.))
                .py_3()
                .gap_3()
                .items_center()
                .justify_between()
                .child(
                    v_flex()
                        .gap_0p5()
                        .min_w_0()
                        .child(title)
                        .child(div().type_secondary().text_color(t.tx3).child(hint)),
                )
        };
        v_flex()
            .gap(px(10.))
            .child(div().type_body_strong().child("Permissions"))
            .child(approval)
            .child(max_agents)
            .child(
                row(
                    "Notify when an agent needs me",
                    "Approval, input and failure. Dock badge shows count.",
                )
                .child(
                    Switch::new("notify-attention")
                        .small()
                        .color(t.acc)
                        .checked(self.attention.enabled)
                        .on_click(cx.listener(|this, on: &bool, window, cx| {
                            this.set_notify_attention(*on, window, cx)
                        })),
                ),
            )
            .child(
                row(
                    "Let agents create and drive workspaces",
                    "Agents get tools to start child workspaces, prompt them and read them.",
                )
                .child(
                    Switch::new("orchestration")
                        .small()
                        .color(t.acc)
                        .checked(self.agent_setup.orchestration)
                        .on_click(cx.listener(|this, on: &bool, window, cx| {
                            this.set_orchestration(*on, window, cx)
                        })),
                ),
            )
    }

    /// "Max concurrent agents": starts beyond it queue (see `queue.rs`).
    fn render_max_agents_row(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        card(cx).child(
            h_flex()
                .px(px(14.))
                .py_3()
                .gap_3()
                .items_center()
                .justify_between()
                .child(
                    v_flex().gap_0p5().child("Max concurrent agents").child(
                        div()
                            .type_secondary()
                            .text_color(t.tx3)
                            .child("Extra workspaces queue as Idle."),
                    ),
                )
                .child(
                    NumberInput::new(&self.settings_ui.max_agents)
                        .small()
                        .flex_none()
                        .w(px(96.)),
                ),
        )
    }

    /// Reads the saved approval level once, in the background.
    fn load_approval_level(&self, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        if self.settings_ui.approval_started.replace(true) {
            return;
        }
        cx.spawn(async move |this, cx| {
            let level = cx
                .background_executor()
                .spawn(async move { ops::approval_level(&*store) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.settings_ui.approval.get_or_insert(level);
                cx.notify();
            });
        })
        .detach();
    }

    fn set_approval_level(
        &mut self,
        level: ApprovalLevel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings_ui.approval = Some(level);
        // Running chats answer reads at the new level; modes change with the next session.
        for live in self.live.sessions.values() {
            if let AgentSurface::Chat { view, .. } = &live.surface {
                view.update(cx, |chat, _| chat.set_approval(level));
            }
        }
        self.persist(window, cx, move |store| {
            ops::save_approval_level(store, level)
        });
        cx.notify();
    }

    /// "Default approval mode": the mode structured agents start in.
    fn render_approval_mode_row(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        self.load_approval_level(cx);
        let level = self.settings_ui.approval.unwrap_or_default();
        let selected = ApprovalLevel::ALL
            .iter()
            .position(|l| *l == level)
            .unwrap_or(0);
        card(cx).child(
            h_flex()
                .px(px(14.))
                .py_3()
                .gap_4()
                .items_center()
                .justify_between()
                .child(
                    v_flex()
                        .id("approval-mode-help")
                        .gap_0p5()
                        .tooltip(|window, cx| {
                            Tooltip::new(
                                "Structured agents only: terminal agents keep their own \
                                 settings. Applied when an agent starts. Auto-approve reads \
                                 keeps the agent's asking mode and Hangar allows read and \
                                 search requests once. Full auto uses the agent's bypass mode \
                                 (Claude Code: Bypass permissions, Codex: Full Access, Gemini: \
                                 YOLO); OpenCode has none and keeps its own permissions.",
                            )
                            .build(window, cx)
                        })
                        .child("Default approval mode")
                        .child(
                            div()
                                .type_secondary()
                                .text_color(t.tx3)
                                .child("Agents can override per workspace."),
                        ),
                )
                .child(
                    Segmented::new("approval-mode", ApprovalLevel::ALL.map(|l| l.label()))
                        .selected(selected)
                        .on_select(cx.listener(|this, i: &usize, window, cx| {
                            if let Some(level) = ApprovalLevel::ALL.get(*i) {
                                this.set_approval_level(*level, window, cx);
                            }
                        })),
                ),
        )
    }
}
