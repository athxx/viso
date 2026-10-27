//! Warm-tier paint style for a node: paint-facing data read by the paint walk.
//!
//! [`BoxStyle`] is the paint-time description a leaf (or a container with a
//! background) lowers to a [`viso_render::Quad`]. It is a plain value copied
//! into the node's warm side-storage; it carries no behavior and no heap data,
//! so the paint walk reads flat structs.

use crate::state::{StateId, StateStore, StateValue};
use viso_render::{Border, Rgba};

/// The visual fill of a box: background color, corner radius, and border.
///
/// A `fill.a == 0` box paints nothing (a bare layout container). This mirrors
/// the renderer's [`viso_render::Quad`] fields so [`crate::paint::paint_tree`]
/// can lower a node with one field copy and no translation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoxStyle {
    /// Fill color; transparent means "no background".
    pub fill: Rgba,
    /// Corner radius in pixels (0 = sharp).
    pub radius: f32,
    /// More corner radius, as a share of the shorter side of the box the node
    /// paints: `0.5` rounds a pill. Usually `0`.
    pub radius_pct: f32,
    /// Border stroke.
    pub border: Border,
}

impl BoxStyle {
    /// A transparent, borderless, sharp-cornered box — a pure layout container
    /// that paints nothing itself.
    pub const NONE: BoxStyle = BoxStyle {
        fill: Rgba::TRANSPARENT,
        radius: 0.0,
        radius_pct: 0.0,
        border: Border::NONE,
    };

    /// A solid fill with no border and sharp corners.
    pub const fn solid(fill: Rgba) -> Self {
        BoxStyle {
            fill,
            radius: 0.0,
            radius_pct: 0.0,
            border: Border::NONE,
        }
    }

    /// This style with a corner radius applied.
    pub const fn with_radius(mut self, radius: f32) -> Self {
        self.radius = radius;
        self
    }

    /// Whether this box contributes any pixels (a visible fill or a border).
    #[inline]
    pub fn is_visible(&self) -> bool {
        self.fill.a > 0.0 || self.border.width > 0.0
    }
}

impl Default for BoxStyle {
    fn default() -> Self {
        BoxStyle::NONE
    }
}

/// A node's *interaction-state box selection*: which whole [`BoxStyle`] the node
/// paints given its current pressed / hover reactive cells.
///
/// It picks *one whole box* from a fixed set of variants by pointer
/// interaction state. A control keeps its resting / hover / pressed boxes here
/// alongside the cells that drive them; resolving reads the cells' current
/// values and returns the winning box, with priority `pressed > hover >
/// resting`. Heap-free — every field is `Copy`, so resolving allocates nothing.
///
/// A `None` cell means "this node has no such state" (a hover-only control
/// leaves `pressed_cell` `None`); a cell that reads anything but
/// [`StateValue::Bool`]`(true)` counts as inactive, so a missing or wrong-kind
/// cell never selects its variant.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InteractionStyle {
    /// The box painted when neither pressed nor hovered.
    pub resting: BoxStyle,
    /// The box painted while hovered (and not pressed).
    pub hover: BoxStyle,
    /// The box painted while pressed (wins over hover).
    pub pressed: BoxStyle,
    /// The cell whose `Bool(true)` selects `pressed`; `None` = no pressed state.
    pub pressed_cell: Option<StateId>,
    /// The cell whose `Bool(true)` selects `hover`; `None` = no hover state.
    pub hover_cell: Option<StateId>,
}

impl InteractionStyle {
    /// Select the box to paint from the current cell values, priority
    /// `pressed > hover > resting`. A `None` cell or a non-`Bool(true)` value
    /// counts as inactive. Theme-free and allocation-free.
    pub fn resolve(&self, states: &StateStore) -> BoxStyle {
        if is_active(self.pressed_cell, states) {
            self.pressed
        } else if is_active(self.hover_cell, states) {
            self.hover
        } else {
            self.resting
        }
    }
}

/// Whether `cell` is present and currently holds `Bool(true)`.
#[inline]
fn is_active(cell: Option<StateId>, states: &StateStore) -> bool {
    matches!(
        cell.and_then(|id| states.get(id)),
        Some(StateValue::Bool(true))
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{StateStore, StateValue};
    use viso_render::Rgba;

    /// Three distinct boxes so a resolve result names the selected variant.
    fn variants() -> (BoxStyle, BoxStyle, BoxStyle) {
        let rest = BoxStyle::solid(Rgba {
            r: 0.2,
            g: 0.4,
            b: 0.8,
            a: 1.0,
        });
        let hov = BoxStyle::solid(Rgba {
            r: 0.3,
            g: 0.5,
            b: 0.9,
            a: 1.0,
        });
        let pressed = BoxStyle::solid(Rgba {
            r: 0.1,
            g: 0.3,
            b: 0.6,
            a: 1.0,
        });
        (rest, hov, pressed)
    }

    /// The full priority table: pressed beats hover beats resting, a missing
    /// cell is inactive, and a non-`Bool(true)` value never selects its variant.
    #[test]
    fn interaction_resolve_priority_table() {
        let (rest, hov, pressed) = variants();
        let mut states = StateStore::new();
        let pc = states.alloc(StateValue::Bool(false));
        let hc = states.alloc(StateValue::Bool(false));
        let style = InteractionStyle {
            resting: rest,
            hover: hov,
            pressed,
            pressed_cell: Some(pc),
            hover_cell: Some(hc),
        };

        // Neither active → resting.
        assert_eq!(style.resolve(&states), rest);

        // Hover only → hover.
        states.set(hc, StateValue::Bool(true));
        assert_eq!(style.resolve(&states), hov);

        // Pressed wins over hover.
        states.set(pc, StateValue::Bool(true));
        assert_eq!(style.resolve(&states), pressed);

        // Pressed only (hover cleared) → still pressed.
        states.set(hc, StateValue::Bool(false));
        assert_eq!(style.resolve(&states), pressed);
    }

    /// A `None` cell is inactive: a hover-only style (no pressed cell) never
    /// selects the pressed box, and a wrong-kind value counts as inactive.
    #[test]
    fn interaction_resolve_none_and_wrong_kind_are_inactive() {
        let (rest, hov, pressed) = variants();
        let mut states = StateStore::new();
        // Wrong kind (Int, not Bool) in the hover cell → inactive.
        let hc = states.alloc(StateValue::Int(1));
        let style = InteractionStyle {
            resting: rest,
            hover: hov,
            pressed,
            pressed_cell: None,
            hover_cell: Some(hc),
        };
        assert_eq!(
            style.resolve(&states),
            rest,
            "None pressed cell + non-bool hover cell → resting"
        );
    }
}
