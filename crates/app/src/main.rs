//! Hangar desktop app.
//!
//! `Hangar mcp-bridge` runs the MCP stdio bridge instead of the app (checked before anything
//! GPUI starts): agents launched from a workspace start it as their `hangar` MCP server. It
//! relays stdin/stdout to the running app's socket (`HANGAR_MCP_SOCKET`) with the workspace's
//! session secret (`HANGAR_MCP_SESSION`), prints errors to stderr and exits non-zero on failure.
//! `Hangar notify <event>` is a Claude Code hook command: it sends what the event means for the
//! agent's state (working, approval, input, turn ended) to the app the same way, and always exits
//! 0 so a closed Hangar never gets in Claude Code's way.
//!
//! Environment:
//! - `HANGAR_DB` — database file (default `~/Library/Application Support/Hangar/hangar.db`).
//! - `HANGAR_WORKTREES` — worktree root (default `~/Library/Application Support/Hangar/worktrees`).
//! - `RUST_LOG` — log filter (default `info`, logs go to stderr).
//! - `HANGAR_DEV_OPEN=<repo>` — at startup, add the repository as a project and create one
//!   workspace (`HANGAR_DEV_WORKSPACE`, default `dev`) running `HANGAR_DEV_AGENT` (default
//!   `shell`), then type `HANGAR_DEV_INPUT` followed by Enter into it (once it runs, if the
//!   start queued at "Max concurrent agents"). Exercises the full
//!   add-project → worktree → store → agent flow without clicking. Several comma-separated agents
//!   create one grouped workspace per agent (best of N), as ⌘-clicking agents in the sheet does.
//! - `HANGAR_DEV_OLLAMA=<host>` — before `HANGAR_DEV_OPEN`, turn Ollama on at that host for this run (not
//!   saved) and wait for its model list, so an OpenCode agent starts with the local models.
//!
//! After `HANGAR_DEV_OPEN` (if set), these run in order against workspaces found by slug,
//! bypassing the confirmation dialogs:
//! - `HANGAR_DEV_ARCHIVE=<slug>` — archive the workspace (stops its agent).
//! - `HANGAR_DEV_RESTORE=<slug>` — restore an archived workspace (does not start an agent).
//! - `HANGAR_DEV_DELETE=<slug>` — delete the workspace's worktree and rows.
//!   `HANGAR_DEV_DELETE_BRANCH=1` also deletes its branch (never forced);
//!   `HANGAR_DEV_DELETE_FORCE=1` discards uncommitted/untracked files.
//! - `HANGAR_DEV_REMOVE_PROJECT=<repo>` — forget the project (refused while it has active
//!   workspaces). Never touches the repository.
//!
//! `HANGAR_DEV_FILES=<path>` opens the Files tab on the dev workspace, opens that file and logs the
//! listed files, tree rows and viewer lines.
//!
//! `HANGAR_DEV_APPEARANCE=dark|light|system` renders every settings page, then sets and saves
//! the appearance.
//!
//! `HANGAR_DEV_OVERVIEW=1` (after the review hook) opens the overview, logs the per-state
//! counts, then goes back.
//!
//! `HANGAR_DEV_REVIEW=1` opens the full-width review screen once files are listed (after the
//! review hooks below), logs its row counts, renders the inline mode, then goes back.
//!
//! `HANGAR_DEV_SHOW=<screen>` runs last and leaves the window on `workspace`, `review` (split),
//! `review-inline`, `overview`, `compare` (the focused workspace's attempts), `settings-{agents,permissions,devices,usage,appearance,keybindings}`, `palette` or
//! `sheet` (New workspace) or `device` (device panel), for screenshots.
//! `HANGAR_DEV_SCREENSHOT=1` then waits for the workspace device's first frame and takes the
//! device panel's ⌘S screenshot for the agent.
//! `HANGAR_DEV_APPROVE=<seconds>` then waits for a structured agent's permission request and
//! allows it once that many seconds later, through the overview's and palette's path.
//! `HANGAR_DEV_ANSWER="<s>|<text or choice index>"` opens the first question an agent asks
//! (`ask_user`) and answers it that many seconds later.
//! `HANGAR_DEV_KEYS="<s>|<keys>||<s>|<keys>"` types raw keys into the focused agent after each
//! delay (`\r`, `\n`, `\e` escapes), e.g. to answer Claude Code's folder trust prompt.
//!
//! Device pool (one device per workspace):
//! - `HANGAR_DEV_WORKSPACES=a,b` — `HANGAR_DEV_OPEN` creates these workspaces instead of
//!   `HANGAR_DEV_WORKSPACE`.
//! - `HANGAR_DEV_TARGET=ios:<UDID>|android:<AVD id>` — base device of the created workspaces.
//! - `HANGAR_DEV_DEVICE_LIMIT=auto|1..8` — "Devices at once" for this launch only (not saved).
//! - `HANGAR_DEV_RUN=1` — builds and runs every created workspace (else the focused one) on its
//!   own device at the same time (⌘R), logs `HANGAR_DEV_RUN done/failed`; `=<slug,slug>` runs
//!   those workspaces.
//! - `HANGAR_DEV_POOL_FRAMES=<dir>` — per created workspace, through the device API: status, a
//!   frame saved as `<slug>.png`, a tap at the screen centre, the next frame as `<slug>-tap.png`,
//!   then 5 log lines.
//! - `HANGAR_DEV_SELECT=<slug>` — shows that workspace after the runs.
//! - `HANGAR_PRESSURE=normal|warn|critical` — overrides the memory pressure reading for the boot
//!   guard and the 10 s monitor (testing only; the real reading is used otherwise).
//!
//! `HANGAR_DEV_CHANGES=1` opens the changes panel before the hooks above run; a
//! worktree-relative path instead of `1` also selects that file once it shows up.
//!
//! Review hooks run right after `HANGAR_DEV_OPEN` (before archive/restore/delete), on the
//! workspace shown in the changes panel, through the panel's own operations; outcomes are
//! logged (`HANGAR_DEV_* ...` lines):
//! - `HANGAR_DEV_COMMIT=<message>` — stage everything (retrying up to ~15 s until the agent's
//!   files show up), then commit.
//! - `HANGAR_DEV_PUSH=1` — push the branch (sets the upstream on the first push).
//! - `HANGAR_DEV_PR=<title>` — open a draft pull request through `gh` (`1` = default title).
//! - `HANGAR_DEV_MERGE=noff|squash` — merge into the base branch with the default message.
//!
//! Device panel (⌘⇧P; one per window, shared by all workspaces):
//! - `HANGAR_DEV_DEVICE=<udid>` — open the device panel on that (booted) device; logs
//!   `HANGAR_DEV_DEVICE frame painted` once a frame is on screen.
//! - `HANGAR_DEV_DEVICE_LOGS=1` — open the logs pane under the device screen (saved with the panel
//!   state otherwise; `RUST_LOG=hangar_app=trace` prints per-poll timings).
//! - `HANGAR_DEV_DEVICE_TAP=<x>,<y>` — then tap at those device points and log whether a later
//!   painted frame differs (`HANGAR_DEV_DEVICE_TAP painted frame changed`).
//! - `HANGAR_DEV_DEVICE_HOME=1` — press Home first (same check).
//! - `HANGAR_DEV_DEVICE_TEXT=<text>` — type that text (after tap/home) with the same check.
//! - `HANGAR_DEV_DEVICE_BOOT=1` — with `HANGAR_DEV_DEVICE=<AVD id>` of a shut-down Android
//!   virtual device: boot it from the panel first, then open it (waits up to 5 minutes). Also works for a
//!   shut-down iOS simulator UDID.
//! - `HANGAR_DEV_DEVICE_BUTTONS=lock,volume-up,...` — press these hardware controls (`home`, `back`,
//!   `lock`, `volume-up`, `volume-down`, `app-switch`) after the text, with the same check.
//! - `HANGAR_DEV_DEVICE_KEYS=Backspace,Enter,...` — then press these keys (`Backspace`, `Enter`,
//!   `Tab`, `Escape`, `ArrowUp`/`Down`/`Left`/`Right`).
//! - `HANGAR_DEV_DEVICE_SHUTDOWN=1` — finally shut the device down from the panel.
//! - `HANGAR_DEVICE_BACKEND=private|simctl|grpc|adb` — force an iOS (`private`, `simctl`) or
//!   Android (`grpc`, `adb`) backend (no automatic fallback).
//! - `HANGAR_DEVICE_STATS=1` — force the stats overlay on.
//! - The stats overlay (⌘⌥⇧P or the panel's Stats button) also logs `[device-stats]` lines.
//!
//! `HANGAR_GH=<program>` runs that `gh` instead of the one on the login PATH. Point it at a
//! fake script when testing: the real `gh` acts on the user's GitHub account.

