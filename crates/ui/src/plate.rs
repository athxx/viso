//! What a plate shows: a badge's count, an avatar's initials and the color
//! a name gives its avatar.
//!
//! A plate is a node whose caption sits in the middle of the pill or circle
//! its background draws ([`CheckShape::Plate`](crate::CheckShape::Plate)); the
//! badge and avatar widgets of both the Rust and the view libraries make
//! their captions and fills here, so the two show the same.

use viso_render::{Rgba, Srgb};

pub use viso_schema::plate::{avatar_rgb, badge_count, initials};

/// A badge's fill when nothing tints it: the error red, `#b3261e`.
pub const BADGE_FILL: Rgba = Rgba {
    r: 0.4508,
    g: 0.0194,
    b: 0.013,
    a: 1.0,
};

/// A badge's caption over its fill.
pub const BADGE_INK: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// The fill an avatar of `name` takes: the same for the same name, spread
/// over a set deep enough under white initials.
pub fn avatar_fill(name: &str) -> Rgba {
    Srgb::from_rgba32((avatar_rgb(name) << 8) | 0xff).into_linear_straight()
}
