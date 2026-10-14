//! Settings (⌘,): agents, devices, appearance and a read-only list of the keybindings.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::Icon;

use super::*;
use crate::device_panel::Platform;
use crate::ui::components::{AgentMark, CountBadge, Segmented, UiButton, card, kbd_keys};
use crate::ui::theme::Appearance;
use crate::ui::{ActiveTokens as _, FONT_MONO, RADIUS_CONTROL, ROW_HEIGHT, TypeScale as _};

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
            Self::Agents => "Agents",
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
    ("Review changes", "cmd-shift-r", "Workspace"),
    ("Build and run on the device", "cmd-r", "Workspace"),
    ("Stop the run", "cmd-.", "Workspace"),
    ("Right panel: Device", "cmd-1", "Workspace"),
    ("Right panel: Changes", "cmd-2", "Workspace"),
    ("Right panel: Files", "cmd-3", "Workspace"),
    ("Toggle changes panel", "cmd-shift-d", "Workspace"),
    ("Toggle device panel", "cmd-shift-p", "Workspace"),
    ("Device stats overlay", "cmd-alt-shift-p", "Device"),
    ("Back to workspace", "escape", "Review"),
    ("Create workspace", "cmd-enter", "New workspace sheet"),
    ("Cancel", "escape", "New workspace sheet"),
];

/// `cmd-shift-r` → `["⌘", "⇧", "R"]`.
pub(super) fn key_caps(keystroke: &str) -> Vec<String> {
    keystroke
        .split('-')
        .map(|part| match part {
            "cmd" => "⌘".to_owned(),
            "shift" => "⇧".to_owned(),
            "alt" => "⌥".to_owned(),
            "ctrl" => "⌃".to_owned(),
            "enter" => "⏎".to_owned(),
            "escape" => "esc".to_owned(),
            other => other.to_uppercase(),
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
        self.appearance = appearance;
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
        self.settings_tab = tab;
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

    fn render_settings_agents(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let detected = self.path.is_some();
        let rows = self.agent_choices.iter().map(|choice| {
            let id = SharedString::from(choice.id.clone());
            let is_default = self.agent.as_ref() == choice.id;
            let program = agents::preset(&choice.id)
                .map(|p| p.program.clone())
                .unwrap_or_default();
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
                        .font_family(FONT_MONO)
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
                .child(
                    UiButton::new(SharedString::from(format!("default-{}", choice.id)))
                        .label("Make default")
                        .small()
                        .disabled(is_default || !choice.installed)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.agent = id.clone();
                            this.agent_chosen_by_user = true;
                            let agent = this.agent.clone();
                            this.agent_select.update(cx, |select, cx| {
                                select.set_selected_value(&agent, window, cx)
                            });
                            this.save_agent(window, cx);
                            cx.notify();
                        })),
                )
        });
        v_flex()
            .gap_4()
            .child(self.settings_heading(
                "Agents",
                "CLIs Hangar can launch in a workspace. Each runs in its own worktree and terminal.",
                cx,
            ))
            .child(card(cx).children(rows))
    }

    fn render_settings_devices(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let picker = self.device_panel.read(cx).picker();
        let groups = Platform::ALL.map(|platform| {
            let rows: Vec<AnyElement> = picker
                .entries()
                .iter()
                .filter(|e| e.key.platform == platform)
                .map(|entry| {
                    let key = entry.key.clone();
                    let ready = entry.ready;
                    let can_boot = !ready && platform.can_boot(&entry.info);
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
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_weight(FontWeight::MEDIUM)
                                .child(entry.info.name.clone()),
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
                                .font_family(FONT_MONO)
                                .text_size(px(11.))
                                .text_color(color)
                                .child(div().size(px(6.)).rounded_full().bg(color))
                                .child(entry.info.state.to_lowercase()),
                        )
                        .child(
                            UiButton::new(SharedString::from(format!(
                                "settings-device-{}",
                                entry.key.value()
                            )))
                            .label(if ready { "Show" } else { "Boot" })
                            .small()
                            .disabled(!ready && !can_boot)
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    let key = key.clone();
                                    this.device_panel.update(cx, |panel, cx| {
                                        if ready {
                                            panel.select_device(key, window, cx)
                                        } else {
                                            panel.boot_device(key, window, cx)
                                        }
                                    });
                                    if ready {
                                        this.set_screen(Screen::Workspace, window, cx);
                                        this.set_device_open(true, window, cx);
                                    }
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
                        "Simulators and emulators available to workspaces.",
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
            .children(errors)
            .children(groups)
    }

    fn render_settings_appearance(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let selected = Appearance::ALL
            .iter()
            .position(|a| *a == self.appearance)
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
        let rows = SHORTCUTS.iter().map(|(label, keys, context)| {
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
        });
        v_flex()
            .gap_4()
            .child(self.settings_heading(
                "Keybindings",
                "Every action is reachable from the keyboard.",
                cx,
            ))
            .child(card(cx).children(rows))
    }

    pub(super) fn render_settings(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let nav = SettingsTab::ALL.map(|tab| {
            let on = tab == self.settings_tab;
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
        let page = match self.settings_tab {
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
                    .bg(t.app)
                    .child(div().max_w(px(820.)).px_10().py_7().child(page)),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{SHORTCUTS, key_caps};

    #[test]
    fn caps() {
        assert_eq!(key_caps("cmd-shift-r"), ["⌘", "⇧", "R"]);
        assert_eq!(key_caps("escape"), ["esc"]);
        assert_eq!(key_caps("cmd-,"), ["⌘", ","]);
    }

    /// The keybindings page lists exactly what `init` binds.
    #[test]
    fn shortcuts_match_init() {
        let module = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/shell/mod.rs"),
        )
        .unwrap();
        let mut bound: Vec<&str> = module
            .match_indices("KeyBinding::new(\"")
            .map(|(at, m)| {
                let rest = &module[at + m.len()..];
                &rest[..rest.find('"').unwrap()]
            })
            .collect();
        let mut listed: Vec<&str> = SHORTCUTS.iter().map(|(_, k, _)| *k).collect();
        bound.sort_unstable();
        listed.sort_unstable();
        assert_eq!(bound, listed);
    }
}
