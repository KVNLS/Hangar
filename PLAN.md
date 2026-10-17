# Hangar — Plan

Hangar is a macOS desktop app (Rust + GPUI) that runs several CLI coding agents in parallel, each in its own
Git worktree, with live diffs and an embedded iOS simulator / Android emulator. Think Superset, for mobile.

## Principles
- Agents are the user's own CLIs (Claude Code, Codex CLI, OpenCode, Gemini CLI). No model calls are proxied.
- Headless core: only `app` depends on GPUI. Everything else is testable without a window.
- macOS first; Linux kept possible. iOS-specific code is `#[cfg(target_os = "macos")]`.
- Private Apple frameworks only inside `devices` (or its helper), behind the `Device` trait, with a public
  fallback (`simctl`) always available.
- Hexagonal: dependencies point inwards to `core`; see docs/ARCHITECTURE.md for the dependency rule
  (checked by `crates/app/tests/architecture.rs`) and the migration roadmap.

## Crates
| crate    | owns                                                                             | depends on     |
|----------|----------------------------------------------------------------------------------|----------------|
| core     | domain: Project / Workspace / Session, AgentState, device, changes, review; ports | —              |
| usecases | application services: workspaces, sessions, settings, review; pure policies: agent queue and attention, batch / best-of-N, orchestration guard rails, history; device pool state machine with binding, reservations and device sharing | core |
| store-sqlite | SQLite Store (projects, workspaces, sessions with history and cost, usage ledger, settings; schema v9) | core  |
| worktree | git/gh CLI wrapper; `GitCli` and `Gh` implement the git and code-host ports      | core           |
| agents   | AgentAdapter trait, PtyAdapter (a `Launch`); ACP client (`acp`); Ollama; Claude Code transcripts | — |
| terminal | PTY (portable-pty), emulation (alacritty_terminal), grid snapshots, key encoding | —              |
| files    | file watching (notify, debounced, git-ignore aware), LiveDiff over `Changes`     | core           |
| devices  | Device trait and capabilities; iOS (private + simctl fallback), Android          | core           |
| runner   | build/run per stack: Xcode, Gradle, React Native, Expo                           | core           |
| mcp      | MCP server for agents: protocol engine, socket server, stdio bridge; device tools (`device_acquire` / `device_release` for sharing), orchestration and `ask_user` tools; Claude Code hook mapping (`Hangar notify`) | core |
| app      | GPUI shell, all views (terminal, chat, diff, device), wiring                     | all            |

Views live in `app`; library crates expose plain data and input enums. Syntax highlighting uses
gpui-component's tree-sitter features (per-language, enabled in `app`), so it lives in `app`, not `files`.
Dart is not bundled (needs `tree-sitter-dart`, not yet approved). Git access is the `git` CLI with porcelain
output, not a library.

## Roadmap
- **M1 Spikes**: PTY terminal in GPUI; iOS simulator frames + tap (private + simctl fallback).
- **M2 Parallel terminals**: projects/workspaces/sessions persisted; worktree per task; sidebar + panes.
- **M3 Live changes & review**: file watching, per-workspace diff, highlighting, merge/PR actions.
- **M4 Android device panel**: emulator boot, gRPC frames and input (adb fallback).
- **M5 iOS device panel**: headless boot, hardware buttons, keyboard (no rotation on Xcode 27).
- **M6 Mobile run loop**: build/run per stack, device pool, per-worktree dev-server ports.
- **M7 ACP structured agents**: chat pane, tool calls, permissions, modes, model/effort, resume.
- **M8 MCP device tools**: screenshot, tap, type, logs, build_and_run so agents verify their own UI work.
- **M9 Orchestration**: agent queue, approvals inbox, tasks, batch and best-of-N, orchestration MCP tools,
  `ask_user`, usage/cost and session history.
- **M10 Hardening** (owner decision, 2026-10-08):
  - Architecture (docs/ARCHITECTURE.md step 7): split the files over 800 lines; `DeviceCatalog` and
    `HostResources` ports; `DevicePool` as a sans-IO state machine; `HangarApp` groups as controllers so views
    do no I/O; `changes/review.rs` reaches `base_branch_name` through `ReviewOps`.
  - On-screen verification: walk the checklist below with the dev hooks, fix what breaks.
  - Onboarding (done 2026-10-08): a setup checklist on the welcome screen and in Settings › Setup (git and
    its identity, agents and ACP adapters, `gh` and its sign-in, Xcode, iOS runtimes, Android SDK and AVDs,
    JDK, Node), each with the command that fixes it; Hangar never runs installers itself.
  - Distribution: .app bundle, .icns and DMG shipped as v0.1.0 (`scripts/release.sh`, 2026-10-14), signed
    with an Apple Development certificate, arm64 only. Still open: Developer ID + notarisation (question 6),
    universal binary, light/tinted icon variants.
  - Done when: ARCHITECTURE "Done when" all ticked, the checklist seen, a signed .app runs on a clean user.

