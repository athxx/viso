//! The style a text node's entries give, read from their behavior values: a
//! `FontWeight`, `TextOverflow` or `TextAlign` is its variant's tag, a
//! `FontFamily` its tag or, named, a record of the name tagged by it, and an
//! absent `Option` is nil.

use std::num::NonZeroU32;
use std::sync::Arc;

use viso_behavior::Value;
use viso_ui::{FontFamily, FontWeight, LengthEnv, TextAlign, TextOverflow, TextStyle};

use crate::layout_values::length;

/// The weights of `FontWeight`'s variants, in their order.
const WEIGHTS: [u16; 7] = [100, 300, 400, 500, 600, 700, 900];

/// The style `font_weight`, `font_family`, `line_height`, `max_lines`,
/// `overflow` and `align` give, a line height folding against `env` and the
/// text's own `font` size. A value of the wrong shape, which only a compiler
/// bug makes, reads the property's default.
pub(crate) fn text_style(
    [weight, family, line_height, max_lines, overflow, align]: [&Value; 6],
    env: &LengthEnv,
    font: f32,
) -> TextStyle {
    let tag = |value: &Value| value.as_int().and_then(|v| usize::try_from(v).ok());
    TextStyle {
        weight: tag(weight)
            .and_then(|at| WEIGHTS.get(at))
            .map_or(FontWeight::REGULAR, |&w| FontWeight(w)),
        family: font_family(family),
        line_height: length(line_height).and_then(|terms| terms.fold_font(env, font)),
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

/// A `FontFamily`, `Option::None` the interface face.
fn font_family(value: &Value) -> FontFamily {
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
