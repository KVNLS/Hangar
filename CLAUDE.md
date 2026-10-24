# Hangar — working notes for Claude

See PLAN.md for the roadmap and known gaps, docs/ARCHITECTURE.md for architecture, docs/HISTORY.md for what
shipped, docs/notes/ for spike results and per-milestone engineering notes.

## Commands
- Build: `cargo build --workspace`
- Format: `cargo fmt --all` (CI: `cargo fmt --all -- --check`)
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Test: `cargo test --workspace`
- Spike A: `cargo run -p hangar-examples --example spike_terminal -- [command]` (examples live in `examples/examples/`) (default `$SHELL`, try `claude`)
- Spike B: `cargo run -p hangar-examples --example spike_sim -- [--backend private|simctl] [--udid <UDID>]`
- Release: `scripts/release.sh all` builds `target/release/bundle/Hangar.app` and a signed DMG (no tag);
  after a smoke test, `scripts/release.sh publish` (on a clean main) tags `v<version>` and creates the GitHub
  release. Version comes from `[workspace.package]`; `HANGAR_SIGN_IDENTITY` overrides the codesign identity.
- App: `cargo run -p hangar-app`. Dev env: `HANGAR_DB`, `HANGAR_WORKTREES` override storage paths;
  `HANGAR_DEV_OPEN=<repo>` (+ `HANGAR_DEV_AGENT`, `HANGAR_DEV_INPUT`) drives the add-project → workspace →
  agent flow without clicks (see `crates/app/src/main.rs`). Never point these at real data in tests.
- Structured agents (ACP): `HANGAR_DEV_AGENT=claude-acp` runs Claude Code through `claude-agent-acp` (must be on
  PATH: `npm install -g @agentclientprotocol/claude-agent-acp`); `HANGAR_DEV_INPUT` becomes the first prompt.
  `HANGAR_DEV_AGENT=opencode-acp` runs `opencode acp` (`curl -fsSL https://opencode.ai/install | bash`),
  `codex-acp` runs `codex-acp` (npm `@zed-industries/codex-acp`), `gemini-acp` runs `gemini --acp --skip-trust`.
  Protocol tests: `cargo test -p hangar-agents acp`.
- Ollama (OpenCode only): Settings › Agents › Ollama, or `HANGAR_DEV_OLLAMA=127.0.0.1:11434` with
  `HANGAR_DEV_AGENT=opencode-acp` (needs `ollama serve` and a pulled model). Tests: `cargo test -p hangar-agents ollama`.
- MCP bridge: `Hangar mcp-bridge` (agents start it as their `hangar` MCP server; needs `HANGAR_MCP_SOCKET` and
  `HANGAR_MCP_SESSION`, set by the app on each agent). End-to-end test: `cargo test -p hangar-app --test mcp_bridge`.

## Toolchain
- Rust 1.99.0 pinned in `rust-toolchain.toml`, edition 2024, one Cargo workspace.
- GPUI via the `gpui-kit` umbrella crate (gpui-component repo, re-exports gpui + gpui-component + platform) pinned to git rev `0c830f4d257e69fdd17200650533ab4ca9a40cc0` (v0.7.0), which uses
  `gpui-pre =0.3.7`. Bump both together and re-check examples. Read the pinned GPUI / gpui-component source
  (`~/.cargo/registry`, `~/.cargo/git`) before writing UI code; do not rely on memory for its API.

## Rules
- Only `app` (and `examples`) may depend on GPUI. Library crates are headless.
- Architecture: docs/ARCHITECTURE.md (dependency rule, ports, roadmap). `crates/app/tests/architecture.rs`
  enforces it; a new crate gets a layer there. Library crates depend on `core` only. Ports live in
  `core::ports`; `app::adapters` picks their adapters; use cases go in `usecases` (tested with the doubles
  in `usecases::fakes`). Settings keys go in `app::setting_keys`; a new agent preset gets a row in
  `agents::PROFILES`. A new `Device` backend declares `capabilities()` (see `devices::contract`).
- Errors: `thiserror` in libraries, `anyhow` in binaries/examples and inside `app` (the binary's crate). No `unwrap`/`expect` outside tests.
- Async on tokio. Never block the GPUI main thread: PTY reads, git, simctl and frame capture run on background
  threads/tasks and push results through channels.
- Git access goes through the `git` CLI in `worktree` (porcelain output only).
- Private Apple frameworks only inside `devices` (or its helper), behind the `Device` trait, macOS-only cfg,
  with the `simctl` fallback always working.
- Unit-test `core`, `worktree`, `agents` against temporary Git repos (`tempfile`).
- Never stub or fake behaviour silently. Unimplemented paths return an explicit `Unsupported` error and are
  listed in PLAN.md.
- New dependencies or architecture changes need the owner's approval first. Approved so far: gpui(-pre),
  gpui-component, portable-pty, alacritty_terminal, notify, tree-sitter, tokio, thiserror, anyhow, rusqlite,
  serde, serde_json, uuid, tracing, tracing-subscriber, tempfile (dev), objc2 family, block2, libc, image,
  notify-rust.
- Do not copy code from Zed's GPL crates, idb, SimView or baguette; learn from them and credit in CREDITS.md.
- Code, comments, logs and UI copy in English. Minimal comments: only the non-obvious why.
- File layout: modules as `foo.rs` next to `foo/` (no `mod.rs`). A source file past about 800 lines
  is split by concern into child modules, each with its own `impl Type` block; helpers shared between
  them are `pub(super)`.
- Tests live with the code and the layer they cover:
  - A test goes in the module, at the layer, whose code makes it pass (use case behaviour in
    `usecases` with `usecases::fakes`, Git/SQLite behaviour in the adapter, `app` only what it
    decides). A wrapper gets no tests of behaviour it only forwards.
  - Unit tests start inline in `#[cfg(test)] mod tests`; past about 150 lines (or 100 and longer than
    the code) they move to one `foo/tests.rs` via `#[cfg(test)] mod tests;`. Never a `tests/`
    directory under `src/` (`architecture.rs` checks): when `foo/tests.rs` outgrows ~800 lines or
    several concerns, split `foo.rs` and the tests follow.
  - Fixtures shared by a module's children: `foo/fixtures.rs`, `#[cfg(test)] mod fixtures;`, items
    `pub(super)`.
  - `crates/<x>/tests/<module>.rs`: public API, live environment, or use cases wired to real adapters
    (`app/tests/workspaces.rs`), named after what they cover.

## Workflow
- Plan each step, keep it small, commit after fmt + clippy + tests pass.
