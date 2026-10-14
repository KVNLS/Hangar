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
| core     | domain: Project / Workspace / Session, AgentState, device vocabulary, Setting<T> | —              |
| store-sqlite | SQLite Store (projects, workspaces, sessions, settings)                      | core           |
| worktree | git CLI wrapper: worktree add/remove/list, status, diff                          | —              |
| agents   | AgentAdapter trait, PtyAdapter; ACP client (`acp`: protocol, stdio, AcpAdapter)  | terminal       |
| terminal | PTY (portable-pty), emulation (alacritty_terminal), grid snapshots, key encoding | —              |
| files    | file watching (notify, debounced, git-ignore aware), LiveDiff model              | worktree       |
| devices  | Device trait; iOS (private + simctl fallback), Android (M4)                      | core           |
| runner   | build/run per stack: Xcode, Gradle, React Native, Expo                           | core           |
| mcp      | MCP device tools for agents: protocol engine, socket server, stdio bridge (M8)   | core           |
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
5. ~~Per-worktree ports for Metro/Expo~~ — resolved (M6): 8100–8199 per workspace, saved in `settings`, must bind
   on localhost; passed as `--port` and `RCT_METRO_PORT`, `adb reverse` on Android (spikes/m6-run-notes.md).
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
- `agents::ollama`: `https://` Ollama hosts (`OllamaError::Unsupported`); the app shows the error.

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
- [x] M3 Live changes & review — watcher, live diff, changes panel, commit/push/PR/merge
- [x] M4 Android device panel — emulator boot/shutdown, gRPC frames/input (adb fallback), run live 2026-10-05
- [x] M5 iOS device panel — headless boot/shutdown, hardware buttons, keyboard (no rotation on Xcode 27)
- [x] UI v0.1 — design system + views, merged into main 2026-10-05
- [x] M6 Mobile run loop (first cut) — detection, ports, Run ⌘R in terminal tabs, native install/launch
- [x] M7 ACP structured agents (first cut) — Claude Code over `claude-agent-acp`, chat pane, Approval state
- [x] M8 MCP device tools (first cut) — per-workspace devices, capacity guard, tools over `Hangar mcp-bridge`

## Done since M2 first cut
- Terminal: selection (drag/word/line/shift-extend, auto-scroll), Cmd-C/Cmd-A, mouse reporting (1000/1002/1003,
  X10/UTF-8/SGR; Shift bypasses), focus reporting (1004).
- Workspaces: archive/restore, delete (status-checked, optional non-forced branch delete, only linked worktrees),
  remove project, Escape cancels, layout + agent picker persisted (schema v2 `settings`), store errors surfaced.
- Panes: one pane by default; split right (⌘D, pane tab bar, palette) up to 4 panes, close a pane (⌘W, ✕) while
  its agent keeps running. Layout saves each pane's width share; old two-pane layouts migrate (empty panes
  dropped, `split` → shares).
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
- M4 leftovers (2026-10-05): running emulators keyed by AVD id in the picker (saved selection survives
  reboots; a booted AVD opens without reselecting), boot stage + elapsed time while booting, stylus
  handwriting turned off on emulators Hangar boots (the emulator touchscreen reports stylus support, so
  Gboard's "Try out your stylus" sheet took every text field). Run live through the dev hooks
  (Pixel_10_Pro, gRPC): boot then auto-open, relaunch reopens the saved AVD, tap, text, Lock, shutdown.
  Volume keys: the first press only shows the volume panel (Android behaviour, same with adb).
- M5 panel: iOS boot/shutdown from the panel (all simulators listed, booted first), Home/Lock/Volume, keyboard
  text + keys. Rotation not available on iOS (Xcode 27).
- Device logs: `Device::logs` stream (adb logcat, simctl log stream) and a logs pane in the device panel
  (spikes/device-logs-notes.md).

## M6 run loop (first cut)
- `hangar-runner`: `detect` (Expo, React Native, Xcode, Gradle at the root, `ios/`, `android/`; Flutter dropped
  by the owner),
  `pick_stack` by device platform, `allocate_port`, `plan` (command lines + install/launch steps), `setup`
  (JS dependency install), `resolve_artifact` (xcodebuild build settings, AGP `output-metadata.json`).
- App: Run (⌘R, pane tab bar, palette) builds the focused workspace for the device open in the panel; Stop
  (⌘.), Restart dev server (palette). Build, install and dev-server terminals are tabs next to the agent;
  native builds install and launch through the device; the status bar shows the run state.
- Live: Xcode on the simulator, Gradle on the emulator, Expo on the simulator (spikes/m6-run-notes.md).
  React Native CLI is unit-tested only.
- Gaps: several Xcode schemes need a
  saved choice but there is no picker yet (the error lists them); no parsed build progress or crash overlay;
  `expo run` prebuild output lands in the worktree; the JDK/Gradle setup is the user's.

## Device pool and MCP device tools (first cut)
- One device per workspace (`device:<workspace id>` in settings): iOS = a simulator created from the base
  device's type + runtime (`Hangar · <project>/<slug>`), Android = a `-read-only` instance of the base AVD (the
  emulator allows several instances of one AVD only if all are read-only; a normally booted base AVD blocks them
  and the panel offers to shut it down). Booted on demand, shut down on archive, deleted with the workspace.
  The device panel shows the focused workspace's device; ⌘R runs each workspace on its own device.
