//! Settings (⌘,): agents, devices, appearance and a read-only list of the keybindings.

use std::cell::Cell;
use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::input::{Input, NumberInput};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Icon, Sizable as _};

use gpui_kit::component::tooltip::Tooltip;
use hangar_agents::acp::approval::ApprovalLevel;
use hangar_devices::capacity::Pressure;

use super::new_workspace::tilde;
use super::*;
use crate::device_panel::Platform;
use crate::device_pool::policy::{self, Limit};
use crate::ui::components::{AgentMark, CountBadge, Segmented, UiButton, card, kbd_keys};
use crate::ui::theme::Appearance;
use crate::ui::{ActiveTokens as _, RADIUS_CONTROL, ROW_HEIGHT, TypeScale as _};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum SettingsTab {
    #[default]
    Agents,
    Devices,
    Appearance,
    Keybindings,
}

impl SettingsTab {
    pub(super) const ALL: [SettingsTab; 4] = [
        Self::Agents,
        Self::Devices,
        Self::Appearance,
        Self::Keybindings,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Agents => "Agents & providers",
            Self::Devices => "Devices",
            Self::Appearance => "Appearance",
            Self::Keybindings => "Keybindings",
        }
    }

    fn icon(self) -> Lucide {
        match self {
            Self::Agents => Lucide::Bot,
            Self::Devices => Lucide::Smartphone,
            Self::Appearance => Lucide::SunMoon,
            Self::Keybindings => Lucide::Keyboard,
        }
    }
}

/// Every key binding of the shell: label, keystroke (as bound in `init`), context. A test keeps
/// it in step with `init`.
pub(super) const SHORTCUTS: &[(&str, &str, &str)] = &[
    ("Command palette", "cmd-k", "Global"),
    ("New workspace", "cmd-n", "Global"),
    ("Overview", "cmd-shift-o", "Global"),
    ("Next workspace needing you", "cmd-j", "Global"),
    ("Settings", "cmd-,", "Global"),
    ("Filter workspaces", "cmd-p", "Global"),
    ("Add project", "cmd-o", "Global"),
    ("Toggle sidebar", "cmd-b", "Global"),
    ("Quit Hangar (stops running agents)", "cmd-q", "Global"),
    ("Review changes", "cmd-shift-r", "Workspace"),
    ("Build and run on the device", "cmd-r", "Workspace"),
    ("Stop the run", "cmd-.", "Workspace"),
    ("Split pane right", "cmd-d", "Workspace"),
    ("Split pane right (alternate)", "cmd-\\", "Workspace"),
    ("Split pane down", "cmd-|", "Workspace"),
    ("Focus pane 1", "cmd-alt-1", "Workspace"),
    ("Focus pane 2", "cmd-alt-2", "Workspace"),
    ("Focus pane 3", "cmd-alt-3", "Workspace"),
    ("Focus pane 4", "cmd-alt-4", "Workspace"),
    ("Close pane", "cmd-w", "Workspace"),
    ("Right panel: Device", "cmd-1", "Workspace"),
    ("Right panel: Changes", "cmd-2", "Workspace"),
    ("Right panel: Files", "cmd-3", "Workspace"),
    ("Toggle right panel (last tab)", "cmd-shift-d", "Workspace"),
    ("Toggle device tab", "cmd-shift-p", "Workspace"),
    ("Device stats overlay", "cmd-alt-shift-p", "Device"),
    ("Screenshot to the agent", "cmd-s", "Device panel"),
    ("Device Home button", "cmd-shift-h", "Device panel"),
    ("Back to workspace", "escape", "Review"),
    ("Back to workspace", "escape", "Overview"),
    ("Back to workspace", "escape", "Settings"),
    ("Create workspace", "cmd-enter", "New workspace sheet"),
    ("Cancel", "escape", "New workspace sheet"),
    ("Close", "escape", "Compare attempts"),
    ("Send answer", "cmd-enter", "Answer sheet"),
    ("Answer later", "escape", "Answer sheet"),
];

