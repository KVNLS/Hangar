# M4 — Android emulator backend (hangar-devices)

Machine: Apple M5 Pro, macOS 27.0.1, Rust 1.99.0. Android SDK at `~/Library/Android/sdk`: emulator 36.5.11
(build 15261927), adb 1.0.41, AVD `Pixel_10_Pro` (Android 17 / API 37, `google_apis_playstore_ps16k`,
arm64, 1280x2856 px @ 480 dpi = 426.7x952 dp).

## How to run

```sh
# Lists AVDs + running emulators; boots the AVD headless if it is not running; opens a GPUI window.
cargo run -p hangar-examples --example spike_android -- --boot Pixel_10_Pro
cargo run -p hangar-examples --example spike_android -- [--backend grpc|adb] [--serial emulator-5554 | --avd ID]

# headless
cargo run --release -p hangar-examples --example spike_android -- --bench 10 [--backend adb] [--out DIR]
cargo run -p hangar-examples --example spike_android -- --tap 263,664        # dp; saves input_before/after.png
cargo run -p hangar-examples --example spike_android -- --back | --home | --text "wifi"
cargo run -p hangar-examples --example spike_android -- --boot Pixel_10_Pro [--cold] [--window host|none|hidden] \
    [--emu-arg ARG ...]
cargo run -p hangar-examples --example spike_android -- --relaunch          # Device::shutdown + Device::boot
cargo run -p hangar-examples --example spike_android -- --text "" --shutdown # just shut it down

cargo test -p hangar-devices                                                # unit tests (no SDK needed)
cargo test -p hangar-devices --test android_emulator -- --ignored           # needs a booted emulator
```

The SDK is found via `ANDROID_HOME`, `ANDROID_SDK_ROOT`, then `~/Library/Android/sdk` (macOS) /
`~/Android/Sdk` (Linux). Nothing needs to be on `PATH`. No permissions are needed.

## Design

`hangar-devices::android` is headless and cross-platform (no `cfg(target_os)`; only the pid liveness
check uses `libc` on unix).

- `sdk`: `AndroidSdk::locate()` / `at(root)` → paths of `adb`, `emulator`, optional `avdmanager`
  (absent here: no cmdline-tools installed; only needed to create AVDs, which we do not do).
- `avd`: `emulator -list-avds` + `<avd home>/<id>.ini` → `config.ini` (`hw.lcd.width/height/density`,
  `target`, display name). Screen size is known before the AVD runs.
- `discovery`: each emulator writes `pid_<pid>.ini` (mode 0600) with `port.serial`, `grpc.port`,
  `grpc.token`, `avd.id`, `avd.dir`, … On macOS with emulator 36.5 the directory is
  `~/Library/Caches/TemporaryItems/avd/running/` (also probed: `$TMPDIR/avd/running`); on Linux
  `$XDG_RUNTIME_DIR/avd/running`, else `/tmp/android-$USER/avd/running`. Files of dead pids are skipped
  (crashed emulators leave them behind). The pid is the qemu process, not the `emulator` launcher.
- `adb`: `adb devices -l`, `wm size`/`wm density` (overrides win), `getprop`, `emu kill`, `install -r`,
  `monkey -p <pkg> -c LAUNCHER 1` (launch without knowing the activity).
- `emulator`: `list_running` (discovery files joined with adb state; screen from `wm`, else `config.ini`),
  `boot` and `kill_via_adb`.
