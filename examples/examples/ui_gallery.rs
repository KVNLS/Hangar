//! Design system gallery: tokens, type, metrics, agent states and components, the GPUI rebuild of
//! the "Tokens & components" screen in `design/Hangar.dc.html`. Compare the two side by side.
//!
//! `cargo run -p hangar-examples --example ui_gallery [-- light]`

use gpui_kit::assets::IconName;
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_app::ui::components::*;
use hangar_app::ui::tokens::{DARK_HEX, LIGHT_HEX};
use hangar_app::ui::*;
use hangar_core::AgentState;

struct Gallery {
    segment: usize,
    tab: usize,
}

fn swatch(hex: u32) -> Hsla {
    rgba(hex).into()
}

impl Gallery {
    fn section(title: &str, cx: &App) -> Div {
        div()
            .type_title()
            .text_color(cx.tokens().tx)
            .child(title.to_owned())
    }

    fn colors(&self, cx: &App) -> impl IntoElement {
        let t = cx.tokens();
        let dark = DARK_HEX.entries();
        let light = LIGHT_HEX.entries();
        div()
            .flex()
            .flex_wrap()
            .gap_2()
            .children(dark.into_iter().zip(light).map(|((name, d), (_, l))| {
                card(cx)
                    .w(px(250.))
                    .overflow_hidden()
                    .child(
                        div()
                            .flex()
                            .h(px(36.))
                            .child(
                                div()
                                    .flex_1()
                                    .bg(swatch(d))
                                    .border_r_1()
                                    .border_color(t.line),
                            )
                            .child(div().flex_1().bg(swatch(l))),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .px(px(9.))
                            .py(px(7.))
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(name),
                            )
                            .child(div().type_mono_small().text_color(t.tx2).child(format!(
                                "#{:06X} · #{:06X}",
                                d >> 8,
                                l >> 8
                            ))),
                    )
            }))
    }

    fn type_scale(&self, cx: &App) -> impl IntoElement {
        let t = cx.tokens();
        let row = |name: &'static str, spec: &'static str, sample: Div| {
            div()
                .flex()
                .items_center()
                .gap_4()
                .px(px(14.))
                .py(px(10.))
                .border_b_1()
                .border_color(t.line)
                .child(
                    div()
                        .w(px(120.))
                        .text_size(px(12.))
                        .text_color(t.tx2)
                        .child(name),
                )
                .child(sample.flex_1())
                .child(
                    div()
                        .w(px(180.))
                        .type_mono_small()
                        .text_color(t.tx3)
                        .child(spec),
                )
        };
        card(cx)
            .child(row(
                "Title",
                "15 / 20 · 600",
                div().type_title().child("Overview"),
            ))
            .child(row(
                "Body",
                "13 / 18 · 400",
                div().type_body().child("Fix cart badge count"),
            ))
            .child(row(
                "Body strong",
                "13 / 18 · 600",
                div().type_body_strong().child("Ready for review"),
            ))
            .child(row(
                "Secondary",
                "12 / 16 · 400",
                div().type_secondary().child("Ask before edits"),
            ))
            .child(row(
                "Label",
                "11 / 14 · 600",
                div().type_label().text_color(t.tx3).child("PROJECTS"),
            ))
            .child(row(
                "Mono",
                "12 / 18 · Geist Mono",
                div().type_mono().child("xcodebuild -scheme Pantry"),
            ))
            .child(row(
                "Mono small",
                "11 / 16 · Geist Mono",
                div()
                    .type_mono_small()
                    .text_color(t.tx2)
                    .child("+38 −3 · 09:41:02.114"),
            ))
    }

    fn metrics(&self, cx: &App) -> impl IntoElement {
        let t = cx.tokens();
        let spaces = [
            SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_5, SPACE_6, SPACE_8,
        ];
        let radii = [
            (RADIUS_BADGE, "badge, kbd"),
            (RADIUS_CONTROL, "row, button, input"),
            (RADIUS_CARD, "card, popover"),
            (RADIUS_SHEET, "window, sheet"),
        ];
        div()
            .flex()
            .gap_3()
            .child(
                card(cx)
                    .flex_row()
                    .items_end()
                    .gap_3()
                    .p(px(14.))
                    .children(spaces.map(|s| {
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1p5()
                            .child(div().size(s).bg(t.accbg).border_1().border_color(t.acc))
                            .child(
                                div()
                                    .type_mono_small()
                                    .text_color(t.tx2)
                                    .child(format!("{}", s.as_f32())),
                            )
                    })),
            )
            .child(
                card(cx)
                    .flex_row()
                    .gap_3()
                    .p(px(14.))
                    .w(px(520.))
                    .children(radii.map(|(r, usage)| {
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .gap_1p5()
                            .child(
                                div()
                                    .h(px(40.))
                                    .border_1()
                                    .border_color(t.line2)
                                    .bg(t.elev)
                                    .rounded(r),
                            )
                            .child(
                                div()
                                    .type_mono_small()
                                    .text_color(t.tx2)
                                    .child(format!("{}px", r.as_f32())),
                            )
                            .child(div().text_size(px(11.)).text_color(t.tx3).child(usage))
                    })),
            )
    }

    fn states(&self, cx: &App) -> impl IntoElement {
        let t = cx.tokens();
        div()
            .flex()
            .gap_2()
            .children(AgentState::ORDER.map(|state| {
                let (_, bg) = state_colors(state, t);
                card(cx)
                    .flex_1()
                    .gap(px(10.))
                    .p(px(14.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .size(px(22.))
                                    .rounded(RADIUS_CONTROL)
                                    .bg(bg)
                                    .child(StateGlyph::new(state).size(px(12.))),
                            )
                            .child(div().type_body_strong().child(state.label())),
                    )
                    .child(
                        div()
                            .type_mono_small()
                            .text_color(t.tx2)
                            .child(state.short_label()),
                    )
            }))
    }

    fn components(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let label = |s: &'static str| div().type_mono_small().text_color(t.tx3).child(s);
        let tab = self.tab;
        let tabs = ["Device", "Changes", "Files"];
        div()
            .flex()
            .flex_wrap()
            .gap_3()
            .child(
                card(cx)
                    .w(px(440.))
                    .gap(px(10.))
                    .p_4()
                    .child(label("Button"))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .items_center()
                            .child(
                                UiButton::new("pr")
                                    .label("Create PR")
                                    .primary()
                                    .on_click(|_, _, _| {}),
                            )
                            .child(
                                UiButton::new("commit")
                                    .label("Commit")
                                    .on_click(|_, _, _| {}),
                            )
                            .child(
                                UiButton::new("cancel")
                                    .label("Cancel")
                                    .ghost()
                                    .on_click(|_, _, _| {}),
                            )
                            .child(
                                UiButton::new("discard")
                                    .label("Discard")
                                    .danger()
                                    .on_click(|_, _, _| {}),
                            )
                            .child(
                                UiButton::new("refresh")
                                    .icon(IconName::RefreshCw)
                                    .ghost()
                                    .tooltip("Reload ⌘R")
                                    .on_click(|_, _, _| {}),
                            )
                            .child(
                                UiButton::new("create")
                                    .label("Create workspace")
                                    .primary()
                                    .large()
                                    .kbd("⌘⏎"),
                            )
                            .child(UiButton::new("off").label("Disabled").disabled(true)),
                    )
                    .child(label("Kbd · badge · delta · chip"))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(kbd_keys(["⌘", "K"]))
                            .child(AgentMark::new("CC"))
                            .child(AgentMark::new(">_"))
                            .child(CountBadge::new("4"))
                            .child(Delta::new(38, 3))
                            .child(Delta::new(0, 0))
                            .child(Chip::new("hangar/reorder-button").icon(IconName::GitBranch)),
                    ),
            )
            .child(
                card(cx)
                    .w(px(440.))
                    .gap(px(10.))
                    .p_4()
                    .child(label("Segmented · tabs"))
                    .child(
                        Segmented::new("seg", ["Split", "Inline"])
                            .selected(self.segment)
                            .on_select(cx.listener(|this, i: &usize, _, cx| {
                                this.segment = *i;
                                cx.notify();
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .border_1()
                            .border_color(t.line)
                            .rounded(RADIUS_CONTROL)
                            .overflow_hidden()
                            .bg(t.panel)
                            .child(
                                UiTab::new("t1", TabStyle::Pane, "Claude Code · Fix cart badge")
                                    .leading(StateGlyph::new(AgentState::Approval))
                                    .active(true),
                            )
                            .child(
                                UiTab::new("t2", TabStyle::Pane, "shell").icon(IconName::Terminal),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(2.))
                            .px(px(6.))
                            .border_b_1()
                            .border_color(t.line)
                            .children(tabs.iter().enumerate().map(|(i, name)| {
                                UiTab::new(("p", i), TabStyle::Panel, *name)
                                    .icon(IconName::Smartphone)
                                    .kbd(format!("⌘{}", i + 1))
                                    .when(i == 1, |this| this.trailing(CountBadge::new("4")))
                                    .active(i == tab)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.tab = i;
                                        cx.notify();
                                    }))
                            })),
                    ),
            )
            .child(
                card(cx)
                    .w(px(440.))
                    .gap(px(10.))
                    .p_4()
                    .child(label("Toast · empty state"))
                    .child(toast(
                        IconName::GitPullRequest,
                        t.grn,
                        "Opened PR #482 → main",
                        cx,
                    ))
                    .child(
                        div().flex().justify_center().py_4().child(
                            EmptyState::new(IconName::Smartphone, "No device for this workspace")
                                .hint("Pick a simulator or emulator to build and run this branch.")
                                .action(
                                    UiButton::new("boot")
                                        .label("Boot iPhone 16 Pro")
                                        .primary()
                                        .large(),
                                ),
                        ),
                    ),
            )
    }
}