/// How long `<agent> --version` may take before it is given up on.
const VERSION_TIMEOUT: Duration = Duration::from_secs(3);

/// View state of the settings screen that outlives a render.
pub(super) struct SettingsUi {
    keys_filter: Entity<InputState>,
    /// Agent id → version printed by `--version`, read once per run.
    versions: HashMap<String, String>,
    versions_started: Cell<bool>,
    /// The saved default approval level, once read.
    approval: Option<ApprovalLevel>,
    approval_started: Cell<bool>,
    /// "Max concurrent agents" (see `queue.rs`).
    pub(super) max_agents: Entity<InputState>,
    _max_agents_sub: Subscription,
    /// The settings page's scroll position.
    pub(super) scroll: ScrollHandle,
}

impl SettingsUi {
    pub(super) fn new(window: &mut Window, cx: &mut Context<HangarApp>) -> (Self, Subscription) {
        let keys_filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search actions or shortcuts"));
        let sub = cx.subscribe(&keys_filter, |_, _, _: &InputEvent, cx| cx.notify());
        let (max_agents, max_agents_sub) = HangarApp::max_agents_input(window, cx);
        (
            Self {
                keys_filter,
                versions: HashMap::new(),
                versions_started: Cell::new(false),
                approval: None,
                approval_started: Cell::new(false),
                max_agents,
                _max_agents_sub: max_agents_sub,
                scroll: ScrollHandle::new(),
            },
            sub,
        )
    }
}

/// The first version-looking word of `--version` output (`2.1.4 (Claude Code)` → `2.1.4`,
/// `zsh 5.9 (arm64-apple-darwin24.0)` → `5.9`, `v0.3.1` → `0.3.1`).
pub(super) fn parse_version(output: &str) -> Option<String> {
    output
        .lines()
        .find(|l| !l.trim().is_empty())?
        .split_whitespace()
        .find_map(|word| {
            let word = word.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            let word = word.strip_prefix('v').unwrap_or(word);
            (word.starts_with(|c: char| c.is_ascii_digit()) && word.contains('.'))
                .then(|| word.to_owned())
        })
}

/// Runs `<program> --version` with `path` as `PATH`; `None` when it fails, prints no version or
/// outlasts [`VERSION_TIMEOUT`]. Blocking: call it off the UI thread.
fn program_version(program: &Path, path: Option<&str>) -> Option<String> {
    let mut command = Command::new(program);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    if let Some(path) = path {
        command.env("PATH", path);
    }
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("agent-version".into())
        .spawn(move || {
            let mut out = String::new();
            let _ = stdout.read_to_string(&mut out);
            let _ = tx.send(out);
        })
        .ok()?;
    let out = rx.recv_timeout(VERSION_TIMEOUT).ok();
    let _ = child.kill();
    let _ = child.wait();
    parse_version(&out?)
}

/// Whether a SHORTCUTS row matches the keybindings search: by action, context or keys (`cmd-k`,
/// `⌘K`).
pub(super) fn shortcut_matches(label: &str, keys: &str, context: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return true;
    }
    let caps = key_caps(keys).concat().to_lowercase();
    let compact: String = query.chars().filter(|c| !c.is_whitespace()).collect();
    label.to_lowercase().contains(&query)
        || context.to_lowercase().contains(&query)
        || keys.contains(&query)
        || caps.contains(&compact)
}

/// `cmd-shift-r` → `["⌘", "⇧", "R"]`.
pub(super) fn key_caps(keystroke: &str) -> Vec<String> {
    keystroke
        .split('-')
        .flat_map(|part| match part {
            "cmd" => vec!["⌘".to_owned()],
            "shift" => vec!["⇧".to_owned()],
            "alt" => vec!["⌥".to_owned()],
            "ctrl" => vec!["⌃".to_owned()],
            "enter" => vec!["⏎".to_owned()],
            "escape" => vec!["esc".to_owned()],
            // macOS reports ⇧\ as `|`; show the keys the user presses.
            "|" => vec!["⇧".to_owned(), "\\".to_owned()],
            other => vec![other.to_uppercase()],
        })
        .collect()
}

