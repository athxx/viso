//! The `theme` a view reads: a value of the prelude's `Theme` record, which
//! the state store publishes as the `theme` field of its environment. Setting
//! it raises that field's cell, so exactly the bindings that read `theme`
//! re-evaluate. A store without one publishes [`default_theme`].

use std::rc::Rc;

use viso_behavior::{Aggregate, Value};
use viso_ui::StateStore;
use viso_ui::adaptive::{AdaptiveEnv, ThemeValue};

/// Makes `theme`, a value of the prelude's `Theme` record (such as
/// [`ViewHost::theme`](crate::ViewHost::theme) evaluates), the theme every
/// view over `states` reads.
pub fn set_theme(states: &mut StateStore, theme: Value) {
    states.set_theme(ThemeValue(Rc::new(theme)));
}

/// The theme `env` publishes.
pub(crate) fn current_theme(env: &AdaptiveEnv) -> Value {
    env.theme()
        .and_then(|theme| theme.0.downcast_ref::<Value>())
        .cloned()
        .unwrap_or_else(default_theme)
}

/// The theme a store publishes before one is set: a light palette over the
/// schema's default scales.
pub fn default_theme() -> Value {
    thread_local! {
        static DEFAULT: Value = build_default();
    }
    DEFAULT.with(Value::clone)
}

fn build_default() -> Value {
    let colors = record(
        [
            0xffff_ffff, // background
            0x1b1f_24ff, // foreground
            0xf5f6_f8ff, // surface
            0x1b1f_24ff, // on_surface
            0x3b6c_f0ff, // primary
            0xffff_ffff, // on_primary
            0x2f5c_d6ff, // primary_hover
            0x8a4f_ffff, // accent
            0x6b72_80ff, // muted
            0xd0d5_ddff, // outline
            0xd93a_3aff, // error
            0xffff_ffff, // on_error
            0x3b6c_f099, // focus_ring
            0x0000_0066, // scrim
        ]
        .map(|rgba: u32| Value::Int(i64::from(rgba))),
    );
    let typography = record([14.0, 0.85, 1.25, 1.6].map(Value::Float));
    let spacing = record([2.0, 4.0, 8.0, 16.0, 24.0].map(Value::Float));
    let radius = record([4.0, 8.0, 12.0].map(Value::Float));
    let shadow = |offset_y: f64, blur: f64, alpha: u32| {
        record([
            Value::Float(offset_y),
            Value::Float(blur),
            Value::Int(i64::from(alpha)),
        ])
    };
    let elevation = record([
        shadow(1.0, 2.0, 0x0000_0024),
        shadow(2.0, 6.0, 0x0000_002e),
        shadow(6.0, 16.0, 0x0000_0038),
    ]);
    // `Easing::ease_out` and `Easing::ease_in_out`, by tag.
    let motion = record([
        Value::Float(0.1),
        Value::Float(0.2),
        Value::Float(0.35),
        Value::Int(2),
        Value::Int(3),
    ]);
    record([colors, typography, spacing, radius, elevation, motion])
}

fn record<const N: usize>(fields: [Value; N]) -> Value {
    Value::Agg(Rc::new(Aggregate {
        tag: 0,
        fields: Box::new(fields),
    }))
}
