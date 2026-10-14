use super::*;

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
                .spawn(async move { ops::usage_since(&*store, since, read_settings) })
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

    pub(super) fn set_usage_period(
        &mut self,
        period: Period,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ui = &mut self.settings_ui.usage;
        if ui.settings.period == period {
            return;
        }
        ui.settings.period = period;
        ui.back = 0;
        self.save_usage_settings(window, cx);
        self.load_usage(window, cx);
    }

    pub(super) fn step_usage_period(
        &mut self,
        earlier: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ui = &mut self.settings_ui.usage;
        ui.back = if earlier {
            ui.back.saturating_add(1)
        } else {
            ui.back.saturating_sub(1)
        };
        self.load_usage(window, cx);
    }

    pub(super) fn save_usage_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = self.settings_ui.usage.settings.clone();
        self.persist(window, cx, move |store| {
            ops::save_usage_settings(store, &settings)
        });
    }

    /// Fills the reference picker (auto, cloud models seen, priced models) and the price fields
    /// for the reference model in use.
    pub(super) fn sync_usage_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
    pub(super) fn commit_reference_price(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ui = &self.settings_ui.usage;
        let Some(model) = ui.summary().and_then(|s| s.reference) else {
            return;
        };
        let (input, output) = match typed_price(
            &ui.input_price.read(cx).value(),
            &ui.output_price.read(cx).value(),
        ) {
            TypedPrice::Price(input, output) => (input, output),
            TypedPrice::Empty => {
                self.sync_usage_controls(window, cx);
                return;
            }
            // Left as typed, so the user can correct it.
            TypedPrice::Invalid => return,
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
}
