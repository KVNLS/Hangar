//! The changes panel: per-workspace live diff, file list and highlighted diff view.

mod dialogs;
pub mod highlight;
pub mod language;
pub mod live;
pub mod model;
pub mod review;
mod view;

pub use live::WorkspaceDiff;

/// ⌘⏎ commits from the side panel and the review screen.
pub fn init(cx: &mut gpui_kit::App) {
    cx.bind_keys([
        gpui_kit::KeyBinding::new("cmd-enter", CommitStaged, Some("ReviewScreen")),
        gpui_kit::KeyBinding::new("cmd-enter", CommitStaged, Some("ChangesPanel")),
    ]);
}
pub use view::{ChangesEvent, ChangesPanel, CommitStaged, ExitReview, PanelSource, WorkspaceAgent};
