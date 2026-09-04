//! The analysis engine: goto-definition, find-references, rename, and formatting
//! as pure functions over an [`OpenDoc`].
//!
//! Every function here takes a document (and, where relevant, a cursor position)
//! and returns spans or edits. There is **no protocol dependency** — no LSP types,
//! no transport, no async runtime. The stdio frontend (the `viso-lsp` bin) is the
//! only thing that turns these results into protocol messages; the engine itself is
//! fully headless unit-testable, which is the whole point of the two-layer split.
//!
//! Positions here are byte offsets and byte [`TextRange`]s — the frontend's native
//! coordinate. The UTF-16 conversion the protocol needs happens at the boundary via
//! [`crate::position`], not in the engine, so the analysis logic never deals with
//! encoding concerns.
//!
//! Cold-path tooling throughout (AGENTS 7.2).

use viso_dsl::TextRange;
use viso_dsl::TextSize;
use viso_dsl::resolve::Resolution;
use viso_dsl::{SyntaxKind, tokenize};

use crate::source_map::OpenDoc;

pub use crate::format::format;

/// A resolved source location: just a byte range within the document. (The document
/// identity is implicit — the engine works on one [`OpenDoc`] at a time; the
/// frontend pairs the range with the request's URI.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Location {
    /// The byte range of the located name.
    pub range: TextRange,
}

/// One text edit: replace `range` with `new_text`. Rename produces a batch of these.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    /// The byte range to replace.
    pub range: TextRange,
    /// The replacement text.
    pub new_text: String,
}

/// Why a rename was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenameError {
    /// The cursor was not on a resolvable name.
    NotAName,
    /// The proposed new name is not a single valid identifier (empty, whitespace,
    /// punctuation, multiple tokens, or a reserved keyword).
    InvalidName,
}

/// The resolution the cursor sits on, if any.
///
/// First checks the resolved references (every name *use*, including a local's
/// binding site, which the resolver records as a self-reference). If the cursor is
/// not on a use, it falls back to the declaration name of a module symbol — the
/// resolver does not record a self-reference for a module-level `state`/`computed`/…
/// binding, so a cursor on the definition name is matched against the recorded
/// declaration spans instead. This makes goto/references/rename work from the
/// definition site as well as from a use.
fn resolution_at(doc: &OpenDoc, offset: TextSize) -> Option<Resolution> {
    if let Some(res) = doc
        .resolved
        .refs
        .iter()
        .find(|r| r.range.contains(offset))
        .map(|r| r.to)
    {
        return Some(res);
    }
    doc.index.symbol_at(offset).map(Resolution::Symbol)
}

/// Goto-definition: the declaration span of the name under the cursor.
///
/// For a symbol, this is the declaration's name-token span (only when the symbol is
/// declared in this document; a symbol imported from elsewhere has no local
/// declaration span, so this returns `None`). For a local, it is the binding site.
pub fn goto_definition(doc: &OpenDoc, offset: TextSize) -> Option<Location> {
    let res = resolution_at(doc, offset)?;
    doc.index.decl_of(res).map(|range| Location { range })
}

/// Find-references: every use of the name under the cursor.
///
/// The document's use list already includes the declaration/binding occurrence
/// (the resolver records it as a self-reference), so `include_declaration` only
/// controls whether a symbol's separately-recorded declaration span is added when
/// it is not already among the uses.
pub fn find_references(
    doc: &OpenDoc,
    offset: TextSize,
    include_declaration: bool,
) -> Vec<Location> {
    let Some(res) = resolution_at(doc, offset) else {
        return Vec::new();
    };
    let mut locations: Vec<Location> = doc
        .index
        .uses_of(res)
        .iter()
        .map(|&range| Location { range })
        .collect();
    if include_declaration
        && let Some(decl) = doc.index.decl_of(res)
        && !locations.iter().any(|l| l.range == decl)
    {
        locations.push(Location { range: decl });
    }
    locations
}

/// Rename: the edits that rewrite every occurrence of the name under the cursor to
/// `new_name`.
///
/// Rewrites the declaration/binding site and every use in this document. Returns
/// [`RenameError::NotAName`] if the cursor is not on a resolvable name, or
/// [`RenameError::InvalidName`] if `new_name` is not a single valid identifier.
pub fn rename(
    doc: &OpenDoc,
    offset: TextSize,
    new_name: &str,
) -> Result<Vec<TextEdit>, RenameError> {
    let res = resolution_at(doc, offset).ok_or(RenameError::NotAName)?;
    if !is_valid_identifier(new_name) {
        return Err(RenameError::InvalidName);
    }
    // Every occurrence: the uses (which include the binding site for a local) plus
    // the declaration span for a symbol, de-duplicated.
    let mut ranges: Vec<TextRange> = doc.index.uses_of(res).to_vec();
    if let Some(decl) = doc.index.decl_of(res)
        && !ranges.contains(&decl)
    {
        ranges.push(decl);
    }
    Ok(ranges
        .into_iter()
        .map(|range| TextEdit {
            range,
            new_text: new_name.to_string(),
        })
        .collect())
}

