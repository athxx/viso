//! The languages the editor knows.

#[cfg(feature = "vs")]
pub mod vs;

use std::rc::Rc;

use crate::highlight::{Language, PlainText};

/// The language a file at `path` is in, by its extension: plain text for
/// one the editor does not know.
pub fn for_path(path: &str) -> Rc<dyn Language> {
    let ext = path.rsplit_once('.').map(|(_, e)| e);
    match ext {
        #[cfg(feature = "vs")]
        Some("vs") => Rc::new(vs::Vs),
        _ => Rc::new(PlainText),
    }
}
