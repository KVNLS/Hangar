//! The "New workspace" sheet (⌘N): name, the base branch and the new branch's name, an optional
//! task typed into the agent once it starts, the agent and the target device. Enter in the name
//! field or ⌘⏎ creates it; Escape or the scrim cancels. ⌘-clicking agents runs the task with each
//! (best of N, grouped to compare); "One workspace per line" makes a workspace per task line.

use gpui_kit::component::Sizable as _;
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, Textarea, TextareaState};
use gpui_kit::component::select::Select;

use hangar_core::slugify;
use hangar_usecases::batch::{self, BatchError, BatchRequest};

use super::*;
use crate::device_panel::{DeviceKey, Platform};
use crate::device_pool::backend;
use crate::ui::components::{AgentMark, Chip, Segmented, UiButton};
use crate::ui::{ActiveTokens as _, RADIUS_SHEET, TITLEBAR_HEIGHT, TypeScale as _, overlay_shadow};

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
    /// The branch to start from; the project's default base until the user picks another.
    base_branch: Entity<SelectState<Vec<TargetOption>>>,
    /// The new branch's name; empty means `hangar/<slug>`.
    branch: Entity<InputState>,
    task: Entity<TextareaState>,
    /// The agents to run, in the order picked; several run the same task side by side.
    agents: Vec<SharedString>,
    /// Each task line becomes its own workspace.
    per_line: bool,
    platform: Platform,
    base: Option<String>,
    /// Base devices per platform, once listed.
    bases: Option<HashMap<Platform, Vec<(String, String)>>>,
    device: Entity<SelectState<Vec<TargetOption>>>,
    _subscriptions: Vec<Subscription>,
}