- Capacity (`devices::capacity`): RAM, available memory, P/E cores and memory pressure from macOS; per-device
  cost (iOS 2.5 GB/1 core, Android AVD RAM + 1.5 GB / its cores) after an 8 GB + 4 core reserve gives the
  recommended number of devices at once. Settings › Devices: Auto or 1–8 (warning above the recommendation).
  Limit 1 = single-device mode: workspaces share the base device through FIFO leases ("Waiting for the device");
  Auto detecting 1 alerts once. Runtime guard: no boot under memory pressure or low free memory (falls back to
  a running device), and at critical pressure idle unfocused devices are shut down.
- MCP (`crates/mcp` + `app::mcp_host`): agents start `Hangar mcp-bridge`, which relays MCP stdio to the app's
  0700/0600 Unix socket with a per-workspace session secret (`HANGAR_MCP_SESSION`, in the agent's env only).
  Tools: device_info, screenshot, tap, swipe, type_text, press, logs, build_and_run, launch_app. Wiring:
  Claude Code `--mcp-config` (inline), OpenCode `OPENCODE_CONFIG_CONTENT`, Codex `-c mcp_servers.hangar=…`
  (`env_vars`), Gemini `GEMINI_CLI_SYSTEM_DEFAULTS_PATH` + `--skip-trust` (owner decision). Nothing is written
  in the worktree. Agents also get `HANGAR_DEVICE_PLATFORM`, `HANGAR_DEVICE_ID`, `ANDROID_SERIAL`.
- Live (2026-10-05, scratch repo): a shell agent in a workspace ran `Hangar mcp-bridge` with only its own env:
  initialize, tools/list, build_and_run (built and launched on the workspace's own new simulator), device_info,
  screenshot, tap (the counter went 0 → 1), screenshot; a wrong session got -32001.
- Not verified live: Codex and Gemini wiring (not installed), OpenCode's MCP permission default, a real agent
  session calling the tools (would use the owner's account); Gradle/Expo builds on a pool Android instance.
- Gaps: `HANGAR_DEVICE_ID`/`ANDROID_SERIAL` are empty when the agent starts before its device exists (created
  on first use; agents can call `device_info`); iOS `logs` only returns what is logged during a ~1.5 s capture; no queue position in single-device
  mode; `AvdCost` can understate RAM (the emulator raises it for new API levels); Expo picks Android devices by
  AVD name, ambiguous with several instances; MCP cancellation and resources/prompts not implemented.

## M7 ACP structured agents (first cut)
- `hangar_agents::acp`: hand-rolled ACP v1 client (owner decision: no ACP crate). JSON-RPC over the agent's
  stdio on std threads; `initialize` (no fs/terminal client capabilities, so the agent uses its own tools in
  the worktree), `session/new` with `mcpServers`, `session/prompt` (text), `session/cancel`, permission
  requests answered selected/cancelled, other agent requests get -32601. Update kinds Hangar does not know are
  reported as `Other` and logged. The agent runs in its own process group; stop = close stdin, SIGTERM, SIGKILL
  after 3 s.
- Preset "Claude Code (structured)" (`claude-acp`) runs `claude-agent-acp` (npm
  `@agentclientprotocol/claude-agent-acp`, Node 22+) from PATH; nothing is downloaded at runtime. Settings ›
  Agents shows the install command when it is missing. The hangar MCP bridge goes into `session/new`
  (secret in the server env, not argv).
- App: `LiveSession` holds a terminal or a chat (`AgentSurface`). `ChatView` (`crates/app/src/chat_view`):
  streamed markdown (gpui-component `TextView`), collapsed thoughts, tool-call cards (kind, status, clickable
  locations → Files tab, text output, line diffs, 40-line cap), plan, permission prompt (⏎ allow once, esc
  deny), "Working…" line, composer (⏎ send, ⇧⏎ newline, Stop/esc cancels the turn). `AgentState::from_acp`:
  pending permission → Approval, running turn → Working, failed turn or bad exit → Failed, changes → Review.
  The New workspace task and `HANGAR_DEV_INPUT` become the first prompt.
- Composer: the mode chip is a menu of the agent's modes (`session/set_mode`; claude-agent-acp answers `{}`
  and sends no `current_mode_update`, so the chip changes on success). Typing `/prefix` lists matching slash
  commands (from `available_commands_update`, up to 8); clicking one fills `/name `. Both are protocol- and
  build-checked; neither was seen on screen yet (the display slept during the check).
- Resume: each workspace's ACP session id is saved as `chat:<workspace id>` (with the preset that ran it).
  When the selected agent matches, the pane offers "Resume …" (`session/load`: the agent replays the history
  as updates before answering, so the transcript comes back) next to "New conversation". A refused load
  (e.g. -32002 unknown session) starts a new session and says so in the transcript. Checked against
  claude-agent-acp at the protocol level (history replayed, unknown id refused); not yet clicked in the app.
- Preset "OpenCode (structured)" (`opencode-acp`) runs `opencode acp` (OpenCode 1.18.34 checked). It sends
  its modes (build, plan) only as the `mode` select of `configOptions` (no `modes`), so `parse_modes` falls
  back to it and `config_option_update` maps to the current mode; `session/set_mode` still switches it.
  The model select is ignored. The hangar MCP bridge goes into `session/new` as for Claude: live check in the
  app listed the nine `hangar_*` tools. `session/load` replays history (Resume). bash runs without asking under
  OpenCode's default permissions; with `"bash": "ask"` it sends standard allow_once / allow_always /
  reject_once options (request id 0). Checked on screen: tool cards, mode menu (build/plan) and switch, slash
  list, Resume after restart, Approval state with Allow once and Reject. OpenCode does not keep the mode
  across `session/load` (comes back as `build`).
- Presets "Codex (structured)" (`codex-acp`, npm `@zed-industries/codex-acp` 0.16) and "Gemini CLI
  (structured)" (`gemini-acp`: `gemini --acp --skip-trust`, Gemini CLI 0.62; `--experimental-acp` is
  deprecated). Both get the hangar bridge through `session/new`. codex-acp checked at the protocol level:
  standard `modes` (Read Only default, Default, Full Access), loadSession, tool calls, session/new MCP servers
  used (MCP tool calls ask for approval). Its turn fails while `~/.codex/config.toml` picks a model the
  ChatGPT login refuses (owner's config, left alone). Gemini is not signed in here (Google sign-in for
  individuals no longer works with Gemini CLI): `session/new` answers
  -32000, which Hangar now reports as `NotSignedIn` with each preset's login hint. Gemini turns unchecked.
- Model and effort: the composer shows a chip per non-mode `configOptions` select the agent offers (Claude
  Code: model, effort, fast mode; Codex: model, then reasoning effort once a model with efforts is picked;
  OpenCode: model). `session/set_config_option` answers with the full new list, which replaces the chips;
  `config_option_update` does the same. Choices are saved per agent (`agent_config:<agent id>`, mode left
  out: it decides permissions) and re-applied right after each new session starts, model first. Checked on
  screen with Codex (GPT-5.5 → effort chip, Low; next session starts on GPT-5.5 / Low) and Claude Code.
- Quit (⌘Q, new) and window close stop every live agent (`on_app_quit`): codex-acp ignores a closed stdin
  and would outlive Hangar. A crash or SIGKILL of Hangar still leaves such agents running.
- Mono text (`TypeScale::font_mono`, terminal) turns Geist Mono's `liga`/`calt` off: its ligatures drew `--`
  over the preceding space (`git log--oneline`). The Files code viewer (gpui-component editor, theme font
  family only) still uses them.
- Gaps: no images or @-context in the composer, no
  keyboard navigation in the slash list (Shift-Tab is the text area's outdent, so no mode cycling key); `Input` state unused (ACP has
  no question request); tool cards show a simple changed-lines diff, not hunks; ⌘. stays Run › Stop, so
  stopping a turn is esc or the Stop button; no transcript virtualization (long sessions render every item).
- Live (2026-10-05, scratch repo, spikes/m7-acp-notes.md): streaming, tool cards, three permission prompts
  (Approval in the sidebar, ⏎ approves), `device_info` over the MCP bridge, an edit → Review `+1 −0`, esc
  stops a streaming turn, quitting leaves no agent process. `claude-agent-acp` puts the MCP session secret in
  its `claude` child's argv (visible to the same user in `ps`).

## UI v0.1 (design/Hangar.dc.html)
- Design system in `crates/app/src/ui`: tokens (dark/light, `cx.tokens()`), gpui-component theme mapping,
  Geist/Geist Mono, metrics, type scale, components; `ui_gallery` example. `AgentState` in `core`.
- Views: 38px title bar, status bar, sidebar, pane tab bars, right panel with Device/Changes tabs, review
  screen (⌘⇧R, split/inline), New workspace sheet (⌘N), command palette (⌘K), Overview (⌘⇧O), first-run
  welcome, Settings (⌘,) with saved appearance, Files tab (⌘3, tree + read-only viewer, no highlighting),
  Next needing you (⌘J).
- PTY agents only report running/exited: Working = terminal output in the last 3 s; Review = exited 0 with
  changes; Failed = exited non-zero.
- Deferred until backed (not rendered, no placeholders): chat stream, tool-call cards, permission prompts and
  the Approval/Input states (M7); parsed build progress, crash and "in use" device overlays; status bar RAM;
  Clone repository; custom agent commands; approval-mode and concurrency settings; phone bezel around the device.
- Visual pass 2026-10-05 (Claude, screenshots through `HANGAR_DEV_SHOW`): every screen in dark and light, plus
  the device panel on the iOS simulator and the Pixel_10_Pro emulator, compared with design/Hangar.dc.html.
  Fixed: title bar ⌘K search field and button order, changes/review footer (commit box and actions under the
  diff, design-system buttons, Create PR primary), no-remote repos no longer show gh's error, empty states and
  file tree rows in narrow panes, overview header order.
- Brand (design/Hangar Logo.dc.html, logo 8e): `crates/app/assets/brand/` holds the mark (256 tile, titlebar
  16px via `img()`) and the macOS app icon SVG (1024 icon grid). The Dock icon is rasterized and set at launch
  (`app_icon::set_dock_icon`), so `cargo run` shows it. `export_icon` example writes the 1024px PNG. No .app
  bundle / .icns yet; light (9b–9g) and tinted variants not shipped.

## Device panel visual pass (2026-10-05)
- Seen on screen (iOS, private backend): live frame, tap (opens Settings), Home, tooltips, logs pane (level,
  process, search, Pause/Follow/Copy/Clear), Shut down; no Rotate on iOS (unsupported). Android: the conflict
  state with its explanation and "Shut down emulator-5554 and retry".
- Fixed: a start that failed at launch lost its error (startup reconcile dropped closed entries), so the panel
  showed "its own device is not running" with no reason; failed entries are kept and the fallback title reads
  "This workspace's device". The control row no longer jumps when the input status line appears.

## Remaining gaps
- Device panel (design call): the `backend: …` line is not in the mockup, but in the normal case it is the
  only place naming the workspace's own clone (the picker names the base model). Options: hide it unless a
  fallback/forced backend; keep only the device name; or name the clone in the picker.
- Devices: lines from several processes can arrive a few ms out of order. Android boot stage text not seen
  yet (the only AVD was already running outside Hangar). Emulators started outside Hangar keep Gboard's stylus
  sheet until `stylus_handwriting_enabled` is turned off.

## Closed gaps (2026-10-05, round 2, unit-tested, not checked on screen)
- New workspace sheet: Base branch (default base first, then local branches) and New branch (empty:
  `hangar/<slug>`; a chosen name must be valid and free) fields.
- Terminal: right-click menu (Copy / Paste / Select all; not while a program tracks the mouse), middle-click
  pastes the selection else the clipboard, Option-drag block selection.
- Review: a non-fast-forward push rejection offers "Pull and rebase" (`pull_rebase`: fetch, rebase
  `--autostash`, conflicts aborted); the merge dialog can push the base afterwards (off by default).
- Device logs: list height dragged from its top edge (80–600 px); long lines scroll sideways.
- ACP agents: a watchdog in the agent's process group stops it if Hangar dies (crash, SIGKILL).
- No Geist Mono ligatures anywhere: the Files viewer already used `font_mono`; device logs and chat
  markdown code now turn them off too.

## Closed gaps (2026-10-05, unit-tested, not checked on screen)
- Push: git runs in its own session (no terminal for ssh prompts) and push gives up after 120 s
  (`Error::Timeout`, git's process group killed).
- Diff: untracked files go through one `git diff -M` via a scratch index (intent-to-add) and scratch object
  store in the temp dir; plain `mv` shows as a rename; nothing is written in the git directory (watchers).
- Pane layout saves are debounced (flushed on quit); Settings › Agents shows "Default" on the default row;
  clicking an archived workspace restores and opens it; the overview's last line skips a waiting prompt.

## Ollama local models (2026-10-05, unit-tested, not checked on screen)
- Settings › Agents › "Ollama (local models)": On/Off, host (plain HTTP, default `127.0.0.1:11434`), default
  model, status + Refresh. Saved under the `ollama` setting.
- `hangar_agents::ollama` lists models with a hand-written HTTP/1.0 `GET /api/tags` (no new dependency), off
  the UI thread, 2 s timeout; refreshed at startup when on, on opening Settings › Agents, on host change.
- OpenCode only (`opencode`, `opencode-acp`): Hangar adds an OpenAI-compatible `ollama` provider (`<host>/v1`,
  every installed model, `model` = the default) to `OPENCODE_CONFIG_CONTENT`, merged with the MCP config.
  In ACP chat the models show in OpenCode's model chip. Hangar never calls the models.
- Ollama on but unreachable or empty: the agent starts on its usual provider with a warning toast.
- Not done (owner decision for now): Codex (`--oss` / `model_provider`), Claude Code (`ANTHROPIC_BASE_URL`
  on Ollama's Anthropic-compatible API), filtering models without tool calling (`/api/show` capabilities).
- Dev: `HANGAR_DEV_OLLAMA=<host>` turns it on for the run (not saved) and waits for the model list before
  `HANGAR_DEV_OPEN` spawns the agent.

## Next
- Gemini over ACP: Google sign-in for individuals is retired ("migrate to Antigravity"); needs a
  `GEMINI_API_KEY` (or Vertex AI) before a turn, modes and MCP can be checked live, or drop the presets.
- Android boot stage text and Rotate on screen (needs Pixel_10_Pro shut down first, or a second AVD).
