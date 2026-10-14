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
  `thiserror` and `uuid` (no I/O, runtime or UI crate). Library crates depend on `core` at most,
  never on each other (dev-dependencies aside, so adapter tests can use real neighbours). Only
  `app` and `examples` (the composition roots) use GPUI and `anyhow`.
- Enforced by `crates/app/tests/architecture.rs` (`cargo metadata`, no extra tool). A new crate must
  be given a layer there.
- Ports (`core::ports`): `Store` (with raw JSON settings, typed by `usecases::settings`),
  `VersionControl`, `Changes`, `ReviewOps`, `CodeHost`, `ToolProbe`, `DeviceCatalog` (one per
  platform: list, boot, open, probe/create/boot pool targets, shutdown, running set, clones, cost,
  bases; `type Device` is the adapter's open device) and `HostMonitor` (host resources, memory
  pressure with the `HANGAR_PRESSURE` override). `app::adapters` is the one place that picks
  their adapters (`store-sqlite`, `worktree::GitCli`, `worktree::Gh`,
  `devices::catalog::{Simulators, Emulators, SystemHost}`).

## Decisions

| Topic | Decision | Why |
|---|---|---|
| Ports | Plain synchronous traits, used as `Arc<dyn Port>` | All I/O already runs blocking on background threads. Sync traits stay dyn-compatible without `async-trait` (new dependency). vtable cost is nothing next to git, SQLite or a PTY. |
| Where ports live | In `core` (a `ports` module), next to the domain types they speak | Adapters then depend on `core` only. No separate "ports" crate. |
| When to add a port | Only at a real I/O boundary with two implementations (real adapter, test double) or a runtime choice | Traits that exist only for tests harden the code. Pure logic stays plain functions and state machines. |
| Pure logic first | Functional core, imperative shell; sans-IO state machines | The device allocator, `AgentState` and the protocol engines take events plus the current `Instant` and return decisions. The GPUI shell or a thread runs them. Tests need no test doubles. |
| Enum vs trait (OCP) | Closed sets we own (agent presets, platforms, MCP wiring) are data tables and enums; open sets (backends chosen at runtime) are traits | Exhaustive `match` is a feature for closed sets; one table row per agent is the extension point. |
| Liskov on `Device` | Keep one trait; add `capabilities()`; methods keep returning `Unsupported` as a safety net | Callers stop probing by calling. `devices::contract::violations` checks that an undeclared capability answers `Unsupported`; it only exercises absent capabilities, so it is safe on a real device (checking declared ones would boot, rotate or shut it down). Splitting into five role traits does not pay with four implementations whose consumers hold the whole device. |
| Errors | `thiserror` enums per library crate or service; adapter errors wrapped as opaque sources; `anyhow` + `.context()` only in `app` | Foreign types (`rusqlite::Error`, git output) stay out of public APIs. User-facing wording stays in the UI. |
| Test doubles | Hand-written in-memory doubles under `#[cfg(test)]` (`usecases::fakes`) for service tests; each adapter tested through its port against the real thing (temp git repos, SQLite in memory) | No `mockall` (new dependency, brittle interaction tests). Production code never fakes behaviour. |
| Composition root | `app::adapters` returns the adapter behind each port; services are small borrowing structs (`Workspaces::new(&store, &git)`) built where they are used | No DI framework and no GPUI global used as a service locator. Building a service costs two pointers. |
| Agents and PTYs | `agents` describes a start (`Launch`: program, args, cwd, env); `app::pty` runs it in a `TerminalSession` | Removes the `agents → terminal` edge without a launcher port; `usecases::sessions` is generic over the running agent. |
| Device frames | `Frame.pixels` is `FramePixels::Bgra` (captured bytes) or `FramePixels::Live` (a `core::device::LiveSurface` the backend keeps drawing into); the app downcasts the iOS one to `devices::ios::LiveFramebuffer` and paints its IOSurface with `paint_surface` | Zero-copy display while `core` stays platform-free. Readers that need bytes call `Frame::snapshot`; a live surface is "now", so diffs compare snapshots. |
| Settings | Typed keys: `hangar_core::Setting<T>`; every key in `app::setting_keys` | A value is read back as the type it was written with; one place lists what Hangar saves. |

## Roadmap

Each step is a series of small commits that leave behaviour unchanged, each passing fmt, clippy
`-D warnings` and tests. No step adds an external dependency. The owner approved the new
`store-sqlite` and `usecases` crates.

1. **Guardrails** — done
   - Layering test, this document, coverage check of `ops.rs` and `device_pool`. The one gap
     found (the rollback of a workspace whose row cannot be saved) is now tested in `usecases`.
2. **Quick wins** — done
   - One `Platform`, typed setting keys, the `AgentProfile` table, `EventBus` and `tokio` gone
     from `core`.
3. **Pure domain** — done
   - `Store` in `crates/store-sqlite`; device vocabulary in `core::device`; naming rules in
     `core`; changes and diffs in `core::changes`; review data and `ReviewError` in
     `core::review`. `runner`, `mcp` and `files` depend on `core` only.
   - The allocation policy (`usecases::device_pool::policy`) was already pure. The rest of
     `DevicePool`'s orchestration is step 7.
   - Host capacity (`HostResources`, `Pressure`, `DeviceCost`, `BootRefusal`, slot arithmetic) is
     in `core::device::capacity`; `devices::capacity` only reads the host and re-exports it.
