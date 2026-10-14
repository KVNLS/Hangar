# M3 — live changes: watcher, unified diff, LiveDiff

## How to run

```sh
cargo test -p hangar-worktree -p hangar-files
cargo run --release -p hangar-files --example live_latency                       # temp repo
cargo run --release -p hangar-files --example live_latency -- <repo> [base] [n]  # any repo
```

`live_latency` starts a `LiveDiff`, writes and deletes `hangar-latency-probe-<i>.txt` at the
repo root `n` times (default 20), times each write/delete until the matching `DiffUpdate`
arrives, then times the individual git calls. Probe files are removed afterwards.

## Design

**`hangar-worktree` (git CLI only, blocking, no index writes)**
- `diff_files(wt, base)` / `diff_files_with(.., &DiffOptions)` — everything since
  `merge-base(base, HEAD)`: commits on HEAD, staged, unstaged and untracked-not-ignored files.
  `diff_file(wt, base, path) -> Option<FileDiff>` (None = unchanged). `diff_paths(wt, rev,
  paths, opts)` is the primitive: literal pathspecs (`--literal-pathspecs`), a directory covers
  everything below it, empty = all. `merge_base(wt, base)`.
- One `git diff --raw --patch -z -M <rev> --` call for tracked files. The `-z` raw section
  gives exact statuses (`A/M/D/R/C/T/U`), modes and unquoted path bytes; the textual patch
  gives hunks. They are joined **by path**, not by order, because git prints a type change as
  two patch sections (deleted + new) for one raw entry; those are merged into one
  `TypeChanged` file. Patch paths are also parsed (C-style unquoting, `rename/copy from/to`,
  `diff --git a/X b/X` split in half since both names are identical for non-renames), so the
  parser works without a raw section too.
- Forced flags against user config: `--no-color --no-ext-diff --no-textconv
  --submodule=short --src-prefix=a/ --dst-prefix=b/ --no-relative`; `-M` or `--no-renames`
  explicitly. `GIT_OPTIONAL_LOCKS=0` (from the existing runner) keeps `status`/`diff` from
  rewriting the index, which would otherwise feed back into the watcher.
- Untracked files: `git ls-files --others --exclude-standard -z` then one
  `git diff --no-index -- /dev/null <file>` per file (exit 1 accepted). No `add -N`, no
  temporary index. Nested repos (`dir/`) are skipped; a file that vanishes in between is
  dropped rather than failing the whole diff.
- Types: `FileDiff { path, old_path, status, old_mode, new_mode, binary, truncated,
  insertions, deletions, hunks }`, `FileStatus { Added, Modified, Deleted, Renamed, Copied,
  TypeChanged, Untracked, Unmerged }`, `Hunk { old_start, old_lines, new_start, new_lines,
  header, lines }`, `DiffLine { kind: Context/Added/Removed/NoNewlineAtEof, old_lineno,
  new_lineno, text }`.
- Parser rules: hunks are consumed by the line counts in their header (so a content line that
  looks like `diff --git` cannot desync it); `\ No newline at end of file` becomes its own
  `NoNewlineAtEof` line; one trailing `\r` is stripped from every line (CRLF files show clean
  text; a CRLF↔LF-only change therefore shows as identical `-`/`+` pairs); an empty line
  inside a hunk is context (`diff.suppressBlankEmpty`); anything unexpected is
  `Error::Parse`, never silently skipped. Non-UTF-8 content is decoded lossily.
- Large-file guard (`DiffOptions`): `max_lines_per_file` (default 20 000) stops collecting
  lines and sets `truncated`; insertions/deletions still count the whole patch.
  `max_untracked_bytes` (default 8 MiB) skips diffing big untracked files (reported
  `truncated`, no hunks).
- `check_ignored(wt, paths)` — `git check-ignore --stdin -z` (paths on stdin, written from a
  separate thread so a full stdout pipe cannot deadlock). Works for paths that no longer
  exist; tracked files are never reported ignored.
- `git_dirs(wt)` — absolute `--git-dir` / `--git-common-dir`.
- `status_paths(wt, paths)` + `Status::merge_partial(partial, paths)` — path-limited status
  patched into a previous full one.
- `Git::run_bytes_with(args, stdin, ok_codes)` added to the runner.

**`hangar-files` (headless, threads + channels)**
- `Watcher::start(wt, WatcherConfig) -> (Watcher, UnboundedReceiver<ChangeBatch>)`.
  `notify::recommended_watcher` = FSEvents on macOS (notify sets latency 0, file-level
  events, `NoDefer`). Watches the worktree recursively; for a linked worktree also its
  `git_dir` (`.git/worktrees/<name>`, non-recursive: HEAD, index) and the common dir
  (non-recursive for `packed-refs`, `refs/` recursive). All roots are canonicalised
  (`/var` → `/private/var`) because FSEvents reports real paths.
- Classification per event path: own git dir `HEAD|index|MERGE_HEAD|REBASE_HEAD|
  CHERRY_PICK_HEAD`, any `refs/**` or `packed-refs` → `git_meta_changed` (`*.lock`,
  `objects/`, `logs/` ignored; the main checkout's HEAD/index are ignored from a linked
  worktree). Inside the tree: any `.git` component or a `skip_dirs` name (`node_modules`,
  `Pods`, `build`, `DerivedData`, `.gradle`, `target`) → dropped without asking git.
  Create/modify events on directories are dropped (git tracks files; their own events
  follow); directory renames/removals are kept. `Access` events are dropped. FSEvents
  `MustScanSubDirs` / watcher errors → `rescan: true`.
