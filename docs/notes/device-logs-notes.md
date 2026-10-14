# Device log streaming notes

## Design
- `Device::logs(&LogFilter) -> Result<LogStream>` (`crates/devices/src/logs.rs`). Free functions:
  `android::logcat(serial, &filter)` (`logcat_with(adb_path, ..)`), `ios::log_stream(udid, &filter)`.
- `LogStream` owns the child process, a stdout reader thread and a stderr-tail thread. The queue is a
  `Mutex<VecDeque>` + `Condvar` with drop-oldest back-pressure: the reader never blocks, `dropped()` counts
  discarded entries. Consumers poll: `try_recv`, `drain` (UI timer), `recv_timeout` (background tasks).
  `is_closed()` / `error()` (stderr tail) report a child that exited by itself. `Drop` kills and reaps the
  child and joins the threads (verified: no `adb logcat` / `log stream` processes left after drop).
- `LogEntry`: device timestamp string (Android has no year/zone, iOS has full ISO with offset), level, pid,
  tid, tag (Android tag / iOS subsystem), category, process (iOS), message, `received_at`.
- Android: `adb -s S logcat -v threadtime -T <1|1000> [--pid=N] [*:L]`. `-T 1` skips the replay of the whole
  buffer. Package filter resolves `pidof -s` once at start (package must be running, else `NotFound`).
  Both `GrpcEmulator` and `AdbDevice` use adb (emulator gRPC has no logcat stream).
- iOS: `xcrun simctl spawn UDID log stream --style ndjson --level debug [--predicate ...]`. Both backends call
  the shared `ios::log_stream`. Process filter: `process == "x"`, plus `subsystem BEGINSWITH "x"` if it has a
  dot (bundle id). Values are quoted/escaped. Min level is applied client-side (`log stream --level` only
  knows default/info/debug).

## Measurements (Pixel_10_Pro, API 37 headless; iPhone 17, iOS 26.5, macOS host)
- Android: marker written with `adb shell log` appears in the stream 13-31 ms after the shell command was
  issued (the entry arrived before the shell call returned, so stream latency itself is below that).
- iOS: event-to-poll latency over 300 entries on an idle simulator: p50 0.9 ms, p95 8.2 ms, max 184 ms.
- iOS volume: a simulator right after boot emits about 7,500 entries/s unfiltered (61k lines in 8 s), so the
  default 10,000-entry queue drops under a stalled consumer; `dropped()` makes that visible.

## Gaps
- iOS history: `log stream` has no backlog; `history = true` returns `Unsupported` (`log show --last` could be
  chained later).
- Android package filter follows one pid; it does not follow an app restart (re-open the stream).
- Android timestamps lack year and time zone; consumers needing absolute time should use `received_at`.
- logcat prints a header per line, so multi-line messages are separate entries (a headerless line inherits the
  previous entry's metadata); no merging.
- Physical iOS devices and multiple logcat buffers (`-b`) are not covered. Only `main,system,crash` defaults.
- No wake-up callback; the UI polls `drain` on a timer.

## Verification
Unit tests on fixtures (`cargo test -p hangar-devices`); live: `HANGAR_TEST_ANDROID=<serial>
HANGAR_TEST_IOS=<udid> cargo test -p hangar-devices --test logs_live -- --ignored --nocapture`;
example `device_logs`.