/// `~/…` for paths under the home directory.
pub(super) fn tilde(path: &std::path::Path) -> String {
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
        let default_base = self
            .work
            .projects
            .project(project_id)
            .map(|p| p.default_base.clone())
            .unwrap_or_default();
        let base_branch = cx.new(|cx| {
            let only = vec![TargetOption {
                id: default_base.clone().into(),
                title: default_base.clone().into(),
            }];
            SelectState::new(only, None, window, cx).searchable(true)
        });
        base_branch.update(cx, |select, cx| {
            select.set_selected_value(&SharedString::from(default_base.clone()), window, cx)
        });
        let branch = cx.new(|cx| InputState::new(window, cx).placeholder("hangar/<name>"));
        let subscriptions = vec![
            cx.subscribe_in(&name, window, |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_new_workspace(window, cx);
                } else {
                    if let Some(sheet) = &this.work.new_workspace {
                        let slug = slugify(sheet.name.read(cx).value().trim());
                        let auto = if slug.is_empty() {
                            "hangar/<name>".to_owned()
                        } else {
                            hangar_core::branch_name(&slug)
                        };
                        sheet
                            .branch
                            .update(cx, |input, cx| input.set_placeholder(auto, window, cx));
                    }
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &branch,
                window,
                |this, _, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.submit_new_workspace(window, cx);
                    } else {
                        cx.notify();
                    }
                },
            ),
            cx.subscribe(
                &base_branch,
                |_, _, _: &SelectEvent<Vec<TargetOption>>, cx| cx.notify(),
            ),
            cx.subscribe(&task, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe(
                &device,
                |this, _, event: &SelectEvent<Vec<TargetOption>>, cx| {
                    if let SelectEvent::Confirm(Some(id)) = event
                        && let Some(sheet) = &mut this.work.new_workspace
                    {
                        sheet.base = Some(id.to_string());
                        cx.notify();
                    }
                },
            ),
        ];
        name.update(cx, |state, cx| state.focus(window, cx));
        let last = self.device_pool.read(cx).last_target().cloned();
        self.work.new_workspace = Some(NewWorkspaceSheet {
            project_id,
            name,
            base_branch,
            branch,
            task,
            agents: vec![self.agent_setup.agent.clone()],
            per_line: false,
            platform: last.as_ref().map_or(Platform::Ios, |k| k.platform),
            base: None,
            bases: None,
            device,
            _subscriptions: subscriptions,
        });
        let repo = self
            .work
            .projects
            .project(project_id)
            .map(|p| p.repo_path.clone());
        cx.spawn_in(window, async move |this, cx| {
            let listed = cx
                .background_executor()
                .spawn(async move {
                    let builds_for = repo
                        .as_ref()
                        .map(|r| backend::stack_platforms(r))
                        .unwrap_or_default();
                    let branches = repo
                        .as_ref()
                        .map(|r| crate::adapters::version_control().branch_tips(r))
                        .transpose()
                        .unwrap_or_else(|err| {
                            tracing::warn!("cannot list branches: {err:#}");
                            None
                        })
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
                    (builds_for, bases, branches)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                let (builds_for, bases, branches) = listed;
                let Some(sheet) = &mut this.work.new_workspace else {
                    return;
                };
                if sheet.project_id != project_id {
                    return;
                }
                // The default base first (it may be a remote branch such as `origin/main`).
                let now = hangar_core::Timestamp::now().as_millis();
                let title = |name: &str| -> SharedString {
                    match branches.iter().find(|tip| tip.name == name) {
                        Some(tip) => format!(
                            "{name} · {} · {}",
                            tip.short_sha,
                            super::status::age_label(tip.committed_at.saturating_mul(1000), now)
                        )
                        .into(),
                        None => name.to_owned().into(),
                    }
                };
                let options: Vec<TargetOption> = std::iter::once(default_base.clone())
                    .chain(
                        branches
                            .iter()
                            .map(|tip| tip.name.clone())
                            .filter(|b| *b != default_base),
                    )
                    .map(|b| TargetOption {
                        title: title(&b),
                        id: b.into(),
                    })
                    .collect();
                sheet.base_branch.update(cx, |select, cx| {
                    let keep = select.selected_value().cloned();
                    select.set_items(options, window, cx);
                    let value = keep.unwrap_or_else(|| default_base.clone().into());
                    select.set_selected_value(&value, window, cx);
                });
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
        let Some(sheet) = &mut self.work.new_workspace else {
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
        if self.work.new_workspace.take().is_some() {
            window.focus(&self.focus, cx);
            self.focus_terminal(window, cx);
            cx.notify();
        }
    }

    /// What the sheet asks for, as a batch (one workspace in the common case).
    fn sheet_request(&self, sheet: &NewWorkspaceSheet, cx: &App) -> BatchRequest {
        let label = |id: &str| -> String {
            let name = self
                .agent_setup
                .agent_choices
                .iter()
                .find(|c| c.id == id)
                .map_or(id, |c| c.name.as_ref());
            name.trim_end_matches(" (structured)").to_owned()
        };
        BatchRequest {
            name: sheet.name.read(cx).value().to_string(),
            task: sheet.task.read(cx).value().to_string(),
            agents: sheet
                .agents
                .iter()
                .map(|id| (id.to_string(), label(id)))
                .collect(),
            per_line: sheet.per_line,
            base: sheet
                .base_branch
                .read(cx)
                .selected_value()
                .map(|b| b.to_string()),
            branch: Some(sheet.branch.read(cx).value().trim().to_owned()).filter(|b| !b.is_empty()),
        }
    }

    pub(super) fn submit_new_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = &self.work.new_workspace else {
            return;
        };
        if self.work.creating.contains(&sheet.project_id) {
            return;
        }
        let Ok(planned) = batch::plan(&self.sheet_request(sheet, cx)) else {
            return;
        };
        let project_id = sheet.project_id;
        let target = sheet.base.clone().map(|id| DeviceKey {
            platform: sheet.platform,
            id,
        });
        self.close_new_workspace(window, cx);
        if planned.len() > 1 {
            tracing::info!(count = planned.len(), "creating several workspaces");
        }
        // One at a time: creates in one project are serialized (`WorkState::creating`).
        cx.spawn_in(window, async move |this, cx| {
            for item in planned {
                let target = target.clone();
                let Ok(create) = this.update_in(cx, |this, window, cx| {
                    this.create_workspace(
                        project_id,
                        item.workspace,
                        item.agent,
                        target,
                        window,
                        cx,
                    )
                }) else {
                    break;
                };
                create.await;
            }
        })
        .detach();
    }

    /// A plain click picks one agent; ⌘-click adds or removes one (never the last).
    fn pick_sheet_agent(&mut self, id: SharedString, toggle: bool, cx: &mut Context<Self>) {
        let Some(sheet) = &mut self.work.new_workspace else {
            return;
        };
        if !toggle {
            sheet.agents = vec![id];
        } else if let Some(i) = sheet.agents.iter().position(|a| *a == id) {
            if sheet.agents.len() > 1 {
                sheet.agents.remove(i);
            }
        } else {
            sheet.agents.push(id);
        }
        cx.notify();
    }

    pub(super) fn render_new_workspace_sheet(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.work.new_workspace.as_ref()?;
        let t = cx.tokens();
        let project = self.work.projects.project(sheet.project_id)?;
        let name = sheet.name.read(cx).value().trim().to_owned();
        let slug = slugify(&name);
        let planned = batch::plan(&self.sheet_request(sheet, cx));
        let count = planned.as_ref().map_or(0, Vec::len);
        let preview = if let Err(err) = &planned
            && !matches!(err, BatchError::EmptyName)
        {
            format!("{}.", capitalized(&err.to_string()))
        } else if count > 1 {
            let base = sheet
                .base_branch
                .read(cx)
                .selected_value()
                .map_or_else(|| project.default_base.clone(), |b| b.to_string());
            let groups = planned
                .iter()
                .flatten()
                .filter_map(|p| p.workspace.group)
                .collect::<HashSet<_>>()
                .len();
            let compare = match groups {
                0 => String::new(),
                1 => " · one task to compare".to_owned(),
                n => format!(" · {n} tasks to compare"),
            };
            format!("{count} workspaces from {base}, each on its own branch{compare}")
        } else if slug.is_empty() {
            "Name the workspace to create its branch and worktree.".to_owned()
        } else {
            let path = paths::worktrees_root()
                .map(|root| {
                    tilde(&hangar_core::worktree_path(
                        &root,
                        &slugify(&project.name),
                        &slug,
                    ))
                })
                .unwrap_or_default();
            let typed = sheet.branch.read(cx).value().trim().to_owned();
            let branch = if typed.is_empty() {
                hangar_core::branch_name(&slug)
            } else {
                typed
            };
            let base = sheet
                .base_branch
                .read(cx)
                .selected_value()
                .map_or_else(|| project.default_base.clone(), |b| b.to_string());
            format!("{branch} from {base} · {path}")
        };
        let creating = self.work.creating.contains(&sheet.project_id);
        let can_create = count > 0 && !creating;
        let label = |text: &'static str| {
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.tx2)
                .child(text)
        };
        let agents = self.agent_setup.agent_choices.iter().map(|choice| {
            let selected = sheet.agents.iter().any(|a| a.as_ref() == choice.id);
            let id = SharedString::from(choice.id.clone());
            let installed = choice.installed;
            let structured = matches!(
                agents::preset(&choice.id),
                Some(agents::AgentPreset::Acp(_))
            );
            let ollama = agents::agent_profile(&choice.id).is_some_and(|p| p.ollama);
            let subtitle = match (structured, ollama) {
                (true, true) => "Structured · Ollama",
                (true, false) => "Structured (ACP)",
                (false, true) => "Terminal · Ollama",
                (false, false) => "Terminal",
            };
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
                .when(installed, |this| {
                    this.cursor_pointer()
                        .when(!selected, |this| this.hover(|s| s.border_color(t.tx3)))
                        .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                            let toggle = event.modifiers().platform;
                            this.pick_sheet_agent(id.clone(), toggle, cx)
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
                        .text_size(px(11.))
                        .text_color(t.tx2)
                        .truncate()
                        .child(subtitle),
                )
                .child(
                    div()
                        .font_mono()
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
                            .font_mono()
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
                        div()
                            .grid()
                            .grid_cols(2)
                            .gap_3()
                            .child(
                                v_flex()
                                    .gap_1p5()
                                    .child(label("Base branch"))
                                    .child(Select::new(&sheet.base_branch).small()),
                            )
                            .child(
                                v_flex()
                                    .gap_1p5()
                                    .child(label("New branch"))
                                    .child(Input::new(&sheet.branch).small()),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(label("Task"))
                            .child(Textarea::new(&sheet.task))
                            .child(
                                h_flex()
                                    .gap_3()
                                    .items_center()
                                    .child(
                                        div()
                                            .flex_1()
                                            .text_size(px(11.))
                                            .text_color(t.tx3)
                                            .child("Typed into the agent once it starts."),
                                    )
                                    .child(
                                        Checkbox::new("sheet-per-line")
                                            .label("One workspace per line")
                                            .small()
                                            .checked(sheet.per_line)
                                            .on_click(cx.listener(
                                                |this, checked: &bool, _, cx| {
                                                    if let Some(sheet) =
                                                        &mut this.work.new_workspace
                                                    {
                                                        sheet.per_line = *checked;
                                                        cx.notify();
                                                    }
                                                },
                                            )),
                                    ),
                            ),
                    )
                    .child(
                        v_flex()
                            .gap_1p5()
                            .child(label("Agent"))
                            .child(div().grid().grid_cols(4).gap_1p5().children(agents))
                            .child(div().text_size(px(11.)).text_color(t.tx3).child(
                                "⌘-click to run the task with several agents and compare them.",
                            )),
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
                            .label(match (creating, count) {
                                (true, _) => "Creating…".to_owned(),
                                (false, 0 | 1) => "Create workspace".to_owned(),
                                (false, n) => format!("Create {n} workspaces"),
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

/// `text` with its first letter upper-case (error messages are lower-case).
fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}
