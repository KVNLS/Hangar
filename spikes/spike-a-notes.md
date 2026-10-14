# Spike A — PTY terminal in GPUI

## How to run

```sh
cargo run -p hangar-examples --example spike_terminal                # $SHELL as a login shell
cargo run -p hangar-examples --example spike_terminal -- claude      # any command + args
HANGAR_TERM_STATS=1 cargo run --release -p hangar-examples --example spike_terminal -- claude
HANGAR_TERM_STATS=1 HANGAR_SPIKE_AUTOTYPE=30 cargo run --release -p hangar-examples --example spike_terminal -- cat
cargo test -p hangar-terminal --release --test throughput -- --nocapture --test-threads=1
```

`HANGAR_TERM_STATS=1` prints once per second to stderr:
`frames/s wakes/s pty_bytes/s key->paint latency (n/min/median/max) window_visible`.
`HANGAR_SPIKE_AUTOTYPE=N` sends N synthetic `x` keystrokes, 150 ms apart, through GPUI's
key dispatch and the view's input handler (use it with `cat`, which only echoes).

## Design

**`hangar-terminal` (headless, no GPUI)**
- `PtySession` — portable-pty spawn (program/args/cwd/env/size; `TERM=xterm-256color`,
  `COLORTERM=truecolor`, `TERM_PROGRAM=Hangar`; no program = `$SHELL` login shell). Three
  threads: reader (64 KiB reads → bounded channel of 64 chunks, so a flood is throttled
  instead of buffered without limit), writer (queued writes, so the UI never blocks on a full
  PTY input buffer), waiter (child exit status). Resize, kill, `try_wait`/`wait`.
- `Terminal` — `alacritty_terminal::Term` + `vte::ansi::Processor`. `advance(bytes)`,
  `resize`, `scroll`, `snapshot()`; emulator replies (DSR, OSC 10/11/4 colour queries, text
  area size) are collected and returned by `take_replies()` — no I/O inside. Handles DEC 2026
  synchronized updates (buffered until end or the vte timeout). Title, OSC 52 copy, bell.
- `Snapshot` — plain data: `rows × cols` cells (`char` + combining chars, fg/bg as resolved
  RGB with inverse/hidden/dim applied, `CellFlags`), cursor (viewport row/col/shape or `None`),
  display offset, history size, modes.
- `Palette` — 16 ANSI + 6×6×6 cube + grey ramp + truecolor, runtime overrides (OSC 4) win.
- `encode_key` — `Key` + `Modifiers` + `KeyMode` → bytes (xterm legacy encoding: Ctrl
  combos, Alt = ESC prefix, DECCKM app-cursor, CSI `1;m` modified cursor keys, `~` keys,
  F1–F12, Shift-Tab). `encode_paste` handles bracketed paste.
- `TerminalSession` — PtySession + `Arc<Mutex<Terminal>>` + a pump thread that parses output
  (up to 256 KiB per lock acquisition), writes replies back, and calls an `on_update` callback.

**`hangar-app::terminal_view`**
- `TerminalView` (entity) + `TerminalElement` (custom `Element`). Prepaint computes cell size
  from the font (`advance('m')`, line height 1.25×), derives cols/rows from the bounds and
  resizes PTY + grid when they change, snapshots under a short lock, builds background quads
  (merged per run), one `ShapedLine` per style run with `force_width = cell width` so glyphs
  sit on the grid (wide glyphs get their own run), and the cursor (focused block = inverted
  cell; unfocused = hollow; beam/underline = quads).
- Input: `on_key_down` encodes named keys, Ctrl combos and (optionally) Option-as-Meta and
  stops propagation; plain text is left to the platform input handler
  (`EntityInputHandler::replace_text_in_range`) so dead keys / IME commits work. Marked
  (in-progress IME) text is drawn as an underlined overlay at the cursor. Cmd-V pastes
  (bracketed when enabled). `terminal_view::init` binds Tab/Shift-Tab to `NoAction` in the
  `Terminal` key context, because gpui-kit's Root binds them to focus navigation.
- Scroll wheel: pixel deltas accumulate into lines → scrollback; on the alternate screen
  with DECSET 1007 and no mouse reporting, wheel → Up/Down arrows. Typing snaps to bottom.
- Off-main-thread: the pump thread parses; it calls `Notify::notify_one()`; a GPUI task
  awaits the `Notify` (one stored permit = natural coalescing), calls `cx.notify()`, then
  waits 8 ms before listening again, so a flood requests at most ~125 redraws/s while an
  isolated echo is handled immediately.

## What worked
- Everything compiled against the pinned gpui-pre 0.3.7 / gpui-kit with no forks or patches;
  `canvas`-free custom `Element`, `shape_line(.., force_width)`, `ElementInputHandler`,
  `dispatch_keystroke` were all sufficient.
- alacritty_terminal 0.26 as a library is pleasant: parsing, scrollback, modes, sync updates
  and query replies come for free; we only add colour resolution and a snapshot.
- Throughput is bounded by the producer, not by us (numbers below).

## What didn't / surprises
- **macOS drops unread PTY output when the last slave fd closes.** A fast `/bin/echo hello`
  occasionally produced no output (2 of 25 full-suite runs). Fix: keep the parent's slave fd
  open until the child has exited and `FIONREAD` on the master reports 0 pending bytes
  (2 s cap), then close it so the reader sees EOF. 0 failures in 30 subsequent runs. This
  needed `libc` in `hangar-terminal` (already on the approved list / workspace deps).
- **BSD `seq` prints `1e+06`** for the last value; the throughput test accepts both forms.
- **Unthrottled wakes hurt.** Waking the view for every pump batch gave ~20–25k wakes/s
  during `seq` and cut GUI throughput to ~3 MB/s with ~40 frames/s; the 8 ms throttle fixed
  both (see numbers).
