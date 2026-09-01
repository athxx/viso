//! Dirty invalidation classes.
//!
//! Every property/state binding must declare what it invalidates, using these
//! explicit classes rather than a single coarse `dirty = true`. Propagation
//! must stop at valid boundaries — a paint-only change must not make ancestors
//! layout-dirty.

use core::ops::{BitAnd, BitOr, BitOrAssign};

/// A set of invalidation classes, stored as a bitset.
///
/// Examples of what bindings map to:
/// - text content → MEASURE | LAYOUT | PAINT | SEMANTICS
/// - text color   → PAINT
/// - width        → MEASURE | LAYOUT
/// - transform    → TRANSFORM | HIT_TEST | PAINT
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DirtyClass(u8);

impl DirtyClass {
    pub const EMPTY: Self = Self(0);
    pub const STRUCTURE: Self = Self(1 << 0);
    pub const STYLE: Self = Self(1 << 1);
    pub const MEASURE: Self = Self(1 << 2);
    pub const LAYOUT: Self = Self(1 << 3);
    pub const TRANSFORM: Self = Self(1 << 4);
    pub const PAINT: Self = Self(1 << 5);
    pub const HIT_TEST: Self = Self(1 << 6);
    pub const SEMANTICS: Self = Self(1 << 7);

    /// The classes that propagate up the parent chain when a node is marked.
    /// STRUCTURE, MEASURE, and SEMANTICS bubble; the rest stay local — most
    /// importantly PAINT, so a paint-only change never dirties ancestor layout.
    pub const BUBBLING: Self = Self(Self::STRUCTURE.0 | Self::MEASURE.0 | Self::SEMANTICS.0);

    /// Whether every class in `other` is set.
    #[inline]
    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether any class in `other` is also set here.
    #[inline]
    pub fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    #[inline]
    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

impl BitAnd for DirtyClass {
    type Output = Self;
    #[inline]
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitOr for DirtyClass {
    type Output = Self;
    #[inline]
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for DirtyClass {
    #[inline]
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

#[cfg(test)]
mod tests {
    use super::DirtyClass;

    #[test]
    fn compose_and_query() {
        let d = DirtyClass::MEASURE | DirtyClass::LAYOUT | DirtyClass::PAINT;
        assert!(d.contains(DirtyClass::LAYOUT));
        assert!(!d.contains(DirtyClass::TRANSFORM));
        assert!(!d.is_empty());
        assert!(DirtyClass::EMPTY.is_empty());
    }
}