- Debounce thread: a batch opens on the first relevant event and closes after `debounce`
  (default 150 ms) of quiet, or `max_delay` (default 1 s) after it opened while events keep
  coming. Irrelevant events neither open a batch nor extend it. On close, candidate paths
  go through one `check_ignored` call; empty batches are not sent.
  `ChangeBatch { paths: BTreeSet<PathBuf> (relative), git_meta_changed, rescan, at }`
  (`at` = first event of the batch).
- Channel: `tokio::sync::mpsc::UnboundedReceiver`. Chosen because its `recv()` future works
  on any executor without a tokio runtime, so the app can `rx.recv().await` from a GPUI
  background task (or `blocking_recv()` from a thread). Unbounded is fine: batches are
  debounced (≤ ~7/s). Dropping the `Watcher` drops the notify watcher, the debounce thread
  sees the disconnect and exits, closing the channel.
- `LiveDiff::start(wt, base, LiveDiffConfig) -> (LiveDiff, UnboundedReceiver<Result<DiffUpdate>>)`.
  Owns the `Watcher`; a worker thread does all git work. Emits an initial full update, then one
  per batch that changed something (files, removals or summary). Inputs queued while it works
  are coalesced into one job.
  - Full recompute (startup, `git_meta_changed`, `rescan`, `refresh()`, `set_base()`, batch
    larger than `max_incremental_paths` = 256, or after an error): new merge-base, full
    `status`, full diff.
  - Incremental: pathspecs = batch paths + both sides of any known rename touched; covered
    snapshot entries = those under a pathspec; `diff_paths` + `status_paths` on those only;
    covered entries not returned → `removed`.
  - Files listed as conflicted by status get `FileStatus::Unmerged` (`git diff <commit>`
    itself shows conflicts as plain modifications with markers).
  - `DiffUpdate { changed, removed, summary: DiffSummary { status, merge_base, files,
    insertions, deletions }, full, trigger }`; `snapshot()`, `file(path)`, `summary()` read
    the shared state. Errors are sent as `Err` and the worker keeps going.

## Measurements

Apple M5 Pro (18 cores), macOS 27, git 2.54.0, release build, default config (150 ms
debounce). Latency = `fs::write` returns → matching `DiffUpdate` received, n = 20 each.

| Repo | write → update p50 / p90 / max | delete → update p50 / max | initial full |
|---|---|---|---|
| temp repo, 50 files | 223 / 225 / 227 ms | 217 / 220 ms | 35 ms |
| 42 807 files (crate sources), clean | 246 / 251 / 252 ms | 229 / 241 ms | 450 ms |
| same, 200 dirty files | 240 / 249 / 251 ms | 228 / 235 ms | 455 ms |
| same, before incremental status | 419 / 476 / 504 ms | 411 / 494 ms | 462 ms |

Git call costs (avg of 10) — small / 43k-file repo: `check_ignored` 1 path 5.7 / 8 ms,
`status` 6.7 / 191 ms, `status_paths` 1 path – / 11 ms, `diff_paths` 1 path 11 / 17 ms, full
diff 12 / 200–215 ms. So an incremental update costs ≈ 25 ms (small) / 40 ms (large) of git
on top of the 150 ms debounce and FSEvents delivery; full recomputes on the large repo cost
≈ 400 ms (status + diff, both stat every tracked file).

CPU: idle cost is zero (FSEvents is push-based, the debounce thread blocks on a channel).
On the 43k-file repo a full `status` or full diff burns ≈ 0.6 s of system CPU (git's parallel
lstat); before incremental status every keystroke-level batch paid that, now only
commits/checkouts/rescans do. Memory: ~20 MB RSS for the whole example process on the large
repo.

Tests: `hangar-files` integration tests ran 10× in a row and 5× more with every core pinned
by `yes`, no failures. Negative checks ("ignored files produce no batch", "reading git state
does not set git_meta_changed") write a sentinel file afterwards and inspect every batch up to
it instead of relying on "nothing within N ms".

## Gaps / follow-ups

- **Syntax highlighting is not started**: needs tree-sitter grammar crates, which are not
  approved yet. Add a highlighting step over `DiffLine::text` (per file language, both old and
  new sides) once approved — tracked for PLAN.md.
- No rename detection between a deleted tracked file and a new untracked one (`mv` without
  `git mv`): shown as Deleted + Untracked. Would need `add -N` into a temporary index.
- Untracked files cost one `git diff --no-index` process each (~5 ms): an agent scaffolding
  hundreds of files makes the first full diff slow. Could be parallelised or diffed in-process.
- `skip_dirs` drops `build/` and `target/` even if a repo tracks files there (rare; the list
  is configurable).
- Git-ignored but frequently written paths that are not in `skip_dirs` (e.g. `dist/`, `.next/`)
  cost one `check-ignore` process per debounce window while they churn; a prefix cache of
  ignored directories would avoid it.
- Full recomputes on very large repos cost ~400 ms; `core.fsmonitor` / `core.untrackedCache`
  (user config) speed up `status`, not tested.
- `git diff <rev>` reports conflicts as modifications; `Unmerged` is set from `status`, so
  `diff_files` alone never returns it.
- A submodule's internal changes show as a `Subproject commit` hunk only.
- Not wired into the app yet (diff view, per-workspace LiveDiff lifecycle) — `crates/app` is
  owned by another stream.
