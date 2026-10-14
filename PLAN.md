# Hangar — Plan

Hangar is a macOS desktop app (Rust + GPUI) that runs several CLI coding agents in parallel, each in its own
Git worktree, with live diffs and an embedded iOS simulator / Android emulator. Think Superset, for mobile.

## Principles
- Agents are the user's own CLIs (Claude Code, Codex CLI, OpenCode, Gemini CLI). No model calls are proxied.
- Headless core: only `app` depends on GPUI. Everything else is testable without a window.
- macOS first; Linux kept possible. iOS-specific code is `#[cfg(target_os = "macos")]`.
- Private Apple frameworks only inside `devices` (or its helper), behind the `Device` trait, with a public
  fallback (`simctl`) always available.

## Crates
| crate    | owns                                                                             | depends on     |
|----------|----------------------------------------------------------------------------------|----------------|
| core     | Project / Workspace / Session models, EventBus, SQLite Store                     | —              |
| worktree | git CLI wrapper: worktree add/remove/list, status, diff                          | —              |
| agents   | AgentAdapter trait; PtyAdapter (now), AcpAdapter (M7)                            | core, terminal |
| terminal | PTY (portable-pty), emulation (alacritty_terminal), grid snapshots, key encoding | —              |
| files    | file watching (notify, debounced, git-ignore aware), LiveDiff model              | worktree       |
| devices  | Device trait; iOS (private + simctl fallback), Android (M4)                      | —              |
| runner   | build/run per stack: Xcode, Gradle, React Native, Flutter                        | devices        |
| mcp      | MCP server exposing device tools to agents (M8)                                  | devices        |
| app      | GPUI shell, all views (terminal, diff, device), wiring                           | all            |

Views (terminal, diff, device) live in `app`; library crates expose plain data and input enums.
Syntax highlighting uses gpui-component's tree-sitter features (per-language, enabled in `app`), so it lives
in `app`, not `files`. Dart is not bundled (needs `tree-sitter-dart`, not yet approved).
Git access is the `git` CLI with porcelain output, not a library.

## Roadmap
- **M1 Spikes**: PTY terminal in GPUI; iOS simulator frames + tap (private + simctl fallback).
- **M2 Parallel terminals**: projects/workspaces/sessions persisted; worktree per task; sidebar + tiled panes.
- **M3 Live changes & review**: file watching, per-workspace diff, highlighting, merge/PR actions.
- **M4 Android device panel**: emulator list/boot, frames (emulator gRPC or scrcpy-style), input via adb.
- **M5 iOS device panel**: production version of Spike B, rotation, hardware buttons, keyboard.
- **M6 Mobile run loop**: build/run per stack, device pool, per-worktree Metro/dev-server ports.
- **M7 ACP structured agents**: AcpAdapter, tool-call/plan views.
- **M8 MCP device tools**: screenshot, tap, type, logs so agents verify their own UI work.

## Open questions
1. Where do worktrees live? Current default `~/Library/Application Support/Hangar/worktrees/<project>/<slug>`
   (only in `hangar-app::paths::worktrees_root`) vs `<repo>/../.hangar-worktrees/<slug>`.
2. ~~Rust FFI vs Swift helper for Indigo HID~~ — resolved: objc2 works on Xcode 27, no helper (SPIKES.md).
   Keep `sim_probe` to re-check each Xcode release.
3. Frame transport into GPUI: today BGRA copied twice (~12.6 MB/frame). Zero-copy via GPUI `paint_surface`
   needs a CoreVideo binding (new dependency, needs approval).
4. Android frames: emulator gRPC console vs scrcpy protocol.
5. Per-worktree ports for Metro/Expo (M6): allocation and env injection strategy.
6. Distribution: signing/notarisation implications of loading Xcode private frameworks.
7. Terminal keys: what Shift-Enter sends (`\r`, `ESC \r`, kitty protocol); Option-as-Meta default.
8. simctl fallback has no tap on Xcode 27 (no Simulator.app, needs Accessibility, moves the real pointer):
   ship it as view-only?
9. Login PATH: `$SHELL -l -c` misses PATH set only in `.zshrc` (nvm, etc.); `-i -l` catches it but runs
   interactive startup scripts.
10. Agent choice per workspace: stored per workspace, or the global picker at spawn time (current)?

## Explicitly unimplemented (return `Unsupported`)
- `devices`: `set_orientation`/`orientation` on iOS (no working route on Xcode 27, see spikes/m5-ios-notes.md);
  Android rotation needs a sensor-following foreground app and upside-down is usually refused (spikes/m4-android-notes.md);
  iOS log history (`LogFilter::history`); simctl backend: input on Xcode 27 (Device Hub), `send_text`,
  `Lock`/volume buttons (private backend only); iOS `AppSwitch`/`Back`; text beyond printable ASCII +
  `\n`/`\t` on the private backend.

## Device logs pane (app)
- `crates/app/src/device_panel/logs/`: collapsible pane under the device screen (virtualized 16 px rows, 20k-line
  ring buffer, client-side level and text filters, process filter restarts the stream, Pause/Follow/Clear/Copy,
  Cmd-C copies the selection or all visible lines). Pane visibility and min level are saved in `DeviceLayout`.
