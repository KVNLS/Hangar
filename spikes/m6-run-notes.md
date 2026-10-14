# M6 run loop — notes from the first live runs (2026-10-05)

Scratch projects in the session scratchpad, driven with `HANGAR_DEV_OPEN` + `HANGAR_DEV_DEVICE` +
`HANGAR_DEV_RUN=1`. Mac: Xcode 27, iPhone 17 Pro simulator (iOS 26.5), Pixel_10_Pro AVD (API 37).

## What runs where
- `hangar-runner` detects the stack (root, `ios/`, `android/`), picks the one for the open device's
  platform, and turns it into command lines. The app runs them through `$SHELL -c` in terminal tabs next to
  the agent, with the login `PATH`, the Android SDK `platform-tools`/`emulator` prepended (adb is not on the
  login PATH here), `ANDROID_HOME`, `ANDROID_SERIAL` and `RCT_METRO_PORT`.
- Native builds then install and launch through the device (`DeviceHandle::install/launch`, off the input
  queue). The JS CLIs install and launch themselves.
- Ports: 8100–8199, one per workspace, saved as `run:<workspace id>` in `settings`. An allocated port must
  also bind on localhost, so a Metro started outside Hangar is skipped. 8081 stays free.
- Xcode derived data goes to `<data>/builds/<workspace id>`, outside the worktree, so builds never show up
  as changes.

## Timings
| stack | target | result |
|---|---|---|
| Xcode (hand-written SwiftUI project) | iPhone 17 Pro | build 5 s, install + launch 4 s |
| Gradle (AGP 9.1, plain Activity) | Pixel_10_Pro | `:app:assembleDebug` 1.4 s (warm daemon, 32 tasks), install + launch 0.5 s |
| Expo SDK 57 (blank template) | iPhone 17 Pro | `npm install` 4–10 s in a fresh worktree; `expo run:ios --port 8100` (prebuild, pods, build, install, launch) 82 s on the second run; bundle of 696 modules served from :8100 in 1.6 s |

## Quirks
- Worktrees are fresh checkouts: no `node_modules`. JS stacks run an install first (lockfile picks
  bun/pnpm/yarn/`npm ci`, else `npm install`) in the build tab; Metro and the build start when it succeeds.
- `expo run:* --port P --no-bundler` is refused ("mutually exclusive"). `expo run` therefore starts Metro
  itself on the workspace port and its tab stays open as the dev server (reload keys work there).
- `expo run` prebuilds: it writes `ios/` (and `android/`) into the worktree, so the changes panel shows
  thousands of added lines unless the project ignores them (continuous native generation).
- AGP 9.2 needs Gradle 9.4.1; AGP's jlink step fails on JDK 27. The scratch project pins JDK 21 with
  `org.gradle.java.home`. Hangar passes the user's environment through and does not choose a JDK.
- The terminal's `[process exited]` banner and IME backgrounds were painted over their own text (found
  here, fixed in the terminal element).

## Not run live
- Two workspaces running at once: port allocation (taken and bound ports) is unit-tested, not run live.
- React Native CLI (`run-ios`/`run-android` with `--no-packager`): command lines are unit-tested only.
- Flutter support was dropped (owner decision).
