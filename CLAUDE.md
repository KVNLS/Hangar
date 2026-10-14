# Hangar — working notes for Claude

See PLAN.md for architecture and roadmap, SPIKES.md for spike results.

## Commands
- Build: `cargo build --workspace`
- Format: `cargo fmt --all` (CI: `cargo fmt --all -- --check`)
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Test: `cargo test --workspace`
- Spike A: `cargo run -p hangar-examples --example spike_terminal -- [command]` (examples live in `examples/examples/`) (default `$SHELL`, try `claude`)
- Spike B: `cargo run -p hangar-examples --example spike_sim -- [--backend private|simctl] [--udid <UDID>]`
- App: `cargo run -p hangar-app`

## Toolchain
- Rust 1.99.0 pinned in `rust-toolchain.toml`, edition 2024, one Cargo workspace.
- GPUI via the `gpui-kit` umbrella crate (gpui-component repo, re-exports gpui + gpui-component + platform) pinned to git rev `0c830f4d257e69fdd17200650533ab4ca9a40cc0` (v0.7.0), which uses
  `gpui-pre =0.3.7`. Bump both together and re-check examples. Read the pinned GPUI / gpui-component source
  (`~/.cargo/registry`, `~/.cargo/git`) before writing UI code; do not rely on memory for its API.

## Rules
- Only `app` (and `examples`) may depend on GPUI. Library crates are headless.
- Errors: `thiserror` in libraries, `anyhow` in binaries/examples. No `unwrap`/`expect` outside tests.
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
  serde, serde_json, uuid, tracing, tracing-subscriber, tempfile (dev), objc2 family, block2, libc, image.
- Do not copy code from Zed's GPL crates, idb, SimView or baguette; learn from them and credit in SPIKES.md.
- Code, comments, logs and UI copy in English. Minimal comments: only the non-obvious why.

## Workflow
- Plan each step, keep it small, commit after fmt + clippy + tests pass.
