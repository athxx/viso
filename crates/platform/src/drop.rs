//! Drags across the app's edge: what one from outside offers a window and
//! the answer the window gives the OS, and what one the app carries out of
//! its windows holds.
//!
//! A backend sends a [`RawDrop`] for each step of a drag over one of its
//! windows — every move over it, its leaving, its drop — with one
//! [`DropReply`] for the whole visit. Where the OS asks for the answer while
//! its callback is on the stack the backend hands the step to the app there
//! and answers with what it said; where it cannot reach the app from the
//! callback it answers with what the reply holds, the app's answer to the
//! step before, which a drag's stream of moves makes good at once.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use crate::control::WindowId;

/// Which kinds of data a drag offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DropKinds {
    pub text: bool,
    pub uri: bool,
    pub files: bool,
}

impl DropKinds {
    /// Whether it offers any kind.
    pub fn any(self) -> bool {
        self.text || self.uri || self.files
    }
}

/// What a drag from outside offers: the kinds always, their contents where
/// the OS gives them before the drop and at the drop.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DropOffer {
    pub kinds: DropKinds,
    pub text: Option<String>,
    /// The first link, where it is not a file.
    pub uri: Option<String>,
    pub files: Vec<PathBuf>,
}

/// What a drop does with what it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropAction {
    Move,
    Copy,
    Link,
}

/// Which actions a drag's source allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropActions {
    pub moves: bool,
    pub copies: bool,
    pub links: bool,
}

impl DropActions {
    /// Every action.
    pub const ALL: DropActions = DropActions {
        moves: true,
        copies: true,
        links: true,
    };

    /// Whether it allows `action`.
    pub fn allows(self, action: DropAction) -> bool {
        match action {
            DropAction::Move => self.moves,
            DropAction::Copy => self.copies,
            DropAction::Link => self.links,
        }
    }

    /// The action a drop the window wants to do `wanted` does: that one
    /// where allowed, else a copy, a move or a link, the first allowed.
    pub fn settle(self, wanted: Option<DropAction>) -> Option<DropAction> {
        let wanted = wanted?;
        [wanted, DropAction::Copy, DropAction::Move, DropAction::Link]
            .into_iter()
            .find(|&a| self.allows(a))
    }
}

/// Where a drag from outside is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropPhase {
    /// It came over the window or moved over it.
    Over,
    /// It left the window, or was given up over it.
    Leave,
    /// It was let go over the window.
    Drop,
}

/// One step of a drag from outside over `window`, at (`x`, `y`) in logical
/// points.
#[derive(Debug, Clone, PartialEq)]
pub struct RawDrop {
    pub window: WindowId,
    pub phase: DropPhase,
    pub x: f64,
    pub y: f64,
    pub offer: DropOffer,
    pub reply: DropReply,
}

/// The window's answer to a drag from outside: what a drop where it is now
/// would do, none for one it would not take.
#[derive(Debug, Clone, Default)]
pub struct DropReply(Rc<Cell<Option<DropAction>>>);

impl PartialEq for DropReply {
    fn eq(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for DropReply {}

impl DropReply {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&self, action: Option<DropAction>) {
        self.0.set(action);
    }

    pub fn get(&self) -> Option<DropAction> {
        self.0.get()
    }
}

/// A drag the app carries out of its windows: its text, its link and its
/// files, any of which may be absent, and the actions a drop may do.
#[derive(Debug, Clone, PartialEq)]
pub struct DragOut {
    pub text: Option<String>,
    pub uri: Option<String>,
    pub files: Vec<PathBuf>,
    pub actions: DropActions,
}

impl DragOut {
    /// Its files and link as a `text/uri-list`, files first.
    pub fn uri_list(&self) -> String {
        let mut list = String::new();
        for uri in self.files.iter().filter_map(|f| file_uri(f)) {
            list.push_str(&uri);
            list.push_str("\r\n");
        }
        if let Some(uri) = &self.uri {
            list.push_str(uri);
            list.push_str("\r\n");
        }
        list
    }

