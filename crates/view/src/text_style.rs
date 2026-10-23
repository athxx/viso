//! The style a text node's entries give, read from their behavior values: a
//! `FontWeight`, `FontStyle`, `TextOverflow` or `TextAlign` is its variant's
//! tag, a `FontFamily` its tag or, named, a record of the name tagged by it,
//! an absent `Option` is nil, and a `Span` is a record of its fields in
//! order.

use std::num::NonZeroU32;
use std::sync::Arc;

use viso_behavior::Value;
use viso_ui::{
    FontFamily, FontSlant, FontWeight, SpanStyle, TextAlign, TextOverflow, TextSpan, TextStyle,
};

use crate::layout_values::length;

/// The weights of `FontWeight`'s variants, in their order.
const WEIGHTS: [u16; 7] = [100, 300, 400, 500, 600, 700, 900];

fn tag(value: &Value) -> Option<usize> {
    value.as_int().and_then(|v| usize::try_from(v).ok())
}

/// A `FontWeight`, `None` for a value that is none.
fn weight(value: &Value) -> Option<FontWeight> {
    tag(value)
        .and_then(|at| WEIGHTS.get(at))
        .map(|&w| FontWeight(w))
}

/// A `FontStyle`, `None` for a value that is none.
fn slant(value: &Value) -> Option<FontSlant> {
    tag(value).map(|at| match at {
        1 => FontSlant::Italic,
        _ => FontSlant::Normal,
    })
}

/// The style `font_weight`, `font_style`, `font_family`, `line_height`,
/// `max_lines`, `overflow` and `align` give. A value of the wrong shape,
/// which only a compiler bug makes, reads the property's default.
pub(crate) fn text_style(
    [
        weight_of,
        slant_of,
        family,
        line_height,
        max_lines,
        overflow,
        align,
    ]: [&Value; 7],
) -> TextStyle {
    TextStyle {
        slant: slant(slant_of).unwrap_or_default(),
        weight: weight(weight_of).unwrap_or(FontWeight::REGULAR),
        family: font_family(family),
        line_height: length(line_height),
        max_lines: max_lines
            .as_int()
            .and_then(|v| u32::try_from(v).ok())
            .map(|v| NonZeroU32::new(v).unwrap_or(NonZeroU32::MIN)),
        overflow: match tag(overflow) {
            Some(1) => TextOverflow::Ellipsis,
            _ => TextOverflow::Clip,
        },
        align: match tag(align) {
            Some(1) => TextAlign::Center,
            Some(2) => TextAlign::End,
            _ => TextAlign::Start,
        },
    }
}

/// The text a `List<Span>` reads as, its pieces joined, and how each piece
/// draws; `None` for no pieces, when the node's `text` shows.
pub(crate) fn spans(value: &Value) -> Option<(String, Vec<TextSpan>)> {
    let Value::List(pieces) = value else {
        return None;
    };
    if pieces.is_empty() {
        return None;
    }
    let mut text = String::new();
    let mut spans = Vec::new();
    for piece in pieces.iter() {
        let Value::Agg(span) = piece else { continue };
        let [
            shown,
            color,
            background,
            weight_of,
            slant_of,
            family,
            size,
            raise,
            underline,
            strikethrough,
            link,
        ] = &span.fields[..]
        else {
            continue;
        };
        let start = text.len();
        text.push_str(shown.as_str().unwrap_or_default());
        let paint = |value: &Value| value.as_int().map(|_| crate::values::color(value));
        let style = SpanStyle {
            family: font_family(family),
            size: length(size),
            weight: weight(weight_of),
            slant: slant(slant_of),
            color: paint(color),
            background: paint(background),
            underline: underline.as_int().is_some_and(|v| v != 0) || link.as_str().is_some(),
            strikethrough: strikethrough.as_int().is_some_and(|v| v != 0),
            raise: length(raise),
            link: link.as_str().map(Arc::from),
            cue: false,
        };
        if style != SpanStyle::default() && start < text.len() {
            spans.push(TextSpan {
                range: start..text.len(),
                style,
            });
        }
    }
    Some((text, spans))
}

/// An `Option<FontFamily>`, `Option::None` the theme's family.
fn font_family(value: &Value) -> Option<FontFamily> {
    match value {
        Value::Nil => None,
        value => Some(family(value)),
    }
}

/// A `FontFamily`; a value of the wrong shape the interface face.
pub(crate) fn family(value: &Value) -> FontFamily {
    match value {
        Value::Int(1) => FontFamily::Serif,
        Value::Int(2) => FontFamily::Monospace,
        Value::Agg(agg) => match &agg.fields[..] {
            [name] => name.as_str().map_or(FontFamily::SystemUi, |name| {
                FontFamily::Named(Arc::from(name))
            }),
            _ => FontFamily::SystemUi,
        },
        _ => FontFamily::SystemUi,
    }
}