## Status
- [x] M1 Spike A — PTY terminal; Spike B — iOS simulator, private backend; simctl view-only (docs/notes/spike-b-notes.md)
- [x] M2 (first cut) — store, worktrees, sidebar, panes
- [x] M3 Live changes & review
- [x] M4 Android device panel — run live 2026-10-05
- [x] M5 iOS device panel
- [x] UI v0.1 — design system + views, merged 2026-10-05; design gap round 2026-10-07
- [x] M6 Mobile run loop (first cut)
- [x] M7 ACP structured agents (first cut) — Claude Code, OpenCode, Codex; Gemini unchecked
- [x] M8 MCP device tools (first cut)
- [x] M9 Orchestration — merged 2026-10-08, with follow-ups (`ask_user`, cost per day, terminal transcripts,
      file layout)
- [ ] M10 Hardening

What each milestone shipped and how it was checked: docs/HISTORY.md.

## Open questions
Numbers are stable; resolved questions moved to docs/HISTORY.md ("Resolved questions").
1. Where do worktrees live? Current default `~/Library/Application Support/Hangar/worktrees/<project>/<slug>`
   (only in `hangar-app::paths::worktrees_root`) vs `<repo>/../.hangar-worktrees/<slug>`.
4. Android frames: emulator gRPC console vs scrcpy protocol.
6. Distribution: signing/notarisation implications of loading Xcode private frameworks.
7. Terminal keys: what Shift-Enter sends (`\r`, `ESC \r`, kitty protocol); Option-as-Meta default.
8. simctl fallback has no tap on Xcode 27 (no Simulator.app, needs Accessibility, moves the real pointer):
   ship it as view-only?
9. Login PATH: `$SHELL -l -c` misses PATH set only in `.zshrc` (nvm, etc.); `-i -l` catches it but runs
   interactive startup scripts.

## Explicitly unimplemented (return `Unsupported`)
- `mcp` orchestration: `notifications/tools/list_changed` when the setting changes (agents see it on their
  next `tools/list` or in a new session).
- `devices`: `set_orientation`/`orientation` on iOS (no working route on Xcode 27, see docs/notes/m5-ios-notes.md);
  Android rotation needs a sensor-following foreground app and upside-down is usually refused (docs/notes/m4-android-notes.md);
  iOS log history (`LogFilter::history`); simctl backend: input on Xcode 27 (Device Hub), `send_text`,
  `Lock`/volume buttons (private backend only); iOS `AppSwitch`/`Back`; text beyond printable ASCII +
  `\n`/`\t` on the private backend.
- `mcp` device sharing: a push notification when a device grant arrives (no server-initiated MCP messages;
  agents poll `device_acquire` with `wait_s`).
- `agents::ollama`: `https://` Ollama hosts (`OllamaError::Unsupported`); the app shows the error.

## Not yet seen on screen (M10 checklist)
Unit-tested or protocol-checked only. Each needs one pass through the dev hooks (`HANGAR_DEV_SHOW`, …).
- Shell: palette "Approve" group; Dock bounce; in-use overlay (needs a run holding a lease); New workspace
  sheet with several agents picked; Compare › Keep; Base branch / New branch fields.
- Terminal: right-click menu, middle-click paste, Option-drag block selection.
- Review: "Pull and rebase" after a rejected push; merge dialog pushing the base.
- Devices: Android boot stage text and Rotate (Pixel_10_Pro shut down first, or a second AVD); logs height
  drag and sideways scroll; device sharing with two or three workspaces, `HANGAR_DEV_DEVICE_LIMIT=2` and
  `HANGAR_DEV_AGENT=claude-acp`: queue in the in-use overlay, waiting position in the sidebar, hand-over
  after idle expiry, Take over, the stale-app hint; the same with an Android AVD.
