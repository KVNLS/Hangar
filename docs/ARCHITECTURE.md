# Hangar — Architecture

Target: hexagonal (ports and adapters) with SOLID applied where it pays off in Rust. This file holds
the dependency rule, the target layout, the decisions behind it and the migration roadmap. PLAN.md
keeps the product roadmap.

## Dependency rule

```
                 ┌──────────── driving adapters ────────────┐
                 │  app (GPUI views)   mcp server/bridge  dev │
                 └───────────────┬───────────────────────────┘
                                 ▼
               usecases  (application services, headless)
                                 │
                                 ▼
               core  (domain model, pure rules, port traits)
                                 ▲ implemented by
     ┌──────────┬──────────┬─────┴─────┬──────────┬──────────┐
  store-sqlite  worktree   agents     devices    files     runner
  (rusqlite)    (git/gh)   (pty/acp)  (simctl…)  (notify)
```

- Dependencies point inwards. `core` depends on no workspace crate and only on `serde`,
  `thiserror` and `uuid` (no I/O, runtime or UI crate). Adapter crates depend on `core`, never on each other's internals or on
  `usecases`. Only `app` and `examples` (the composition roots) use GPUI and `anyhow`.
- Enforced by `crates/app/tests/architecture.rs` (`cargo metadata`, no extra tool). A new crate must
  be given a layer there. Two edges between adapter crates remain (`agents → terminal`,
  `files → worktree`); the `Changes` and `AgentLauncher` ports remove them.

## Decisions

| Topic | Decision | Why |
|---|---|---|
| Ports | Plain synchronous traits, used as `Arc<dyn Port>` | All I/O already runs blocking on background threads. Sync traits stay dyn-compatible without `async-trait` (new dependency). vtable cost is nothing next to git, SQLite or a PTY. |
| Where ports live | In `core` (a `ports` module), next to the domain types they speak | Adapters then depend on `core` only. No separate "ports" crate. |
| When to add a port | Only at a real I/O boundary with two implementations (real adapter, test double) or a runtime choice | Traits that exist only for tests harden the code. Pure logic stays plain functions and state machines. |
| Pure logic first | Functional core, imperative shell; sans-IO state machines | The device allocator, `AgentState` and the protocol engines take events plus the current `Instant` and return decisions. The GPUI shell or a thread runs them. Tests need no test doubles. |
| Enum vs trait (OCP) | Closed sets we own (agent presets, platforms, MCP wiring) are data tables and enums; open sets (backends chosen at runtime) are traits | Exhaustive `match` is a feature for closed sets; one table row per agent is the extension point. |
| Liskov on `Device` | Keep one trait; add `capabilities()`; methods keep returning `Unsupported` as a safety net | Callers stop probing by calling. A contract test checks that a reported capability never answers `Unsupported`. Splitting into five role traits does not pay with four implementations whose consumers hold the whole device. |
| Errors | `thiserror` enums per library crate or service; adapter errors wrapped as opaque sources; `anyhow` + `.context()` only in `app` | Foreign types (`rusqlite::Error`, git output) stay out of public APIs. User-facing wording stays in the UI. |
| Test doubles | Hand-written in-memory doubles under `#[cfg(test)]`, plus one contract suite per port (`macro_rules!`) run against both double and real adapter | No `mockall` (new dependency, brittle interaction tests). Contract tests stop doubles drifting. Production code never fakes behaviour. |
| Composition root | `app/src/main.rs` builds adapters and passes `Arc<Services>` to entity constructors | Constructor injection, no DI framework and no GPUI global used as a service locator. |
| Settings | Typed keys: `hangar_core::Setting<T>`; every key in `app::setting_keys` | A value is read back as the type it was written with; one place lists what Hangar saves. |

## Roadmap

Each step is a series of small commits that leave behaviour unchanged, each passing fmt, clippy
`-D warnings` and tests. No step adds an external dependency. The owner approved the new
`store-sqlite` and `usecases` crates.

1. **Guardrails** — done
   - Layering test (`crates/app/tests/architecture.rs`).
   - This document.
   - Coverage check of `ops.rs` (21 tests: create, delete, archive/restore, sessions, settings) and
     `device_pool` (15 tests). Gap found: the rollback of `create_workspace_from` when the row
     cannot be saved; now covered by a `usecases` test with a refusing store double.
