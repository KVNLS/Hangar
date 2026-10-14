//! The changes panel: per-workspace live diff, file list and highlighted diff view.

pub mod highlight;
pub mod language;
pub mod live;
pub mod model;
mod view;

pub use live::WorkspaceDiff;
pub use view::{ChangesEvent, ChangesPanel};