- Platform switch: an Expo / React Native repo with `HANGAR_DEV_DEVICE_LIMIT=1` and two workspaces with
  `HANGAR_DEV_AGENT=claude-acp`. Agent A verifies on iOS, then calls `device_acquire { platform: "android" }`,
  `build_and_run` and a screenshot; agent B, waiting on iOS, gets the iOS device at A's switch; switching
  back to iOS reuses the clone with no "creating" stage; the panel picker switches without deleting the
  clone; a Swift-only repo refuses `android`.
- Agents: Claude Code Resume click; Claude Code mode chip and slash list; Ollama settings and model chip;
  overview subagent rows and subagent chat; Input from `elicitation_dialog`; terminal transcripts read by a
  lead; ACP watchdog after a Hangar crash.
- Setup: copy button pasted in a terminal; re-check on coming back to the front after `gh auth logout`;
  the review screen's "Open Setup" row; the "Open Setup" action on a missing-agent error; the Command Line
  Tools and Xcode-missing states.
- MCP: Codex and Gemini wiring; OpenCode's MCP permission default; a real agent session using the device
  tools; Gradle/Expo builds on a pool Android instance; `ask_user` waits over a minute on agents other than
  Claude Code; `device_acquire` / `device_release` and `wait_s` polling from a real agent, including the
  "you are 2nd in line" error and the pending notices.

## Known gaps
- **QA sweep 2026-10, not fixed yet** (reports kept outside the repo, in the owner's notes):
  - Workspaces: a linked worktree can be added as its own project; delete stops the agent before
    checking the worktree can go; best-of-N has no rollback.
  - Review: submodule edits, mode-only and CRLF changes are shown poorly.
  - Run loop and devices: Android and the memory-pressure guard unchecked.
  - Agents: an `ask_user` whose bridge died while its agent lives stays open until the 30-minute timeout
    (no MCP cancellation); the chat shows only a long message's first 64 KB (no "show all"); resumed
    sessions may book their earlier cost again (unverified).
  - UI: the Keybindings page misses context-scoped bindings (terminal ⌘C/⌘A, chat esc, ⌘⏎).
  - Persistence: a read-only database has no read-only mode (move aside, continue without saving or quit);
    git version/identity probes have no deadline.
- **Devices:** simctl and Android frames still copied into the atlas (PLAN question 3 in docs/HISTORY.md); log lines from several
  processes can arrive a few ms out of order; logs: no multi-line merging, level filter client-side (low lines still fill the ring buffer),
  follow-tail ignores scrollbar drags (no scrollbar); emulators started outside Hangar keep Gboard's stylus
  sheet; iOS `logs` tool only returns a ~1.5 s capture; `AvdCost` can understate RAM;
  `HANGAR_DEVICE_ID`/`ANDROID_SERIAL` empty when the agent starts before its device;
  the `backend: …` line is not in the mockup (design call: hide unless fallback, or name the clone in the
  picker).
- **Device sharing:** a device tool call gives up after its budget (call timeout minus 3 s) while its device
  still boots, so a cold boot needs a second call; an Expo build that never prints its launched line holds its lease until
  its build tab exits or the run is replaced; agents poll with `wait_s` (no push on grant); holds are
  counted per workspace, not per call: after a base change replaces a workspace's own device, a stale
  release from an old call can lower the new device's hold count (needs per-call lease identity); on a
  base change of the same platform the old device's pending shutdown result can overwrite a new bring-up
  of the same key (keys are per platform, so a platform switch cannot); after a platform switch
  `HANGAR_DEVICE_PLATFORM`, `HANGAR_DEVICE_ID` and `ANDROID_SERIAL` still name the old device (the agent's
  environment cannot change; `device_acquire` and `device_info` give the real one);
  `mcp_host.rs` is near the 800-line split threshold (773 lines).
- **Run loop:** no parsed build progress or crash overlay; `expo run` prebuild
  output lands in the worktree; Expo picks Android devices by AVD name (ambiguous with pool instances);
  React Native CLI unit-tested only; the JDK/Gradle setup is the user's.
- **Agents / chat:** composer has no images or @-context; no keyboard navigation in the slash list and no
  mode-cycling key; tool cards show a changed-lines diff, not hunks; stopping a turn is esc or Stop (⌘. stays
  Run › Stop); a cross-message selection drops parts scrolled off screen; OpenCode does not keep its mode
  across `session/load`; Gemini needs an API key (Google sign-in retired); terminal agents other than Claude
  Code only report state by output timing; Claude Code's folder trust prompt shows as Idle.
- **Subagents:** no per-subagent diff counts or device on overview rows, and no device banner in the
  subagent view (the updated mockup shows both); only Claude Code reports parent links; nested subagents
  show inside their top-level one.
- **Usage and history:** no cost or tokens for PTY agents; no session history beyond today (the usage
  ledger keeps cost per model for any period); no diff when a workspace's live diff never ran; sessions
  stopped at startup count as ended then; the status bar's "today" counts a session started yesterday
  fully. Usage dashboard: a turn's cost is split across its models by token share (agents give no cost
  per model); OpenCode's `PromptResponse.usage` is unverified, so its local tokens (and the saving) may be
  unknown; savings are estimates at list prices (built-in table: Claude models only, others need a price
  typed in Settings); bars use the current UTC offset (off by an hour across a DST change); rows from
  before schema v8 have no model.