- `grpc`: client generated at build time from the vendored `proto/emulator_controller.proto` with
  `protox` (pure Rust, no `protoc`) + `tonic-prost-build` (client only, no transport helpers, comments
  stripped so upstream snippets do not become doctests). `tonic` features: `channel`, `codegen` (no
  server, no TLS: the emulator only listens on 127.0.0.1). Bearer-token interceptor; max message 64 MB
  (a frame is 14.6 MB, tonic's default limit is 4 MB).
- `GrpcEmulator` (primary): owns a 2-thread tokio runtime (`android-grpc`), so callers need none. Frames:
  `streamScreenshot(RGBA8888, device size)`, converted to BGRA on the runtime and published on a `watch`
  channel. The stream only runs while someone holds a receiver from `frames()` (and restarts when one
  appears), so an unwatched device costs nothing. `captured_at` is back-dated to the emulator's frame
  timestamp (`timestampUs`, same host clock), so latencies are measured from frame production.
  Input: `sendTouch` (one finger, pressure 0x400, contact 0x500 — the values the emulator itself uses
  for mouse-driven touches, checked with `getevent`), `sendKey` with W3C key names `GoHome` / `GoBack`
  (the evdev `KEY_HOMEPAGE` code did nothing), `sendKey{text}` for text. `send_input` blocks only for the
  RPC (Tap = down, 30 ms, up) and works from any thread, including inside another tokio runtime (it
  never calls `block_on`). Drop uses `shutdown_background`.
- `AdbDevice` (fallback, also works for physical devices): frames by polling `adb exec-out screencap -p`
  (PNG decoded with `image`; raw `screencap` is supported but slower, see below), input via
  `input tap`, `input motionevent DOWN/MOVE/UP`, `input keyevent KEYCODE_HOME/BACK`, `input text`
  (shell-escaped). Polls only while watched.
- RGBA→BGRA works on two pixels per `u64` step: 13 ms vs 100 ms per 1280x2856 frame in a debug build
  (the crate is not optimised in dev builds; the per-byte loop capped debug builds at 9 fps).

### Units

`Input` coordinates are **dp** (density-independent pixels), Android's equivalent of iOS points.
`ScreenSpec { width_px, height_px, scale = dpi / 160 }`, so `width_pt()`/`height_pt()` are in dp and
`px = dp * scale` — the same contract as iOS. The panel can therefore map clicks with
`geometry::view_to_device(…, screen.width_pt(), screen.height_pt())` for both platforms. The backends
convert with `geometry::device_to_pixels` (rounded, clamped). Frames are device pixels.

### Boot / shutdown

`emulator -avd <id> -grpc <port> -grpc-use-token -no-boot-anim -no-window -gpu host -no-snapshot-save`,
in its own process group (Ctrl-C in Hangar's terminal does not kill it), stdout/stderr to
`$TMPDIR/hangar-emulator-<avd>.log`, launcher reaped on a thread. The gRPC port is the first free one
from 8554; the discovery file is matched on that port. Then we poll `getprop sys.boot_completed` via
adb (250 ms) until `timeout` (default 240 s). The emulator outlives Hangar on purpose (like Android
Studio); shutdown is explicit.

Why these defaults (what a developer wants: fast, invisible, does not disturb Android Studio):

| mode | flags | boot | CPU scrolling | notes |
|---|---|---|---|---|
| **default** `NoWindowHostGpu` | `-no-window -gpu host` | 7.1–7.4 s (always cold) | ~85% | headless binary, no UI process, no focus steal |
| `NoWindow` | `-no-window` | 10.2 s cold | ~1000–1100% | software GLES (SwiftShader/lavapipe) |
| `HiddenWindow` | `-qt-hide-window` | 5.7 s (Studio's snapshot) | ~85–95% | registers as a UI app and becomes frontmost at launch |

- Android Studio's quick-boot snapshot only loads with the Qt-window renderer config; headless modes
  log "different renderer configured" and cold boot. Cold boot is fast enough here (7 s), so the
  default avoids the focus steal instead.
- `-no-snapshot-save` (default): shutdown takes ~1.5 s and never overwrites the snapshot Android Studio
  uses. `BootOptions { save_snapshot: true, cold_boot, window, grpc_port, timeout, log_file,
  extra_args }` change this.
- Shutdown: gRPC `setVmState(SHUTDOWN)`, wait for the qemu pid to exit (30 s), fall back to
  `adb emu kill`. Measured 1.3–2.0 s. `Device::boot` on a `GrpcEmulator` whose emulator was shut down
  relaunches the same AVD and reconnects (frames resume; measured 7.4 s boot, first new frame 50 ms after).

## API surface (added)

Trait/enum additions in `hangar_devices` (all non-breaking except the new enum variant for exhaustive
matches outside the crate):
- `Input::Back` (iOS backends return `Error::Unsupported("Back button on iOS")`).
- `enum Platform { Ios, Android }`; `Device::platform(&self) -> Platform` (default `Ios`).
- `Device::send_text(&self, text: &str) -> Result<()>` (default `Unsupported("text input")`).
- `Device::shutdown(&self) -> Result<()>` (default `Unsupported("shutdown")`).
- `geometry::device_to_pixels(x, y, &ScreenSpec) -> (i32, i32)`.

`hangar_devices::android` (blocking; call from background threads):
- `list() -> Result<Vec<DeviceInfo>>`: running emulators (`udid` = adb serial `emulator-5554`,
  `device_type` = AVD id), attached physical devices (`udid` = serial), and AVDs that are not running
  (`udid` = AVD id). `state` ∈ `state::{BOOTED="Booted", BOOTING="Booting", OFFLINE="Offline",
  SHUTDOWN="Shutdown"}`. `runtime` e.g. `Android 17 (API 37)` (running) or `Android API 37.0` (from
  `config.ini`). `screen` filled in dp units whenever known (also for shut-down AVDs).
- `open(&DeviceInfo) -> Result<Box<dyn Device>>`: gRPC for emulators, adb fallback if gRPC fails; adb
  for physical devices; `Error::NotFound` for an AVD that is not running.
- `open_grpc(&DeviceInfo)`, `open_adb(&DeviceInfo)`: one backend only.
- `boot(avd_id: &str) -> Result<DeviceInfo>` with `BootOptions::default()`; `boot_with(&sdk, avd_id,
  &BootOptions) -> Result<RunningEmulator>`.
- Lower level: `AndroidSdk`, `list_avds`, `list_running`, `find_running`, `open_with(sdk, running,
  Backend::{Grpc, Adb})`, `GrpcEmulator::{open, open_with(.., FrameSize), status, set_boot_options}`,
  `AdbDevice::open(adb, info, Capture::{Png, Raw})`.
- Device methods implemented for Android: `frames`, `send_input` (Tap/TouchDown/Move/Up/Home/Back),
  `send_text`, `boot` (gRPC: relaunch if shut down, no-op if running), `shutdown` (gRPC; adb: emulators
  only), `install` (`adb install -r`, **untested**: no APK at hand), `launch` (package name, tested with
  `com.android.settings`). `logs` stays `Unsupported` (the trait returns `()`, so there is nothing to
  stream into; logcat needs an API decision).

## Measurements

Method: `spike_android --bench 10` (release build unless noted), same as Spike B:
- fps: consumer polls the `watch` channel; "received" = distinct frames, "content-changed" = sampled
  pixels differ from the previous frame. Scrolling = Settings list swiped up and down continuously with
  gRPC `sendTouch` (used as animator for both backends; the animator's own frame stream is idle because
  nobody watches it).
- capture→consumer: gRPC = emulator frame timestamp → consumer read; adb = start of the `screencap`
  call → consumer read (the capture happens somewhere inside the round trip).
- tap→first changed frame: home screen stable for 700 ms, `Tap` on the Photos icon (263, 664 dp) at t0,
  first frame captured after t0 that differs > 0.2 % from the baseline. Verified: Photos opens in every
  run (`tap{N}_after.png` inspected: Google Photos screen / sign-in sheet).
- CPU: `ps -o time` deltas over the phase; emulator = qemu + children (netsimd); 100 % = one core.

Default mode (`-no-window -gpu host`), emulator booted 2026-10-04:

| | gRPC (release) | gRPC (debug) | adb `screencap -p` (release) |
|---|---|---|---|
| idle, static Settings screen | 2.5 fps received, 0.2 fps changed | — | 8.6 fps (each poll = frame) |
| idle, home screen (animated wallpaper) | 33 fps received, 28 changed | — | 1.9 fps (~530 ms per capture) |
| scrolling, received / changed | **53.9 / 51.2 fps** | 52.2 / 49.7 fps | 8.1 / 7.9 fps |
| capture→consumer p50 / p95 | **9.5 / 10.4 ms** | 16.2 / 17.2 ms | 125 / 133 ms |
| tap call (incl. 30 ms hold) | 32–34 ms | 32–34 ms | 18–20 ms (`input tap`) |
| tap → first changed frame captured | **105–159 ms** (3 runs) | 133–164 ms | 370–400 ms |
| emulator CPU idle (static) / scrolling | 25 % / 85 % | 69 % scrolling | 106 % / 138 % |
| this process CPU, scrolling | 28 % | 72 % | 7 % |
| boot (cold, `sys.boot_completed`) | 7.1 s, 7.4 s (relaunch) | | |
| shutdown (`setVmState`) | 1.3–2.0 s | | |

- The emulator stream is change-driven: ~2.5 fps of mostly identical frames on a static screen, full
  rate only when the guest renders. Without a consumer the idle emulator uses ~8 % CPU (`top`, 10 s).
- Tap → first change is mostly Android: a launcher icon reacts on touch-up and the app-launch animation
  starts ~75–130 ms after it. gRPC input itself is sub-millisecond (`sendKey` returns in 0.3–0.8 ms).
- Software renderer (`-no-window` alone): 49.8 fps while scrolling but ~1070 % emulator CPU; in a debug
  build with the old per-byte conversion it fell to 9.4 fps (fixed, see Design).
- `-qt-hide-window`: 56 fps, 9.1 ms p50, emulator 83–95 % CPU scrolling, quick boot 5.7 s.
- adb raw `screencap` (14.6 MB/frame): 1.2–1.5 fps while scrolling vs 6.3–8.1 fps for PNG, hence PNG by
  default. `screencap` is slow on the animated home screen (~0.5 s per capture) and faster on static
  screens.
- GUI paint numbers (`HANGAR_SIM_STATS=1`) were not obtained: the display was asleep during this run
  (`screencapture` fails), so GPUI did not paint. The window path reuses `spike_sim/view.rs` unchanged.

## What works / gaps

Works: SDK discovery, AVD listing with screen spec, running-emulator discovery (gRPC port + token),
headless boot/shutdown/relaunch, gRPC frames at ~54 fps / ~10 ms, gRPC tap/drag/Home/Back/text,
adb fallback frames and input, launch by package, physical devices through adb (listing/open code
path; not tested, no phone attached).

Gaps:
- `logs` Unsupported (needs a streaming API on the trait); `install` implemented but untested.
- Frames are 14.6 MB each and copied: HTTP/2 → `Vec` → converted in place → `Arc<[u8]>` (one copy) →
  the app's `RenderImage`. The emulator's `MMAP` transport (shared file, no gRPC payload) or a requested
  smaller `FrameSize` would cut this; not done.
- Multi-touch (pinch) not exposed (`sendTouch` supports 10 slots; `Input` has one finger).
- Rotation/foldables: frames follow the device orientation but input mapping assumes portrait.
- `DeviceInfo` has no platform field (adding one would break struct literals in other crates);
  `Device::platform()` and the `Android …` runtime string carry it instead.
- After `Device::boot` relaunches on another console port, `info().udid` keeps the old serial
  (`install`/`launch` use the live serial).
- The `HiddenWindow` mode steals focus at launch; kept as an option only.
- Boot needs adb to report `sys.boot_completed`; an AVD with a broken adb would time out (240 s).
- Physical devices: listing and adb backend written but not exercised.

## Licences

- `crates/devices/proto/emulator_controller.proto`: vendored unmodified from the SDK's emulator 36.5.11
  (`emulator/lib/`), Apache-2.0, © The Android Open Source Project (header kept; see `proto/README.md`).
- New crates (owner-approved): `tonic`, `tonic-prost` (MIT), `prost` (Apache-2.0), build-only
  `tonic-prost-build` (MIT), `protox` (MIT OR Apache-2.0).
- Learned from Android Studio's emulator integration (Apache-2.0): bearer-token auth from the discovery
  file and the W3C `GoHome`/`GoBack` key names. No code copied.

## Recommendation

Ship gRPC as the Android path for M4: it matches the iOS private backend (≈54 fps, ≈10 ms, ~100–160 ms
tap→pixels) with public, documented APIs and no permissions. Keep adb as the fallback (and the only path
for physical devices) — usable for viewing at ~8 fps with ~0.4 s input feedback. Boot headless with
`-no-window -gpu host` by default and shut down via gRPC. Next: wire `Platform::Android` in the device
panel (`android::list/open/boot`), add a log stream API, consider `MMAP` frames or scaled frames to cut
the 14.6 MB copies, and a hardware-key row (Back/Home/Recents) in the panel.