impl HangarApp {
    pub(super) fn set_appearance(
        &mut self,
        appearance: Appearance,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout.appearance = appearance;
        appearance.apply(window, cx);
        self.persist(window, cx, move |store| {
            ops::save_appearance(store, appearance)
        });
        cx.notify();
    }

    pub(super) fn open_settings_tab(
        &mut self,
        tab: SettingsTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout.settings_tab = tab;
        if tab == SettingsTab::Agents
            && self.agent_setup.ollama.settings.enabled
            && self.agent_setup.ollama.status != ollama::OllamaStatus::Loading
        {
            self.refresh_ollama(window, cx);
        }
        if tab == SettingsTab::Devices && !self.device_panel.read(cx).picker().listed() {
            self.device_panel
                .update(cx, |panel, cx| panel.refresh(window, cx));
        }
        cx.notify();
    }

    fn settings_heading(&self, title: &'static str, hint: &'static str, cx: &App) -> Div {
        let t = cx.tokens();
        v_flex()
            .gap_1()
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(div().type_secondary().text_color(t.tx2).child(hint))
    }

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

    fn render_settings_agents(&self, cx: &mut Context<Self>) -> Div {
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

    /// "Devices at once", the detected capacity and memory pressure.
    fn render_device_capacity(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let pool = self.device_pool.read(cx);
        let limit = pool.limit();
        let selected = match limit {
            Limit::Auto => 0,
            Limit::Manual(n) => n as usize,
        };
        let ios = pool.recommended(Platform::Ios);
        let android = pool.recommended(Platform::Android);
        let android_base = pool.base(Platform::Android).map(str::to_owned);
        let host = pool.host().map(policy::host_summary);
        let detected = match &host {
            Some(host) => format!(
                "Detected: {host} · room for {ios} iOS simulator{} and {android} Android \
                 emulator{}{}",
                if ios == 1 { "" } else { "s" },
                if android == 1 { "" } else { "s" },
                android_base.map(|b| format!(" ({b})")).unwrap_or_default()
            ),
            None => "Detecting this Mac's memory and cores…".to_owned(),
        };
        let warning = (limit.exceeds(ios) || limit.exceeds(android)).then(|| {
            format!(
                "More than this Mac can comfortably run ({}: {ios} iOS / {android} Android \
                 recommended). Boots are still refused under memory pressure.",
                host.clone().unwrap_or_default()
            )
        });
        let pressure = pool.pressure();
        let pressure_color = match pressure {
            Some(Pressure::Normal) => t.grn,
            Some(Pressure::Warn) => t.amb,
            Some(Pressure::Critical) => t.red,
            None => t.tx3,
        };
        let booted = pool.booted_count();
        let options = std::iter::once("Auto".to_owned())
            .chain((1..=hangar_devices::capacity::MAX_SLOTS).map(|n| n.to_string()));
        card(cx)
            .child(
                v_flex()
                    .px(px(14.))
                    .py_3()
                    .gap_2()
                    .items_start()
                    .border_b_1()
                    .border_color(t.line)
                    .child(v_flex().gap_0p5().child("Devices at once").child(
                        div().type_secondary().text_color(t.tx3).child(
                            "Booted at the same time. At the limit the least recently used \
                                 idle device shuts down; with 1, workspaces share one device.",
                        ),
                    ))
                    .child(
                        Segmented::new("device-limit", options)
                            .selected(selected)
                            .on_select(cx.listener(|this, i: &usize, _, cx| {
                                let limit = match *i {
                                    0 => Limit::Auto,
                                    n => Limit::Manual(n as u32),
                                };
                                this.device_pool
                                    .update(cx, |pool, cx| pool.set_limit(limit, cx));
                            })),
                    ),
            )
            .child(
                v_flex()
                    .px(px(14.))
                    .py(px(10.))
                    .gap_1p5()
                    .type_mono_small()
                    .text_color(t.tx2)
                    .child(detected)
                    .when_some(warning, |this, warning| {
                        this.child(div().text_color(t.amb).child(warning))
                    })
                    .child(
                        h_flex()
                            .gap_1p5()
                            .items_center()
                            .child(div().size(px(6.)).rounded_full().bg(pressure_color))
                            .child(format!(
                                "Memory pressure: {} · {booted} pool device{} running",
                                pressure.map_or("unknown".to_owned(), |p| p.to_string()),
                                if booted == 1 { "" } else { "s" }
                            )),
                    ),
            )
    }

    /// Devices the pool manages: workspaces' own devices and shared base devices.
    fn render_pool_devices(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let rows: Vec<AnyElement> = self
            .device_pool
            .read(cx)
            .rows()
            .into_iter()
            .map(|row| {
                let running = matches!(row.state, "open" | "running");
                let color = if running { t.grn } else { t.tx3 };
                let id = SharedString::from(format!("{:?}", row.key));
                let shutdown_key = row.key.clone();
                let delete_key = row.key.clone();
                h_flex()
                    .h(px(36.))
                    .px(px(14.))
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(t.line)
                    .text_size(px(12.))
                    .child(
                        Icon::new(Lucide::Smartphone)
                            .size(px(13.))
                            .text_color(t.tx3),
                    )
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(row.name.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .type_mono_small()
                                    .text_color(t.tx3)
                                    .child(format!(
                                        "{} · {}",
                                        row.platform.label(),
                                        if row.workspaces.is_empty() {
                                            "no workspace".to_owned()
                                        } else {
                                            row.workspaces.join(", ")
                                        }
                                    )),
                            ),
                    )
                    .child(
                        h_flex()
                            .w(px(110.))
                            .gap_1p5()
                            .items_center()
                            .font_mono()
                            .text_size(px(11.))
                            .text_color(color)
                            .child(div().size(px(6.)).rounded_full().bg(color))
                            .child(if row.busy && running {
                                "in use"
                            } else {
                                row.state
                            }),
                    )
                    .child(
                        UiButton::new(SharedString::from(format!("pool-shutdown-{id}")))
                            .label("Shut down")
                            .small()
                            .disabled(!running || row.busy)
                            .tooltip(if row.busy {
                                "A run or an agent is using it"
                            } else {
                                "Shut the device down"
                            })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let key = shutdown_key.clone();
                                this.device_pool
                                    .update(cx, |pool, cx| pool.shutdown(&key, cx))
                                    .detach();
                            })),
                    )
                    .when_some(row.clone_udid.clone(), |this, _| {
                        this.child(
                            UiButton::new(SharedString::from(format!("pool-delete-{id}")))
                                .label("Delete")
                                .small()
                                .disabled(row.busy)
                                .tooltip("Delete this simulator; the workspace gets a new one when it needs it")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let key = delete_key.clone();
                                    let task = this
                                        .device_pool
                                        .update(cx, |pool, cx| pool.delete_clone(&key, cx));
                                    cx.spawn_in(window, async move |this, cx| {
                                        if let Err(err) = task.await {
                                            let _ = this.update_in(cx, |this, window, cx| {
                                                this.report(&err, window, cx)
                                            });
                                        }
                                    })
                                    .detach();
                                })),
                        )
                    })
                    .into_any_element()
            })
            .collect();
        let empty = rows.is_empty();
        v_flex()
            .gap_2()
            .child(div().type_body_strong().child("Workspace devices"))
            .child(card(cx).children(rows).when(empty, |this| {
                this.child(
                    div()
                        .px(px(14.))
                        .py_3()
                        .text_size(px(12.))
                        .text_color(t.tx3)
                        .child("None yet: a workspace gets its device when it first runs."),
                )
            }))
    }

    fn render_settings_devices(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let callout = self
            .device_pool
            .read(cx)
            .single_device_explanation()
            .map(|text| {
                h_flex()
                    .gap_2()
                    .items_start()
                    .px(px(14.))
                    .py(px(10.))
                    .rounded(px(6.))
                    .bg(t.ambbg)
                    .border_1()
                    .border_color(t.amb)
                    .text_size(px(12.))
                    .child(
                        Icon::new(Lucide::TriangleAlert)
                            .size(px(14.))
                            .text_color(t.amb),
                    )
                    .child(div().flex_1().child(text))
            });
        let capacity = self.render_device_capacity(cx);
        let pool_devices = self.render_pool_devices(cx);
        let bases: Vec<Option<String>> = Platform::ALL
            .iter()
            .map(|p| self.device_pool.read(cx).base(*p).map(str::to_owned))
            .collect();
        let picker = self.device_panel.read(cx).picker();
        let groups = Platform::ALL.map(|platform| {
            let base = bases
                .get(
                    Platform::ALL
                        .iter()
                        .position(|p| *p == platform)
                        .unwrap_or(0),
                )
                .cloned()
                .flatten();
            let mut seen = HashSet::new();
            let rows: Vec<AnyElement> =
                picker
                    .entries()
                    .iter()
                    .filter(|e| e.key.platform == platform)
                    .filter(|e| {
                        !e.info
                            .name
                            .starts_with(crate::device_pool::backend::CLONE_PREFIX)
                    })
                    .filter(|e| seen.insert(e.key.clone()))
                    .map(|entry| {
                        let base_key = entry.key.clone();
                        let boot_key = entry.key.clone();
                        let can_boot = entry.key.platform.can_boot(&entry.info);
                        let is_base = base.as_deref() == Some(entry.key.id.as_str());
                        let ready = entry.ready;
                        let color = if ready { t.grn } else { t.tx3 };
                        h_flex()
                            .h(px(36.))
                            .px(px(14.))
                            .gap_3()
                            .items_center()
                            .border_b_1()
                            .border_color(t.line)
                            .text_size(px(12.))
                            .child(
                                Icon::new(Lucide::Smartphone)
                                    .size(px(13.))
                                    .text_color(t.tx3),
                            )
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_2()
                                    .items_center()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(entry.info.name.clone()),
                                    )
                                    .when(is_base, |this| this.child(CountBadge::new("Base"))),
                            )
                            .child(
                                div()
                                    .w(px(110.))
                                    .type_mono_small()
                                    .text_color(t.tx2)
                                    .truncate()
                                    .child(entry.info.runtime.clone()),
                            )
                            .child(
                                h_flex()
                                    .w(px(90.))
                                    .gap_1p5()
                                    .items_center()
                                    .font_mono()
                                    .text_size(px(11.))
                                    .text_color(color)
                                    .child(div().size(px(6.)).rounded_full().bg(color))
                                    .child(entry.info.state.to_lowercase()),
                            )
                            .when(can_boot, |this| {
                                this.child(
                                    UiButton::new(SharedString::from(format!(
                                        "settings-boot-{}",
                                        entry.key.value()
                                    )))
                                    .label("Boot")
                                    .small()
                                    .tooltip("Boot this device and open it in the Device panel")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let key = boot_key.clone();
                                        this.device_panel.update(cx, |panel, cx| {
                                            panel.boot_device(key, window, cx)
                                        });
                                    })),
                                )
                            })
                            .child(
                                UiButton::new(SharedString::from(format!(
                                    "settings-device-{}",
                                    entry.key.value()
                                )))
                                .label("Make base")
                                .small()
                                .disabled(is_base)
                                .tooltip(
                                    "New workspaces of this platform get their own device made \
                                 from this one",
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        let key = base_key.clone();
                                        this.device_pool.update(cx, |pool, cx| {
                                            pool.set_platform_base(key.platform, key.id, cx)
                                        });
                                    },
                                )),
                            )
                            .into_any_element()
                    })
                    .collect();
            let empty = rows.is_empty();
            v_flex()
                .gap_2()
                .child(div().type_body_strong().child(platform.label()))
                .child(card(cx).children(rows).when(empty, |this| {
                    this.child(
                        div()
                            .px(px(14.))
                            .py_3()
                            .text_size(px(12.))
                            .text_color(t.tx3)
                            .child(if picker.listed() {
                                "None found."
                            } else {
                                "Listing…"
                            }),
                    )
                }))
        });
        let errors = picker.errors().iter().map(|(platform, err)| {
            div()
                .text_size(px(11.))
                .text_color(t.red)
                .child(format!("{}: {err}", platform.label()))
        });
        v_flex()
            .gap_4()
            .child(
                h_flex()
                    .items_start()
                    .child(div().flex_1().child(self.settings_heading(
                        "Devices",
                        "Each workspace runs on its own simulator or emulator, made from a base \
                         device and booted when needed.",
                        cx,
                    )))
                    .child(
                        UiButton::new("settings-devices-refresh")
                            .icon(Lucide::RefreshCw)
                            .label("Refresh")
                            .disabled(picker.is_listing())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.device_panel
                                    .update(cx, |panel, cx| panel.refresh(window, cx))
                            })),
                    ),
            )
            .children(callout)
            .child(capacity)
            .child(pool_devices)
            .child(
                div()
                    .type_body_strong()
                    .text_color(t.tx2)
                    .child("Base devices"),
            )
            .children(errors)
            .children(groups)
    }

    fn render_settings_appearance(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let selected = Appearance::ALL
            .iter()
            .position(|a| *a == self.layout.appearance)
            .unwrap_or(0);
        v_flex()
            .gap_4()
            .child(self.settings_heading("Appearance", "Theme of the whole window.", cx))
            .child(
                card(cx).child(
                    h_flex()
                        .px(px(14.))
                        .py_3()
                        .items_center()
                        .justify_between()
                        .child(
                            v_flex().gap_0p5().child("Theme").child(
                                div()
                                    .type_secondary()
                                    .text_color(t.tx3)
                                    .child("System follows the macOS appearance."),
                            ),
                        )
                        .child(
                            Segmented::new("appearance", Appearance::ALL.map(|a| a.label()))
                                .selected(selected)
                                .on_select(cx.listener(|this, i: &usize, window, cx| {
                                    if let Some(a) = Appearance::ALL.get(*i) {
                                        this.set_appearance(*a, window, cx);
                                    }
                                })),
                        ),
                ),
            )
    }

    fn render_settings_keys(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let filter = &self.settings_ui.keys_filter;
        let query = filter.read(cx).value().to_string();
        let rows: Vec<Div> = SHORTCUTS
            .iter()
            .filter(|(label, keys, context)| shortcut_matches(label, keys, context, &query))
            .map(|(label, keys, context)| {
                h_flex()
                    .h(px(32.))
                    .px(px(14.))
                    .gap_3()
                    .items_center()
                    .border_b_1()
                    .border_color(t.line)
                    .text_size(px(12.))
                    .child(div().flex_1().child(*label))
                    .child(
                        div()
                            .w(px(160.))
                            .text_size(px(11.))
                            .text_color(t.tx3)
                            .child(*context),
                    )
                    .child(
                        h_flex()
                            .w(px(110.))
                            .justify_end()
                            .child(kbd_keys(key_caps(keys))),
                    )
            })
            .collect();
        let empty = rows.is_empty();
        v_flex()
            .gap_4()
            .child(self.settings_heading(
                "Keybindings",
                "Every action is reachable from the keyboard.",
                cx,
            ))
            .child(
                Input::new(filter)
                    .small()
                    .prefix(Icon::new(IconName::Search).size(px(12.)).text_color(t.tx3)),
            )
            .child(card(cx).children(rows).when(empty, |this| {
                this.child(
                    div()
                        .px(px(14.))
                        .py_3()
                        .text_size(px(12.))
                        .text_color(t.tx3)
                        .child("No shortcut matches."),
                )
            }))
    }

    pub(super) fn render_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let nav = SettingsTab::ALL.map(|tab| {
            let on = tab == self.layout.settings_tab;
            div()
                .id(SharedString::from(format!("settings-nav-{}", tab.label())))
                .flex()
                .items_center()
                .gap_2()
                .h(ROW_HEIGHT)
                .px_2()
                .rounded(RADIUS_CONTROL)
                .text_size(px(13.))
                .text_color(if on { t.tx } else { t.tx2 })
                .when(on, |this| this.bg(t.active))
                .when(!on, |this| {
                    this.cursor_pointer()
                        .hover(|s| s.bg(t.hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_settings_tab(tab, window, cx)
                        }))
                })
                .child(Icon::new(tab.icon()).size(px(14.)).text_color(t.tx3))
                .child(tab.label())
        });
        let page = match self.layout.settings_tab {
            SettingsTab::Agents => self.render_settings_agents(cx),
            SettingsTab::Devices => self.render_settings_devices(cx),
            SettingsTab::Appearance => self.render_settings_appearance(cx),
            SettingsTab::Keybindings => self.render_settings_keys(cx),
        };
        h_flex()
            .flex_1()
            .min_h_0()
            .child(
                v_flex()
                    .w(px(220.))
                    .flex_none()
                    .h_full()
                    .gap(px(2.))
                    .px_2()
                    .py_3()
                    .bg(t.panel)
                    .border_r_1()
                    .border_color(t.line)
                    .child(
                        div()
                            .px_2()
                            .pb_1p5()
                            .type_label()
                            .text_color(t.tx3)
                            .child("SETTINGS"),
                    )
                    .children(nav),
            )
            .child(
                div()
                    .id("settings-page")
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.settings_ui.scroll)
                    .bg(t.app)
                    .child(div().max_w(px(820.)).px_10().py_7().child(page)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{SHORTCUTS, key_caps, parse_version, shortcut_matches};

    #[test]
    fn versions() {
        assert_eq!(
            parse_version("2.1.4 (Claude Code)\n").as_deref(),
            Some("2.1.4")
        );
        assert_eq!(
            parse_version("zsh 5.9 (arm64-apple-darwin24.0)").as_deref(),
            Some("5.9")
        );
        assert_eq!(parse_version("\nv0.3.1").as_deref(), Some("0.3.1"));
        assert_eq!(parse_version("codex-cli 0.46.0").as_deref(), Some("0.46.0"));
        assert_eq!(parse_version("usage: foo [options]"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn shortcut_search() {
        assert!(shortcut_matches("Command palette", "cmd-k", "Global", ""));
        assert!(shortcut_matches(
            "Command palette",
            "cmd-k",
            "Global",
            "PALETTE"
        ));
        assert!(shortcut_matches(
            "Command palette",
            "cmd-k",
            "Global",
            "cmd-k"
        ));
        assert!(shortcut_matches("Command palette", "cmd-k", "Global", "⌘K"));
        assert!(shortcut_matches(
            "Review changes",
            "cmd-shift-r",
            "Workspace",
            "⌘ ⇧ r"
        ));
        assert!(shortcut_matches(
            "Cancel",
            "escape",
            "New workspace sheet",
            "sheet"
        ));
        assert!(!shortcut_matches(
            "Command palette",
            "cmd-k",
            "Global",
            "⌘J"
        ));
    }

    #[test]
    fn caps() {
        assert_eq!(key_caps("cmd-shift-r"), ["⌘", "⇧", "R"]);
        assert_eq!(key_caps("escape"), ["esc"]);
        assert_eq!(key_caps("cmd-,"), ["⌘", ","]);
        assert_eq!(key_caps("cmd-|"), ["⌘", "⇧", "\\"]);
    }

    /// The keybindings page lists exactly what `init` binds.
    #[test]
    fn shortcuts_match_init() {
        let module = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shell/mod.rs"),
        )
        .unwrap();
        // Source text: an escaped backslash (`\\`) is one character at runtime.
        let mut bound: Vec<String> = module
            .match_indices("KeyBinding::new(\"")
            .map(|(at, m)| {
                let rest = &module[at + m.len()..];
                rest[..rest.find('"').unwrap()].replace("\\\\", "\\")
            })
            .collect();
        let mut listed: Vec<String> = SHORTCUTS.iter().map(|(_, k, _)| k.to_string()).collect();
        bound.sort_unstable();
        listed.sort_unstable();
        assert_eq!(bound, listed);
    }
}
