# M7 — ACP structured agents: live notes (2026-10-05)

Agent: `@agentclientprotocol/claude-agent-acp` 0.85.1 (Node 26.5, Claude Code 2.1.289), installed globally
with npm, run by Hangar from PATH as preset `claude-acp`.

## Protocol as seen on the wire
- `initialize` → `protocolVersion: 1`, `loadSession: true`, `promptCapabilities {image, embeddedContext}`,
  `mcpCapabilities {http, sse}`, `authMethods: []` (it uses the existing Claude Code login), plus `_meta`
  extensions (`promptQueueing`, `steering`).
- `session/new` → `sessionId`, `modes` (`default` "Manual", `acceptEdits`, `plan`, `auto`,
  `bypassPermissions`) and `configOptions` (mode, model). Hangar shows the current mode name in the composer.
- Updates seen in one short turn: `agent_message_chunk`, `tool_call`, `tool_call_update`,
  `available_commands_update`, plus two kinds outside our subset: `usage_update` (context used/size) and
  `session_info_update`. Notification `_auth/status_update` (account label and email) is ignored and never
  stored.
- `tool_call` arrives first as a placeholder (`title: "Preparing file…"`, empty content) and is filled by
  `tool_call_update`s; diffs come as `{"type": "diff", "oldText": null, "newText": …}` for new files.
- Permission requests carry the tool call (title, kind, diff content, locations) and three options:
  `allow_once` "Yes", `allow_always` (e.g. "Yes, allow all edits during this session" or "…for rtk grep *
  commands"), `reject_once` "No". MCP tools ask too (`mcp__hangar__device_info`).
- `session/prompt` ends with `stopReason: end_turn` (plus `usage`); `session/cancel` mid-stream ends it with
  `cancelled` and the partial text stays.
- Tool output text is Markdown (fenced with ```` ```console ````); the tool card strips the fence lines.

## In Hangar
- Sidebar and status bar showed Approval (diamond, "1 approval") while a request was pending, Review (✓,
  `+1 −0`) after the edit; ⏎ in an empty composer approved, esc stopped a running turn.
- The hangar MCP bridge reached through `session/new` worked: `device_info` returned the workspace's own
  simulator.
- Quitting Hangar (SIGTERM) left no `claude-agent-acp` or `claude` process: closing stdin ends the agent.
- The adapter passes `mcpServers` to its `claude` child as `--mcp-config` JSON in argv, so the workspace's
  MCP session secret is visible in `ps` to the same user (Hangar itself keeps it out of argv). The socket is
  0700/0600, so only that user can use it anyway.

## Session modes (2026-10-07, `session/new` in an empty directory)
- claude-agent-acp: `default` "Manual" (Always ask before making changes), `acceptEdits` "Accept edits"
  (Automatically accept all file edits), `plan`, `auto` (Claude handles permission decisions),
  `bypassPermissions` "Bypass permissions" (Accepts all permissions). Also mirrored as the `mode` config option.
- codex-acp: `read-only` "Read Only" (current; reads, asks to edit or go online), `auto` "Default" (reads, edits
  and runs commands in the workspace without asking), `full-access` "Full Access" (no approval at all).
- opencode acp: no `modes`; `mode` config option with `build` (tools per OpenCode's configured permissions) and
  `plan` (no edit tools).
- Gemini CLI 0.62 (from its `buildAvailableModes`, not signed in here): `default` (Prompts for approval),
  `autoEdit`, `yolo` (Auto-approves all tools), `plan` when plan mode is enabled.
- No agent has a mode that approves only reads, so Settings › Permissions "Auto-approve reads" keeps the asking
  mode and Hangar answers `read`/`search` permission requests with allow-once (`acp::approval`).
