//! The logs pane under the device screen: a virtualized, level-coloured list of the device log.
//!
//! The stream (a child process) is started on the background executor and drained from a
//! timer while the pane is open. It is dropped (killing the child, off the UI thread) when the
//! pane is closed, the process filter changes, or the pane itself goes away with the device.

mod model;

use std::ops::{Range, RangeInclusive};
use std::time::Duration;

use crate::adapters::devices::{LogFilter, LogStream};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_kit::component::{
    ActiveTheme as _, Selectable as _, Sizable as _, StyledExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use hangar_core::device::LogLevel;

use crate::device_view::DeviceHandle;
use model::{BUFFER_CAP, Follow, Line, LogModel};

const POLL: Duration = Duration::from_millis(100);
const ROW_HEIGHT: f32 = 16.;
const LIST_HEIGHT: f32 = 180.;
/// Range of the list height when dragged.
const MIN_LIST_HEIGHT: f32 = 80.;
const MAX_LIST_HEIGHT: f32 = 600.;

/// Drag payload of the list's top edge.
struct LogsResize;

impl Render for LogsResize {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}
/// Capacity of the stream's own queue: it absorbs what arrives between two polls.
const STREAM_CAPACITY: usize = 20_000;

/// What is saved of the pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogsSettings {
    pub open: bool,
    pub min_level: LogLevel,
}

impl Default for LogsSettings {
    fn default() -> Self {
        Self {
            open: false,
            min_level: LogLevel::Verbose,
        }
    }
}

pub enum LogsEvent {
    Settings(LogsSettings),
}

/// A running capture. Dropping it kills the child and joins its threads, so that happens on a
/// helper thread rather than the UI thread.
struct OwnedStream(Option<LogStream>);

impl OwnedStream {
    fn get(&self) -> Option<&LogStream> {
        self.0.as_ref()
    }
}

impl Drop for OwnedStream {
    fn drop(&mut self) {
        if let Some(stream) = self.0.take() {
            let _ = std::thread::Builder::new()
                .name("device-logs-release".into())
                .spawn(move || drop(stream));
        }
    }
}

enum Run {
    Off,
    Starting {
        _task: Task<()>,
    },
    Running {
        stream: OwnedStream,
        _poll: Task<()>,
    },
    /// The child exited by itself; the lines stay.
    Closed {
        error: Option<String>,
    },
    Failed(String),
}

#[derive(Clone, PartialEq)]
struct LevelOption {
    level: LogLevel,
    value: SharedString,
}

impl LevelOption {
    fn new(level: LogLevel) -> Self {
        Self {
            level,
            value: level.as_str().into(),
        }
    }
}

impl SelectItem for LevelOption {
    type Value = SharedString;

    fn title(&self) -> SharedString {
        self.value.clone()
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }
}

const LEVELS: [LogLevel; 6] = [
    LogLevel::Verbose,
    LogLevel::Debug,
    LogLevel::Info,
    LogLevel::Warn,
    LogLevel::Error,
    LogLevel::Fault,
];

