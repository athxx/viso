//! Open and save dialogs.

use std::path::PathBuf;

use crate::reply::Reply;

/// A named group of file extensions a dialog offers, such as
/// `("Images", ["png", "jpg"])`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileFilter {
    pub name: String,
    /// Extensions without the dot.
    pub extensions: Vec<String>,
}

impl FileFilter {
    pub fn new(name: impl Into<String>, extensions: &[&str]) -> Self {
        Self {
            name: name.into(),
            extensions: extensions.iter().map(|e| (*e).to_owned()).collect(),
        }
    }
}

/// What an open dialog asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenOptions {
    pub title: Option<String>,
    /// Empty accepts every file.
    pub filters: Vec<FileFilter>,
    pub multiple: bool,
}

/// What a save dialog proposes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SaveOptions {
    pub title: Option<String>,
    pub suggested_name: String,
    pub filters: Vec<FileFilter>,
}

/// A file the user picked, read in full.
///
/// Mobile and Web pickers grant access to the contents rather than a path,
/// so the contents are always read; the path is there only where the OS
/// has one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickedFile {
    pub name: String,
    pub path: Option<PathBuf>,
    pub contents: Vec<u8>,
}

/// The system's open and save dialogs. Dismissing a dialog answers
/// [`ServiceError::Cancelled`](crate::ServiceError::Cancelled).
pub trait FileDialogs {
    /// Let the user pick one file, or several with `multiple`.
    fn open(&self, options: OpenOptions) -> Reply<Vec<PickedFile>>;

    /// Let the user choose where `contents` go, and write them there. The
    /// answer is the written path where the OS exposes one (not on Web, where
    /// the browser downloads the file).
    fn save(&self, options: SaveOptions, contents: Vec<u8>) -> Reply<Option<PathBuf>>;
}
