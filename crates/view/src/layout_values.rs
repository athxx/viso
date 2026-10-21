//! The layout values a node's entries give, read from their behavior values:
//! a `MixedLength` is a record of its `dp`, `px`, `sp`, `em` and `%` terms, a
//! single-unit length having widened to one where the compiler typed it; an
//! enum's unit variant is its tag, a payload variant a record tagged by it.

use viso_behavior::{Aggregate, Value};
use viso_ui::grid::{AdaptiveColumns, AutoRepeat, TrackMax, TrackSizing};
use viso_ui::{
    Align, AlignItems, Alignment2D, Axis, BoundSize, EdgeTerms, Justify, LayoutValue, LengthEnv,
    LengthTerms,
};

use crate::control::LayoutSlot;

/// The value of `slot` that `value` gives, a track that reads the
/// environment folding against `env` at `font` dp per em; `Err` with why for
/// a value layout does not take yet.
pub(crate) fn layout_value(
    slot: LayoutSlot,
    value: &Value,
    env: &LengthEnv,
    font: f32,
) -> Result<LayoutValue, String> {
    let malformed = || malformed(slot.variant());
    let terms = |value: &Value| length(value).ok_or_else(malformed);
    let optional = |value: &Value| match value {
        Value::Nil => Ok(None),
        value => terms(value).map(Some),
    };
    let index = |value: &Value| {
        value
            .as_int()
            .and_then(|v| u16::try_from(v).ok())
            .ok_or_else(malformed)
    };
    let tag = |value: &Value| variant(value).map(|(tag, _)| tag).ok_or_else(malformed);
    Ok(match slot {
        LayoutSlot::Width => LayoutValue::Width(size(value)?),
        LayoutSlot::Height => LayoutValue::Height(size(value)?),
        LayoutSlot::MinWidth => LayoutValue::MinWidth(terms(value)?),
        LayoutSlot::MaxWidth => LayoutValue::MaxWidth(optional(value)?),
        LayoutSlot::MinHeight => LayoutValue::MinHeight(terms(value)?),
        LayoutSlot::MaxHeight => LayoutValue::MaxHeight(optional(value)?),
        LayoutSlot::Margin => LayoutValue::Margin(edges(value).ok_or_else(malformed)?),
        LayoutSlot::Padding => LayoutValue::Padding(edges(value).ok_or_else(malformed)?),
        LayoutSlot::Gap => LayoutValue::Gap(terms(value)?),
        LayoutSlot::ColumnGap => LayoutValue::ColumnGap(terms(value)?),
        LayoutSlot::RowGap => LayoutValue::RowGap(terms(value)?),
        LayoutSlot::FontSize => LayoutValue::FontSize(terms(value)?),
        LayoutSlot::Axis => LayoutValue::Axis(match tag(value)? {
            0 => Axis::Row,
            _ => Axis::Column,
        }),
        LayoutSlot::Justify => LayoutValue::Justify(match tag(value)? {
            0 => Justify::Start,
            1 => Justify::Center,
            _ => Justify::End,
        }),
        LayoutSlot::Align => LayoutValue::Align(match tag(value)? {
            0 => Align::Start,
            1 => Align::Center,
            2 => Align::End,
            _ => Align::Stretch,
        }),
        LayoutSlot::Columns => LayoutValue::Columns(tracks(value, env, font)?),
        LayoutSlot::Rows => LayoutValue::Rows(tracks(value, env, font)?),
        LayoutSlot::AutoRows => LayoutValue::AutoRows(track(value, env, font)?),
        LayoutSlot::AlignItems => LayoutValue::AlignItems(match tag(value)? {
            0 => AlignItems::Stretch,
            1 => AlignItems::Start,
            2 => AlignItems::Center,
            _ => AlignItems::End,
        }),
        LayoutSlot::AdaptiveColumns => LayoutValue::AdaptiveColumns(match value {
            Value::Nil => None,
            value => Some(adaptive(value, env, font)?),
        }),
        LayoutSlot::ContentAlign => {
            LayoutValue::ContentAlign(alignment(value).ok_or_else(malformed)?)
        }
        LayoutSlot::GridColumn => LayoutValue::GridColumn(match value {
            Value::Nil => None,
            value => Some(index(value)?),
        }),
        LayoutSlot::GridRow => LayoutValue::GridRow(match value {
            Value::Nil => None,
            value => Some(index(value)?),
        }),
        LayoutSlot::GridColumnSpan => LayoutValue::GridColumnSpan(span(value, "grid.column_span")?),
        LayoutSlot::GridRowSpan => LayoutValue::GridRowSpan(span(value, "grid.row_span")?),
        LayoutSlot::StackAlign => LayoutValue::StackAlign(match value {
            Value::Nil => None,
            value => Some(alignment(value).ok_or_else(malformed)?),
        }),
        LayoutSlot::StackLayer => LayoutValue::StackLayer(
            value
                .as_int()
                .and_then(|v| i32::try_from(v).ok())
                .ok_or_else(malformed)?,
        ),
        LayoutSlot::AbsoluteTop => LayoutValue::AbsoluteTop(optional(value)?),
        LayoutSlot::AbsoluteEnd => LayoutValue::AbsoluteEnd(optional(value)?),
        LayoutSlot::AbsoluteBottom => LayoutValue::AbsoluteBottom(optional(value)?),
        LayoutSlot::AbsoluteStart => LayoutValue::AbsoluteStart(optional(value)?),
    })
}

