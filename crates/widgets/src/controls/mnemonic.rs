//! Mnemonics: a label's access key, marked by the `&` before it — `&File`
//! answers to F, `Save &As…` to A — shown underlined while its cues show.
//! `&&` is an `&` of the text, as is one before a space or at the end; a
//! label with no mark has no access key.

use std::ops::Range;

use viso_ui::{SpanStyle, TextSpan};

use crate::{Label, label};

/// A label read for its mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Mnemonic {
    /// The label as it shows, without its marks.
    pub(crate) text: String,
    /// Where its access key sits in [`text`](Self::text), and the key,
    /// lowercase.
    pub(crate) key: Option<(Range<usize>, char)>,
}

impl Mnemonic {
    /// Read `label`'s mark.
    pub(crate) fn of(label: &str) -> Mnemonic {
        let mut text = String::with_capacity(label.len());
        let mut key = None;
        let mut chars = label.chars().peekable();
        while let Some(c) = chars.next() {
            if c != '&' {
                text.push(c);
                continue;
            }
            match chars.next() {
                Some('&') => text.push('&'),
                Some(k) if k.is_whitespace() => {
                    text.push('&');
                    text.push(k);
                }
                Some(k) => {
                    if key.is_none() {
                        let at = text.len();
                        key = Some((at..at + k.len_utf8(), k.to_lowercase().next().unwrap_or(k)));
                    }
                    text.push(k);
                }
                None => text.push('&'),
            }
        }
        Mnemonic { text, key }
    }

    /// Its access key.
    pub(crate) fn char(&self) -> Option<char> {
        self.key.as_ref().map(|(_, k)| *k)
    }

    /// The span that underlines its access key while the window's cues
    /// show — Alt held — or none.
    pub(crate) fn spans(&self) -> Vec<TextSpan> {
        self.key
            .iter()
            .map(|(range, _)| TextSpan {
                range: range.clone(),
                style: SpanStyle::default().cue(),
            })
            .collect()
    }

    /// A label of its text, its access key underlined while the cues show.
    pub(crate) fn label(&self) -> Label {
        self.spans()
            .into_iter()
            .fold(label(self.text.clone()), |out, span| {
                out.span(span.range, span.style)
            })
    }

    /// The spans that show its cue: its access key underlined, or none.
    pub(crate) fn cue(&self, shown: bool) -> Vec<TextSpan> {
        match &self.key {
            Some((range, _)) if shown => vec![TextSpan {
                range: range.clone(),
                style: SpanStyle {
                    underline: true,
                    ..SpanStyle::default()
                },
            }],
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mark_names_the_key_and_leaves_the_text() {
        let m = Mnemonic::of("&File");
        assert_eq!(m.text, "File");
        assert_eq!(m.key, Some((0..1, 'f')));
        let m = Mnemonic::of("Save &As…");
        assert_eq!(m.text, "Save As…");
        assert_eq!(m.char(), Some('a'));
        assert_eq!(m.cue(true)[0].range, 5..6);
        assert!(m.cue(false).is_empty());
    }

    #[test]
    fn a_doubled_mark_is_text_and_a_plain_label_has_no_key() {
        assert_eq!(
            Mnemonic::of("Fish && &Chips"),
            Mnemonic {
                text: "Fish & Chips".to_owned(),
                key: Some((7..8, 'c')),
            }
        );
        assert_eq!(Mnemonic::of("Plain").key, None);
        assert_eq!(Mnemonic::of("End &").text, "End &");
        assert_eq!(Mnemonic::of("Fish & Chips").text, "Fish & Chips");
        assert_eq!(Mnemonic::of("Fish & Chips").key, None);
        let spans = Mnemonic::of("&Open").spans();
        assert_eq!(spans[0].range, 0..1);
        assert!(spans[0].style.cue && !spans[0].style.underline);
        assert_eq!(Mnemonic::of("&Über").key, Some((0..2, 'ü')));
    }
}
