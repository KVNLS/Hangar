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

    fn summary(&self) -> Option<UsageSummary> {
        let range = self.range.as_ref()?;
        Some(usage::summarize(&self.entries, range, &self.settings))
    }
}

impl HangarApp {
    /// Reads the ledger rows of the shown period (and the saved settings, once) off the UI
    /// thread.
    pub(in crate::shell) fn load_usage(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let ui = &mut self.settings_ui.usage;
        let now = Timestamp::now();
        let range = usage::range(ui.settings.period, now, local_utc_offset(now), ui.back);
        let since = range.start;
        let read_settings = !ui.settings_loaded;
        ui.load = Some(cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let settings = if read_settings {
                        store
                            .get_setting(&setting_keys::USAGE)
                            .context("failed to read the usage settings")?
                    } else {
                        None
                    };
                    let entries = store
                        .list_usage_since(since)
                        .context("failed to read the usage history")?;
                    anyhow::Ok((settings, entries))
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok((settings, entries)) => {
                        let ui = &mut this.settings_ui.usage;
                        if read_settings {
                            ui.settings_loaded = true;
                            if let Some(settings) = settings {
                                let period_changed = settings.period != ui.settings.period;
                                ui.settings = settings;
                                if period_changed {
                                    // The saved period differs from the one just loaded.
                                    this.load_usage(window, cx);
                                    return;
                                }
                            }
                        }
                        let ui = &mut this.settings_ui.usage;
                        ui.entries = entries;
                        ui.range = Some(range);
                        ui.loaded = true;
                        this.sync_usage_controls(window, cx);
                    }
                    Err(err) => this.report(&err, window, cx),
                }
                cx.notify();
            });
        }));
    }

    fn set_usage_period(&mut self, period: Period, window: &mut Window, cx: &mut Context<Self>) {
        let ui = &mut self.settings_ui.usage;
        if ui.settings.period == period {
            return;
        }
        ui.settings.period = period;
        ui.back = 0;
        self.save_usage_settings(window, cx);
        self.load_usage(window, cx);
    }

    fn step_usage_period(&mut self, earlier: bool, window: &mut Window, cx: &mut Context<Self>) {
        let ui = &mut self.settings_ui.usage;
        ui.back = if earlier {
            ui.back.saturating_add(1)
        } else {
            ui.back.saturating_sub(1)
        };
        self.load_usage(window, cx);
    }

    fn save_usage_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.settings_ui.usage.settings.clone();
        self.persist(window, cx, move |store| {
            store
                .set_setting(&setting_keys::USAGE, &settings)
                .context("failed to save the usage settings")
        });
    }

    /// Fills the reference picker (auto, cloud models seen, priced models) and the price fields
    /// for the reference model in use.
    fn sync_usage_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ui = &self.settings_ui.usage;
        let mut models: Vec<String> = ui
            .entries
            .iter()
            .filter(|e| !e.local)
            .filter_map(|e| e.model.clone())
            .chain(ui.settings.prices.iter().map(|p| p.model.clone()))
            .chain(ui.settings.reference_model.clone())
            .collect();
        models.sort();
        models.dedup();
        for (id, _) in BUILT_IN {
            if !models.iter().any(|m| m == id) {
                models.push((*id).to_owned());
            }
        }
        let options: Vec<AgentOption> = std::iter::once(AgentOption {
            id: AUTO_REFERENCE.into(),
            title: "Auto · the cloud model used most".into(),
        })
        .chain(models.into_iter().map(|m| AgentOption {
            id: m.clone().into(),
            title: m.into(),
        }))
        .collect();
        let selected = SharedString::from(ui.settings.reference_model.clone().unwrap_or_default());
        ui.reference_select.update(cx, |select, cx| {
            select.set_items(options, window, cx);
            select.set_selected_value(&selected, window, cx);
        });

        let reference = ui.summary().and_then(|s| s.reference);
        let price = reference
            .as_deref()
            .and_then(|m| price_of(m, &ui.settings.prices));
        let text = |v: Option<f64>| v.map(|v| format!("{v:.2}")).unwrap_or_default();
        for (input, value) in [
            (ui.input_price.clone(), text(price.map(|p| p.input))),
            (ui.output_price.clone(), text(price.map(|p| p.output))),
        ] {
            if input.read(cx).value() != value {
                input.update(cx, |input, cx| input.set_value(value, window, cx));
            }
        }
    }

    /// Saves the typed $/Mtok as the reference model's own price.
    fn commit_reference_price(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ui = &self.settings_ui.usage;
        let Some(model) = ui.summary().and_then(|s| s.reference) else {
            return;
        };
        let parse = |input: &Entity<InputState>| {
            input
                .read(cx)
                .value()
                .trim()
                .trim_start_matches('$')
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)
        };
        let (Some(input), Some(output)) = (parse(&ui.input_price), parse(&ui.output_price)) else {
            self.sync_usage_controls(window, cx);
            return;
        };
        let current = price_of(&model, &ui.settings.prices);
        if current.is_some_and(|p| p.input == input && p.output == output) {
            return;
        }
        let price = ModelPrice::from_io(input, output);
        let prices = &mut self.settings_ui.usage.settings.prices;
        prices.retain(|p| p.model != model);
        prices.push(CustomPrice { model, price });
        self.save_usage_settings(window, cx);
        self.sync_usage_controls(window, cx);
        cx.notify();
    }

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

    fn render_usage_header(&self, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let ui = &self.settings_ui.usage;
        let selected = Period::ALL
            .iter()
            .position(|p| *p == ui.settings.period)
            .unwrap_or_default();
        let title = ui
            .range
            .as_ref()
            .map(|r| r.title.clone())
            .unwrap_or_default();
        h_flex()
            .items_center()
            .justify_between()
            .child(
                Segmented::new("usage-period", Period::ALL.map(Period::label))
                    .selected(selected)
                    .on_select(cx.listener(|this, i: &usize, window, cx| {
                        if let Some(period) = Period::ALL.get(*i) {
                            this.set_usage_period(*period, window, cx);
                        }
                    })),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        UiButton::new("usage-earlier")
                            .icon(Lucide::ChevronLeft)
                            .ghost()
                            .small()
                            .tooltip("Earlier")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.step_usage_period(true, window, cx)
                            })),
                    )
                    .child(
                        div()
                            .min_w(px(150.))
                            .text_center()
                            .type_body_strong()
                            .text_color(t.tx)
                            .child(title),
                    )
                    .child(
                        UiButton::new("usage-later")
                            .icon(Lucide::ChevronRight)
                            .ghost()
                            .small()
                            .tooltip("Later")
                            .disabled(ui.back == 0)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.step_usage_period(false, window, cx)
                            })),
                    ),
            )
    }

    fn render_local_card(&self, summary: &UsageSummary, cx: &mut Context<Self>) -> Div {
        let t = cx.tokens();
        let local = &summary.local;
        let ratio = local.ratio().unwrap_or_default();
        let share = if local.tokens > 0 {
            format!(
                "{} of {} tokens ran on local models ({} of {} turns).",
                tokens_label(local.local_tokens),
                tokens_label(local.tokens),
                local.local_turns,
                local.turns
            )
        } else {
            format!(
                "{} of {} turns ran on local models.",
                local.local_turns, local.turns
            )
        };
        let saving = match (&summary.saving, &summary.reference) {
            (Some(saving), _) => format!(
                "≈ {} saved at {} prices.",
                money(saving.cost.amount, &saving.cost.currency),
                saving.reference
            ),
            (None, None) => "Pick a reference model to price local use.".to_owned(),
            (None, Some(reference))
                if price_of(reference, &self.settings_ui.usage.settings.prices).is_none() =>
            {
                format!("Set a price for {reference} to estimate the saving.")
            }
            (None, Some(_)) if local.local_turns > 0 => {
                "Local turns reported no tokens, so the saving is unknown.".to_owned()
            }
            (None, Some(_)) => "No local model ran in this period.".to_owned(),
        };
        let row = || {
            h_flex()
                .px(px(14.))
                .py_3()
                .gap_3()
                .items_center()
                .justify_between()
                .border_b_1()
                .border_color(t.line)
        };
        let label = |title: &'static str, hint: &'static str| {
            v_flex()
                .gap_0p5()
                .min_w_0()
                .child(div().type_body_strong().child(title))
                .child(div().type_secondary().text_color(t.tx3).child(hint))
        };
        let ui = &self.settings_ui.usage;
        card(cx)
            .child(
                v_flex()
                    .px(px(14.))
                    .py_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(t.line)
                    .child(
                        h_flex()
                            .justify_between()
                            .child(div().type_body_strong().child("Local vs cloud"))
                            .child(
                                div()
                                    .font_mono()
                                    .text_size(px(12.))
                                    .text_color(t.grn)
                                    .child(percent(ratio)),
                            ),
                    )
                    .child(
                        div().h(px(6.)).w_full().rounded_full().bg(t.hover).child(
                            div()
                                .h_full()
                                .rounded_full()
                                .bg(t.grn)
                                .w(relative(ratio.clamp(0.0, 1.0) as f32)),
                        ),
                    )
                    .child(div().type_secondary().text_color(t.tx2).child(share))
                    .child(div().type_secondary().text_color(t.tx2).child(saving))
                    .when(
                        local.local_tokens_unknown && summary.saving.is_some(),
                        |this| {
                            this.child(
                                div()
                                    .type_secondary()
                                    .text_color(t.tx3)
                                    .child("Some local turns reported no tokens and are left out."),
                            )
                        },
                    ),
            )
            .child(
                row()
                    .child(label(
                        "Reference model",
                        "Local tokens are priced as if this cloud model had run them.",
                    ))
                    .child(
                        div().w(px(260.)).child(
                            Select::new(&ui.reference_select)
                                .small()
                                .placeholder("Auto · the cloud model used most"),
                        ),
                    ),
            )
            .child(
                row()
                    .border_b_0()
                    .child(label(
                        "Price, $ per million tokens",
                        "Input and output. Press Enter to save your own price for this model.",
                    ))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .child(div().w(px(90.)).child(Input::new(&ui.input_price).small()))
                            .child(div().type_secondary().text_color(t.tx3).child("in"))
                            .child(div().w(px(90.)).child(Input::new(&ui.output_price).small()))
                            .child(div().type_secondary().text_color(t.tx3).child("out")),
                    ),
            )
    }
}

