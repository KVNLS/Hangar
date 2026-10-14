//! First run: no projects yet. Add one, see which agents and devices Hangar found.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::Icon;

use super::*;
use crate::device_panel::Platform;
use crate::ui::components::{AgentMark, UiButton, card};
use crate::ui::{ActiveTokens as _, TypeScale as _};

impl HangarApp {
    pub(super) fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let step = |marker: AnyElement, title: &'static str, body: Div| {
            h_flex()
                .items_start()
                .gap(px(14.))
                .p_4()
                .child(marker)
                .child(
                    v_flex()
                        .flex_1()
                        .gap_1p5()
                        .child(div().type_body_strong().child(title))
                        .child(body),
                )
        };
        let number = |n: &'static str| {
            div()
                .flex()
                .flex_none()
                .size(px(20.))
                .items_center()
                .justify_center()
                .rounded_full()
                .border_1()
                .border_color(t.acc)
                .text_color(t.acc)
                .font_mono()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .child(n)
                .into_any_element()
        };
        let done = || {
            div()
                .flex()
                .flex_none()
                .size(px(20.))
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(t.grn)
                .child(Icon::new(Lucide::Check).size(px(12.)).text_color(t.panel))
                .into_any_element()
        };
        let detected = self.path.is_some();
        let agents = v_flex()
            .gap_1()
            .children(self.agent_choices.iter().map(|c| {
                h_flex()
                    .h(px(24.))
                    .gap(px(10.))
                    .items_center()
                    .text_size(px(12.))
                    .child(AgentMark::new(agents::kind(&c.id).monogram()))
                    .child(div().flex_1().child(c.name.clone()))
                    .child(
                        div()
                            .font_mono()
                            .text_size(px(11.))
                            .text_color(match (detected, c.installed) {
                                (false, _) => t.tx3,
                                (true, true) => t.grn,
                                (true, false) => t.tx3,
                            })
                            .child(match (detected, c.installed) {
                                (false, _) => "checking…",
                                (true, true) => "found",
                                (true, false) => "not on PATH",
                            }),
                    )
            }));
        let picker = self.device_panel.read(cx).picker();
        let devices = if picker.listed() {
            v_flex().gap_1().children(Platform::ALL.map(|platform| {
                let entries: Vec<_> = picker
                    .entries()
                    .iter()
                    .filter(|e| e.key.platform == platform)
                    .collect();
                let ready = entries.iter().filter(|e| e.ready).count();
                h_flex()
                    .justify_between()
                    .text_size(px(12.))
                    .text_color(t.tx2)
                    .child(format!(
                        "{} · {} device{}",
                        platform.label(),
                        entries.len(),
                        if entries.len() == 1 { "" } else { "s" }
                    ))
                    .child(
                        div()
                            .font_mono()
                            .text_size(px(11.))
                            .text_color(t.tx3)
                            .child(format!("{ready} running")),
                    )
            }))
        } else {
            v_flex().child(
                div()
                    .text_size(px(12.))
                    .text_color(t.tx2)
                    .child("Simulators and emulators are listed in the Device tab (⌘1)."),
            )
        };
        let single = self
            .device_pool
            .read(cx)
            .single_device_explanation()
            .map(|text| div().text_size(px(12.)).text_color(t.amb).child(text));
        let devices = devices.children(single);
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .justify_center()
            .bg(t.app)
            .child(
                v_flex()
                    .w(px(560.))
                    .gap_6()
                    .child(
                        v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .flex()
                                    .size(px(40.))
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(9.))
                                    .bg(t.acc)
                                    .text_color(t.accfg)
                                    .text_size(px(18.))
                                    .font_weight(FontWeight::BOLD)
                                    .child("H"),
                            )
                            .child(div().mt_2().text_size(px(22.)).font_weight(FontWeight::SEMIBOLD).child("Welcome to Hangar"))
                            .child(div().text_color(t.tx2).child(
                                "Run many coding agents against one repo, each in its own worktree, with a live simulator next to them.",
                            )),
                    )
                    .child(
                        card(cx)
                            .child(
                                step(
                                    number("1"),
                                    "Add a mobile project",
                                    v_flex()
                                        .gap(px(10.))
                                        .child(div().type_secondary().text_color(t.tx2).child(
                                            "A Git repository: Xcode, Gradle, React Native or Expo.",
                                        ))
                                        .child(
                                            h_flex().child(
                                                UiButton::new("welcome-open")
                                                    .icon(Lucide::FolderOpen)
                                                    .label("Open folder…")
                                                    .kbd("⌘O")
                                                    .primary()
                                                    .large()
                                                    .on_click(cx.listener(|this, _, window, cx| this.pick_project(window, cx))),
                                            ),
                                        ),
                                )
                                .border_b_1()
                                .border_color(t.line),
                            )
                            .child(
                                step(if detected { done() } else { number("2") }, "Agents", agents)
                                    .border_b_1()
                                    .border_color(t.line),
                            )
                            .child(step(if picker.listed() { done() } else { number("3") }, "Devices", devices)),
                    )
                    .child(
                        h_flex()
                            .gap_4()
                            .text_size(px(12.))
                            .text_color(t.tx3)
                            .child("⌘K command palette")
                            .child("⌘N new workspace")
                            .child("⌘O add project"),
                    ),
            )
    }
}
