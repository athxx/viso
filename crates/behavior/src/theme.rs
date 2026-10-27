//! The value of the prelude's `Theme` record a view reads when no theme is
//! set.

use alloc::boxed::Box;
use alloc::rc::Rc;

use crate::value::{Aggregate, Value};

/// The theme a store publishes before one is set: a light palette over the
/// schema's default scales.
#[cfg(feature = "std")]
pub fn default_theme() -> Value {
    std::thread_local! {
        static DEFAULT: Value = build(LIGHT);
    }
    DEFAULT.with(Value::clone)
}

/// The theme a store publishes before one is set: a light palette over the
/// schema's default scales. Without `std` there is no thread-local to cache
/// it in, so each call builds it.
#[cfg(not(feature = "std"))]
pub fn default_theme() -> Value {
    build(LIGHT)
}

/// The default theme's dark counterpart: a dark palette over the same
/// scales, which a window follows while the system is dark and the app set
/// no theme.
#[cfg(feature = "std")]
pub fn default_dark_theme() -> Value {
    std::thread_local! {
        static DARK_THEME: Value = build(DARK);
    }
    DARK_THEME.with(Value::clone)
}

/// The default dark theme, built anew without `std`.
#[cfg(not(feature = "std"))]
pub fn default_dark_theme() -> Value {
    build(DARK)
}

/// The default theme's colors, sRGB `0xRRGGBBAA` in the order of the
/// prelude's `ColorPalette`.
pub const LIGHT: [u32; 14] = [
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
];

/// The default dark theme's colors, in the same order.
pub const DARK: [u32; 14] = [
    0x1a1c_21ff, // background
    0xe6e8_ebff, // foreground
    0x2a2d_33ff, // surface
    0xe6e8_ebff, // on_surface
    0x82b3_edff, // primary
    0xfcfc_ffff, // on_primary
    0x91bf_f6ff, // primary_hover
    0xb391_ffff, // accent
    0x9aa1_acff, // muted
    0x454a_53ff, // outline
    0xff6b_6bff, // error
    0x1b1f_24ff, // on_error
    0x82b3_ed99, // focus_ring
    0x0000_0099, // scrim
];

fn build(palette: [u32; 14]) -> Value {
    let colors = record(palette.map(|rgba: u32| Value::Int(i64::from(rgba))));
    // `FontFamily::system_ui`, by tag.
    let typography = record([
        Value::Float(14.0),
        Value::Float(0.85),
        Value::Float(1.25),
        Value::Float(1.6),
        Value::Int(0),
    ]);
    let spacing = record([2.0, 4.0, 8.0, 16.0, 24.0].map(Value::Float));
    let radius = record([4.0, 8.0, 12.0].map(Value::Float));
    // A `Shadow` falling `y` below its box: `x`, `y`, `blur`, `spread`,
    // `color` and `inset`.
    let shadow = |y: f64, blur: f64, alpha: u32| {
        record([
            Value::Float(0.0),
            Value::Float(y),
            Value::Float(blur),
            Value::Float(0.0),
            Value::Int(i64::from(alpha)),
            Value::Int(0),
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
