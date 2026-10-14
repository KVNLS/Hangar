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
1. Where do worktrees live? `<repo>/../.hangar-worktrees/<slug>` vs `~/Library/Application Support/Hangar/worktrees`.
2. Indigo HID ABI changes across Xcode versions (baguette reports the call idb/AXe used broke in Xcode 26) —
   Rust FFI vs a small Swift helper process?
3. Frame transport into GPUI: copy BGRA per frame vs zero-copy IOSurface → Metal texture (needs GPUI support).
4. Android frames: emulator gRPC console vs scrcpy protocol.
5. Per-worktree ports for Metro/Expo (M6): allocation and env injection strategy.
6. Distribution: signing/notarisation implications of loading Xcode private frameworks.

## Status
- [ ] M1 Spike A — PTY terminal
- [ ] M1 Spike B — iOS simulator
- [ ] M2 started