/// An enum value's variant tag and payload.
fn variant(value: &Value) -> Option<(i64, &[Value])> {
    match value {
        Value::Int(tag) => Some((*tag, &[])),
        Value::Agg(agg) => Some((i64::from(agg.tag), &agg.fields)),
        _ => None,
    }
}

/// A record's fields.
fn fields(value: &Value) -> Option<&[Value]> {
    match value {
        Value::Agg(agg) => {
            let Aggregate { fields, .. } = &**agg;
            Some(fields)
        }
        _ => None,
    }
}

/// A `MixedLength`'s terms; its `%` term a fraction of the basis.
pub(crate) fn length(value: &Value) -> Option<LengthTerms> {
    let [dp, px, sp, em, pct] = fields(value)? else {
        return None;
    };
    let term = |v: &Value| v.as_float().map(|v| v as f32);
    Some(LengthTerms {
        dp: term(dp)?,
        px: term(px)?,
        sp: term(sp)?,
        em: term(em)?,
        pct: term(pct)? / 100.0,
    })
}

/// An `EdgeInsets`'s edges.
fn edges(value: &Value) -> Option<EdgeTerms> {
    let [top, end, bottom, start] = fields(value)? else {
        return None;
    };
    Some(EdgeTerms {
        top: length(top)?,
        end: length(end)?,
        bottom: length(bottom)?,
        start: length(start)?,
    })
}

/// A `Sizing`.
fn size(value: &Value) -> Result<BoundSize, String> {
    let malformed = || malformed("Sizing");
    let (tag, payload) = variant(value).ok_or_else(malformed)?;
    Ok(match (tag, payload) {
        (0, [terms]) => BoundSize::Fixed(length(terms).ok_or_else(malformed)?),
        (1, [weight]) => BoundSize::Fill {
            weight: weight.as_float().ok_or_else(malformed)? as f32,
        },
        (2, []) => BoundSize::Fit,
        (3, []) => BoundSize::MinContent,
        (4, []) => BoundSize::MaxContent,
        _ => return Err(malformed()),
    })
}

/// An `Alignment2D`.
fn alignment(value: &Value) -> Option<Alignment2D> {
    let tag = usize::try_from(value.as_int()?).ok()?;
    Alignment2D::ALL.get(tag).copied()
}

/// A span of at least one track.
fn span(value: &Value, name: &str) -> Result<u16, String> {
    match value.as_int().and_then(|v| u16::try_from(v).ok()) {
        Some(0) => Err(format!("a `{name}` spans at least one track")),
        Some(span) => Ok(span),
        None => Err(malformed(name)),
    }
}