- **Occluded windows paint nothing** (GPUI stops the display link). An early autotype run
  showed ~155 ms "latency" = the keystroke interval, because frames only happened when the
  next synthetic event forced a draw; the window was covered. Stats now print
  `window_visible`, and the numbers below are from runs where it was `true`.
- `gpui_kit::*` glob-imports GPUI's `test` macro; test modules must import items explicitly
  (`use super::*` breaks `#[test]` with a recursion-limit error).

## Numbers (Apple M5 Pro, macOS 27; single runs, not averaged)

| What | Build | Result | Status |
|---|---|---|---|
| `seq 1 1000000` → PtySession → Terminal (120×40, 10k scrollback) | release | 7.89 MB in 0.89 s = **8.9 MB/s** | measured (test) |
| same, debug build | debug | 7.89 MB in 0.92 s = 8.6 MB/s | measured (test) |
| raw PTY drain of `seq 1 1000000`, no parsing | release | 8.7 MB/s, 18 B/read (430k reads) | measured (test) |
| `seq 1 1000000 \| cat` (block-buffered producer) → Terminal | release | 7.89 MB in 0.10–0.11 s = **71–81 MB/s** | measured (test) |
| parser only (same bytes from memory, 64 KiB chunks) | release / debug | **91–96 MB/s** / 23 MB/s | measured (test) |
| GUI, `seq 1 1000000` in a visible window, after throttle | release | ~6.8 MB/s PTY, **93 frames/s**, 93 wakes/s | measured (`HANGAR_TERM_STATS`) |
| GUI, same, before throttle | release | 2.3–3.5 MB/s, 37–42 frames/s, 20–25k wakes/s | measured |
| GUI, colourful 10 lines/s + `seq 200000`, before throttle | debug | 13–21 frames/s during the flood | measured (debug only, not re-measured after throttle) |
| key → paint latency, 30 synthetic keys into `cat` | release | min 0.6 ms, median 2–5 ms, max 8.6 ms | measured, see caveats |

Method / caveats:
- `seq` alone is line-buffered on a TTY (one `write` per line), so the plain-`seq` numbers
  measure the kernel PTY path, not us: raw drain ≈ full pipeline. The `| cat` and parse-only
  rows show the emulator's headroom.
- Latency starts in the view when the key is encoded/sent (after GPUI dispatch) and stops at
  the first `paint` whose snapshot (taken under the same lock as the byte counter) contains
  output that arrived after the key. It excludes OS input delivery and GPU presentation
  (vsync/compositor), so real end-to-end latency is higher by roughly one or two frames.
  Keys came from `window.dispatch_keystroke`, not a physical keyboard.
- `frames/s` counts `paint` calls, not presented frames.

## Verification status

Verified automatically:
- 32 unit tests + 4 integration tests in `hangar-terminal`: encoder, palette, SGR/256/
  truecolor/inverse/dim/strike in snapshots, wide chars, scrollback + cap, resize, DSR and
  OSC 11 replies (also end-to-end through a real PTY), modes, title, cursor shape, sync
  updates, PTY echo/env/size/resize/input/exit codes/spawn failure, throughput.
- `hangar-app` unit tests for key-name mapping and run splitting.
- The GUI example launches, paints, logs stats, renders output-driven frames, resizes to the
  window on first layout (PTY size follows), handles synthetic keystrokes through the input
  handler, and runs `claude --version` and an interactive `claude` (started, drew ~3 KB of UI,
  no panic) — I could not see the window.

Needs manual verification by the user:
- Colours, bold/italic/underline/strikethrough and cursor look right; glyph alignment on the
  grid (box drawing, emoji, CJK, Powerline symbols with font fallback).
- Real typing in zsh and in `claude`: Ctrl-C/D/R/L, arrows, Home/End, Tab completion,
  Shift-Tab (Claude mode switch), Esc, Option combos, dead keys / IME (e.g. Japanese).
- Live window resize (PTY + grid follow; Claude redraws correctly).
- Mouse wheel scrolling through 10k lines; wheel in `less`/`vim` (alternate scroll).
- Cmd-V paste, including multi-line paste into Claude (bracketed paste).

## Known gaps (not implemented, not faked)
- Mouse selection and copy (Cmd-C does nothing useful); mouse reporting modes (clicks/drag
  are never sent to programs); focus in/out reporting (DECSET 1004).
- Kitty keyboard protocol (disabled in the emulator config, so programs fall back to legacy).
- Shift-Enter sends `\r` like xterm; Claude's newline needs Option-Enter (`ESC \r`) — only with
  `option_as_meta`, or `\` + Enter. Option-as-Meta is an option, off by default.
- No bold-as-bright, no hyperlinks (OSC 8), no cursor blinking, bell ignored, no ligatures,
  no OSC 52 paste (copy is written to the clipboard).
- No damage tracking: each frame re-snapshots and re-shapes the visible grid (GPUI's line
  layout cache absorbs unchanged lines). Fine at 93 fps for 120×40; not profiled for 4K.
- Wheel deltas on the alternate screen ignore modifiers; no scrollbar.
- Window close quits the example; SIGTERM leaves the child to be HUP'd by the kernel.
- Latency numbers exclude OS input and presentation (see caveats).

## Recommendation
Go: keep this architecture (headless `hangar-terminal` with alacritty_terminal + a pump
thread, thin GPUI element) for M2. Before M2 multi-pane work: (1) the user validates the
manual checklist above, especially Claude typing/resize; (2) add selection + copy and mouse
reporting (Claude Code and vim use it); (3) decide Shift-Enter behaviour (xterm `\r` vs
`ESC \r` vs kitty protocol). The macOS slave-close fix should stay in `PtySession`.
