//! Settings › Devices: the device cap, the pool's devices and the platforms' base devices.

use super::*;

impl HangarApp {
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
            .chain((1..=hangar_core::device::capacity::MAX_SLOTS).map(|n| n.to_string()));
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
                            .font_mono()
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

    pub(super) fn render_settings_devices(&self, cx: &mut Context<Self>) -> Div {
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
            let rows: Vec<AnyElement> =
                picker
                    .entries()
                    .iter()
                    .filter(|e| e.key.platform == platform)
                    .filter(|e| !e.info.name.starts_with(hangar_core::device::CLONE_PREFIX))
                    .filter(|e| seen.insert(e.key.clone()))
                    .map(|entry| {
                        let base_key = entry.key.clone();
                        let boot_key = entry.key.clone();
                        let can_boot = entry.key.platform.can_boot(&entry.info);
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
                                    .font_mono()
                                    .text_size(px(11.))
                                    .text_color(color)
                                    .child(div().size(px(6.)).rounded_full().bg(color))
                                    .child(entry.info.state.to_lowercase()),
                            )
                            .when(can_boot, |this| {
                                this.child(
                                    UiButton::new(SharedString::from(format!(
                                        "settings-boot-{}",
                                        entry.key.value()
                                    )))
                                    .label("Boot")
                                    .small()
                                    .tooltip("Boot this device and open it in the Device panel")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        let key = boot_key.clone();
                                        this.device_panel.update(cx, |panel, cx| {
                                            panel.boot_device(key, window, cx)
                                        });
                                    })),
                                )
                            })
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
}