fn render_kpis(summary: &UsageSummary, cx: &App) -> Div {
    let t = cx.tokens();
    let tile = |label: &'static str, value: String, hint: Option<String>| {
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_1()
            .px(px(14.))
            .py_3()
            .child(div().type_label().text_color(t.tx3).child(label))
            .child(
                div()
                    .text_size(px(18.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.tx)
                    .truncate()
                    .child(value),
            )
            .when_some(hint, |this, hint| {
                this.child(
                    div()
                        .type_secondary()
                        .text_color(t.tx3)
                        .truncate()
                        .child(hint),
                )
            })
    };
    let cost = crate::chat_view::stored_costs_label(&summary.totals).unwrap_or_else(|| "—".into());
    let local = summary
        .local
        .ratio()
        .map(percent)
        .unwrap_or_else(|| "—".into());
    let (saved, saved_hint) = match &summary.saving {
        Some(s) => (
            money(s.cost.amount, &s.cost.currency),
            Some(format!("vs {}", s.reference)),
        ),
        None => ("—".to_owned(), None),
    };
    card(cx)
        .flex_row()
        .child(tile("COST", cost, None))
        .child(tile("TURNS", summary.turns.to_string(), None))
        .child(tile(
            "TOKENS",
            tokens_label(summary.tokens.total()),
            Some(format!(
                "{} in · {} out",
                tokens_label(
                    summary.tokens.input + summary.tokens.cache_read + summary.tokens.cache_write
                ),
                tokens_label(summary.tokens.output)
            )),
        ))
        .child(tile("LOCAL", local, None))
        .child(tile("SAVED (EST.)", saved, saved_hint))
}