/// Why a value of `what` is not one: the compiler typed it, so only a bug
/// reaches this.
fn malformed(what: &str) -> String {
    format!("internal: a `{what}` value of the wrong shape")
}

/// A list of `Track`s.
fn tracks(value: &Value, env: &LengthEnv, font: f32) -> Result<Vec<TrackSizing>, String> {
    let Value::List(list) = value else {
        return Err(malformed("List<Track>"));
    };
    list.iter().map(|t| track(t, env, font)).collect()
}

/// A `Track`: a fixed length of plain `dp` is a fixed track, of plain `%` a
/// percentage of the grid's content box, and of any other its terms, which
/// the node store folds against the environment as it moves.
fn track(value: &Value, env: &LengthEnv, font: f32) -> Result<TrackSizing, String> {
    let (tag, payload) = variant(value).ok_or_else(|| malformed("Track"))?;
    Ok(match (tag, payload) {
        (0, [len]) => {
            let terms = length(len).ok_or_else(|| malformed("Track"))?;
            let plain = LengthTerms {
                dp: 0.0,
                pct: 0.0,
                ..terms
            } == LengthTerms::ZERO;
            match (plain, terms.dp != 0.0, terms.pct != 0.0) {
                (true, _, false) => TrackSizing::Fixed(terms.dp.max(0.0)),
                (true, false, true) => TrackSizing::Percent(terms.pct.max(0.0)),
                _ => TrackSizing::Length(terms),
            }
        }
        (1, [fr]) => TrackSizing::Fr(share(fr)?),
        (2, []) => TrackSizing::Auto,
        (3, [min, max]) => {
            let min = dp(min, env, font)?;
            match track_max(max, env, font)? {
                TrackMax::Px(max) => TrackSizing::Minmax(min, max),
                TrackMax::Fr(fr) => TrackSizing::FlexMin(min, fr),
            }
        }
        (4, [limit]) => TrackSizing::FitContent(dp(limit, env, font)?),
        _ => return Err(malformed("Track")),
    })
}

/// A `TrackMax`.
fn track_max(value: &Value, env: &LengthEnv, font: f32) -> Result<TrackMax, String> {
    match variant(value).ok_or_else(|| malformed("TrackMax"))? {
        (0, [len]) => Ok(TrackMax::Px(dp(len, env, font)?)),
        (1, [fr]) => Ok(TrackMax::Fr(share(fr)?)),
        _ => Err(malformed("TrackMax")),
    }
}

/// An `AdaptiveColumns`: its `mode`, `min` and `max`.
fn adaptive(value: &Value, env: &LengthEnv, font: f32) -> Result<AdaptiveColumns, String> {
    let [mode, min, max] = fields(value).ok_or_else(|| malformed("AdaptiveColumns"))? else {
        return Err(malformed("AdaptiveColumns"));
    };
    Ok(AdaptiveColumns {
        mode: match variant(mode).ok_or_else(|| malformed("AutoRepeat"))?.0 {
            0 => AutoRepeat::Fill,
            _ => AutoRepeat::Fit,
        },
        min: dp(min, env, font)?,
        max: track_max(max, env, font)?,
    })
}

/// A track length folded to non-negative dp against `env`; one with a `%`
/// term has no extent before the grid's.
fn dp(value: &Value, env: &LengthEnv, font: f32) -> Result<f32, String> {
    let terms = length(value).ok_or_else(|| malformed("MixedLength"))?;
    if terms.pct != 0.0 {
        return Err(
            "a track bound with a `%` term does not lay out yet; a `dp`, `px`, `sp` or `em` one does"
                .to_string(),
        );
    }
    terms
        .fixed(env, font)
        .map(|v| v.max(0.0))
        .ok_or_else(|| "a track length is not finite".to_string())
}

/// A non-negative `fr` share.
fn share(value: &Value) -> Result<f32, String> {
    value
        .as_float()
        .map(|v| (v as f32).max(0.0))
        .ok_or_else(|| malformed("F32"))
}
