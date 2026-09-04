//! Open-document tracking: the `FileId` / `SourceMap` boundary (net-new piece 3).
//!
//! The `viso-dsl` frontend is single-source: it has no notion of a file handle.
//! A language server, by contrast, tracks a set of open documents addressed by URI
//! and mutated by incremental edits. This module is that thin boundary — it assigns
//! each open document a [`FileId`], holds its current text alongside the derived
//! frontend artifacts ([`OpenDoc`]), and re-derives them on edit.
//!
//! Each document is resolved as its own single-module graph. Cross-module
//! resolution (following `import` edges across open files) is a later concern; the
//! minimal usable set answers goto/references/rename/format within one document.
//!
//! Re-deriving on edit is a full re-tokenize → re-parse → re-resolve. The frontend
//! does offer incremental relexing ([`reparse_tokens`](viso_dsl::reparse_tokens)),
//! but editor edits are rare next to frame work and a document is small, so the
//! simpler full path is the right cold-path choice here (AGENTS 7.2); switching to
//! incremental is a transparent optimization behind this same interface.

use std::collections::HashMap;

use viso_dsl::LineIndex;
use viso_dsl::Parse;
use viso_dsl::resolve::{
    ModuleGraph, ModulePath, NameInterner, ResolvedModule, SourceUnit, resolve,
};
use viso_dsl::syntax::grammar::parse;
use viso_dsl::tokenize;

use crate::index::ReferenceIndex;

/// A stable identifier for an open document, assigned by [`SourceMap::open`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FileId(u32);

impl FileId {
    /// The raw index behind this id.
    #[inline]
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One open document and everything derived from its current text: the line index
/// for position conversion, the parse tree (lossless CST + parse diagnostics), the
/// resolved module (symbol table, resolved refs, declaration spans, resolve
/// diagnostics), and the reverse [`ReferenceIndex`] over it.
pub struct OpenDoc {
    /// The document's current full text.
    pub source: String,
    /// Byte ↔ line/column ↔ UTF-16 index over `source`.
    pub line_index: LineIndex,
    /// The parse result: lossless green tree plus parse diagnostics.
    pub parse: Parse,
    /// The resolved module: symbol table, resolved references, declaration spans,
    /// and resolution diagnostics.
    pub resolved: ResolvedModule,
    /// Reverse def→use index and declaration spans over `resolved`.
    pub index: ReferenceIndex,
}

impl OpenDoc {
    /// Derives every artifact from source text, resolving it as a single-module
    /// graph under `module_path`.
    fn derive(source: String, module_path: &[&str]) -> OpenDoc {
        let line_index = LineIndex::new(&source);
        let parse_result = parse(&tokenize(&source), &source);
        let mut interner = NameInterner::new();
        let path = ModulePath::intern(&mut interner, module_path);
        let units = vec![SourceUnit::new(path, parse_result.clone())];
        let graph = ModuleGraph::build(&units, &interner);
        let mut resolved = resolve(&graph, &units, &mut interner, "");
        // A single-unit graph resolves to exactly one module.
        let resolved = resolved.pop().unwrap_or_else(|| ResolvedModule {
            table: Default::default(),
            refs: Vec::new(),
            decls: Vec::new(),
            errors: Vec::new(),
        });
        let index = ReferenceIndex::build(&resolved);
        OpenDoc {
            source,
            line_index,
            parse: parse_result,
            resolved,
            index,
        }
    }
}

/// The set of open documents, addressed by [`FileId`].
#[derive(Default)]
pub struct SourceMap {
    docs: HashMap<FileId, OpenDoc>,
    next_id: u32,
}

impl SourceMap {
    /// An empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens a document with the given text, deriving its artifacts, and returns its
    /// fresh [`FileId`]. `module_path` names the module the document resolves as.
    pub fn open(&mut self, source: String, module_path: &[&str]) -> FileId {
        let id = FileId(self.next_id);
        self.next_id += 1;
        self.docs.insert(id, OpenDoc::derive(source, module_path));
        id
    }

    /// Replaces an open document's full text and re-derives its artifacts. A no-op if
    /// `id` is not open.
    pub fn update(&mut self, id: FileId, source: String, module_path: &[&str]) {
        if self.docs.contains_key(&id) {
            self.docs.insert(id, OpenDoc::derive(source, module_path));
        }
    }

    /// Closes a document, dropping its artifacts.
    pub fn close(&mut self, id: FileId) {
        self.docs.remove(&id);
    }

    /// The open document for `id`, if any.
    pub fn get(&self, id: FileId) -> Option<&OpenDoc> {
        self.docs.get(&id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_update_close_lifecycle() {
        let mut map = SourceMap::new();
        let id = map.open("component C {\n  state count = 0;\n}\n".to_string(), &["m"]);
        assert!(map.get(id).is_some());
        // An edit re-derives: the new source has an extra member the index sees.
        map.update(
            id,
            "component C {\n  state count = 0;\n  computed d = count;\n}\n".to_string(),
            &["m"],
        );
        let doc = map.get(id).expect("still open");
        assert!(
            doc.source.contains("computed d"),
            "update replaced the text"
        );
        // The reverse index now has a use of `count` from the computed.
        assert!(
            !doc.resolved.refs.is_empty(),
            "resolution ran on the updated text"
        );
        map.close(id);
        assert!(map.get(id).is_none());
    }

    #[test]
    fn update_of_unopened_file_is_noop() {
        let mut map = SourceMap::new();
        map.update(FileId(999), "component C {}".to_string(), &["m"]);
        assert!(map.get(FileId(999)).is_none());
    }
}
