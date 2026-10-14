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
