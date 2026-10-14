//! The "New workspace" sheet (⌘N): name, an optional task typed into the agent once it starts,
//! the agent and the target device. Enter in the name field or ⌘⏎ creates it; Escape or the
//! scrim cancels.

use gpui_kit::component::Sizable as _;
use gpui_kit::component::input::{Input, Textarea, TextareaState};
use gpui_kit::component::select::Select;

use hangar_core::slugify;

use super::*;
use crate::device_panel::{DeviceKey, Platform};
use crate::device_pool::backend;
use crate::ui::components::{AgentMark, Chip, Segmented, UiButton};
use crate::ui::{
    ActiveTokens as _, FONT_MONO, RADIUS_SHEET, TITLEBAR_HEIGHT, TypeScale as _, overlay_shadow,
};

#[derive(Clone, Debug, PartialEq)]
pub(super) struct TargetOption {
    id: SharedString,
    title: SharedString,
}

impl SelectItem for TargetOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.title.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.id
    }
}

pub(super) struct NewWorkspaceSheet {
    pub project_id: ProjectId,
    name: Entity<InputState>,
    task: Entity<TextareaState>,
    agent: SharedString,
    platform: Platform,
    base: Option<String>,
    /// Base devices per platform, once listed.
    bases: Option<HashMap<Platform, Vec<(String, String)>>>,
    device: Entity<SelectState<Vec<TargetOption>>>,
    _subscriptions: Vec<Subscription>,
}

/// `~/…` for paths under the home directory.
fn tilde(path: &std::path::Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if path.starts_with(&home) => {
            format!("~/{}", path.strip_prefix(&home).unwrap_or(path).display())
        }
        _ => path.display().to_string(),
    }
}