    /// Its text, else its link, else its files' paths, a line each.
    pub fn plain_text(&self) -> String {
        if let Some(text) = self.text.as_ref().or(self.uri.as_ref()) {
            return text.clone();
        }
        let paths: Vec<_> = self.files.iter().map(|f| f.to_string_lossy()).collect();
        paths.join("\n")
    }
}

/// The `file:` URI of absolute path `path`, its bytes outside the unreserved
/// set escaped; `None` for a relative path.
pub fn file_uri(path: &std::path::Path) -> Option<String> {
    if !path.is_absolute() {
        return None;
    }
    let text = path.to_string_lossy();
    let text = text.replace('\\', "/");
    let mut uri = String::from("file://");
    if !text.starts_with('/') {
        // A Windows drive path goes as `/C:/…`.
        uri.push('/');
    }
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~:".contains(&b) {
            uri.push(b as char);
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    Some(uri)
}

/// What a `text/uri-list` holds: its files, by path, and its first link
/// that is no file. Comment lines and blank ones are skipped.
pub fn uri_list(list: &str) -> (Vec<PathBuf>, Option<String>) {
    let mut files = Vec::new();
    let mut link = None;
    for line in list.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        match file_path(line) {
            Some(path) => files.push(path),
            None if link.is_none() => link = Some(line.to_owned()),
            None => {}
        }
    }
    (files, link)
}

/// The path a `file:` URI names, its escapes decoded; `None` for any other
/// URI, or one of another host.
pub fn file_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = match rest.find('/') {
        Some(0) => rest,
        Some(at) if matches!(&rest[..at], "localhost") => &rest[at..],
        _ => return None,
    };
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex(bytes[i + 1]), hex(bytes[i + 2]))
        {
            out.push(hi << 4 | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    let path = String::from_utf8(out).ok()?;
    // A Windows drive path comes as `/C:/…`.
    let path = match path.as_bytes() {
        [b'/', drive, b':', ..] if drive.is_ascii_alphabetic() && cfg!(windows) => {
            path[1..].to_owned()
        }
        _ => path,
    };
    Some(PathBuf::from(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uri_list_holds_its_files_and_its_first_link() {
        let (files, link) = uri_list(
            "# dragged\r\nfile:///home/a/b%20c.txt\r\nhttps://example.org/x\r\n\
             file://localhost/tmp/%E4%BD%A0.png\r\nhttps://other\r\n",
        );
        assert_eq!(
            files,
            [
                PathBuf::from("/home/a/b c.txt"),
                PathBuf::from("/tmp/你.png")
            ]
        );
        assert_eq!(link.as_deref(), Some("https://example.org/x"));
        assert_eq!(file_path("file://elsewhere/x"), None);
    }

    #[test]
    fn a_drag_out_lists_its_files_as_uris_that_read_back() {
        let out = DragOut {
            text: None,
            uri: Some("https://example.org/x".into()),
            files: vec![PathBuf::from("/tmp/b c/你.png")],
            actions: DropActions::ALL,
        };
        let list = out.uri_list();
        assert_eq!(
            list,
            "file:///tmp/b%20c/%E4%BD%A0.png\r\nhttps://example.org/x\r\n"
        );
        let (files, link) = uri_list(&list);
        assert_eq!(files, out.files);
        assert_eq!(link, out.uri);
        assert_eq!(out.plain_text(), "https://example.org/x");
        assert_eq!(file_uri(std::path::Path::new("rel")), None);
    }

    #[test]
    fn a_drop_does_what_the_window_wants_where_the_source_allows() {
        let copies = DropActions {
            moves: false,
            copies: true,
            links: false,
        };
        assert_eq!(
            copies.settle(Some(DropAction::Move)),
            Some(DropAction::Copy)
        );
        assert_eq!(
            DropActions::ALL.settle(Some(DropAction::Link)),
            Some(DropAction::Link)
        );
        assert_eq!(copies.settle(None), None);
    }
}