/// Whether `name` is a single valid identifier token (and not a keyword).
///
/// Lexes the candidate and accepts it only if it is exactly one non-trivia token
/// (before end-of-input) classified as an identifier — so keywords, punctuation,
/// numbers, whitespace-containing strings, and multi-token inputs are all rejected.
fn is_valid_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let tokens = tokenize(name);
    let mut significant = tokens
        .iter()
        .filter(|t| !t.is_trivia() && t.kind != SyntaxKind::Eof);
    match (significant.next(), significant.next()) {
        (Some(tok), None) => {
            matches!(tok.kind, SyntaxKind::Ident | SyntaxKind::RawIdent)
                && tok.error.is_none()
                && tok.range.len().to_usize() == name.len()
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_map::SourceMap;

    fn doc(src: &str) -> (SourceMap, crate::source_map::FileId) {
        let mut map = SourceMap::new();
        let id = map.open(src.to_string(), &["m"]);
        (map, id)
    }

    fn at(src: &str, needle: &str, occurrence: usize) -> TextSize {
        let mut idx = 0usize;
        let mut found = 0usize;
        loop {
            let rel = src[idx..].find(needle).expect("needle present");
            let pos = idx + rel;
            if found == occurrence {
                return TextSize::new(pos as u32 + 1); // inside the token
            }
            found += 1;
            idx = pos + 1;
        }
    }

    #[test]
    fn goto_definition_from_use_to_declaration() {
        let src = "component C {\n  state count = 0;\n  computed d = count;\n}\n";
        let (map, id) = doc(src);
        let doc = map.get(id).unwrap();
        // Cursor on the `count` use inside the computed (second occurrence).
        let loc = goto_definition(doc, at(src, "count", 1)).expect("goto resolves");
        // Points at the state declaration (first occurrence).
        assert_eq!(
            loc.range.start().to_usize(),
            src.find("count").unwrap(),
            "goto lands on the declaration"
        );
    }

    #[test]
    fn find_references_lists_declaration_and_use() {
        let src = "component C {\n  state count = 0;\n  computed d = count;\n}\n";
        let (map, id) = doc(src);
        let doc = map.get(id).unwrap();
        let refs = find_references(doc, at(src, "count", 1), true);
        // The declaration and the one use → two locations.
        assert_eq!(refs.len(), 2, "declaration + one use");
    }

    #[test]
    fn rename_rewrites_every_occurrence() {
        let src = "component C {\n  state count = 0;\n  computed d = count;\n}\n";
        let (map, id) = doc(src);
        let doc = map.get(id).unwrap();
        let edits = rename(doc, at(src, "count", 0), "total").expect("rename ok");
        assert_eq!(edits.len(), 2, "declaration + one use rewritten");
        assert!(edits.iter().all(|e| e.new_text == "total"));
    }

    #[test]
    fn rename_rejects_keyword_and_junk() {
        let src = "component C {\n  state count = 0;\n}\n";
        let (map, id) = doc(src);
        let doc = map.get(id).unwrap();
        let pos = at(src, "count", 0);
        assert_eq!(rename(doc, pos, "state"), Err(RenameError::InvalidName));
        assert_eq!(rename(doc, pos, "a b"), Err(RenameError::InvalidName));
        assert_eq!(rename(doc, pos, ""), Err(RenameError::InvalidName));
        assert_eq!(rename(doc, pos, "1x"), Err(RenameError::InvalidName));
    }

    #[test]
    fn goto_from_declaration_name_resolves_to_itself() {
        // A cursor on the state declaration name (occurrence 0) is not on a recorded
        // use, but must still resolve — to the symbol's own declaration span.
        let src = "component C {\n  state count = 0;\n  computed d = count;\n}\n";
        let (map, id) = doc(src);
        let doc = map.get(id).unwrap();
        let loc = goto_definition(doc, at(src, "count", 0)).expect("goto from decl resolves");
        assert_eq!(
            loc.range.start().to_usize(),
            src.find("count").unwrap(),
            "goto from the declaration lands on the declaration"
        );
        // And references from the declaration site list both the use and the decl.
        let refs = find_references(doc, at(src, "count", 0), true);
        assert_eq!(refs.len(), 2, "declaration + one use from the decl site");
    }

    #[test]
    fn cursor_off_any_name_yields_nothing() {
        let src = "component C {\n  state count = 0;\n}\n";
        let (map, id) = doc(src);
        let doc = map.get(id).unwrap();
        // Offset 0 is on the `component` keyword, not a resolved name use.
        assert!(goto_definition(doc, TextSize::new(0)).is_none());
        assert!(find_references(doc, TextSize::new(0), true).is_empty());
        assert_eq!(
            rename(doc, TextSize::new(0), "x"),
            Err(RenameError::NotAName)
        );
    }

    #[test]
    fn valid_identifier_accepts_plain_and_raw() {
        assert!(is_valid_identifier("count"));
        assert!(is_valid_identifier("r#state"));
        assert!(!is_valid_identifier("state"));
        assert!(!is_valid_identifier("with space"));
        assert!(!is_valid_identifier(""));
    }
}