4. **Ports** — done
   - `Store`, `VersionControl`, `Changes`, `ReviewOps`, `CodeHost`; `PortError` keeps adapter
     error types out of `core`; review refusals keep their shape (`ReviewError`). `GitCli` is
     tested through the ports against a temp repository.
   - `agents → terminal` removed without a port (see Decisions).
   - `DeviceCatalog` and `HostMonitor` (M10), speaking `core::device::catalog` (`Target`,
     `Running`, `Conflict`, `RunningSet`, `DeviceKey`, clone naming). `devices::catalog`
     implements them (`Simulators`, `Emulators`, `SystemHost`), tested through the ports on the
     paths that boot nothing. They replace the `PlatformDevices` extension trait and the free
     functions of `device_pool::backend`; only what needs `runner` (`stack_platforms`) or the
     app's `DeviceSession` stays in `app`.
   - `runner` stays as is: detection reads a few files and is tested on temp dirs.
5. **`usecases` crate** — done except `DeviceTools`
   - `workspaces` (lifecycle with rollback), `sessions` (start outcome, finish, stop; generic over
     the running agent), `settings` (`SettingsExt` over any `Store`), `review` (toolbar info,
     PR after publishing, merge). Each has `thiserror` errors with the old wording and tests on
     doubles. `app::ops` and `changes::review` are thin wrappers.
   - Not done: `DeviceTools`, the tool dispatch of `mcp_host`, which needs the device side of
     step 7 first.
6. **`Device::capabilities()`** — done
   - Declared per backend, checked by `devices::contract`, read by the device panel (buttons,
     rotate, shutdown) and by MCP input, which now gets a typed `Unsupported`.
7. **Split `HangarApp` and `DevicePool`** — started
   - Done: `HangarApp` groups its state into `Layout`, `AgentSetup`, `LiveState` and
     `WorkState` (55 fields to 14), field paths only.
   - Done: the agent queue and attention policies live in `usecases::agents`; `app` only applies
     their decisions.
   - Done (M9): `usecases::batch`, `usecases::orchestration` and `usecases::history` are pure
     policies tested on plain data. The approved `AgentRuntime` port was not added: dispatch to live
     agents stays in `app` like the device tools, and a port would only wrap GPUI calls.
   - Done (M10, device side): `usecases::device_pool::PoolMachine` is the pool as a sans-IO
     state machine: calls and I/O results with the current `Instant` in, `Command`s out
     (bring-up steps, shutdowns, waiter replies by token, lease grants, notices, settings to
     save, host and running scans). Results of an abandoned bring-up are ignored by attempt. It
     is tested on plain data (boot, join, guard, cap and eviction, full cap timeout, leases,
     archive, pressure, simulators gone, conflict, reassignment, launch scan, limit change). The
     `DevicePool` entity keeps its API and only feeds it and runs its commands through
     `DeviceCatalog` and `HostMonitor` on the background executor. To check on macOS with the dev
     hooks and real simulators/emulators before merging.
   - Next (M10 in PLAN.md): make those groups entities (controllers) with services passed in, so
     views do no I/O.
8. **Docs and cleanup** — done for this round: PLAN.md crate table, CLAUDE.md rules, large test
   modules moved to sibling files.
   - File layout (2026-10-08): `foo.rs` next to `foo/` everywhere (no `mod.rs`), large unit-test
     modules in `tests.rs` in every crate, and the device pool, device panel, changes view, dev
     hooks and settings split by concern. PLAN.md trimmed to the current plan; the milestone log
     moved to docs/HISTORY.md.

## Done when

- [x] The layering test passes with no edges between library crates.
- [x] No `hangar_worktree::` or SQLite `Store` paths in `app/src` outside `app::adapters` (tests
  aside).
- [ ] No `hangar_devices::` paths in `app/src` outside the adapters (today 26 files, tests aside:
  mostly the device pool, panel and views, plus `mcp_host`, `state` and nine `shell` modules;
  many only name types that `core::device` also exports).
- [x] `HangarApp` has at most 15 fields.
- [ ] No source file over about 800 lines (today, all in `app` unless noted: `shell.rs` 1,065,
  `shell/lifecycle.rs` 1,025, `chat_view.rs` 1,003, `chat_view/render.rs` 984, `shell/overview.rs`
  890, `ops/tests.rs` 870, `mcp/src/tools.rs` 862, `shell/run.rs` 842).
- [x] Every port is tested against its real adapter.

## References

- howtocodeit, "Master hexagonal architecture in Rust" — https://www.howtocodeit.com/guides/master-hexagonal-architecture-in-rust
- Effective Rust, item 12 (generics vs trait objects) — https://www.lurklurk.org/effective-rust/generics.html
- Firezone, sans-IO — https://www.firezone.dev/blog/sans-io
- Possible Rust, enum or trait object — https://www.possiblerust.com/guide/enum-or-trait-object
- Luca Palmieri, error handling — https://www.lpalmieri.com/posts/error-handling-rust/
- matklad, large Rust workspaces — https://matklad.github.io/2021/08/22/large-rust-workspaces.html