/// One bar per hour, day or month: its label, the amount per chart series and their total.
#[derive(Clone)]
struct Bar {
    label: SharedString,
    parts: Vec<(SharedString, f64)>,
    total: f64,
}

fn render_chart(summary: &UsageSummary, cx: &App) -> Div {
    let t = cx.tokens();
    let chart = &summary.chart;
    let Some(currency) = chart.currency.clone() else {
        return div();
    };
    let bars: Vec<Bar> = chart
        .bars
        .iter()
        .map(|(label, amounts)| Bar {
            label: label.clone().into(),
            parts: chart
                .series
                .iter()
                .zip(amounts)
                .filter(|(_, a)| **a > 0.0)
                .map(|(s, a)| (s.clone().into(), *a))
                .collect(),
            total: amounts.iter().sum(),
        })
        .collect();
    let ticks = bars.len().min(12);
    let accent = t.acc;
    let tooltip_currency = currency.clone();
    let axis_currency = currency.clone();
    card(cx)
        .px(px(14.))
        .py_3()
        .gap_2()
        .child(
            div()
                .type_label()
                .text_color(t.tx3)
                .child(format!("COST · {currency}")),
        )
        .child(
            div().h(px(180.)).child(
                BarChart::new(bars)
                    .band(|b: &Bar| b.label.clone())
                    .value(|b: &Bar| b.total)
                    .fill(move |_, _, _, _| accent)
                    .value_axis(true)
                    .value_tick_count(3)
                    .value_tick_format(move |v| money(v, &axis_currency))
                    .band_tick_count(ticks)
                    .tooltip_content(move |b: &Bar, _, cx| {
                        let t = cx.tokens();
                        v_flex()
                            .gap_1()
                            .child(div().type_body_strong().child(b.label.clone()))
                            .children(b.parts.iter().map(|(model, amount)| {
                                h_flex()
                                    .gap_3()
                                    .justify_between()
                                    .child(
                                        div()
                                            .type_mono_small()
                                            .text_color(t.tx2)
                                            .child(model.clone()),
                                    )
                                    .child(
                                        div()
                                            .type_mono_small()
                                            .child(money(*amount, &tooltip_currency)),
                                    )
                            }))
                    })
                    .id("usage-chart"),
            ),
        )
}

