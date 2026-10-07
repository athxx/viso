//! Nearest-name suggestions for an unresolved name (section 105.2): the declared
//! names within a small edit distance, ranked closest first, attached to the
//! `E2001` diagnostic as related spans and `maybe-incorrect` fixes (section 138).
//!
//! Cold path: this runs only when a name already failed to resolve.

use crate::diag::{Applicability, Diagnostic, Fix, Related, TextEdit};
use crate::syntax::TextRange;

/// At most this many suggestions are offered.
const MAX_SUGGESTIONS: usize = 3;

/// One suggestion candidate: its spelling and, when known, where its declaration
/// name is.
pub(crate) struct Candidate<'a> {
    pub(crate) name: &'a str,
    /// The declaring module's `::`-joined path, or `None` for the diagnostic's own
    /// file, and the span of the declaration name.
    pub(crate) declared_at: Option<(Option<&'a str>, TextRange)>,
}

/// The optimal string alignment distance between `a` and `b`, over Unicode
/// scalar values: insertions, deletions, substitutions and transpositions of
/// two adjacent characters each count as one edit, so the common typing slip
/// `cuont` is one edit from `count`.
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    // Three rows: two back, one back, and the one being filled.
    let mut before: Vec<usize> = vec![0; b.len() + 1];
    let mut prior: Vec<usize> = (0..=b.len()).collect();
    let mut row: Vec<usize> = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        row[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (prior[j - 1] + cost).min(prior[j] + 1).min(row[j - 1] + 1);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(before[j - 2] + 1);
            }
            row[j] = best;
        }
        std::mem::swap(&mut before, &mut prior);
        std::mem::swap(&mut prior, &mut row);
    }
    prior[b.len()]
}

/// The candidates close enough to `target` to be a plausible misspelling, closest
/// first and ties broken by spelling so the order is deterministic.
///
/// "Close enough" is an edit distance of at most a third of the target's length,
/// and never less than one edit, so `Badg` suggests `Badge` and `Row` does not
/// suggest `Box`.
pub(crate) fn nearest<'a>(
    target: &str,
    candidates: impl IntoIterator<Item = Candidate<'a>>,
) -> Vec<Candidate<'a>> {
    let limit = (target.chars().count() / 3).max(1);
    let mut ranked: Vec<(usize, Candidate<'a>)> = candidates
        .into_iter()
        .filter(|c| !c.name.is_empty() && c.name != target)
        .map(|c| (edit_distance(target, c.name), c))
        .filter(|&(distance, _)| distance <= limit)
        .collect();
    ranked.sort_by(|(da, a), (db, b)| da.cmp(db).then_with(|| a.name.cmp(b.name)));
    ranked.dedup_by(|(_, a), (_, b)| a.name == b.name);
    ranked.truncate(MAX_SUGGESTIONS);
    ranked.into_iter().map(|(_, c)| c).collect()
}

/// Attaches `suggestions` to `diagnostic`: a related span per locally declared
/// candidate, and one `maybe-incorrect` fix per candidate replacing `at`.
pub(crate) fn attach(diagnostic: &mut Diagnostic, at: TextRange, suggestions: &[Candidate<'_>]) {
    for s in suggestions {
        if let Some((module, range)) = s.declared_at {
            diagnostic.related.push(Related {
                module: module.map(str::to_owned),
                range,
                label: format!("`{}` is declared here", s.name),
            });
        }
        diagnostic.fixes.push(Fix {
            title: format!("replace with `{}`", s.name),
            applicability: Applicability::MaybeIncorrect,
            edits: vec![TextEdit::new(at, s.name)],
        });
    }
    if !suggestions.is_empty() {
        let names: Vec<String> = suggestions
            .iter()
            .map(|s| format!("`{}`", s.name))
            .collect();
        diagnostic
            .notes
            .push(format!("did you mean {}?", names.join(" or ")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> Candidate<'_> {
        Candidate {
            name,
            declared_at: None,
        }
    }

    #[test]
    fn edit_distance_counts_single_character_edits() {
        assert_eq!(edit_distance("Badge", "Badge"), 0);
        assert_eq!(edit_distance("Badg", "Badge"), 1);
        assert_eq!(edit_distance("Bagde", "Badge"), 1, "a transposition");
        assert_eq!(edit_distance("cuont", "count"), 1);
        assert_eq!(edit_distance("ab", "ba"), 1);
        assert_eq!(
            edit_distance("abc", "ca"),
            3,
            "no edit of a transposed pair"
        );
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("café", "cafe"), 1);
    }

    #[test]
    fn nearest_ranks_by_distance_then_spelling_and_drops_the_far() {
        let got: Vec<_> = nearest(
            "Buttn",
            ["Button", "Buttons", "Label", "Buton", "Bottom"].map(named),
        )
        .into_iter()
        .map(|c| c.name)
        .collect();
        assert_eq!(got, ["Buton", "Button"]);
    }

    #[test]
    fn nearest_offers_at_most_three() {
        let got = nearest("Ab", ["Aa", "Ac", "Ad", "Ae"].map(named));
        assert_eq!(got.len(), MAX_SUGGESTIONS);
    }
}
