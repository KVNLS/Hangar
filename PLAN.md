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
| files    | file watching (notify), live diff model, tree-sitter highlighting                | worktree       |
| devices  | Device trait; iOS (private + simctl fallback), Android (M4)                      | —              |
| runner   | build/run per stack: Xcode, Gradle, React Native, Flutter                        | devices        |
| mcp      | MCP server exposing device tools to agents (M8)                                  | devices        |
| app      | GPUI shell, all views (terminal, diff, device), wiring                           | all            |

Views (terminal, diff, device) live in `app`; library crates expose plain data and input enums.
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
- `devices`: `boot`, `install`, `launch`, `logs`; simctl backend input on Xcode 27.

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

## M2 gaps (next)
- Delete/archive workspaces and projects, worktree removal from the UI.
- More than one session per workspace; pane layout persisted across restarts; Escape to cancel inputs.
- Store errors during session bookkeeping are only logged.
- Terminal selection/copy and mouse reporting (from Spike A).

## Next
- Close the M2 gaps above, then M3 live changes and review (`files` crate: notify + diff view).
