//! Spike A: one GPUI window with one PTY-backed terminal pane.
//!
//! `cargo run -p hangar-examples --example spike_terminal -- [command args...]`
//! Runs `$SHELL` (login) when no command is given. `HANGAR_TERM_STATS=1` logs frames/s,
//! PTY bytes/s and keystroke→paint latency to stderr once per second.
//! `HANGAR_SPIKE_AUTOTYPE=N` injects N synthetic `x` keystrokes (150 ms apart, through GPUI's
//! key dispatch and the input handler) to measure in-app latency without a human; use it with
//! a harmless program such as `cat`.

use std::ffi::OsString;
use std::time::Duration;

use gpui_kit::*;
use hangar_app::terminal_view::{StartedTerminal, TerminalView, TerminalViewOptions};
use hangar_terminal::SpawnOptions;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args_os().skip(1);
    let spawn = match args.next() {
        Some(program) => SpawnOptions::command(program, args.collect::<Vec<OsString>>()),
        None => SpawnOptions::default(),
    };
    // Spawn before the window exists so a bad command fails with a plain error.
    let started = StartedTerminal::spawn(&spawn)?;

    gpui_kit::application().run(move |cx| {
        gpui_kit::init(cx);
        hangar_app::terminal_view::init(cx);
        cx.on_window_closed(|cx, _| cx.quit()).detach();

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(size(px(960.), px(640.)), cx)),
            titlebar: Some(TitlebarOptions {
                title: Some("Hangar — Spike A".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let opened = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| TerminalView::new(started, TerminalViewOptions::default(), window, cx))
        });
        match opened {
            Ok((window, _)) => {
                cx.activate(true);
                if let Some(count) = autotype_count() {
                    autotype(window, count, cx);
                }
            }
            Err(err) => {
                eprintln!("failed to open window: {err:#}");
                cx.quit();
            }
        }
    });
    Ok(())
}

fn autotype_count() -> Option<usize> {
    std::env::var("HANGAR_SPIKE_AUTOTYPE").ok()?.parse().ok()
}

fn autotype(window: AnyWindowHandle, count: usize, cx: &mut App) {
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(1500))
            .await;
        for _ in 0..count {
            let typed = cx.update_window(window, |_, window, cx| match Keystroke::parse("x") {
                Ok(keystroke) => window.dispatch_keystroke(keystroke, cx),
                Err(_) => false,
            });
            if typed.is_err() {
                return;
            }
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
        }
    })
    .detach();
}
