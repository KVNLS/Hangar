//! The changes panel: per-workspace live diff, file list and highlighted diff view.

mod dialogs;
pub mod highlight;
pub mod language;
pub mod live;
pub mod model;
pub mod review;
mod view;

pub use live::WorkspaceDiff;
pub use view::{Busy, ChangesEvent, ChangesPanel, PanelSource};
