use super::*;

/// Sessions that ended today, listed at the bottom of the overview.
#[derive(Default)]
pub(in crate::shell) struct DoneToday {
    pub(in crate::shell) sessions: Vec<Session>,
    /// Seconds local time was ahead of UTC when `sessions` were loaded.
    pub(in crate::shell) utc_offset: i32,
    pub(in crate::shell) collapsed: bool,
    /// Bumped per load so an older load finishing late does not overwrite a newer one.
    pub(in crate::shell) generation: u64,
}

/// Seconds local time is ahead of UTC at `at`, per the system time zone; 0 when unknown.
pub(in crate::shell) fn local_utc_offset(at: Timestamp) -> i32 {
    let secs: libc::time_t = at.as_millis().div_euclid(1000);
    // SAFETY: `tm` is plain integers and a nullable pointer, for which all zeroes is valid.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call; `localtime_r` keeps no reference to them.
    let filled = unsafe { libc::localtime_r(&secs, &mut tm) };
    if filled.is_null() {
        0
    } else {
        i32::try_from(tm.tm_gmtoff).unwrap_or(0)
    }
}

/// The sessions that ended since local midnight, and the UTC offset used. Blocks.
fn load_done_today(store: &dyn Store) -> anyhow::Result<(Vec<Session>, i32)> {
    let now = Timestamp::now();
    let offset = local_utc_offset(now);
    let since = history::local_day_start(now, offset);
    Ok((ops::sessions_ended_since(store, since)?, offset))
}

impl HangarApp {
    /// Reloads the sessions that ended today, off the UI thread.
    pub(in crate::shell) fn refresh_done_today(&mut self, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let generation = self.next_done_generation();
        cx.spawn(async move |this, cx| {
            let done = cx
                .background_executor()
                .spawn(async move { load_done_today(&store) })
                .await;
            let _ = this.update(cx, |this, cx| this.apply_done_today(generation, done, cx));
        })
        .detach();
    }

    /// Runs `job` (recording how a session ended) with the store off the UI thread, reports
    /// its error, then reloads the sessions that ended today.
    pub(in crate::shell) fn record_session_end(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        job: impl FnOnce(&dyn Store) -> anyhow::Result<()> + Send + 'static,
    ) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let generation = self.next_done_generation();
        cx.spawn_in(window, async move |this, cx| {
            let (recorded, done) = cx
                .background_executor()
                .spawn(async move { (job(&store), load_done_today(&store)) })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Err(err) = recorded {
                    this.report(&err, window, cx);
                }
                this.apply_done_today(generation, done, cx);
            });
        })
        .detach();
    }

    fn next_done_generation(&mut self) -> u64 {
        self.live.done_today.generation += 1;
        self.live.done_today.generation
    }

    fn apply_done_today(
        &mut self,
        generation: u64,
        done: anyhow::Result<(Vec<Session>, i32)>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.live.done_today.generation {
            return;
        }
        match done {
            Ok((sessions, offset)) => {
                self.live.done_today.sessions = sessions;
                self.live.done_today.utc_offset = offset;
                cx.notify();
            }
            Err(err) => tracing::warn!("could not load the sessions that ended today: {err:#}"),
        }
    }

    /// "Done today": one row per session that ended today; `None` when there is none.
    pub(in crate::shell) fn done_today_section(
        &self,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let t = cx.tokens();
        let done = &self.live.done_today;
        let projects = &self.work.projects;
        let finished = history::done_today(
            &done.sessions,
            |id| projects.workspace(id),
            Timestamp::now(),
            done.utc_offset,
        );
        if finished.is_empty() {
            return None;
        }
        let collapsed = done.collapsed;
        let offset = done.utc_offset;
        let count = finished.len();
        let rows: Vec<AnyElement> = finished
            .into_iter()
            .enumerate()
            .map(|(i, f)| {
                let id = f.workspace;
                let (icon, color) = match f.outcome {
                    Outcome::Done => (Lucide::CircleCheck, t.grn),
                    Outcome::Failed => (Lucide::CircleX, t.red),
                    Outcome::Stopped => (Lucide::Square, t.tx3),
                };
                h_flex()
                    .id(SharedString::from(format!("done-{}", f.session)))
                    .gap_2()
                    .px_3()
                    .py(px(6.))
                    .items_center()
                    .when(i > 0, |row| row.border_t_1().border_color(t.line))
                    .child(AgentMark::new(f.agent.monogram()))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .type_body()
                            .child(f.title),
                    )
                    .when(f.archived, |row| {
                        row.child(div().type_mono_small().text_color(t.tx3).child("archived"))
                    })
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .items_center()
                            .text_color(color)
                            .child(Icon::new(icon).size(px(12.)).text_color(color))
                            .child(div().type_secondary().child(f.outcome.label())),
                    )
                    .children(
                        f.changes
                            .filter(|c| c.files > 0)
                            .map(|c| Delta::new(c.insertions as usize, c.deletions as usize)),
                    )
                    .children(
                        f.cost
                            .as_ref()
                            .and_then(|c| {
                                crate::chat_view::stored_costs_label(std::slice::from_ref(c))
                            })
                            .map(|label| {
                                div()
                                    .flex_none()
                                    .type_mono_small()
                                    .text_color(t.tx3)
                                    .child(label)
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .type_mono_small()
                            .text_color(t.tx3)
                            .child(history::clock_label(f.ended_at, offset)),
                    )
                    .map(|row| {
                        if f.archived {
                            row.opacity(0.6)
                        } else {
                            row.cursor_pointer()
                                .hover(|s| s.bg(t.hover))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.select_workspace(id, window, cx);
                                    this.set_screen(Screen::Workspace, window, cx);
                                }))
                        }
                    })
                    .into_any_element()
            })
            .collect();
        Some(
            v_flex()
                .gap_2()
                .child(
                    h_flex()
                        .id("done-today-toggle")
                        .h(px(24.))
                        .gap_2()
                        .items_center()
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.live.done_today.collapsed = !this.live.done_today.collapsed;
                            cx.notify();
                        }))
                        .child(
                            div().flex().w(px(14.)).justify_center().child(
                                Icon::new(if collapsed {
                                    Lucide::ChevronRight
                                } else {
                                    Lucide::ChevronDown
                                })
                                .size(px(12.))
                                .text_color(t.tx3),
                            ),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Done today"),
                        )
                        .child(
                            div()
                                .type_mono_small()
                                .text_color(t.tx3)
                                .child(count.to_string()),
                        )
                        .child(div().flex_1().ml_1p5().h(px(1.)).bg(t.line)),
                )
                .when(!collapsed, |this| {
                    this.child(
                        v_flex()
                            .rounded(px(6.))
                            .border_1()
                            .border_color(t.line)
                            .bg(t.panel)
                            .overflow_hidden()
                            .children(rows),
                    )
                })
                .into_any_element(),
        )
    }
}
