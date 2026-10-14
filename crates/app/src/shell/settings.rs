//! Settings (⌘,): agents, devices, appearance and a read-only list of the keybindings.

mod agents_tab;
mod devices_tab;
mod setup_tab;
mod usage_tab;

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
use hangar_core::device::Pressure;

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
    Setup,
    Agents,
    Devices,
    Usage,
    Appearance,
    Keybindings,
}

impl SettingsTab {
    pub(super) const ALL: [SettingsTab; 6] = [
        Self::Setup,
        Self::Agents,
        Self::Devices,
        Self::Usage,
        Self::Appearance,
        Self::Keybindings,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Setup => "Setup",
            Self::Agents => "Agents & providers",
            Self::Devices => "Devices",
            Self::Usage => "Usage & cost",
            Self::Appearance => "Appearance",
            Self::Keybindings => "Keybindings",
        }
    }

    fn icon(self) -> Lucide {
        match self {
            Self::Setup => Lucide::Wrench,
            Self::Agents => Lucide::Bot,
            Self::Devices => Lucide::Smartphone,
            Self::Usage => Lucide::ChartColumn,
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
    ("Browse files full width", "cmd-shift-e", "Workspace"),
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
    ("Back to workspace", "escape", "Files"),
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
    pub(super) usage: usage_tab::UsageUi,
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
                usage: usage_tab::UsageUi::new(window, cx),
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
    // Typed key caps match in any modifier order (`⌘⇧R` finds `⇧⌘R`).
    let typed_caps = compact.chars().any(|c| "⌘⇧⌥⌃⏎".contains(c))
        && compact.chars().all(|c| caps.contains(c))
        && compact.chars().count() == caps.chars().count();
    label.to_lowercase().contains(&query)
        || context.to_lowercase().contains(&query)
        || keys.contains(&query)
        || caps.contains(&compact)
        || typed_caps
}

/// `cmd-shift-r` → `["⌘", "⇧", "R"]`.
pub(super) fn key_caps(keystroke: &str) -> Vec<String> {
    let parts: Vec<&str> = keystroke.split('-').collect();
    let (mods, key) = match parts.split_last() {
        // `cmd--` (minus) splits into an empty last part.
        Some((&"", rest)) if !rest.is_empty() => (&rest[..rest.len() - 1], "-"),
        Some((key, mods)) => (mods, *key),
        None => (&[][..], ""),
    };
    // macOS order: ⌃ ⌥ ⇧ ⌘, then the key (as menus and gpui's own hints show them).
    let mut caps: Vec<String> = [("ctrl", "⌃"), ("alt", "⌥"), ("shift", "⇧"), ("cmd", "⌘")]
        .into_iter()
        .filter(|(name, _)| mods.contains(name))
        .map(|(_, cap)| cap.to_owned())
        .collect();
    caps.push(match key {
        "enter" => "⏎".to_owned(),
        "escape" => "esc".to_owned(),
        other => other.to_uppercase(),
    });
    caps
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
        if tab == SettingsTab::Usage {
            self.load_usage(window, cx);
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
            .child(self.render_terminal_prefs(cx))
    }

    fn render_terminal_prefs(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let prefs = self.layout.terminal;
        let row = |title: &'static str, detail: &'static str| {
            h_flex()
                .px(px(14.))
                .py_3()
                .gap_3()
                .items_center()
                .justify_between()
                .child(
                    v_flex()
                        .gap_0p5()
                        .child(title)
                        .child(div().type_secondary().text_color(t.tx3).child(detail)),
                )
        };
        v_flex()
            .gap(px(10.))
            .child(div().type_body_strong().child("Terminal"))
            .child(
                card(cx)
                    .child(
                        row(
                            "Use Option as Meta",
                            "Option+key sends ESC and the key (Emacs, readline) instead of \
                             typing a special character.",
                        )
                        .child(
                            Switch::new("option-as-meta")
                                .small()
                                .color(t.acc)
                                .checked(prefs.option_as_meta)
                                .on_click(cx.listener(move |this, on: &bool, window, cx| {
                                    let prefs = TerminalPrefs {
                                        option_as_meta: *on,
                                        ..this.layout.terminal
                                    };
                                    this.set_terminal_prefs(prefs, window, cx)
                                })),
                        ),
                    )
                    .child(
                        row(
                            "Let programs copy to the clipboard",
                            "Programs in a terminal may replace the clipboard (OSC 52), up \
                             to 1 MB at a time.",
                        )
                        .child(
                            Switch::new("clipboard-write")
                                .small()
                                .color(t.acc)
                                .checked(prefs.clipboard_write)
                                .on_click(cx.listener(move |this, on: &bool, window, cx| {
                                    let prefs = TerminalPrefs {
                                        clipboard_write: *on,
                                        ..this.layout.terminal
                                    };
                                    this.set_terminal_prefs(prefs, window, cx)
                                })),
                        ),
                    ),
            )
    }

    /// Options for a new terminal view of `program`, with the Settings › Appearance switches.
    pub(super) fn terminal_options(&self, program: &std::path::Path) -> TerminalViewOptions {
        TerminalViewOptions {
            option_as_meta: self.layout.terminal.option_as_meta,
            clipboard_write: self.layout.terminal.clipboard_write,
            ..TerminalViewOptions::for_program(program)
        }
    }

    fn set_terminal_prefs(
        &mut self,
        prefs: TerminalPrefs,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.layout.terminal = prefs;
        let views = self
            .live
            .sessions
            .values()
            .filter_map(|live| match &live.surface {
                AgentSurface::Terminal { view, .. } => Some(view.clone()),
                AgentSurface::Chat { .. } => None,
            })
            .chain(
                self.live
                    .runs
                    .values()
                    .flat_map(|run| run.server.iter().chain(&run.build))
                    .map(|tab| tab.view.clone()),
            )
            .collect::<Vec<_>>();
        for view in views {
            view.update(cx, |view, _| {
                view.set_input_options(prefs.option_as_meta, prefs.clipboard_write)
            });
        }
        self.persist(window, cx, move |store| {
            ops::save_flag(store, &setting_keys::OPTION_AS_META, prefs.option_as_meta)?;
            ops::save_flag(store, &setting_keys::CLIPBOARD_WRITE, prefs.clipboard_write)
        });
        cx.notify();
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
            SettingsTab::Setup => self.render_settings_setup(cx),
            SettingsTab::Agents => self.render_settings_agents(cx),
            SettingsTab::Devices => self.render_settings_devices(cx),
            SettingsTab::Usage => self.render_settings_usage(cx),
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
        assert_eq!(key_caps("cmd-shift-r"), ["⇧", "⌘", "R"]);
        assert_eq!(key_caps("escape"), ["esc"]);
        assert_eq!(key_caps("cmd-,"), ["⌘", ","]);
        assert_eq!(key_caps("cmd-|"), ["⌘", "|"]);
        assert_eq!(key_caps("cmd-alt-shift-p"), ["⌥", "⇧", "⌘", "P"]);
    }

    /// The keybindings page lists exactly what `init` binds.
    #[test]
    fn shortcuts_match_init() {
        let module = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shell.rs"),
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
