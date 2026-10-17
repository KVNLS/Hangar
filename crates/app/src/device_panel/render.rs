//! Rendering the panel: header, picker, backend, the body for each state and the controls.

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::select::Select;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_core::WorkspaceId;

use crate::device_pool::{DeviceStatus, copy};
use crate::device_view::DeviceView;
use crate::ui::components::{EmptyState, UiButton};
use crate::ui::{ActiveTokens as _, TypeScale as _};

use super::platform::{BackendReason, Platform, PlatformUi as _};
use super::{
    Connection, DevicePanel, DevicePanelEvent, DeviceState, KEY_CONTEXT, PressHome, Screenshot,
};

impl DevicePanel {
    /// The shown device's state for the header, when there is one to tell.
    fn device_state(&self, cx: &App) -> Option<DeviceState> {
        if let Some(ws) = self.workspace {
            let pool = self.pool.read(cx);
            let held = pool.held_by_other(ws).is_some();
            return DeviceState::of_workspace(&pool.status(ws), held);
        }
        let entry = self.picker.selected_entry();
        let key = entry.map(|e| &e.key);
        if self.booting.as_ref().is_some_and(|b| Some(&b.key) == key) {
            return Some(DeviceState::Booting);
        }
        match &self.connection {
            Connection::Open { .. } if self.shutting_down => Some(DeviceState::ShuttingDown),
            Connection::Open { .. } => Some(DeviceState::Running),
            Connection::Opening { .. } => Some(DeviceState::Opening),
            Connection::Failed { .. } => Some(DeviceState::Failed),
            Connection::None => {
                let entry = entry?;
                Some(
                    if self
                        .boot_error
                        .as_ref()
                        .is_some_and(|(k, _)| *k == entry.key)
                    {
                        DeviceState::Failed
                    } else if entry.ready {
                        DeviceState::Running
                    } else {
                        DeviceState::Off
                    },
                )
            }
        }
    }

