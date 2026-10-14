//! Settings (⌘,): agents, devices, appearance and a read-only list of the keybindings.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::Icon;

use hangar_devices::capacity::Pressure;

use super::*;
use crate::device_panel::Platform;
use crate::device_pool::policy::{self, Limit};
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
    ("Split pane right", "cmd-d", "Workspace"),
    ("Close pane", "cmd-w", "Workspace"),
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
            let program = match agents::preset(&choice.id) {
                Some(agents::AgentPreset::Acp(acp))
                    if detected && !choice.installed && !acp.install_hint.is_empty() =>
                {
                    format!("{} · {}", acp.program, acp.install_hint)
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
                            .font_family(FONT_MONO)
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
            let rows: Vec<AnyElement> = picker
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