- **MCP:** no cancellation, resources or prompts; `claude-agent-acp` puts the session secret in its child's
  argv (visible to the same user in `ps`).
- **Ollama:** OpenCode only (Codex `--oss`, Claude Code `ANTHROPIC_BASE_URL` not done); models without tool
  calling are not filtered.
- **Shell / design (still open):** custom agent commands; recording keybindings; Clone in the in-use
  overlay (Take over is done); phone bezel and flash ring; project kind; Clone repository; agent memory and
  system RAM in the status bar; "Sandbox: worktree only" not shown (agents are not sandboxed); ⌘⏎ missing from the
  keybinding table; the toast covers the device panel's Shut down button for 2.8 s; queued starts not shown
  in the sidebar.
- **Notifications:** clicking one does not open its workspace; unbundled macOS builds (`cargo run`; the
  release .app has its own bundle id) show them as Finder's, and Windows as PowerShell's until Hangar is packaged with its own AUMID; only macOS was run.

## Owner decisions
- No ACP crate: the ACP client is hand-rolled (M7).
- Flutter dropped from the run loop (M6).
- Gemini gets `--skip-trust` and `GEMINI_CLI_SYSTEM_DEFAULTS_PATH` for MCP (M8).
- Ollama: OpenCode only for now (2026-10-05).
- Panes keep one workspace each: no multi-workspace tabs, "shell" tab or "+" tab (2026-10-07).
- ⌘R stays Build and run; the design's "Reload app" maps to it (2026-10-07).
- `VersionControl::branch_tips` is kept (2026-10-07).
- Settings › Permissions approved: notify and Dock badge, default approval mode, max concurrent agents
  (2026-10-07).
- M9: `AgentRuntime` port approved (not needed in the end), Store schema changes, orchestration MCP tools
  off by default and limited to an agent's own children, Claude Code hooks injected inline (2026-10-07).
- Each workspace remembers the agent it last started (2026-10-08).
- `ask_user` for every agent (2026-10-08).
- M10 is a hardening pass (2026-10-08).
- System notifications through `notify-rust`, only while Hangar is in the background, for new waits and
  for any finished turn (Working → Review or Idle) (2026-10-08).
- QA fix round (2026-10-09): one Hangar per database (`<db>.lock`); `agent_prompt` refused while the
  child waits for approval or input, prompts stripped of control characters; programs may write the
  clipboard (OSC 52) only when Settings › Appearance allows it (off by default); size limits on MCP
  messages and tool texts, chat messages, the Files viewer, terminal titles and clipboard writes.
- `Store::setting_keys(prefix)` added to the port, for leftover device assignments (2026-10-12).
- `agent_prompt` only reaches children whose agent reports its waits (`reports_waits` in `PROFILES`):
  terminal Claude Code (hooks) and the ACP presets; never terminal Codex/OpenCode/Gemini or a shell
  (2026-10-12).
- (2026-10-14) Device sharing: inside Hangar only; reservation with idle expiry; at the cap, a workspace
  shares a compatible running device rather than wait for one of its own.
- (2026-10-17) Platform switch: a workspace has one platform attached at a time, never an iOS and an
  Android device held together; an agent switches through the device queue.

## Next
1. Split the files over 800 lines (docs/ARCHITECTURE.md "Done when").
2. `DeviceCatalog` / `HostResources` ports and the `DevicePool` state machine, checked on real simulators and
   emulators.
3. Walk the on-screen checklist above.
4. Developer ID signing and notarisation (the .app bundle and DMG shipped in v0.1.0).
5. Gemini over ACP: needs a `GEMINI_API_KEY` (or Vertex AI) to check turns, modes and MCP live, or drop the
   presets.