impl HangarApp {
    pub(super) fn open_new_workspace(
        &mut self,
        project_id: ProjectId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = cx.new(|cx| InputState::new(window, cx).placeholder("reorder-button"));
        let task = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Describe the task (optional)")
                .auto_grow(3, 8)
        });
        let device = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let subscriptions = vec![
            cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_new_workspace(window, cx);
                } else {
                    cx.notify();
                }
            }),
            cx.subscribe(&task, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(
                &device,
                |this, _, event: &SelectEvent<Vec<TargetOption>>, cx| {
                    if let SelectEvent::Confirm(Some(id)) = event
                        && let Some(sheet) = &mut this.new_workspace
                    {
                        sheet.base = Some(id.to_string());
                        cx.notify();
                    }
                },
            ),
        ];
        name.update(cx, |state, cx| state.focus(window, cx));
        let last = self.device_pool.read(cx).last_target().cloned();
        self.new_workspace = Some(NewWorkspaceSheet {
            project_id,
            name,
            task,
            agent: self.agent.clone(),
            platform: last.as_ref().map_or(Platform::Ios, |k| k.platform),
            base: None,
            bases: None,
            device,
            _subscriptions: subscriptions,
        });
        let repo = self
            .projects
            .project(project_id)
            .map(|p| p.repo_path.clone());
        cx.spawn_in(window, async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move {
                    let builds_for = repo
                        .map(|r| backend::stack_platforms(&r))
                        .unwrap_or_default();
                    let mut bases = HashMap::new();
                    for platform in Platform::ALL {
                        match backend::bases(platform) {
                            Ok(list) => {
                                bases.insert(platform, list);
                            }
                            Err(err) => {
                                tracing::warn!(
                                    platform = platform.label(),
                                    "no base devices: {err:#}"
                                );
                                bases.insert(platform, Vec::new());
                            }
                        }
                    }
                    (builds_for, bases)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let (builds_for, bases) = listed;
                let Some(sheet) = &mut this.new_workspace else {
                    return;
                };
                if sheet.project_id != project_id {
                    return;
                }
                let platform = crate::device_pool::choose_platform(
                    last.as_ref().map(|k| k.platform),
                    &builds_for,
                );
                sheet.bases = Some(bases);
                let preferred = last.filter(|k| k.platform == platform).map(|k| k.id);
                this.set_sheet_platform(platform, preferred, window, cx);
            });
        })
        .detach();
        cx.notify();
    }

    /// Shows `platform`'s base devices in the sheet and picks `preferred`, else the platform's
    /// base from Settings, else the first (a running one when there is one).
    fn set_sheet_platform(
        &mut self,
        platform: Platform,
        preferred: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let settings_base = self.device_pool.read(cx).base(platform).map(str::to_owned);
        let Some(sheet) = &mut self.new_workspace else {
            return;
        };
        sheet.platform = platform;
        let list = sheet
            .bases
            .as_ref()
            .and_then(|b| b.get(&platform))
            .cloned()
            .unwrap_or_default();
        let has = |id: &str| list.iter().any(|(known, _)| known == id);
        let base = preferred
            .filter(|id| has(id))
            .or(settings_base.filter(|id| has(id)))
            .or_else(|| list.first().map(|(id, _)| id.clone()));
        sheet.base = base.clone();
        let options: Vec<TargetOption> = list
            .into_iter()
            .map(|(id, title)| TargetOption {
                id: id.into(),
                title: title.into(),
            })
            .collect();
        sheet.device.update(cx, |select, cx| {
            select.set_items(options, window, cx);
            match base {
                Some(base) => select.set_selected_value(&SharedString::from(base), window, cx),
                None => select.set_selected_index(None, window, cx),
            }
        });
        cx.notify();
    }

    pub(super) fn close_new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.new_workspace.take().is_some() {
            window.focus(&self.focus, cx);
            self.focus_terminal(window, cx);
            cx.notify();
        }
    }

    pub(super) fn submit_new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = &self.new_workspace else {
            return;
        };
        let name = sheet.name.read(cx).value().trim().to_owned();
        if name.is_empty() || self.creating.contains(&sheet.project_id) {
            return;
        }
        let task = sheet.task.read(cx).value().trim().to_owned();
        let (project_id, agent) = (sheet.project_id, sheet.agent.to_string());
        let target = sheet.base.clone().map(|id| DeviceKey {
            platform: sheet.platform,
            id,
        });
        self.close_new_workspace(window, cx);
        let create = self.create_workspace(project_id, name, agent, target, window, cx);
        cx.spawn(async move |this, cx| {
            let id = create.await?;
            if task.is_empty() {
                return Some(());
            }
            this.update(cx, |this, cx| {
                let live = this.sessions.get(&id)?;
                if let Err(err) = live.send_text(&task, cx) {
                    tracing::error!("could not send the task to the agent: {err:#}");
                }
                Some(())
            })
            .ok()
            .flatten()
        })
        .detach();
    }

    pub(super) fn render_new_workspace_sheet(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.new_workspace.as_ref()?;
        let t = cx.tokens();
        let project = self.projects.project(sheet.project_id)?;
        let name = sheet.name.read(cx).value().trim().to_owned();
        let slug = slugify(&name);
        let preview = if slug.is_empty() {
            "Name the workspace to create its branch and worktree.".to_owned()
        } else {
            let path = paths::worktrees_root()
                .map(|root| tilde(&paths::worktree_path(&root, &slugify(&project.name), &slug)))
                .unwrap_or_default();
            format!("{} · {path}", paths::branch_name(&slug))
        };
        let creating = self.creating.contains(&sheet.project_id);
        let can_create = !slug.is_empty() && !creating;
        let label = |text: &'static str| {
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.tx2)
                .child(text)
        };
        let agents = self.agent_choices.iter().map(|choice| {
            let selected = sheet.agent.as_ref() == choice.id;
            let id = SharedString::from(choice.id.clone());
            let installed = choice.installed;
            div()
                .id(SharedString::from(format!("sheet-agent-{}", choice.id)))
                .flex()
                .flex_col()
                .gap_1p5()
                .px(px(10.))
                .py_2()
                .rounded(px(6.))
                .border_1()
                .border_color(if selected { t.acc } else { t.line2 })
                .bg(if selected { t.accbg } else { t.app })
                .when(!installed, |this| this.opacity(0.5))
                .when(installed && !selected, |this| {
                    this.cursor_pointer()
                        .hover(|s| s.border_color(t.tx3))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(sheet) = &mut this.new_workspace {
                                sheet.agent = id.clone();
                                cx.notify();
                            }
                        }))
                })
                .child(
                    h_flex()
                        .gap_1p5()
                        .items_center()
                        .child(AgentMark::new(agents::kind(&choice.id).monogram()))
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(choice.name.clone()),
                        ),
                )
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_size(px(11.))
                        .text_color(t.tx3)
                        .child(if installed { "installed" } else { "not found" }),
                )
        });
        let panel = v_flex()
            .id("new-workspace-sheet")
            .key_context("NewWorkspaceSheet")
            .w(px(640.))
            .bg(t.elev)
            .border_1()
            .border_t_0()
            .border_color(t.line2)
            .rounded_b(RADIUS_SHEET)
            .shadow(overlay_shadow())
            .text_color(t.tx)
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_action(cx.listener(|this, _: &SubmitSheet, window, cx| {
                this.submit_new_workspace(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &CloseSheet, window, cx| {
                    this.close_new_workspace(window, cx)
                }),
            )
            .child(
                h_flex()
                    .h(px(44.))
                    .px_4()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(t.line)
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("New workspace"),
                    )
                    .child(Chip::new(project.name.clone()))
                    .child(div().flex_1())
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(10.))
                            .text_color(t.tx3)
                            .child("esc"),
                    ),
            )
            .child(
                v_flex()
                    .p_4()
                    .gap_4()
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(label("Name"))
                            .child(Input::new(&sheet.name)),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(label("Task"))
                            .child(Textarea::new(&sheet.task))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(t.tx3)
                                    .child("Typed into the agent once it starts."),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(label("Agent"))
                            .child(div().grid().grid_cols(4).gap_1p5().children(agents)),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(label("Target device"))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        Segmented::new(
                                            "sheet-platform",
                                            Platform::ALL.map(Platform::label),
                                        )
                                        .selected(
                                            Platform::ALL
                                                .iter()
                                                .position(|p| *p == sheet.platform)
                                                .unwrap_or(0),
                                        )
                                        .on_select(
                                            cx.listener(|this, i: &usize, window, cx| {
                                                if let Some(p) = Platform::ALL.get(*i) {
                                                    this.set_sheet_platform(*p, None, window, cx);
                                                }
                                            }),
                                        ),
                                    )
                                    .child(div().flex_1().min_w_0().child(
                                        Select::new(&sheet.device).small().placeholder(
                                            if sheet.bases.is_some() {
                                                "No device of this platform"
                                            } else {
                                                "Listing devices…"
                                            },
                                        ),
                                    )),
                            )
                            .child(div().text_size(px(11.)).text_color(t.tx3).child(
                                "The workspace gets its own device made from this one, \
                                     booted when it is first needed.",
                            )),
                    ),
            )
            .child(
                h_flex()
                    .h(px(52.))
                    .px_4()
                    .gap_2()
                    .items_center()
                    .border_t_1()
                    .border_color(t.line)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .type_mono_small()
                            .text_color(t.tx3)
                            .child(preview),
                    )
                    .child(
                        UiButton::new("sheet-cancel")
                            .label("Cancel")
                            .ghost()
                            .large()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.close_new_workspace(window, cx)
                            })),
                    )
                    .child(
                        UiButton::new("sheet-create")
                            .label(if creating {
                                "Creating…"
                            } else {
                                "Create workspace"
                            })
                            .kbd("⌘⏎")
                            .primary()
                            .large()
                            .disabled(!can_create)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.submit_new_workspace(window, cx)
                            })),
                    ),
            );
        Some(
            div()
                .id("new-workspace-scrim")
                .absolute()
                .top(TITLEBAR_HEIGHT)
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .justify_center()
                .items_start()
                .bg(t.scrim)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_new_workspace(window, cx)),
                )
                .child(panel)
                .into_any_element(),
        )
    }
}