- Gaps: no horizontal scroll (lines are clipped, cut at 1000 chars); no multi-line message merging; the level
  filter is client-side only, so a high min level still fills the ring buffer with low-level lines; follow-tail
  reacts to the wheel and the Follow button, not to scrollbar drags (no scrollbar is drawn).

## What M1 taught us
- GPUI comes through `gpui-kit` (same repo/rev as gpui-component). gpui-kit's Root binds Tab/Shift-Tab for
  focus navigation; terminal views must override them in their key context.
- PTY output must be coalesced before waking GPUI (one redraw per 8 ms) or throughput and fps collapse.
- macOS PTYs can lose the tail of a fast-exiting child's output unless the master stays open until drained.
- Xcode 27 moved SimulatorKit and replaced Simulator.app with Device Hub; private APIs drift every release.

## Status
- [x] M1 Spike A — PTY terminal (SPIKES.md)
- [x] M1 Spike B — iOS simulator, private backend; simctl view-only
- [x] M2 (first cut) — core models + Store + EventBus, worktree CLI wrapper, agents PtyAdapter + presets,
      sidebar (projects → workspaces, add project, new workspace) + two resizable terminal panes.
      End-to-end flow verified headlessly; UI look and click paths still need a manual pass.

## Done since M2 first cut
- Terminal: selection (drag/word/line/shift-extend, auto-scroll), Cmd-C/Cmd-A, mouse reporting (1000/1002/1003,
  X10/UTF-8/SGR; Shift bypasses), focus reporting (1004).
- Workspaces: archive/restore, delete (status-checked, optional non-forced branch delete, only linked worktrees),
  remove project, Escape cancels, layout + agent picker persisted (schema v2 `settings`), store errors surfaced.
- M3 headless: unified diff parser + `diff_files`/`diff_file` (incl. untracked, renames, binary, truncation),
  `check_ignored`; `hangar-files` Watcher (150 ms debounce) + LiveDiff (incremental status).
  Write → DiffUpdate ~220–245 ms p50 (see spikes/m3-files-notes.md).

- M3 UI: per-workspace changes panel (file list + virtualized diff view, Swift/Kotlin/TS/... highlighting via
  gpui-component tree-sitter features), sidebar change badges, panel state persisted; LiveDiff started lazily,
  stopped on archive/delete. Verified end to end: edit → panel update 214 ms.

- M3 review actions: stage/commit (hooks respected), push (never forced; workspace branches created with
  `--no-track` so push never lands on base), PR via `gh` (status, create, link), merge into base (NoFf/Squash)
  only in a clean checkout of base, pre-checked with `git merge-tree`.

- M5 headless (iOS): `simctl::list_all`, `ios::boot/shutdown`, `Device::{boot,shutdown,install,launch}` on both
  backends, private-backend `Lock`/`VolumeUp`/`VolumeDown` and `send_text` through Indigo HID
  (spikes/m5-ios-notes.md).

- M4 Android: SDK discovery, headless emulator boot/shutdown, gRPC frames/input (adb fallback), device panel
  with Boot/Shutdown, Back/Home/Apps/Power/Volume, keyboard text + `Input::Key`, rotation with landscape-aware
  input (spikes/m4-android-notes.md).
- M5 panel: iOS boot/shutdown from the panel (all simulators listed, booted first), Home/Lock/Volume, keyboard
  text + keys. Rotation not available on iOS (Xcode 27).
- Device logs: `Device::logs` stream (adb logcat, simctl log stream) and a logs pane in the device panel
  (spikes/device-logs-notes.md).

## UI v0.1 (design/Hangar.dc.html)
- Design system in `crates/app/src/ui`: tokens (dark/light, `cx.tokens()`), gpui-component theme mapping,
  Geist/Geist Mono, metrics, type scale, components; `ui_gallery` example. `AgentState` in `core`.
- Views: 38px title bar, status bar, sidebar, pane tab bars, right panel with Device/Changes tabs, review
  screen (⌘⇧R, split/inline), New workspace sheet (⌘N), command palette (⌘K), Overview (⌘⇧O), first-run
  welcome, Settings (⌘,) with saved appearance.
- PTY agents only report running/exited: Working = terminal output in the last 3 s; Review = exited 0 with
  changes; Failed = exited non-zero.
- Deferred until backed (not rendered, no placeholders): chat stream, tool-call cards, permission prompts and
  the Approval/Input states (M7); build progress, crash and "in use" device overlays, Run/Reload (M6); Files
  tab (tree + viewer); "Next needing you ⌘J"; status bar build/RAM; per-workspace target device; Clone
  repository; custom agent commands; approval-mode and concurrency settings; phone bezel around the device.
- Not checked visually by Claude (no screen capture): every view needs the owner's manual pass.

## Remaining gaps
- Archived rows: click does nothing (restore via menu). Layout saved without debounce.
- Terminal: no block selection, right-click menu, middle-click paste.
- Diff: `mv` without `git mv` shows delete + untracked; one git process per untracked file.
- Review: push has no timeout (SSH prompts); no pull/rebase after rejection; merged base not pushed.
- Devices: device panel controls, logs pane and Rotate never checked visually (no screen capture in agent
  sessions); Android Lock/Volume not run live; picker key for running emulators is the adb serial (changes
  per boot); no boot progress; logs pane fixed height, no horizontal scroll.

## Next
- Manual pass over the device panel (both platforms), then M6 mobile run loop.
