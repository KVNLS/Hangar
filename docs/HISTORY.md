# Hangar — History

What each milestone shipped and how it was checked; PLAN.md holds the current plan.

## What M1 taught us
- GPUI comes through `gpui-kit` (same repo/rev as gpui-component). gpui-kit's Root binds Tab/Shift-Tab for
  focus navigation; terminal views must override them in their key context.
- PTY output must be coalesced before waking GPUI (one redraw per 8 ms) or throughput and fps collapse.
- macOS PTYs can lose the tail of a fast-exiting child's output unless the master stays open until drained.
- Xcode 27 moved SimulatorKit and replaced Simulator.app with Device Hub; private APIs drift every release.
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
  (../spikes/m5-ios-notes.md).

- M4 Android: SDK discovery, headless emulator boot/shutdown, gRPC frames/input (adb fallback), device panel
  with Boot/Shutdown, Back/Home/Apps/Power/Volume, keyboard text + `Input::Key`, rotation with landscape-aware
  input (../spikes/m4-android-notes.md).
- M4 leftovers (2026-10-05): running emulators keyed by AVD id in the picker (saved selection survives
  reboots; a booted AVD opens without reselecting), boot stage + elapsed time while booting, stylus
  handwriting turned off on emulators Hangar boots (the emulator touchscreen reports stylus support, so
  Gboard's "Try out your stylus" sheet took every text field). Run live through the dev hooks
  (Pixel_10_Pro, gRPC): boot then auto-open, relaunch reopens the saved AVD, tap, text, Lock, shutdown.
  Volume keys: the first press only shows the volume panel (Android behaviour, same with adb).
- M5 panel: iOS boot/shutdown from the panel (all simulators listed, booted first), Home/Lock/Volume, keyboard
  text + keys. Rotation not available on iOS (Xcode 27).
- Device logs: `Device::logs` stream (adb logcat, simctl log stream) and a logs pane in the device panel
  (../spikes/device-logs-notes.md).

## M6 run loop (first cut)
- `hangar-runner`: `detect` (Expo, React Native, Xcode, Gradle at the root, `ios/`, `android/`; Flutter dropped
  by the owner),
  `pick_stack` by device platform, `allocate_port`, `plan` (command lines + install/launch steps), `setup`
  (JS dependency install), `resolve_artifact` (xcodebuild build settings, AGP `output-metadata.json`).
- App: Run (⌘R, pane tab bar, palette) builds the focused workspace for the device open in the panel; Stop
  (⌘.), Restart dev server (palette). Build, install and dev-server terminals are tabs next to the agent;
  native builds install and launch through the device; the status bar shows the run state.
- Live: Xcode on the simulator, Gradle on the emulator, Expo on the simulator (../spikes/m6-run-notes.md).
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
- Default approval mode (Settings › Agents & providers › Permissions; `permissions:approval_level`, Ask every
  time when unset): `acp::approval::ApprovalLevel::mode` picks each new session's mode right after the saved
  config options, by known id, else by name/description. Ask / Auto-approve reads → Claude `default`, Codex
  `read-only`, Gemini `default`; Full auto → `bypassPermissions`, `full-access`, `yolo`. OpenCode (build/plan)
  has neither, so it keeps its default (logged). No agent has a reads-only mode: at Auto-approve reads and
  Full auto the chat answers `read`/`search` permission requests with allow once (`fetch` excluded). A mode
  picked in the composer is saved per workspace (`mode:<workspace id>`) and wins for that agent. Terminal
  agents are not affected. Not seen on screen yet; Gemini's modes come from its source, not a live session.
- Quit (⌘Q, new) and window close stop every live agent (`on_app_quit`): codex-acp ignores a closed stdin
  and would outlive Hangar. A crash or SIGKILL of Hangar still leaves such agents running.
- Mono text (`TypeScale::font_mono`, terminal) turns Geist Mono's `liga`/`calt` off: its ligatures drew `--`
  over the preceding space (`git log--oneline`). The Files code viewer (gpui-component editor, theme font
  family only) still uses them.
- Gaps: no images or @-context in the composer, no
  keyboard navigation in the slash list (Shift-Tab is the text area's outdent, so no mode cycling key); `Input` state unused (ACP has
  no question request); tool cards show a simple changed-lines diff, not hunks; ⌘. stays Run › Stop, so
  stopping a turn is esc or the Stop button. Transcript virtualization: see M9.
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

## Closed gaps (2026-10-05, unit-tested, not checked on screen)
- Push: git runs in its own session (no terminal for ssh prompts) and push gives up after 120 s
  (`Error::Timeout`, git's process group killed).
- Diff: untracked files go through one `git diff -M` via a scratch index (intent-to-add) and scratch object
  store in the temp dir; plain `mv` shows as a rename; nothing is written in the git directory (watchers).
- Pane layout saves are debounced (flushed on quit); Settings › Agents shows "Default" on the default row;
  clicking an archived workspace restores and opens it; the overview's last line skips a waiting prompt.

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

## Device logs pane (app)
- `crates/app/src/device_panel/logs/`: collapsible pane under the device screen (virtualized 16 px rows, 20k-line
  ring buffer, client-side level and text filters, process filter restarts the stream, Pause/Follow/Clear/Copy,
  Cmd-C copies the selection or all visible lines). Pane visibility and min level are saved in `DeviceLayout`.
- Gaps: no horizontal scroll (lines are clipped, cut at 1000 chars); no multi-line message merging; the level
  filter is client-side only, so a high min level still fills the ring buffer with low-level lines; follow-tail
  reacts to the wheel and the Follow button, not to scrollbar drags (no scrollbar is drawn).

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

## Overview subagents (2026-10-06, unit-tested, not checked on screen)
- Claude Code (ACP) subagents: `Agent`/`Task` tool calls (`_meta.claudeCode.toolName`) and their children
  (`_meta.claudeCode.parentToolUseId`) become `acp::subagents::Subagent`s (running / waiting for approval /
  done / failed, current activity). Hangar stays on the adapter's fallback mode (no `subagents` client
  capability, so no `subagent_spawned` child sessions).
- Overview cards list the current turn's subagents ("N subagents · …", up to 4 rows, "+N more"); a row opens
  that subagent's chat. The header counts running subagents.
- Chat: every item has an owner (top-level subagent launch, or the agent). The agent's transcript hides
  subagent items (each launch card has "Open"); a subagent's view shows its task (`rawInput.prompt`), text,
  tool calls and what it returned, with a back header (esc) and no composer. Permission prompts show in both.
  Subagent text chunks (stamped with the parent id) no longer merge into the agent's messages; Hangar
  advertises `clientCapabilities._meta["subagent-transcript"]` for the text that was not streamed.
- Not done: no per-subagent diff counts or devices on the rows; OpenCode, Codex and Gemini report no
  parent link; a nested subagent shows inside its top-level one, not in a view of its own.

## Design gap round (2026-10-07, unit-tested, not checked on screen)
- **Navigation:**
  - esc leaves Overview and Settings, and the title crumb follows the screen.
  - The right panel has one width shared by its tabs (440, min 360) and one toggle, ⌘⇧D, that remembers the last tab.
  - Panes are columns of stacked panes (max 4): ⌘\ or ⌘D splits right, ⌘| (⌘⇧\) splits down, ⌘⌥1–4 focus a pane, and a "…" menu holds the rest.
  - Terminal footer `pty · <program> · cols×rows`. The sidebar is resizable (256, min 200).
- **Review:**
  - "Full review ⌘⇧R" in the Changes panel opens the review screen on the selected file.
  - Diffs read "→ <base branch>" (`worktree::base_branch_name`).
  - The commit box has a subject and a body; the subject is prefilled with the workspace name.
  - "Co-authored-by agent" trailer toggle (`review:coauthor_trailer`), ⌘⏎ commits (`changes::init`).
  - "Request changes" and "Ask agent about this file" type a prompt into the agent without sending it.
  - The PR toast and button read "PR #N".
  - Viewed files fade in the list.
  - The Files viewer has syntax highlighting and an added-line background.
  - The chat shows a "Ready for review" card after a turn that left changes.
- **Device panel:**
  - Coloured state label next to the device select, and booted dots in the picker.
  - Screenshot ⌘S saves a PNG under `$TMPDIR/Hangar/screenshots/<ws>` and types its path into the agent.
  - ⌘⇧H presses Home.
  - The logs bar counts warnings and errors.
  - An "In use by another workspace" overlay offers "Use <device> instead" (single-device mode only).
- **Shell:**
  - Sidebar state-count strip and a PROJECTS label.
  - The status bar counts the devices the pool runs (names in a tooltip); builds show only while running or failed, memory only under pressure.
  - Design toast for info messages (2.8 s, bottom right, as in the mockup).
  - Palette groups: Needs you / Workspaces / Actions / Device / App.
  - Overview cards get per-state actions and tints, plus a device chip.
  - Welcome shows agent paths and toolchain locations.
  - Settings show agent paths and versions, a Boot button on base devices, and a keybinding search.
  - New workspace shows each base branch's tip and age (new port method `VersionControl::branch_tips`, kept by the owner).
  - Settings › Permissions › Max concurrent agents (1–32, default 8): starts beyond it queue as Idle in FIFO order (pane: place, Start now, Cancel; status bar and overview: queued count) and start, with their agent and first task, when an agent ends. Lowering it never stops agents. The queue is saved (`agents:queue`) and comes back after a relaunch: restored starts run once the login PATH is known (see M9).
  - Settings › Permissions › Default approval mode (see M7: ACP modes; Hangar allows read/search requests once at "Auto-approve reads").
  - Settings › Permissions › Notify when an agent needs me (default on): the Dock badge counts workspaces in Approval/Input/Failed, and the Dock bounces once when one starts waiting while Hangar is in the background. Banner notifications need the .app bundle (unbundled, `UNUserNotificationCenter` throws and `NSUserNotificationCenter` is nil).
- **Still open** (owner calls or not backed; pane tabs and ⌘R are decided, see owner decisions below):
  - Custom agent commands.
  - Recording keybindings.
  - Build progress and crash overlays.
  - Clone / Take over in the in-use overlay (the pool can't do either).
  - Phone bezel and flash ring.
  - Project kind.
  - Clone repository.
  - Composer "@" attachments and context.
  - Agent memory and system RAM in the status bar.
  - "Sandbox: worktree only": not shown, because agents are not sandboxed.
  - ⌘⏎ is missing from the Settings keybinding table.
- On-screen pass (2026-10-07, dark + light, `HANGAR_DEV_SHOW` on a scratch repo):
  - Seen: workspace, review, overview, palette, new-workspace sheet, Settings › Agents / Devices / Keybindings, Device tab.
  - Fixed after the pass:
    - An idle agent that is still alive reads "Open" on overview cards, not "Resume".
    - Palette "Split pane down".
    - Keybindings show ⌘⇧\ for split down, and the ⌘\ row is labelled "(alternate)".
    - Agents settings no longer show "Default" twice.
  - The device select no longer shows the base model's state ("· Shutdown"): the dot says it, and the label beside it shows the device actually shown (the workspace's clone).
  - Seen with a live Claude Code (ACP) turn: "wants to run a command", ⏎ approving once, then the "Ready for review" card with "1 file changed, +1 −0" and the trailer on by default.
  - Seen with `HANGAR_DEV_SCREENSHOT=1` (new hook): the PNG is saved, its path is typed into the terminal agent, and the toast shows. The toast covers the device panel's Shut down button for 2.8 s (bottom right, as in the mockup).
  - Not seen live: the in-use overlay (needs a lease held by a run, so a buildable app; unit-tested).
  - Seen: Settings › Permissions (three rows) and, with the limit at 1, the queued pane ("Queued · next", Start now, Cancel) and "1 queued" in the status bar. With the level at Full auto, Claude Code (structured) starts in "Bypass permissions". Not seen: the Dock bounce.
- Owner decisions (2026-10-07):
  - Panes keep one workspace each: no multi-workspace tabs, "shell" tab or "+" tab.
  - ⌘R stays Build and run; the design's "Reload app" maps to it.
  - `VersionControl::branch_tips` is kept.
  - Settings › Permissions is approved: notify and Dock badge, default approval mode, max concurrent agents (in progress).
- `changes/review.rs` calls `hangar_worktree::base_branch_name` directly instead of through a `ReviewOps` port.


## System notifications (2026-10-08, branch `agent-notifications`)
- Owner decisions: `notify-rust` (macOS `NSUserNotificationCenter`, Windows toasts, freedesktop D-Bus);
  only while no Hangar window is key; "finished" is any turn end.
- Done: `Attention::observe` returns which workspaces started waiting (with their state) and which
  finished a turn (Working → Review or Idle; never on first sight). `app::notify` builds the text
  ("<workspace>", "Waiting for approval · <project>", "Ready for review · …", "Finished · …"), merges more
  than 3 into one, and posts off the UI thread. Settings › Permissions: "Notify when an agent needs me"
  now also gates these notifications; new "Notify when an agent finishes" (`permissions:notify_finished`,
  default on). A bundled .app attributes them to its bundle id; unbundled they come from Finder.
- Checked: unit tests (policy, text, settings round trip); an unbundled `notify-rust` send on macOS
  returned Ok. Not run on Windows or Linux.

## Zero-copy simulator frames (2026-10-08, branch `zero-copy-frames`)
- Owner decisions: vendor-patch GPUI's Metal renderer in the repo (`third_party/gpui-pre-apple`, port of zed
  PR #61291 with linear sampling) until the gpui-component pin ships BGRA surfaces; `core-video` and
  `core-foundation` as direct `hangar-app` deps (already built by gpui-pre).
- Done: `FramePixels::{Bgra, Live}` and `LiveSurface` in `core::device` with `Frame::snapshot`; the private
  backend publishes `devices::ios::LiveFramebuffer` on each seed change (no lock, no copy); `device_view`
  wraps it once in a `CVPixelBuffer` (`device_view::surface`) and calls `paint_surface`; other frames and
  unwrappable surfaces keep the `RenderImage` path (logged once). MCP screenshots, `save_png` and the
  dev-hook diffs snapshot.
- Checked: unit tests (core snapshot, IOSurface snapshot packing, CVPixelBuffer format/size/cache),
  `booted_simulator` on an iOS 26.5 simulator, and on screen: colours, orientation and scaled text match
  `simctl io screenshot`, Settings opening updated live at ~50 fps, the simctl backend still renders.
  Same 20 s app-switching workload, debug build: CPU 35.0% → 23.2% of a core, capture→paint ~9 → ~7 ms,
  frame rates unchanged.

## M9 Orchestration (2026-10-07 – 10-08)
- Owner decisions: approved a `core::ports::AgentRuntime` port with an agents use case in `usecases`; Store
  schema changes (`Workspace.task`, persisted queue, session history, parent links); MCP orchestration tools
  (off by default, an agent acts only on the workspaces it created); Claude Code hooks injected inline for
  terminal agents (`--settings`, nothing written in the worktree).
- Done: `hangar_usecases::agents` holds the max-concurrent-agents queue (`AgentQueue`) and the attention policy
  (`Attention`), moved out of `app` unchanged. The `AgentRuntime` port comes with its first caller (MCP tools).
- Done: workspaces store their task (`Workspace.task`, schema v3, `ALTER TABLE workspaces ADD COLUMN task`;
  older rows read `None`). `Workspace::title` (the task's first line, else the name) prefills the commit
  subject and the PR title, follows "Review ·" in the title bar, and shows under the name on overview cards.
  Queued starts (`StartRequest`, now in `usecases::agents`) are saved on every change and restored on launch
  for active workspaces. Checked live (scratch repo, limit 1, two workspaces): task saved and shown on the
  cards, second start queued and saved; after quit and relaunch it started and the saved queue emptied.
  Not shown in the sidebar (rows stay one line). A restored start begins without a click: it was asked for
  before the quit.
- Done: batch create and best of N. The New workspace sheet takes several agents (⌘-click adds or removes
  one) and "One workspace per line"; `usecases::batch::plan` turns the request into workspaces (lines ×
  agents, at most 12, branch name only for a single one, names "<name> · <agent>" or from the line), created
  one at a time and started through the queue. Several agents on one task share a `WorkspaceGroupId`
  (`Workspace.group`, schema v4). Overview cards of a group get "Compare N": an overlay with each attempt's
  state, diff (+/−, files; diffs start for compared workspaces) and last line, with Open, Review and "Keep
  this one" (confirms, then archives the other attempts; branches and worktrees stay). `HANGAR_DEV_AGENT`
  takes several comma-separated agents, `HANGAR_DEV_SHOW=compare` opens the overlay. Seen on screen: the
  overlay with two shell attempts and their diffs. Not seen: the sheet with several agents picked, Keep
  (unit-tested: `batch::others_to_archive`).
- Done: each workspace remembers the agent it last started (`workspace_agent:<id>`, saved when an agent
  launches, dropped with the workspace; older workspaces fall back to their saved conversation's agent).
  Overview Resume/Retry and the pane's Start/Resume/Restart run it, and resuming checks that agent's saved
  conversation. Seen: a shell workspace offers "Restart Shell" while the picker is on Claude Code.
- Done: permission requests answered without opening the pane. `ChatView::pending_request` /
  `answer_pending` (allow once, else reject once or outright). Overview cards in Approval show
  "Permission: <title>" with Deny and Allow once; the palette's "Approve" group (after Needs you) lists
  "Allow <workspace>: <title>" for every structured agent waiting with an allow-once option.
  `HANGAR_DEV_APPROVE=<s>` allows the first request through the same path. Live (Claude Code structured,
  scratch repo): `echo done > note.txt` asked, the card showed Deny / Allow once, the hook allowed it, the
  file was written (diff +1 240 ms later). Not seen on screen: the palette group. Claude Code runs
  read-only commands such as `date` without asking. Terminal agents have no request to answer here.
- Done: terminal Claude Code reports its state through hooks. The `claude` preset gets inline
  `--settings` hooks (UserPromptSubmit, PreToolUse, PostToolUse, PermissionRequest, Notification, Stop)
  running `Hangar notify <event>`, which maps the event (`hangar_mcp::hook::signal_for_hook`) to an
  `AgentSignal` and sends `hangar/agentSignal` over the MCP socket with the workspace's session secret; it
  always exits 0 (2 s timeout) so Claude Code is never held up. `AgentState::from_signal` gives Approval
  (permission prompt), Input (`elicitation_dialog`, not seen live), Review/Idle after Stop; a `Working` only
  holds while the terminal draws, because a late hook once landed after Stop. Other terminal agents keep the
  output-timing guess. `HANGAR_DEV_KEYS` types timed raw keys into the focused agent. Live (terminal Claude
  Code 2.1.285, scratch repo): prompt → working, permission dialog → Approval in sidebar, tab and status bar
  ("1 approval"), Enter → working → turn ended, file written. Seen: every new worktree first shows Claude
  Code's folder trust prompt (not a hook event, so Hangar shows Idle there); the owner's Claude config
  starts in auto mode, so most commands never ask.
- Done (2026-10-08, built by four parallel subagents on separate branches, cherry-picked onto
  `orchestration-core`; the coordinator resolved overlaps and ordered the migrations v5 then v6):
  - **MCP orchestration tools**, off by default (Settings › Permissions › "Let agents create and drive
    workspaces", `agents:orchestration`). When on, the hangar MCP server also lists `workspaces_list`,
    `workspace_create {task, name?, agent?}`, `agent_prompt {workspace, text}`, `agent_transcript {workspace,
    last? 1–50}`; when off they are not listed and a call gets an error naming the setting. `Workspace.parent`
    (schema v5, `parent_id`). A child starts from its lead's base branch in the same project, is named from
    the task's first line, runs the lead's agent unless one is given, and starts through the agent queue
    (reply: started, queued or failed to start). Guard rails are pure (`usecases::orchestration`; no
    `AgentRuntime` port was needed): only own active children (by id or name), depth 2, at most 8 active
    children (in-flight creates count); worktree creation is serialized. Creates and prompts show as a notice
    in a structured lead's chat or a toast for a terminal lead; children's overview cards show "↳ <lead>".
    Live (scratch repo, Claude Code structured as lead, Full auto): `workspace_create` made a shell child
    with `parent_id` set and the child ran its task. Seen on screen: the "Created workspace “…” with Shell"
    notice and the `workspaces_list` card (task, branch, agent, state, +1/−0, queued) in the lead's chat,
    "↳ lead" on the child's card, the Settings switch (`HANGAR_DEV_SHOW=settings-permissions` scrolls there).
  - **Usage and cost**: ACP `usage_update` `{used, size, cost?}` parsed into `SessionUpdate::Usage` and
    folded into `acp::usage::SessionUsage`; cost is the agent's running session total (a drop is treated as
    a restart and the earlier total is kept). Overview cards and the composer show "ctx 4% · $0.27", the
    status bar "$X live" summed over running structured agents. Claude Code and OpenCode report cost, Codex
    context only, Gemini nothing (shapes in spikes/m7-acp-notes.md). Seen on screen with Claude Code.
    Not done: per-turn `PromptResponse.usage`, cost across restarts or per day, PTY agents.
  - **Session history**: sessions record their first prompt and the workspace diff when they end, stop or
    are archived (schema v6: `sessions.prompt/files/insertions/deletions`; `Store::finish_session`,
    `Store::list_sessions_since`). `usecases::history` (pure) builds "Done today" from local midnight
    (offset from `localtime_r`). Overview has a collapsible "Done today" section (both groupings); rows open
    the workspace, archived ones are listed but not openable. Palette search also matches the task. Seen on
    screen. Gaps: no diff when the workspace's live diff never ran; no history beyond today; sessions marked
    stopped at startup count as ended then.
  - **Transcript virtualization**: the chat renders through GPUI's variable-height `list` (`ListState`,
    `FollowMode::Tail`); `chat_view::rows` (pure, tested) maps the current view's items (agent or subagent)
    plus task/result/done/working rows to list rows and turns new and changed items into splice/remeasure
    operations, so a streaming chunk remeasures one row. Seen on screen with a 40-line reply and a tool call.
    Known limit: a cross-message selection drops the parts scrolled well off screen (gpui-component sweeps
    unpainted selection participants).
- Follow-ups (2026-10-08, branch `m10-followups`):
  - **`ask_user`** for every agent (owner decision): `{question, choices? ≤6}` → `{answer, choice}`. The
    workspace reads Needs input (Dock badge, ⌘J) until answered from the answer sheet (overview card
    "Answer", pane banner "<agent> asks: …", palette "Answer" group, ⌘J opens it); choices are buttons,
    a typed answer sends with ⌘⏎, esc keeps it for later. The call waits up to 30 min
    (`Timeouts.ask`); the question is withdrawn when the agent stops or the workspace closes; a lead hears
    when a child asks. Codex gets `tool_timeout_sec=1800` (its default is 60 s). `HANGAR_DEV_ANSWER`.
    Live: Claude Code (structured) asked with two choices, choice 1 was picked, it created that file. Seen
    on screen: the pane banner, the answer sheet (question, choice buttons, typed answer, Later / Send ⌘⏎),
    Needs input in sidebar/tab/status bar, and the chat's "Waiting for your answer" line. Other agents'
    MCP timeouts for a long wait are unchecked (only waits under a minute were tried).
  - **Terminal transcripts**: terminal Claude Code's hooks pass `transcript_path`, accepted only as a
    `.jsonl` under `~/.claude/projects` after resolving links (`mcp_host::accept_transcript`), parsed by
    `agents::claude_transcript` off the UI thread. Other terminal agents return their recent screen and
    scrollback (`Terminal::recent_lines`, 120 lines, box rows dropped) as one approximate entry.
    Unit-tested; not run live with a lead.
  - **Cost across restarts and per day**: `Session.cost` (schema v7 `cost_amount/cost_currency`,
    `Store::record_session_cost`) saved when an agent reports a new total; status bar "$X today"
    (`history::cost_today`: today's ended sessions + this run's, each once), Done-today rows show cost.
    Live: a turn's $0.25 was saved while running and kept after a relaunch closed the session. A session
    started yesterday and still running counts fully today. Seen on screen: "$0.27 live · $0.27 today",
    and "$0.27 today" again after a relaunch; card chip "ctx 4% · $0.27".

## Usage & cost dashboard (2026-10-08, branch `cost-dashboard`)
- **Ledger**: `core::changes::UsageEntry` (session, time, model, local, cost, `TokenCount`), schema v8
  `usage` table (sessions that already had a cost become one row each, model unknown),
  `Store::record_usage` / `list_usage_since`.
- **Per-turn usage**: `acp::protocol::parse_turn_usage` reads the prompt response's `usage` and
  `_meta.quota.model_usage` (claude-agent-acp, codex-acp); `AcpEvent::TurnEnded { reason, usage }`.
  `SessionUsage::take_cost_delta` hands each reported amount out once (a restarted total included).
- **Booking**: `ChatView::take_spend` after a turn ends (or a cost arrives between turns, or the agent
  exits); `usecases::usage::book_turn` writes one row per model, splitting cost and tokens by each model's
  token share, else one row for the `model` config option. `ollama/…` models are local.
- **Settings › Usage & cost**: Day / Week / Month / Year (Monday weeks, local time) with ‹ › to step back;
  cost, turns, tokens, local share and estimated saving; cost per hour/day/month bar chart
  (gpui-component `BarChart`, per-model breakdown in the tooltip); per-model table; Local vs cloud card
  with the reference model picker (auto: the priced cloud model with the most tokens) and an editable
  $/Mtok saved as a custom price (`usage` setting). `usecases::usage::pricing::BUILT_IN`: Anthropic list
  prices as of 2026-10-06. `HANGAR_DEV_SHOW=settings-usage`.
- Seen on screen with seeded rows (Day view: $4.47, 6 turns, 11% local, ≈$1.07 saved vs Opus 5.5). Not run
  live with a paying agent or with Ollama on this branch.

## Agent choice at Start (2026-10-08, branch `agent-picker-at-start`)
- The sidebar "Agent" picker is gone: new workspaces choose in the New workspace sheet, the default is set in
  Settings › Agents ("Make default").
- A workspace with no session shows "No agent running" over a card of two choices: "Last conversation"
  (the agent that ran the saved chat, and Resume, which always uses that agent) when a chat is saved, and
  "New conversation" (an agent picker, installed agents enabled and current one checked, and Start). Picking an
  agent only selects it and the workspace remembers it (`workspace_agents`). The pane header shows the same
  picker next to Restart/Resume while a finished session is shown.

## Setup checklist (2026-10-08, branch `setup-checklist`)
- `core::setup` turns what a `ToolProbe` finds (login `PATH`, Homebrew, git and its identity, agent CLIs and
  ACP adapters, `gh` and its sign-in, Xcode and its first launch, iOS runtimes, Android SDK and AVDs, JDK,
  Node) into grouped checks, each with why it matters and the command that fixes it. Required: git, a git
  identity and one agent; everything else is optional. Hangar never runs installers: rows have a copy button.
- The welcome screen's step 2 is now "Your setup": found agents, then the required and Essentials/GitHub items
  still missing. Settings › Setup (first tab) lists everything. Re-check reads the login `PATH` again; coming
  back to the front re-checks (at most every 10 s) while either is on screen. The welcome screen scrolls and no
  longer claims Flutter support.
- Missing-agent/git/gh errors get an "Open Setup" action; the review screen shows the `gh` blocker with one.
- Seen on screen: Settings › Setup on a set-up Mac (`HANGAR_DEV_SHOW=settings-setup`, probe ~0.7 s), and the
  welcome screen with `env -i … SHELL=/bin/false PATH=/usr/bin:/bin` (login PATH, Homebrew, agent and gh rows).
  The copy button, re-check on activation and the review/notification actions are not yet clicked through.

## QA sweep and fix round (2026-10-09, branch `qa-fixes`)
- Eight QA agents tested one area each (workspaces, review, terminal, ACP/chat, MCP, devices,
  persistence, UI) with probe tests and the dev hooks on sandboxed databases; findings: 2 P0, 14 P1,
  ~35 P2, ~45 P3. This round fixed every P0 and P1 and the P2s sharing their causes, each with a
  regression test or a dev-hook run:
  - Crashes and memory: block selection past the last column (alacritty panic); Files viewer on
    `/dev/zero`, FIFOs and minified files; `book_turn` underflow; a 30 MB agent message (the chat lays out
    the first 64 KB).
  - MCP: messages capped at 8 MB, calls in flight capped (16 per connection, 64 overall), a write
    timeout, `maxLength` on texts; socket candidates tried in turn and a persistent error when none works;
    a timed-out `ask_user` is withdrawn; `agent_prompt` refused while the child waits for the user,
    control characters dropped, bracketed paste.
  - Agents: PTY kill signals the process groups and escalates to SIGKILL (and before quit); an invalid
    UTF-8 byte no longer drops an ACP connection; exit 0 mid-turn reads Failed; a stopped turn the agent
    never confirms ends after 10 s.
  - Persistence: one Hangar per database (`flock`); unreadable rows skipped; unreadable database:
    move aside / continue / quit; bad `usage` setting falls back; setup probes in parallel with 10 s
    deadlines and their process groups killed ("did not answer in time" rows); custom prices typed one
    field at a time and matched by prefix; the usage ledger survives deleting workspaces (schema v9).
  - Run loop and devices: ports reserved on ⌘R and checked on every address; a simulator shut down
    outside Hangar is closed within 10 s (seen live: 6 s); removing a project deletes its clones; simctl
    input says view only without Simulator.app.
  - Review: Pull and rebase after a rejected first push, conflicts left by the autostash reported,
    merge backend forced.
  - Terminal: bracketed-paste injection, Shift+Enter (ESC CR), Option as Meta and OSC 52 switches in
    Settings › Appearance, title and clipboard caps.
- Flaky `binary_crlf_mode_and_no_newline` was a live-diff bug: with untracked files the diff runs on a
  scratch copy of the index, and the copy's new mtime turned off git's racy-entry check, so a
  same-size edit in the same second as its index entry was missed once the diff ran a second later.
  The copy now keeps the index's mtime (`same_second_same_size_edit_is_seen_with_untracked_files`).
- Seen live with the dev hooks: second instance refused, corrupted database moved aside, 30 MB chat
  message, hung `gh` in setup, archive of a SIGHUP-ignoring agent, external simulator shutdown, exit 0
  mid-turn, the Terminal settings. Not driven live: the chat's stop timer (no dev hook presses Stop),
  removing a project with a real clone.

## Crash safety and leftovers (2026-10-12, branch `crash-safety`)
- PTY watchdog: each terminal agent gets one; when Hangar dies (crash, SIGKILL) it hangs up, then kills,
  the processes still on the agent's terminal (listed once with `ps -t`; the agent leads its own session,
  and macOS `pkill` has no `-s`). Seen live: a SIGHUP-ignoring agent gone 6 s after the app was SIGKILLed.
- `agent_prompt` refuses children whose agent does not report its waits (`reports_waits` column in
  `PROFILES`), since their approval state is only guessed.
- Leftover simulator copies: `Store::setting_keys`; at startup a notice offers to delete copies whose
  workspace no longer exists (this database's assignments only). Seen live with a throwaway copy; the
  Delete button itself was not clicked (it reuses the pool's `delete_clone`).

## Workspace edge cases (2026-10-12, branch `workspace-edges`)
- A repository without commits is refused when added ("no commits yet").
- A project base is never the literal `HEAD`: with a detached HEAD and no main/master, the only local
  branch is used, else the user is asked to check one out; `create` refuses a `HEAD` base left by older
  projects.
- Branch names that clash as paths (`hangar` vs `hangar/dev`, `hangar/dev` vs `hangar/dev/sub`) count as
  taken for the slug or are refused by name; the slug probe reads the branch list once.
- Initialized submodules are counted (`ChangeCounts.submodules`); the delete dialog lists them and forces
  only then, since git will not remove such a worktree otherwise.
- Tested against real repositories (`app::ops`, `worktree` adapter) and with fakes (`usecases`); not run
  through the UI.

## Run detection (2026-10-12, branch `run-detection`)
- Detection lists every candidate: JS apps in `apps/*`, `packages/*` and child folders; Xcode containers
  (folders only; a workspace hides its projects) at the root, in `ios/` and one level down; Gradle
  application modules from the settings `include`s (`apps:phone`), comments ignored. Dependencies and
  build output are skipped.
- Several apps for the device, or several schemes none named after the project: a dialog asks, the pick
  is saved in the run config (`stack`, `scheme`) and the run starts again. Seen live on a simulator.
- A missing or non-executable `gradlew` is reported before the build; Expo's launch line is found in the
  scrollback, not only on screen.

## UI fixes (2026-10-12, branch `ui-fixes`)
- Title bar: crumbs have a zero basis (the bar sized itself to their content), the project crumb and
  branch chip truncate, the search box shrinks to 160 px, and the buttons never shrink. Seen live with a
  long project, workspace and branch name.
- Slugs fold accented Latin letters (é→e, ß→ss); names with no Latin letters get `ws-` and a stable hash.
- A repeated workspace name in a project gets a number (`dev 2`), like its slug.
- Queued overview cards offer "Cancel start" instead of Resume.

## Small fixes (2026-10-12, branch `small-fixes`)
- A changed `.gitignore` (or `info/exclude`) refreshes the whole live diff.
- Non-GitHub remotes: `gh`'s "no GitHub host" answer is `NotGitHub`, Create PR is blocked with the
  reason instead of a lookup error note, and `create_pr` checks `gh` before pushing anything (a signed-out
  `gh` has its own error). Seen live with a local bare origin.
- Palette selection uses the accent tint; shortcut labels follow macOS order (⇧⌘O) everywhere and the
  palette shows ⌘D for Split pane right; a numbered name keeps its number visible in the sidebar; the
  project crumb keeps its width. Seen live.
- Not done: resumed sessions possibly booking their earlier cost again needs OpenCode's real behaviour
  checked first.

