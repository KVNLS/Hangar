use super::*;

impl HangarApp {
    pub(super) fn render_usage_header(&self, cx: &mut Context<Self>) -> Div {
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

    pub(super) fn render_local_card(&self, summary: &UsageSummary, cx: &mut Context<Self>) -> Div {
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
