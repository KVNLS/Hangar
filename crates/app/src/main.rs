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