    /// 40px toolbar: device select and its state, stats overlay toggle, refresh.
    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let t = cx.tokens();
        let state = self.device_state(cx).map(|state| {
            let color = state.color(t);
            h_flex()
                .flex_shrink_0()
                .gap(px(5.))
                .items_center()
                .font_mono()
                .text_size(px(11.))
                .text_color(color)
                .whitespace_nowrap()
                .child(div().size(px(6.)).rounded_full().bg(color))
                .child(state.label())
        });
        h_flex()
            .h(px(40.))
            .px_2()
            .gap_1()
            .flex_shrink_0()
            .items_center()
            .border_b_1()
            .border_color(t.line)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Select::new(&self.select).small().placeholder(
                        if self.picker.listed() {
                            "Select a device"
                        } else {
                            "Listing devices…"
                        },
                    )),
            )
            .children(state)
            .child(
                UiButton::new("device-screenshot")
                    .icon(Lucide::Camera)
                    .ghost()
                    .disabled(!self.can_screenshot())
                    .tooltip("Screenshot ⌘S")
                    .on_click(cx.listener(|this, _, _, cx| this.screenshot(cx))),
            )
            .child(
                UiButton::new("device-stats")
                    .icon(Lucide::Activity)
                    .ghost()
                    .selected(self.stats)
                    .tooltip("Debug overlay: fps and capture → paint latency ⌘⌥⇧P")
                    .on_click(cx.listener(|this, _, _, cx| {
                        let stats = !this.stats;
                        this.set_stats(stats, cx);
                    })),
            )
            .child(
                UiButton::new("device-refresh")
                    .icon(Lucide::RefreshCw)
                    .ghost()
                    .disabled(self.picker.is_listing())
                    .tooltip("Refresh the device list")
                    .on_click(cx.listener(|this, _, window, cx| this.refresh(window, cx))),
            )
    }

    /// Listing errors and "nothing ready" hints under the toolbar.
    fn render_picker(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = cx.tokens();
        let errors: Vec<Div> = self
            .picker
            .errors()
            .iter()
            .map(|(platform, err)| {
                div()
                    .text_size(px(11.))
                    .text_color(t.red)
                    .child(format!("{}: {err}", platform.label()))
            })
            .collect();
        let empty = self.picker.listed() && !self.picker.entries().iter().any(|e| e.ready);
        let hints: Vec<Div> = if empty {
            Platform::ALL
                .map(|p| {
                    div().text_size(px(11.)).text_color(t.tx3).child(format!(
                        "No {} device ready. {}",
                        p.label(),
                        p.not_ready_hint()
                    ))
                })
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        (!errors.is_empty() || !hints.is_empty()).then(|| {
            v_flex()
                .px_3()
                .py_1p5()
                .gap_1()
                .flex_shrink_0()
                .border_b_1()
                .border_color(t.line)
                .children(errors)
                .children(hints)
                .into_any_element()
        })
    }

    /// The device on screen: platform, view and why its backend is not the default.
    fn shown(&self, cx: &App) -> Option<(Platform, &Entity<DeviceView>, Option<&BackendReason>)> {
        if let Some(ws) = self.workspace {
            let a = self.attached.as_ref()?;
            let platform = self
                .pool
                .read(cx)
                .assignment(ws)
                .map_or(Platform::Ios, |a| a.platform);
            return Some((platform, &a.view, a.reason.as_ref()));
        }
        match &self.connection {
            Connection::Open {
                key, view, reason, ..
            } => Some((key.platform, view, reason.as_ref())),
            _ => None,
        }
    }

    fn render_backend(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (_, view, reason) = self.shown(cx)?;
        let backend = view.read(cx).backend();
        // A workspace device is not the base shown in the select: name it.
        let backend = match self.workspace {
            Some(_) => format!("{} · {backend}", view.read(cx).info().name),
            None => backend.to_owned(),
        };
        let t = cx.tokens();
        let (label, color, why) = match reason {
            None => (format!("backend: {backend}"), t.grn, None),
            Some(BackendReason::Fallback(why)) => (
                format!("backend: {backend} (fallback)"),
                t.amb,
                Some(why.clone()),
            ),
            Some(BackendReason::Forced(why)) => (
                format!("backend: {backend} (forced)"),
                t.tx3,
                Some(why.clone()),
            ),
        };
        let note = (view.read(cx).backend() == "simctl")
            .then_some("simctl captures ~2 fps screenshots; input may be unavailable.");
        Some(
            v_flex()
                .px_3()
                .py_1()
                .gap_0p5()
                .flex_shrink_0()
                .font_mono()
                .text_size(px(11.))
                .child(
                    h_flex()
                        .gap(px(5.))
                        .items_center()
                        .child(div().size(px(6.)).rounded_full().bg(color))
                        .child(div().text_color(color).child(label)),
                )
                .when_some(why, |this, why| {
                    this.child(div().text_color(t.tx3).child(why))
                })
                .when_some(note, |this, note| {
                    this.child(div().text_color(t.tx3).child(note))
                })
                .into_any_element(),
        )
    }

    fn render_boot_body(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let entry = self.picker.selected_entry()?;
        let t = cx.tokens();
        let centered = || {
            v_flex()
                .flex_1()
                .min_h_0()
                .p_4()
                .gap_2()
                .items_center()
                .justify_center()
                .text_xs()
        };
        if let Some(booting) = self.booting.as_ref().filter(|b| b.key == entry.key) {
            return Some(
                centered()
                    .gap_3()
                    .text_size(px(11.))
                    .text_color(t.tx2)
                    .child(Spinner::new().color(t.tx3).with_size(px(18.)))
                    .child(booting.status())
                    .into_any_element(),
            );
        }
        if !entry.key.platform.can_boot(&entry.info) {
            return None;
        }
        let key = entry.key.clone();
        let error = self
            .boot_error
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, e)| e.clone());
        Some(
            centered()
                .child(
                    EmptyState::new(
                        Lucide::Smartphone,
                        format!("{} is not running", entry.info.name),
                    )
                    .hint("Boot it to build and run this branch on it.")
                    .action(
                        UiButton::new("device-boot")
                            .label(format!("Boot {}", entry.info.name))
                            .primary()
                            .large()
                            .disabled(self.booting.is_some())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.boot_device(key.clone(), window, cx);
                            })),
                    ),
                )
                .when_some(error, |this, error| {
                    this.child(
                        div()
                            .text_size(px(11.))
                            .text_color(t.red)
                            .child(format!("Boot failed: {error}")),
                    )
                })
                .into_any_element(),
        )
    }

    /// The focused workspace's device: its screen, boot progress, or what keeps it from running.
    fn render_workspace_body(&self, ws: WorkspaceId, cx: &mut Context<Self>) -> AnyElement {
        let t = cx.tokens();
        let centered = || {
            v_flex()
                .flex_1()
                .min_h_0()
                .p_4()
                .gap_2()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .text_color(t.tx2)
        };
        if self.device_state(cx) == Some(DeviceState::InUse)
            && let Some(overlay) = self.render_in_use(ws, cx)
        {
            return overlay;
        }
        if let Some(a) = &self.attached {
            return div()
                .flex_1()
                .min_h_0()
                .p_2()
                .child(a.view.clone())
                .into_any_element();
        }
        let pool = self.pool.read(cx);
        let waiting = pool.is_waiting(ws);
        let single = pool
            .assignment(ws)
            .is_some_and(|a| pool.single_device(a.platform));
        match pool.status(ws) {
            DeviceStatus::Unknown => centered().child("Finding this workspace's device…"),
            DeviceStatus::Opening { name, status } => centered()
                .gap_3()
                .child(Spinner::new().color(t.tx3).with_size(px(18.)))
                .child(format!("{name}: {status}"))
                .when(waiting, |this| {
                    this.child(
                        div()
                            .text_color(t.amb)
                            .child("Waiting for the device: every device slot is in use."),
                    )
                }),
            DeviceStatus::ShuttingDown { name } => centered()
                .gap_3()
                .child(Spinner::new().color(t.tx3).with_size(px(18.)))
                .child(format!("{name}: shutting down…")),
            // Open but the view is not attached yet (attached on the next pool notification).
            DeviceStatus::Open { .. } => centered().child("Opening…"),
            DeviceStatus::Closed {
                name,
                error,
                conflict,
            } => {
                let name = name.to_owned();
                let error = error.map(str::to_owned);
                let conflict = conflict.map(|serial| {
                    UiButton::new("device-clear-conflict")
                        .label(format!("Shut down {serial} and retry"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let task = this.pool.update(cx, |pool, cx| pool.clear_conflict(ws, cx));
                            cx.spawn(async move |_, _| {
                                if let Err(err) = task.await {
                                    tracing::warn!(workspace = %ws, "conflict not cleared: {err:#}");
                                }
                            })
                            .detach();
                        }))
                });
                centered()
                    .child(
                        EmptyState::new(Lucide::Smartphone, format!("{name} is not running"))
                            .hint(if single {
                                "Workspaces share one device on this Mac and take turns (see \
                                 Settings › Devices)."
                            } else {
                                "This workspace's own device. It starts when the agent or ⌘R \
                                 needs it, or start it now to see the screen."
                            })
                            .action(
                                UiButton::new("device-start")
                                    .label("Start device")
                                    .primary()
                                    .large()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.start_workspace_device(ws, cx);
                                    })),
                            ),
                    )
                    .when_some(error, |this, error| {
                        this.child(div().max_w(px(360.)).text_color(t.red).child(error))
                    })
                    .children(conflict)
            }
        }
        .into_any_element()
    }

    /// Another workspace's run or tool call holds this workspace's device. The pool can neither
    /// revoke a lease (no "take over") nor give a workspace its own device while the cap shares
    /// one (no "clone"); in single-device mode it can switch to another booted device.
    fn render_in_use(&self, ws: WorkspaceId, cx: &mut Context<Self>) -> Option<AnyElement> {
        let t = cx.tokens();
        let pool = self.pool.read(cx);
        let (_, holder) = pool.held_by_other(ws)?;
        let holder = holder.to_owned();
        let sharing = pool.sharing(ws);
        let assignment = pool.assignment(ws)?;
        let platform = assignment.platform;
        let alternative = pool
            .single_device(platform)
            .then(|| {
                self.picker.entries().iter().find(|e| {
                    e.key.platform == platform
                        && e.ready
                        && e.key.id != assignment.base
                        && !e.info.name.starts_with(hangar_core::device::CLONE_PREFIX)
                        && !pool.base_held_by_other(&e.key, ws)
                })
            })
            .flatten()
            .map(|e| (e.key.clone(), e.info.name.clone()));
        let mut lines: Vec<String> = Vec::new();
        let mut take_key = None;
        if let Some(sharing) = &sharing {
            lines.push(match sharing.reservation_left {
                Some(left) => format!("Frees in {}", copy::approx(left)),
                None => "Until it is released".to_owned(),
            });
            if !sharing.queue.is_empty() {
                lines.push(format!("Queue: {}", copy::queue_line(&sharing.queue, ws)));
            }
            if let (Some(position), Some(wait)) = (sharing.position, sharing.estimated_wait) {
                lines.push(format!(
                    "You are {} in line, {}",
                    copy::ordinal(position + 1),
                    copy::approx(wait)
                ));
            }
            take_key = Some(sharing.key.clone());
        }
        let take_over = take_key.map(|key| {
            UiButton::new("device-take-over")
                .label("Take over")
                .primary()
                .full_width()
                .tooltip("Ends the other workspace's reservation and puts you first. A call already running finishes.")
                .on_click(cx.listener(move |this, _, _, cx| {
                    let key = key.clone();
                    let result = this
                        .pool
                        .update(cx, |pool, cx| pool.take_over(&key, ws, cx));
                    cx.emit(DevicePanelEvent::TookOver(result));
                }))
        });
        let switch = alternative.map(|(key, name)| {
            UiButton::new("device-use-other")
                .label(format!("Use {name} instead"))
                .ghost()
                .full_width()
                .tooltip("Make it this workspace's device")
                .on_click(cx.listener(move |this, _, _, cx| {
                    let key = key.clone();
                    this.choose_device(ws, key, true, cx);
                }))
        });
        Some(
            v_flex()
                .flex_1()
                .min_h_0()
                .px(px(18.))
                .gap(px(10.))
                .items_center()
                .justify_center()
                .text_center()
                .text_size(px(11.))
                .text_color(t.tx)
                .child(
                    div()
                        .size(px(32.))
                        .rounded(px(8.))
                        .bg(t.viobg)
                        .text_color(t.vio)
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(Icon::new(Lucide::Lock).size(px(15.)).text_color(t.vio)),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("In use by another workspace"),
                )
                .child(
                    div()
                        .text_color(t.tx2)
                        .line_height(rems(1.4))
                        .child(format!("“{holder}” is using this device.")),
                )
                .children(
                    lines
                        .into_iter()
                        .map(|line| div().text_color(t.tx3).line_height(rems(1.4)).child(line)),
                )
                .when_some(take_over, |this, take_over| {
                    this.child(div().w_full().max_w(px(280.)).mt_1().child(take_over))
                })
                .when_some(switch, |this, switch| {
                    this.child(div().w_full().max_w(px(280.)).mt_1().child(switch))
                })
                .into_any_element(),
        )
    }

    fn render_body(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(ws) = self.workspace {
            return self.render_workspace_body(ws, cx);
        }
        if matches!(self.connection, Connection::None)
            && let Some(body) = self.render_boot_body(cx)
        {
            return body;
        }
        let t = cx.tokens();
        let message = |text: SharedString, color: Hsla| {
            div()
                .flex_1()
                .min_h_0()
                .p_4()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(12.))
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        match &self.connection {
            Connection::Open { view, .. } => div()
                .flex_1()
                .min_h_0()
                .p_2()
                .child(view.clone())
                .into_any_element(),
            Connection::Opening { name, .. } => message(format!("Opening {name}…").into(), t.tx2),
            Connection::Failed { name, error } => {
                message(format!("Could not open {name}: {error}").into(), t.red)
            }
            Connection::None => {
                let text = match self.picker.selected() {
                    _ if !self.picker.listed() => "Listing devices…".into(),
                    Some(key) if self.picker.selected_entry().is_none() => {
                        format!("The selected device ({}) is not available.", key.id).into()
                    }
                    Some(_) => match self.picker.selected_entry() {
                        Some(e) if !e.ready => format!(
                            "{} is {}. {}",
                            e.info.name,
                            e.info.state,
                            e.key.platform.not_ready_hint()
                        )
                        .into(),
                        _ => SharedString::from(""),
                    },
                    None => {
                        return div()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                EmptyState::new(Lucide::Smartphone, "No device selected")
                                    .hint("Pick a simulator or emulator above to run this branch."),
                            )
                            .into_any_element();
                    }
                };
                message(text, t.tx2)
            }
        }
    }

    fn render_controls(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (platform, view, _) = self.shown(cx)?;
        let capabilities = view.read(cx).session().capabilities();
        let can_shutdown = capabilities.shutdown;
        let can_rotate = capabilities.rotate;
        let rotate_view = view.clone();
        let t = cx.tokens();
        let buttons = platform
            .controls()
            .iter()
            .filter(|control| capabilities.accepts(&control.input))
            .map(|control| {
                let input = control.input;
                UiButton::new(SharedString::from(format!("device-control-{}", control.id)))
                    .ghost()
                    .small()
                    .icon(control.icon)
                    .tooltip(control.tooltip)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.send(input, cx);
                    }))
            });
        // The row is always there so the controls above do not jump when a status appears.
        let status = div()
            .h(px(16.))
            .min_w_0()
            .truncate()
            .font_mono()
            .text_size(px(11.))
            .line_height(px(16.))
            .when_some(self.input_status.as_ref(), |row, s| {
                row.text_color(if s.error { t.red } else { t.tx3 })
                    .child(s.text.clone())
            });
        Some(
            v_flex()
                .px_2()
                .py_1()
                .gap_1()
                .flex_shrink_0()
                .border_t_1()
                .border_color(t.line)
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_1()
                        .items_center()
                        .children(buttons)
                        .when(can_rotate, |this| {
                            this.children([(true, "Rotate left"), (false, "Rotate right")].map(
                                |(left, label)| {
                                    let view = rotate_view.clone();
                                    UiButton::new(SharedString::from(format!(
                                        "device-rotate-{left}"
                                    )))
                                    .ghost()
                                    .small()
                                    .icon(if left {
                                        gpui_kit::assets::IconName::RotateCcw
                                    } else {
                                        gpui_kit::assets::IconName::RotateCw
                                    })
                                    .tooltip(label)
                                    .on_click(
                                        move |_, _, cx| {
                                            view.update(cx, |view, _| view.rotate(left));
                                        },
                                    )
                                },
                            ))
                        })
                        .when(can_shutdown, |this| {
                            this.child(
                                UiButton::new("device-shutdown")
                                    .label(if self.shutting_down {
                                        "Shutting down…"
                                    } else {
                                        "Shut down"
                                    })
                                    .small()
                                    .disabled(self.shutting_down)
                                    .tooltip("Shut the device down")
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.shutdown_device(cx)),
                                    ),
                            )
                        }),
                )
                .child(status)
                .into_any_element(),
        )
    }
}

impl Render for DevicePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.render_header(cx);
        let picker = self.render_picker(cx);
        let backend = self.render_backend(cx);
        let body = self.render_body(cx);
        let controls = self.render_controls(cx);
        let logs = self.logs().cloned();
        let t = cx.tokens();
        let waiting = self
            .workspace
            .filter(|ws| {
                let pool = self.pool.read(cx);
                // The in-use overlay already says so.
                self.attached.is_some() && pool.is_waiting(*ws) && pool.held_by_other(*ws).is_none()
            })
            .map(|_| {
                div()
                    .px_3()
                    .py_1()
                    .flex_shrink_0()
                    .text_size(px(11.))
                    .text_color(t.amb)
                    .bg(t.ambbg)
                    .child("Waiting for the device: another workspace is using it.")
            });
        v_flex()
            .key_context(KEY_CONTEXT)
            .on_action(cx.listener(|this, _: &Screenshot, _, cx| this.screenshot(cx)))
            .on_action(cx.listener(|this, _: &PressHome, _, cx| this.press_home(cx)))
            .size_full()
            .bg(t.app)
            .child(header)
            .children(waiting)
            .children(picker)
            .children(backend)
            .child(body)
            .children(logs)
            .children(controls)
    }
}