fn render_models(summary: &UsageSummary, cx: &App) -> Div {
    let t = cx.tokens();
    let currency = summary.chart.currency.clone();
    let amount = |row: &usage::ModelRow| -> f64 {
        currency.as_deref().map_or(0.0, |c| {
            row.costs
                .iter()
                .filter(|x| x.currency == c)
                .map(|x| x.amount)
                .sum()
        })
    };
    let top = summary.models.iter().map(&amount).fold(0.0, f64::max);
    let header = h_flex()
        .px(px(14.))
        .py_2()
        .gap_3()
        .border_b_1()
        .border_color(t.line)
        .type_label()
        .text_color(t.tx3)
        .child(div().flex_1().child("MODEL"))
        .child(div().w(px(60.)).text_right().child("TURNS"))
        .child(div().w(px(130.)).text_right().child("TOKENS IN / OUT"))
        .child(div().w(px(90.)).text_right().child("COST"))
        .child(div().w(px(90.)));
    let rows = summary.models.iter().map(|row| {
        let share = if top > 0.0 { amount(row) / top } else { 0.0 };
        let input = row.tokens.input + row.tokens.cache_read + row.tokens.cache_write;
        h_flex()
            .px(px(14.))
            .py(px(10.))
            .gap_3()
            .items_center()
            .border_b_1()
            .border_color(t.line)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .child(div().type_mono().truncate().child(row.name().to_owned()))
                    .when(row.local, |this| this.child(CountBadge::new("Local"))),
            )
            .child(
                div()
                    .w(px(60.))
                    .text_right()
                    .type_mono_small()
                    .child(row.turns.to_string()),
            )
            .child(
                div()
                    .w(px(130.))
                    .text_right()
                    .type_mono_small()
                    .text_color(t.tx2)
                    .child(if row.tokens.total() > 0 {
                        format!(
                            "{} / {}",
                            tokens_label(input),
                            tokens_label(row.tokens.output)
                        )
                    } else {
                        "—".to_owned()
                    }),
            )
            .child(
                div()
                    .w(px(90.))
                    .text_right()
                    .type_mono_small()
                    .child(if row.local {
                        "free".to_owned()
                    } else {
                        crate::chat_view::stored_costs_label(&row.costs)
                            .unwrap_or_else(|| "—".into())
                    }),
            )
            .child(
                div().w(px(90.)).h(px(6.)).rounded_full().bg(t.hover).child(
                    div()
                        .h_full()
                        .rounded_full()
                        .bg(t.acc)
                        .w(relative(share.clamp(0.0, 1.0) as f32)),
                ),
            )
    });
    card(cx).child(header).children(rows)
}

/// `1.2M`, `34.5k`, `812`.
fn tokens_label(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1_000_000.0),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1_000.0),
        n => n.to_string(),
    }
}

fn percent(ratio: f64) -> String {
    let p = ratio * 100.0;
    if p > 0.0 && p < 1.0 {
        "<1%".to_owned()
    } else {
        format!("{p:.0}%")
    }
}

fn money(amount: f64, currency: &str) -> String {
    crate::chat_view::cost_label(&hangar_agents::acp::protocol::Cost {
        amount,
        currency: currency.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::{percent, tokens_label};

    #[test]
    fn labels() {
        assert_eq!(tokens_label(812), "812");
        assert_eq!(tokens_label(34_500), "34.5k");
        assert_eq!(tokens_label(1_234_567), "1.2M");
        assert_eq!(percent(0.004), "<1%");
        assert_eq!(percent(0.0), "0%");
        assert_eq!(percent(0.426), "43%");
    }
}
