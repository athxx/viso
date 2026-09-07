//! Warm-tier paint style for a node: paint-facing data read by the paint walk.
//!
//! [`BoxStyle`] is the paint-time description a leaf (or a container with a
//! background) lowers to a [`viso_render::Quad`]. It is a plain value copied
//! into the node's warm side-storage; it carries no behavior and no heap data,
//! so the paint walk reads flat structs.

use crate::state::{StateId, StateStore, StateValue};
use crate::token::{Theme, TokenId};
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
    /// Border stroke.
    pub border: Border,
}

impl BoxStyle {
    /// A transparent, borderless, sharp-cornered box — a pure layout container
    /// that paints nothing itself.
    pub const NONE: BoxStyle = BoxStyle {
        fill: Rgba::TRANSPARENT,
        radius: 0.0,
        border: Border::NONE,
    };

    /// A solid fill with no border and sharp corners.
    pub const fn solid(fill: Rgba) -> Self {
        BoxStyle {
            fill,
            radius: 0.0,
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

/// A node's *style-token binding*: which theme token (if any) feeds each
/// tokenizable field of its [`BoxStyle`]. The node keeps a `StyleId` alongside
/// its literal `BoxStyle`; resolving folds the tokens' current values onto that
/// literal, so a partially-tokenized style keeps its untokenized fields.
///
/// Only `fill` (a `color.*` token) and `radius` (a `radius.*`/scalar token) are
/// tokenizable this slice — that is all [`BoxStyle`] carries. `border` stays a
/// literal until a border token has a consumer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StyleId {
    /// Token feeding `fill`, if any.
    fill: Option<TokenId>,
    /// Token feeding `radius`, if any.
    radius: Option<TokenId>,
}

impl StyleId {
    /// A style with no token bindings — resolving it returns the base unchanged.
    pub const NONE: StyleId = StyleId {
        fill: None,
        radius: None,
    };

    /// A style whose `fill` is fed by `token`.
    pub const fn fill(token: TokenId) -> Self {
        StyleId {
            fill: Some(token),
            radius: None,
        }
    }

    /// This style with its `fill` fed by `token`.
    pub const fn with_fill(mut self, token: TokenId) -> Self {
        self.fill = Some(token);
        self
    }

    /// This style with its `radius` fed by `token`.
    pub const fn with_radius(mut self, token: TokenId) -> Self {
        self.radius = Some(token);
        self
    }

    /// Resolve this binding against `theme` + `states`, folding each present
    /// token's current value onto `base`. A `None` binding, an unresolved token,
    /// or a token of the wrong kind for its field leaves that base field as-is —
    /// a mismatch is ignored, never a panic.
    pub fn resolve(self, base: BoxStyle, theme: &Theme, states: &StateStore) -> BoxStyle {
        let mut out = base;
        if let Some(token) = self.fill
            && let Some(StateValue::Color(r, g, b, a)) = theme.resolve(token, states)
        {
            out.fill = Rgba { r, g, b, a };
        }
        if let Some(token) = self.radius {
            match theme.resolve(token, states) {
                Some(StateValue::Float(f)) => out.radius = f,
                Some(StateValue::Int(n)) => out.radius = n as f32,
                _ => {}
            }
        }
        out
    }

    /// The tokens this style references, for the caller to bind against the
    /// node. Allocation-free — a fixed walk over the two optional fields.
    pub fn tokens(self) -> impl Iterator<Item = TokenId> {
        [self.fill, self.radius].into_iter().flatten()
    }
}

/// A node's *interaction-state box selection*: which whole [`BoxStyle`] the node
/// paints given its current pressed / hover reactive cells.
///
/// This is orthogonal to [`StyleId`] token folding. Token folding derives *field
/// values* from theme tokens (`fill` from a `color.*` token); interaction
/// selection picks *one whole box* from a fixed set of variants based on a
/// pointer interaction state. A control keeps its resting / hover / pressed
/// boxes here alongside the cells that drive them; resolving reads the cells'
/// current values and returns the winning box, with priority `pressed > hover >
/// resting`. Theme-free (no [`Theme`] argument) and heap-free — every field is
/// `Copy`, so resolving allocates nothing.
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
    use crate::token::{Theme, TokenId, TokenInterner, TokenNamespace};
    use viso_render::Rgba;

    fn theme_with(
        namespace: TokenNamespace,
        name: &str,
        value: StateValue,
    ) -> (Theme, TokenId, StateStore) {
        let mut states = StateStore::new();
        let mut interner = TokenInterner::new();
        let mut theme = Theme::new();
        let token = interner.intern(namespace, name);
        let cell = states.alloc(value);
        theme.define(token, cell);
        (theme, token, states)
    }

    #[test]
    fn resolve_folds_a_color_token_into_the_fill() {
        let (theme, bg, states) = theme_with(
            TokenNamespace::Color,
            "bg",
            StateValue::Color(0.2, 0.4, 0.6, 1.0),
        );
        let style = StyleId::fill(bg);
        let resolved = style.resolve(BoxStyle::NONE, &theme, &states);
        assert_eq!(
            resolved.fill,
            Rgba {
                r: 0.2,
                g: 0.4,
                b: 0.6,
                a: 1.0
            }
        );
    }

    #[test]
    fn resolve_leaves_untokenized_fields_from_base() {
        // Only radius is tokenized; the base's fill/border survive.
        let (theme, r, states) = theme_with(TokenNamespace::Radius, "sm", StateValue::Float(4.0));
        let base = BoxStyle::solid(Rgba {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        });
        let resolved = StyleId::NONE.with_radius(r).resolve(base, &theme, &states);
        assert_eq!(resolved.fill, base.fill, "untokenized fill kept from base");
        assert_eq!(resolved.radius, 4.0, "radius token applied");
    }

    #[test]
    fn wrong_kind_token_is_ignored() {
        // A Bool token pointed at fill is a mismatch: base fill survives.
        let (theme, t, states) = theme_with(TokenNamespace::Color, "weird", StateValue::Bool(true));
        let base = BoxStyle::solid(Rgba {
            r: 0.9,
            g: 0.9,
            b: 0.9,
            a: 1.0,
        });
        let resolved = StyleId::fill(t).resolve(base, &theme, &states);
        assert_eq!(
            resolved.fill, base.fill,
            "type mismatch leaves base unchanged"
        );
    }

    #[test]
    fn tokens_lists_referenced_tokens() {
        let mut interner = TokenInterner::new();
        let bg = interner.intern(TokenNamespace::Color, "bg");
        let r = interner.intern(TokenNamespace::Radius, "sm");
        let style = StyleId::fill(bg).with_radius(r);
        let listed: Vec<TokenId> = style.tokens().collect();
        assert_eq!(listed.len(), 2);
        assert!(listed.contains(&bg) && listed.contains(&r));
        assert_eq!(StyleId::NONE.tokens().count(), 0);
    }

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
