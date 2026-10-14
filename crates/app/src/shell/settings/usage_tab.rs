//! Settings › Usage: what agents spent per model over a day, week, month or year, how much ran
//! on local models, and what that saved against a cloud model.

use anyhow::Context as _;
use gpui_kit::component::chart::BarChart;
use gpui_kit::component::select::Select;
use hangar_core::Timestamp;
use hangar_core::changes::UsageEntry;
use hangar_usecases::settings::SettingsExt as _;
use hangar_usecases::usage::pricing::{BUILT_IN, CustomPrice, ModelPrice, price_of};
use hangar_usecases::usage::{self, Period, Range, UsageSettings, UsageSummary};

use super::*;
use crate::shell::overview::local_utc_offset;

/// The reference picker's value for "the cloud model used most".
const AUTO_REFERENCE: &str = "";

/// View state of the Usage tab.
pub(in crate::shell) struct UsageUi {
    settings: UsageSettings,
    settings_loaded: bool,
    /// How many periods before the current one are shown.
    back: u32,
    range: Option<Range>,
    /// Ledger rows of `range`.
    entries: Vec<UsageEntry>,
    loaded: bool,
    load: Option<Task<()>>,
    reference_select: Entity<AgentSelect>,
    input_price: Entity<InputState>,
    output_price: Entity<InputState>,
    _subscriptions: Vec<Subscription>,
}

impl UsageUi {
    pub(in crate::shell) fn new(window: &mut Window, cx: &mut Context<HangarApp>) -> Self {
        let reference_select = cx.new(|cx| SelectState::new(Vec::new(), None, window, cx));
        let price_input = |window: &mut Window, cx: &mut Context<HangarApp>| {
            cx.new(|cx| InputState::new(window, cx).placeholder("0.00"))
        };
        let input_price = price_input(window, cx);
        let output_price = price_input(window, cx);
        let on_price = |this: &mut HangarApp,
                        _: &Entity<InputState>,
                        event: &InputEvent,
                        window: &mut Window,
                        cx: &mut Context<HangarApp>| {
            if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                this.commit_reference_price(window, cx);
            }
        };
        let subscriptions = vec![
            cx.subscribe_in(
                &reference_select,
                window,
                |this, _, event: &SelectEvent<Vec<AgentOption>>, window, cx| {
                    let SelectEvent::Confirm(model) = event;
                    this.settings_ui.usage.settings.reference_model = model
                        .as_ref()
                        .filter(|m| m.as_ref() != AUTO_REFERENCE)
                        .map(|m| m.to_string());
                    this.save_usage_settings(window, cx);
                    this.sync_usage_controls(window, cx);
                    cx.notify();
                },
            ),
            cx.subscribe_in(&input_price, window, on_price),
            cx.subscribe_in(&output_price, window, on_price),
        ];
        Self {
            settings: UsageSettings::default(),
            settings_loaded: false,
            back: 0,
            range: None,
            entries: Vec::new(),
            loaded: false,
            load: None,
            reference_select,
            input_price,
            output_price,
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn summary(&self) -> Option<UsageSummary> {
        let range = self.range.as_ref()?;
        Some(usage::summarize(&self.entries, range, &self.settings))
    }
}

mod charts;
mod data;
mod format;
mod render;

use charts::{render_chart, render_kpis, render_models};
use format::{TypedPrice, money, percent, tokens_label, typed_price};

impl HangarApp {
    pub(super) fn render_settings_usage(&self, cx: &mut Context<Self>) -> Div {
        let ui = &self.settings_ui.usage;
        let summary = ui.summary();
        v_flex()
            .gap_4()
            .child(self.settings_heading(
                "Usage & cost",
                "What agents spent, per model. Claude Code, Codex and OpenCode report it; \
                 terminal agents don't.",
                cx,
            ))
            .child(self.render_usage_header(cx))
            .child(match summary {
                Some(summary) if !summary.models.is_empty() => v_flex()
                    .gap_4()
                    .child(render_kpis(&summary, cx))
                    .child(render_chart(&summary, cx))
                    .child(render_models(&summary, cx))
                    .child(self.render_local_card(&summary, cx)),
                Some(_) => card(cx).p_6().child(
                    div()
                        .type_secondary()
                        .text_color(cx.tokens().tx3)
                        .child("No usage recorded in this period."),
                ),
                None => card(cx).p_6().child(
                    div()
                        .type_secondary()
                        .text_color(cx.tokens().tx3)
                        .child(if ui.loaded { "" } else { "Loading…" }),
                ),
            })
    }
}