use gpui_kit::*;
use hangar_app::shell::{DevOptions, HangarApp};
use tracing_subscriber::EnvFilter;

fn main() -> std::process::ExitCode {
    if std::env::args_os()
        .nth(1)
        .is_some_and(|arg| arg == "mcp-bridge")
    {
        return match hangar_mcp::run_bridge_from_env() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("Hangar mcp-bridge: {err}");
                std::process::ExitCode::FAILURE
            }
        };
    }
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|arg| arg == "notify") {
        let event = args
            .next()
            .and_then(|e| e.into_string().ok())
            .unwrap_or_default();
        if let Err(err) = hangar_mcp::hook::notify_from_env(&event, std::io::stdin().lock()) {
            eprintln!("Hangar notify {event}: {err}");
        }
        return std::process::ExitCode::SUCCESS;
    }
    run_app();
    std::process::ExitCode::SUCCESS
}

fn run_app() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let dev = DevOptions::from_env();

    gpui_kit::application()
        .with_assets(hangar_app::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            if let Err(err) = hangar_app::app_icon::set_dock_icon(cx) {
                tracing::warn!("could not set the Dock icon: {err:#}");
            }
            if let Err(err) = hangar_app::ui::init(component::ThemeMode::Dark, cx) {
                tracing::error!("design system init failed, using the default theme: {err:#}");
            }
            hangar_app::terminal_view::init(cx);
            hangar_app::chat_view::init(cx);
            hangar_app::changes::init(cx);
            hangar_app::device_view::init(cx);
            hangar_app::shell::init(cx);
            cx.on_window_closed(|cx, _| cx.quit()).detach();

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
                titlebar: Some(hangar_app::shell::titlebar_options()),
                // The custom title bar moves the window itself (see gpui-component TitleBar).
                app_owns_titlebar_drag: true,
                ..Default::default()
            };
            let opened = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| HangarApp::new(dev, window, cx))
            });
            match opened {
                Ok(_) => cx.activate(true),
                Err(err) => {
                    tracing::error!("failed to open window: {err:#}");
                    cx.quit();
                }
            }
        });
}
