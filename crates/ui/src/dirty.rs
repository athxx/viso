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

    /// The raw bitset byte.
    ///
    /// This is the stable wire form of a class set: the release AOT package
    /// (architecture section 41) stores a binding's invalidation classes as this
    /// byte and rebuilds them with [`from_bits`](Self::from_bits) at load time. The
    /// bit positions are the `1 << n` constants above; they are the format's own
    /// contract, so the emitter and loader read and write the same byte directly.
    #[inline]
    pub fn bits(self) -> u8 {
        self.0
    }

    /// A class set from a raw bitset byte, keeping only defined bits.
    ///
    /// The inverse of [`bits`](Self::bits): any bit not backing a named class is
    /// dropped, so a corrupt or forward-versioned byte can never smuggle in an
    /// undefined class. This is what the AOT loader uses to reconstruct a binding's
    /// classes without reinterpreting an untrusted byte wholesale.
    #[inline]
    pub fn from_bits(bits: u8) -> Self {
        const DEFINED: u8 = DirtyClass::STRUCTURE.0
            | DirtyClass::STYLE.0
            | DirtyClass::MEASURE.0
            | DirtyClass::LAYOUT.0
            | DirtyClass::TRANSFORM.0
            | DirtyClass::PAINT.0
            | DirtyClass::HIT_TEST.0
            | DirtyClass::SEMANTICS.0;
        Self(bits & DEFINED)
    }

    /// The set bits paired with their names, low bit first, for introspection.
    ///
    /// This is the readable form the debug surfaces render — a NodeId's pending
    /// invalidation shown as `["MEASURE", "LAYOUT"]` rather than a raw byte, and
    /// the same list a JSON snapshot or the Studio transport serializes. Names
    /// are the constant identifiers above; a cleared set yields an empty iterator.
    pub fn iter_names(self) -> impl Iterator<Item = &'static str> {
        const NAMES: [(u8, &str); 8] = [
            (DirtyClass::STRUCTURE.0, "STRUCTURE"),
            (DirtyClass::STYLE.0, "STYLE"),
            (DirtyClass::MEASURE.0, "MEASURE"),
            (DirtyClass::LAYOUT.0, "LAYOUT"),
            (DirtyClass::TRANSFORM.0, "TRANSFORM"),
            (DirtyClass::PAINT.0, "PAINT"),
            (DirtyClass::HIT_TEST.0, "HIT_TEST"),
            (DirtyClass::SEMANTICS.0, "SEMANTICS"),
        ];
        let bits = self.0;
        NAMES
            .into_iter()
            .filter_map(move |(bit, name)| (bits & bit != 0).then_some(name))
    }
}

impl core::fmt::Display for DirtyClass {
    /// Renders the set classes as `MEASURE | LAYOUT`, or `EMPTY` when none are
    /// set — the one-line form a dirty-reason readout prints.
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if self.is_empty() {
            return f.write_str("EMPTY");
        }
        let mut first = true;
        for name in self.iter_names() {
            if !first {
                f.write_str(" | ")?;
            }
            f.write_str(name)?;
            first = false;
        }
        Ok(())
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

    #[test]
    fn each_class_renders_its_own_name() {
        for (class, name) in [
            (DirtyClass::STRUCTURE, "STRUCTURE"),
            (DirtyClass::STYLE, "STYLE"),
            (DirtyClass::MEASURE, "MEASURE"),
            (DirtyClass::LAYOUT, "LAYOUT"),
            (DirtyClass::TRANSFORM, "TRANSFORM"),
            (DirtyClass::PAINT, "PAINT"),
            (DirtyClass::HIT_TEST, "HIT_TEST"),
            (DirtyClass::SEMANTICS, "SEMANTICS"),
        ] {
            let names: Vec<_> = class.iter_names().collect();
            assert_eq!(names, [name], "one class renders exactly its own name");
            assert_eq!(class.to_string(), name);
        }
    }

    #[test]
    fn names_are_low_bit_first_and_joined() {
        // Composed out of order; the readout is always low-bit-first.
        let d = DirtyClass::PAINT | DirtyClass::MEASURE | DirtyClass::LAYOUT;
        let names: Vec<_> = d.iter_names().collect();
        assert_eq!(names, ["MEASURE", "LAYOUT", "PAINT"]);
        assert_eq!(d.to_string(), "MEASURE | LAYOUT | PAINT");
    }

    #[test]
    fn empty_has_no_names() {
        assert_eq!(DirtyClass::EMPTY.iter_names().count(), 0);
        assert_eq!(DirtyClass::EMPTY.to_string(), "EMPTY");
    }

    #[test]
    fn names_round_trip_through_the_wire_byte() {
        // The readable surface reflects exactly the bits from_bits keeps, so a
        // reconstructed AOT class set renders the same names it was written with.
        let d = DirtyClass::STRUCTURE | DirtyClass::SEMANTICS;
        let back = DirtyClass::from_bits(d.bits());
        assert_eq!(
            d.iter_names().collect::<Vec<_>>(),
            back.iter_names().collect::<Vec<_>>()
        );
    }
}