pub struct LogsPane {
    source: DeviceHandle,
    /// Ask the device for its recent backlog (Android only).
    history: bool,
    settings: LogsSettings,
    model: LogModel,
    follow: Follow,
    paused: bool,
    run: Run,
    generation: u64,
    dropped: u64,
    scroll: UniformListScrollHandle,
    /// The user scrolled since the last render; re-evaluate follow-tail there.
    wheel: bool,
    /// Height of the line list, dragged from its top edge.
    height: f32,
    selection: Option<(u64, RangeInclusive<u64>)>,
    notice: Option<SharedString>,
    /// Process / bundle filter of the stream that filled the buffer.
    filter: Option<String>,
    focus: FocusHandle,
    process: Entity<InputState>,
    search: Entity<InputState>,
    level: Entity<SelectState<Vec<LevelOption>>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<LogsEvent> for LogsPane {}

impl LogsPane {
    pub fn new(
        source: DeviceHandle,
        history: bool,
        settings: LogsSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let process = cx.new(|cx| InputState::new(window, cx).placeholder("Process / package"));
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        let level = cx.new(|cx| {
            let items = LEVELS.map(LevelOption::new).to_vec();
            let mut state = SelectState::new(items, None, window, cx);
            state.set_selected_value(&settings.min_level.as_str().into(), window, cx);
            state
        });
        let subscriptions = vec![
            cx.subscribe_in(&process, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) && this.settings.open {
                    this.start(cx);
                }
            }),
            cx.subscribe(&search, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = input.read(cx).value();
                    this.model.set_query(&query);
                    this.after_filter(cx);
                }
            }),
            cx.subscribe_in(
                &level,
                window,
                |this, _, event: &SelectEvent<Vec<LevelOption>>, _, cx| {
                    let SelectEvent::Confirm(Some(value)) = event else {
                        return;
                    };
                    if let Ok(level) = value.parse::<LogLevel>() {
                        this.set_min_level(level, cx);
                    }
                },
            ),
        ];
        let mut model = LogModel::new(BUFFER_CAP);
        model.set_min_level(settings.min_level);
        let mut pane = Self {
            source,
            history,
            settings,
            model,
            follow: Follow::default(),
            paused: false,
            run: Run::Off,
            generation: 0,
            dropped: 0,
            scroll: UniformListScrollHandle::new(),
            wheel: false,
            height: LIST_HEIGHT,
            selection: None,
            notice: None,
            filter: None,
            focus: cx.focus_handle(),
            process,
            search,
            level,
            _subscriptions: subscriptions,
        };
        if settings.open {
            pane.start(cx);
        }
        pane
    }

    pub fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.settings.open == open {
            return;
        }
        self.settings.open = open;
        if open {
            self.start(cx);
        } else {
            self.stop();
        }
        cx.emit(LogsEvent::Settings(self.settings));
        cx.notify();
    }

    fn set_min_level(&mut self, level: LogLevel, cx: &mut Context<Self>) {
        if self.settings.min_level == level {
            return;
        }
        self.settings.min_level = level;
        self.model.set_min_level(level);
        self.after_filter(cx);
        cx.emit(LogsEvent::Settings(self.settings));
    }

    fn after_filter(&mut self, cx: &mut Context<Self>) {
        self.notice = None;
        if self.follow.is_on() {
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    /// Drops the stream (killing the child) and keeps the lines.
    fn stop(&mut self) {
        self.generation += 1;
        self.run = Run::Off;
    }

    /// (Re)starts the stream with the current process filter, replacing the one running.
    fn start(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let generation = self.generation;
        self.model.clear();
        self.dropped = 0;
        self.selection = None;
        self.notice = None;
        self.paused = false;
        self.follow.set(true);
        let process = self.process.read(cx).value().trim().to_owned();
        self.filter = (!process.is_empty()).then(|| process.clone());
        let filter = LogFilter {
            min_level: None,
            process: self.filter.clone(),
            history: self.history,
            capacity: STREAM_CAPACITY,
        };
        let source = self.source.clone();
        let task = cx.background_executor().spawn(async move {
            source
                .logs(&filter)
                .map(|stream| OwnedStream(Some(stream)))
                .map_err(|e| e.to_string())
        });
        let task = cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| this.on_started(generation, result, cx));
        });
        self.run = Run::Starting { _task: task };
        cx.notify();
    }

    fn on_started(
        &mut self,
        generation: u64,
        result: Result<OwnedStream, String>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        match result {
            Ok(stream) => {
                let poll = cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor().timer(POLL).await;
                        if this.update(cx, |this, cx| this.tick(cx)).is_err() {
                            break;
                        }
                    }
                });
                self.run = Run::Running {
                    stream,
                    _poll: poll,
                };
            }
            Err(err) => {
                tracing::warn!(%err, "device log stream failed to start");
                self.run = Run::Failed(err);
            }
        }
        cx.notify();
    }

    fn tick(&mut self, cx: &mut Context<Self>) {
        if self.paused {
            return;
        }
        let Run::Running { stream, .. } = &self.run else {
            return;
        };
        let Some(stream) = stream.get() else { return };
        // Read before draining so that nothing queued after the exit is lost.
        let closed = stream.is_closed();
        let entries = stream.drain();
        let dropped = stream.dropped();
        let error = closed.then(|| stream.error()).flatten();
        let started = std::time::Instant::now();
        let changed = !entries.is_empty() || dropped != self.dropped || closed;
        self.dropped = dropped;
        let shown = self.model.push(entries);
        if shown > 0 && self.follow.is_on() {
            self.scroll.scroll_to_bottom();
        }
        if closed {
            self.run = Run::Closed { error };
        }
        if changed {
            tracing::trace!(
                lines = shown,
                total = self.model.total(),
                took_us = started.elapsed().as_micros() as u64,
                "device logs polled"
            );
            cx.notify();
        }
    }

    fn click_line(&mut self, seq: u64, shift: bool, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.selection = Some(model::select(
            self.selection.as_ref().map(|(a, _)| *a),
            seq,
            shift,
        ));
        self.notice = None;
        cx.notify();
    }

    /// Copies the selected lines, or every visible line without a selection.
    fn copy(&mut self, cx: &mut Context<Self>) {
        let range = self.selection.as_ref().map(|(_, r)| r);
        let text = self.model.text(range);
        let lines = text.lines().count();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.notice =
            Some(format!("Copied {lines} line{}", if lines == 1 { "" } else { "s" }).into());
        cx.notify();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        self.model.clear();
        self.selection = None;
        self.notice = None;
        cx.notify();
    }

    fn toggle_pause(&mut self, cx: &mut Context<Self>) {
        self.paused = !self.paused;
        cx.notify();
    }

    fn toggle_follow(&mut self, cx: &mut Context<Self>) {
        let on = !self.follow.is_on();
        self.follow.set(on);
        if on {
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    fn status(&self, cx: &App) -> (SharedString, Hsla) {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let count = if self.model.len() == self.model.total() {
            format!("{} lines", self.model.len())
        } else {
            format!("{} of {} lines", self.model.len(), self.model.total())
        };
        match &self.run {
            Run::Off => (SharedString::default(), muted),
            Run::Starting { .. } => ("Starting…".into(), muted),
            Run::Failed(err) => (format!("Failed: {err}").into(), theme.danger),
            Run::Running { .. } if self.paused => {
                (format!("Paused · {count}").into(), theme.warning)
            }
            Run::Running { .. } => (count.into(), muted),
            Run::Closed { error } => match error {
                Some(err) => (format!("Closed: {err}").into(), theme.danger),
                None => (format!("Stream closed · {count}").into(), theme.warning),
            },
        }
    }

    fn render_bar(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let open = self.settings.open;
        let (status, status_color) = match &self.notice {
            Some(notice) => (notice.clone(), theme.muted_foreground),
            None => self.status(cx),
        };
        let dropped = (self.dropped > 0).then(|| {
            div()
                .flex_shrink_0()
                .text_color(theme.warning)
                .child(format!("{} dropped", self.dropped))
        });
        let restartable = matches!(self.run, Run::Closed { .. } | Run::Failed(_));
        let mono = || {
            div()
                .flex_shrink_0()
                .font_family(theme.mono_font_family.clone())
                .font_features(crate::ui::mono_features())
                .text_color(theme.muted_foreground)
                .whitespace_nowrap()
        };
        let counts = (self.model.total() > 0).then(|| mono().child(self.model.counts().summary()));
        let filter = self.filter.clone().map(|f| {
            mono()
                .max_w(px(160.))
                .truncate()
                .child(SharedString::from(f))
        });
        let (filter_open, filter_closed) = if open { (filter, None) } else { (None, filter) };
        h_flex()
            .h_7()
            .px_2()
            .gap_1()
            .flex_shrink_0()
            .items_center()
            .border_t_1()
            .border_color(theme.border)
            .text_xs()
            .child(
                Button::new("device-logs-toggle")
                    .ghost()
                    .xsmall()
                    .label(if open { "Logs ▾" } else { "Logs ▸" })
                    .selected(open)
                    .tooltip("Device log")
                    .on_click(cx.listener(move |this, _, _, cx| this.set_open(!open, cx))),
            )
            .children(counts)
            .when(!open, |this| {
                this.child(div().flex_1()).children(filter_closed)
            })
            .when(open, |this| {
                this.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(status_color)
                        .child(status),
                )
                .children(filter_open)
                .children(dropped)
                .when(restartable, |this| {
                    this.child(
                        Button::new("device-logs-restart")
                            .ghost()
                            .xsmall()
                            .label("Restart")
                            .tooltip("Start the log stream again")
                            .on_click(cx.listener(|this, _, _, cx| this.start(cx))),
                    )
                })
                .child(
                    Button::new("device-logs-pause")
                        .ghost()
                        .xsmall()
                        .label("Pause")
                        .selected(self.paused)
                        .tooltip("Stop reading new lines (the device keeps buffering)")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_pause(cx))),
                )
                .child(
                    Button::new("device-logs-follow")
                        .ghost()
                        .xsmall()
                        .label("Follow")
                        .selected(self.follow.is_on())
                        .tooltip("Stick to the newest line; scrolling up turns it off")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_follow(cx))),
                )
                .child(
                    Button::new("device-logs-copy")
                        .ghost()
                        .xsmall()
                        .label("Copy")
                        .tooltip("Copy the selected lines, or all visible lines (⌘C)")
                        .on_click(cx.listener(|this, _, _, cx| this.copy(cx))),
                )
                .child(
                    Button::new("device-logs-clear")
                        .ghost()
                        .xsmall()
                        .label("Clear")
                        .tooltip("Discard the lines shown")
                        .on_click(cx.listener(|this, _, _, cx| this.clear(cx))),
                )
            })
    }

    fn render_filters(&self) -> impl IntoElement + use<> {
        h_flex()
            .px_2()
            .pb_1()
            .gap_1()
            .flex_shrink_0()
            .items_center()
            .child(
                div()
                    .w(px(96.))
                    .flex_shrink_0()
                    .child(Select::new(&self.level).small()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.process).small().cleanable(true)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(&self.search).small().cleanable(true)),
            )
    }

    fn render_line(&self, ix: usize, line: &Line, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let color = match line.level {
            LogLevel::Verbose | LogLevel::Debug => theme.muted_foreground,
            LogLevel::Info => theme.foreground,
            LogLevel::Warn => theme.warning,
            LogLevel::Error | LogLevel::Fault => theme.danger,
        };
        let seq = line.seq;
        let selected = self
            .selection
            .as_ref()
            .is_some_and(|(_, range)| range.contains(&seq));
        div()
            .id(("device-log-line", ix))
            .h(px(ROW_HEIGHT))
            .w_full()
            .px_2()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_color(color)
            .when(selected, |this| this.bg(theme.selection))
            .when(line.level == LogLevel::Fault, |this| this.font_semibold())
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.click_line(seq, event.modifiers().shift, window, cx);
            }))
            .child(line.text.clone())
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let list = uniform_list(
            "device-logs-list",
            self.model.len(),
            cx.processor(|this, range: Range<usize>, _, cx| {
                tracing::trace!(?range, total = this.model.len(), "device log rows rendered");
                range
                    .filter_map(|ix| this.model.get(ix).map(|l| this.render_line(ix, l, cx)))
                    .collect::<Vec<_>>()
            }),
        )
        .size_full()
        // As wide as the longest visible line: long lines scroll sideways instead of being cut.
        .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
        .with_width_from_item(self.model.widest_index())
        .track_scroll(&self.scroll);
        let empty = (self.model.len() == 0).then(|| {
            let text = if self.model.total() > 0 {
                "No line matches the filters."
            } else if matches!(self.run, Run::Running { .. }) {
                "Waiting for log lines…"
            } else {
                ""
            };
            div()
                .absolute()
                .size_full()
                .p_2()
                .text_color(theme.muted_foreground)
                .child(text)
        });
        let handle = div()
            .id("device-logs-resize")
            .absolute()
            .top(px(-3.))
            .left_0()
            .w_full()
            .h(px(6.))
            .cursor_row_resize()
            .on_drag(LogsResize, |_, _, _, cx| cx.new(|_| LogsResize));
        div()
            .relative()
            .h(px(self.height))
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme.border)
            .font_family(theme.mono_font_family.clone())
            .font_features(crate::ui::mono_features())
            .text_size(px(11.))
            .child(list)
            .children(empty)
            .child(handle)
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<LogsResize>, _, cx| {
                    let height = (event.bounds.bottom() - event.event.position.y).as_f32();
                    this.height = height.clamp(MIN_LIST_HEIGHT, MAX_LIST_HEIGHT);
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(|this, _: &ScrollWheelEvent, _, cx| {
                this.wheel = true;
                cx.notify();
            }))
    }
}

impl Focusable for LogsPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for LogsPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.wheel) {
            self.follow.user_scrolled(self.scroll.is_scrolled_to_end());
            if !self.follow.is_on() {
                // A scroll to the bottom queued before the user moved away must not win.
                self.scroll.0.borrow_mut().deferred_scroll_to_item = None;
            }
        }
        let bar = self.render_bar(cx);
        let open = self.settings.open;
        let filters = open.then(|| self.render_filters());
        let list = open.then(|| self.render_list(cx));
        v_flex()
            .id("device-logs")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let key = &event.keystroke;
                if key.modifiers.platform && key.key == "c" {
                    this.copy(cx);
                    cx.stop_propagation();
                }
            }))
            .w_full()
            .flex_shrink_0()
            .bg(cx.theme().background)
            .child(bar)
            .children(filters)
            .children(list)
    }
}
