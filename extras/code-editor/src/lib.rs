//! A code editor.

pub mod decoration;
pub mod document;
mod edit;
mod find;
pub mod fold;
pub mod highlight;
pub mod history;
pub mod keymap;
pub mod lang;
pub mod layout;
#[cfg(all(
    feature = "lsp",
    not(any(target_arch = "wasm32", target_os = "ios", target_os = "android"))
))]
pub mod lsp;
pub mod motion;
pub mod rope;
pub mod search;
pub mod selection;
pub mod service;
pub mod session;
pub mod text;
pub mod theme;
mod view;

pub use document::{Applied, Assoc, Change, ChangeSet, Document, LineEnding, LineSplice};
pub use history::{EditKind, History};
pub use rope::Rope;
pub use search::Query;
pub use selection::{Selection, Selections};
pub use session::{Config, Session};
pub use theme::Theme;
pub use view::{CodeEditor, DrawnRow, EditorHandle, EditorSlot, OnDefinition, code_editor};
