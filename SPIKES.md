# Spikes (M1)

Both spikes pass. Details, method and full tables: [spikes/spike-a-notes.md](spikes/spike-a-notes.md),
[spikes/spike-b-notes.md](spikes/spike-b-notes.md). Measured on Apple M5 Pro, macOS 27.0.1, Xcode 27.0.

## Spike A — PTY terminal in GPUI

```sh
cargo run -p hangar-examples --example spike_terminal                 # $SHELL
cargo run -p hangar-examples --example spike_terminal -- claude       # any command
HANGAR_TERM_STATS=1 cargo run --release -p hangar-examples --example spike_terminal -- claude
cargo test -p hangar-terminal --release --test throughput -- --nocapture --test-threads=1
```

**Design**: `hangar-terminal` is headless (portable-pty session with reader/writer/waiter threads,
alacritty_terminal wrapper returning plain `Snapshot`s, palette, xterm key encoder). The GPUI `TerminalView`
lives in `hangar-app::terminal_view`; a pump thread parses PTY output and wakes the view through a
`tokio::sync::Notify`, coalesced to one redraw per 8 ms.

| measurement | result |
|---|---|
| `seq 1 1000000` through PTY + emulator (release, headless) | 8.9 MB/s — bound by the kernel PTY, not us |
| emulator alone, bytes from memory (release) | 91–96 MB/s |
| GUI flood, visible window (release) | ~6.8 MB/s at ~93 frames/s |
| key → paint, in-app (synthetic keys into `cat`) | median 2–5 ms, max 8.6 ms (excludes OS input + present) |

**Worked**: colours (16/256/truecolor), attributes, cursor shapes, scrollback (10k lines), resize, DEC 2026
synchronized output, bracketed paste, emulator replies, running `claude` interactively (smoke-tested).
**Did not / not yet**: mouse selection & copy, mouse reporting, focus reporting, kitty keyboard protocol,
hyperlinks, damage tracking (full visible-grid snapshot per frame).
**Needs manual check**: visual correctness of colours/glyph alignment, real typing incl. IME, live resize,
wheel scrollback, Cmd-V.

**Recommendation**: keep this architecture for M2. Add selection/copy and mouse reporting early in M2/M3.

## Spike B — iOS simulator frames + tap

```sh
xcrun simctl boot <UDID>        # any iOS simulator; Device Hub/Simulator.app need not be open
cargo run -p hangar-examples --example spike_sim -- [--backend private|simctl] [--udid <UDID>]
cargo run -p hangar-examples --example spike_sim -- --bench 10        # headless fps + tap latency
cargo run -p hangar-examples --example sim_probe                      # dump private API on a new Xcode
```

**Design**: `hangar-devices` is headless. `Device` trait: `info`, `backend`, `frames` (a `watch` channel with
the latest BGRA `Frame`), `send_input` (`Tap`, `TouchDown/Move/Up`, `Home`). `boot`, `install`, `launch`,
`logs` return `Error::Unsupported` for now. The private backend uses objc2 only (CoreSimulator +
SimulatorKit via dlopen, IOSurface framebuffer callbacks, Indigo HID) on two dedicated threads.
**No Swift/ObjC helper was needed.**

| | private (objc2) | simctl fallback |
|---|---|---|
| frames while swiping | 53–62 fps (only on change, ~0 when idle) | 2.0 fps (polled screenshots) |
| capture → consumer p50 / p95 | 1.5 / 2.3 ms | ~505 / 525 ms |
| GUI painted / capture → paint | 56–60 fps / ~8 ms avg | not measured |
| tap → first changed frame | 12.7–26.8 ms (6 runs) | **no tap** |
| tap verified | yes: Settings icon opens Settings (3/3) | — |

**Xcode 27 findings**: SimulatorKit moved to `Xcode.app/Contents/SharedFrameworks`; IO ports are ROCKit
proxies (raw `objc_msgSend` needed) and ROCKit requires typed block signatures; the Indigo mouse builder takes
6 arguments (the "Xcode 26 breakage" was callers setting the edge flag by mistake); there is no Simulator.app
(Device Hub instead); `simctl io … screenshot -` no longer writes to stdout.

**simctl fallback tap does not work here**: no Simulator.app window to target, no Accessibility permission, and
it would move the user's real pointer. The code returns explicit `Unsupported`/`Permission` errors.

**Recommendation**: private backend as primary; simctl as a **view-only** fallback. Re-run `sim_probe` on each
new Xcode. Frames currently cost ~12.6 MB and two copies each; zero-copy into GPUI (`paint_surface`) needs a
CoreVideo binding — a new dependency, deferred.

## Credits
Learned from, no code copied: baguette (Apache-2.0), SimView (Apache-2.0), idb/FBSimulatorControl (MIT),
alacritty (Apache-2.0). Zed's GPL terminal crates were not used.
