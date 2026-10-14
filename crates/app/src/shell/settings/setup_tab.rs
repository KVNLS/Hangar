//! Settings › Setup: every tool Hangar relies on, what was found, and the command for what is
//! missing.

use hangar_core::setup::SetupGroup;

use super::*;
use crate::ui::components::{section_label, setup_row};

impl HangarApp {
    pub(super) fn render_settings_setup(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let report = self.setup.report.as_ref();
        let status = match report {
            None => "Checking…".to_owned(),
            Some(r) => match r.blockers().count() {
                0 => "Everything Hangar needs is ready.".to_owned(),
                1 => "1 required item is missing.".to_owned(),
                n => format!("{n} required items are missing."),
            },
        };
        let groups =
            SetupGroup::ALL.into_iter().filter_map(|group| {
                let checks: Vec<_> = report?.checks.iter().filter(|c| c.group == group).collect();
                if checks.is_empty() {
                    return None;
                }
                let last = checks.len() - 1;
                Some(
                    v_flex()
                        .gap_2()
                        .child(
                            h_flex()
                                .gap_2()
                                .items_baseline()
                                .child(section_label(group.label(), cx))
                                .children(group.needed_for().map(|hint| {
                                    div().type_secondary().text_color(t.tx3).child(hint)
                                })),
                        )
                        .child(card(cx).children(checks.into_iter().enumerate().map(
                            |(i, check)| {
                                setup_row("settings", check, cx)
                                    .when(i < last, |this| this.border_b_1().border_color(t.line))
                            },
                        ))),
                )
            });
        v_flex()
            .gap_4()
            .child(self.settings_heading(
                "Setup",
                "What Hangar needs on this Mac. Hangar never installs anything itself: copy a \
                 command, run it in a terminal, then re-check.",
                cx,
            ))
            .child(
                h_flex()
                    .gap_3()
                    .items_center()
                    .justify_between()
                    .child(div().text_size(px(12.)).text_color(t.tx2).child(status))
                    .child(
                        UiButton::new("setup-recheck")
                            .icon(Lucide::RefreshCw)
                            .label(if self.setup.checking {
                                "Checking…"
                            } else {
                                "Re-check"
                            })
                            .small()
                            .disabled(self.setup.checking)
                            .on_click(
                                cx.listener(|this, _, window, cx| this.recheck_setup(window, cx)),
                            ),
                    ),
            )
            .children(groups)
    }
}