2. **Quick wins** — done
   - One `Platform`: the app's copy is gone. `hangar_devices::Platform` owns ids, labels and
     readiness/boot/shutdown/rotate rules. The panel adds controls and hints (`PlatformUi`) and
     blocking I/O (`PlatformDevices`) as extension traits.
   - Typed setting keys (`Setting<T>`, `app::setting_keys`, keys pinned by a test).
   - `AgentProfile` table: kind, MCP wiring and Ollama support per preset. No other code
     matches on agent ids; a test keeps presets and profiles in step.
   - Unused `EventBus` deleted; `tokio` removed from `core`.
3. **Pure domain** — done
   - `Store` lives in `crates/store-sqlite`; `core` has no `rusqlite`, `tokio`, `tracing` or
     `serde_json` any more.
   - `core::device`: `Platform` (with its rules), `DeviceInfo`, `ScreenSpec`, `Frame`, `Input`,
     `Key`, `Orientation`, `LogLevel` and the state words; `devices` re-exports them, PNG export
     stays there (`SavePng`). `runner` and `mcp` depend on `core`, not on `devices`.
   - Naming rules (`BRANCH_PREFIX`, `branch_name`, `worktree_path`, `unique_slug`) are in `core`.
   - The allocation policy (`device_pool::policy`: lease queue with an injected `Instant`, slots,
     capacity) was already pure and now only uses `core` types. Moving the rest of `DevicePool`'s
     orchestration (81 GPUI context calls) behind a commands-out state machine needs a live check
     on macOS with simulators; it moves to step 7.
4. **Ports**, only at real I/O boundaries, each tested against its real adapter:
   - Done: `ports::Store` (implemented by `store-sqlite`) and `ports::VersionControl` (the git
     operations of the workspace lifecycle, `worktree::GitCli`, tested against a temp repository).
     `PortError` keeps adapter error types out of `core` and displays as the adapter's error.
   - Next: `Changes` (status, diff, merge base, ignore checks) so `files` stops depending on
     `worktree`; `ReviewOps` and `CodeHost` (`gh`) for the review screen. These need the status and
     patch types (`Status`, `FileDiff`, `Hunk`, …) to move into `core` first.
   - `AgentLauncher` (`spawn_pty`, `start_acp`).
   - `DeviceCatalog` per platform (list, boot, clone, delete) and `HostResources`; this replaces
     the `PlatformDevices` extension trait.
   - `runner` stays as is: detection reads a few files and is tested on temp dirs.
5. **`usecases` crate**
   - Done: `usecases::workspaces::Workspaces` (add project, create with rollback, archive,
     restore, delete, remove project) and `delete_summary`, with a `thiserror` `WorkspaceError`
     whose messages match the old ones. `app::ops` keeps its signatures as thin wrappers.
   - Next: `SessionService` (PTY and ACP start, recording the outcome) behind `AgentLauncher`, and
     `SettingsService`.
   - `DeviceTools`: the tool dispatch of `mcp_host`. The `mcp::ToolHost` impl that calls it lives
     in an adapter (app side), so `usecases` never depends on `mcp`.
6. **`Device::capabilities()`** with its contract test; the panel and MCP tools read flags
   instead of matching on `Unsupported`.
7. **Split `HangarApp`** (about 50 fields) into `WorkspacesController`, `SessionsController`,
   `RunController` and `LayoutState`, and `DevicePool`'s orchestration into a state machine.
   Services are passed in through constructors. Views do no I/O of their own. Check on macOS
   with the dev hooks and real simulators/emulators before merging.
8. **Docs and cleanup**: PLAN.md crate table, CLAUDE.md rules, delete the free-function
   re-exports nothing uses any more, re-run the end-to-end dev flow.

Steps 3–4 unlock the rest; 5 and 6 are independent of each other.

## Done when

- The layering test passes with no edges between adapter crates (today: `agents → terminal`,
  `files → worktree`).
- No `hangar_worktree::`, `hangar_devices::` or `Store` paths in `app/src` outside the composition
  root and the adapter impls.
- `HangarApp` has at most 15 fields and no source file is over about 800 lines.
- Every port has a contract suite run against its real adapter.

## References

- howtocodeit, "Master hexagonal architecture in Rust" — https://www.howtocodeit.com/guides/master-hexagonal-architecture-in-rust
- Effective Rust, item 12 (generics vs trait objects) — https://www.lurklurk.org/effective-rust/generics.html
- Firezone, sans-IO — https://www.firezone.dev/blog/sans-io
- Possible Rust, enum or trait object — https://www.possiblerust.com/guide/enum-or-trait-object
- Luca Palmieri, error handling — https://www.lpalmieri.com/posts/error-handling-rust/
- matklad, large Rust workspaces — https://matklad.github.io/2021/08/22/large-rust-workspaces.html