impl Render for Gallery {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = cx.tokens();
        let dark = cx.theme_is_dark();
        div()
            .id("gallery")
            .size_full()
            .overflow_y_scroll()
            .bg(t.app)
            .text_color(t.tx)
            .font_family(FONT_UI)
            .text_size(px(13.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_10()
                    .px_12()
                    .py_10()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .type_mono_small()
                                    .text_color(t.tx3)
                                    .child("HANGAR · DESIGN SYSTEM v0.1"),
                            )
                            .child(div().flex_1())
                            .child(
                                UiButton::new("theme")
                                    .label(if dark { "Dark theme" } else { "Light theme" })
                                    .icon(IconName::SunMoon)
                                    .on_click(|_, window, cx| {
                                        let mode = if cx.theme_is_dark() {
                                            ThemeMode::Light
                                        } else {
                                            ThemeMode::Dark
                                        };
                                        Theme::change(mode, Some(window), cx);
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(Self::section("Color", cx))
                            .child(self.colors(cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(Self::section("Type", cx))
                            .child(self.type_scale(cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(Self::section("Spacing · radii", cx))
                            .child(self.metrics(cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(Self::section("Agent states", cx))
                            .child(self.states(cx)),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(Self::section("Components", cx))
                            .child(self.components(cx)),
                    ),
            )
    }
}

trait ThemeIsDark {
    fn theme_is_dark(&self) -> bool;
}

impl ThemeIsDark for App {
    fn theme_is_dark(&self) -> bool {
        Theme::global(self).is_dark()
    }
}

fn main() {
    let light = std::env::args().nth(1).as_deref() == Some("light");
    gpui_kit::application()
        .with_assets(hangar_app::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            let mode = if light {
                ThemeMode::Light
            } else {
                ThemeMode::Dark
            };
            if let Err(err) = hangar_app::ui::init(mode, cx) {
                eprintln!("design system init failed: {err:#}");
            }
            cx.on_window_closed(|cx, _| cx.quit()).detach();
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1440.), px(1000.)), cx)),
                ..Default::default()
            };
            let opened = gpui_kit::open_window(options, cx, |_, cx| {
                cx.new(|_| Gallery { segment: 0, tab: 0 })
            });
            match opened {
                Ok(_) => cx.activate(true),
                Err(err) => {
                    eprintln!("failed to open window: {err:#}");
                    cx.quit();
                }
            }
        });
}
