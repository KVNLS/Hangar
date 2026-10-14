//! Hangar desktop app.
//!
//! Environment:
//! - `HANGAR_DB` — database file (default `~/Library/Application Support/Hangar/hangar.db`).
//! - `HANGAR_WORKTREES` — worktree root (default `~/Library/Application Support/Hangar/worktrees`).
//! - `RUST_LOG` — log filter (default `info`, logs go to stderr).
//! - `HANGAR_DEV_OPEN=<repo>` — at startup, add the repository as a project and create one
//!   workspace (`HANGAR_DEV_WORKSPACE`, default `dev`) running `HANGAR_DEV_AGENT` (default
//!   `shell`), then type `HANGAR_DEV_INPUT` followed by Enter into it. Exercises the full
//!   add-project → worktree → store → agent flow without clicking.
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
//! - `HANGAR_DEV_DEVICE_TAP=<x>,<y>` — then tap at those device points and log whether a later
//!   painted frame differs (`HANGAR_DEV_DEVICE_TAP painted frame changed`).
//! - `HANGAR_DEVICE_BACKEND=private|simctl` — force an iOS backend (no automatic fallback).
//! - The stats overlay (⌘⌥⇧P or the panel's Stats button) also logs `[device-stats]` lines.
//!
//! `HANGAR_GH=<program>` runs that `gh` instead of the one on the login PATH. Point it at a
//! fake script when testing: the real `gh` acts on the user's GitHub account.

use gpui_kit::*;
use hangar_app::shell::{DevOptions, HangarApp};
use tracing_subscriber::EnvFilter;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let dev = DevOptions::from_env();

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            hangar_app::terminal_view::init(cx);
            hangar_app::shell::init(cx);
            cx.on_window_closed(|cx, _| cx.quit()).detach();

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::centered(size(px(1280.), px(800.)), cx)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Hangar".into()),
                    ..Default::default()
                }),
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
