use super::*;

pub(super) fn render_kpis(summary: &UsageSummary, cx: &App) -> Div {
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

pub(super) fn render_chart(summary: &UsageSummary, cx: &App) -> Div {
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

pub(super) fn render_models(summary: &UsageSummary, cx: &App) -> Div {
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
