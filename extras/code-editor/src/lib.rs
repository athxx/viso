//! A code editor.

pub mod decoration;
pub mod document;
mod edit;
pub mod fold;
pub mod highlight;
pub mod history;
pub mod keymap;
pub mod lang;
pub mod layout;
pub mod motion;
pub mod rope;
pub mod search;
pub mod selection;
pub mod session;
pub mod text;
pub mod theme;
mod view;

pub use document::{Applied, Assoc, Change, ChangeSet, Document, LineEnding, LineSplice};
pub use history::{EditKind, History};
pub use rope::Rope;
pub use selection::{Selection, Selections};
pub use session::{Config, Session};
pub use theme::Theme;
pub use view::{CodeEditor, DrawnRow, EditorHandle, EditorSlot, code_editor};
