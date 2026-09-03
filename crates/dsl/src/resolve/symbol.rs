//! Durable symbol identity — the id that leaves the compiler.
//!
//! A [`SymbolId`] is the 128-bit fingerprint of a declaration's *canonical
//! identity*, not of its source text or its position in a file. Two builds of the
//! same declaration — reordered, reformatted, moved within a file, recompiled on a
//! different machine — mint the same `SymbolId`; an unrelated declaration, or the
//! same name in a different module, mints a different one. This is what lets hot
//! reload (Slice O) and AOT artifacts (Slice P) match a new build's symbol against
//! an old one without a name table.
//!
//! The fingerprint is a fixed, versioned **FNV-1a-128** we own outright: a constant
//! offset basis and prime, folded byte-by-byte into two `u64` lanes. FNV over a hash
//! with more throughput is deliberate — identity needs determinism and low collision
//! on a cold path, not speed, and a self-contained algorithm with pinned constants
//! is the one we can guarantee never shifts under us. There is no [`std`] hasher
//! here on purpose: `DefaultHasher` is unspecified and seedable, so it could never
//! back a durable id.
//!
//! Canonical fingerprint inputs (doc section 10.4.3): package identity, module path,
//! declaration kind, and canonical declaration path — each length-prefixed so no two
//! distinct input tuples can collide by concatenation. The [`FINGERPRINT_VERSION`]
//! tag rides along in artifact metadata; bumping it (or any constant below) is an
//! identity break, which is why [`tests`] pins a known-answer vector.

/// The FNV-1a-128 offset basis (the standard constant), as two `u64` lanes
/// `(hi, lo)` of the 128-bit value `0x6c62272e07bb0142_62b821756295c58d`.
const OFFSET_BASIS_HI: u64 = 0x6c62_272e_07bb_0142;
const OFFSET_BASIS_LO: u64 = 0x62b8_2175_6295_c58d;

/// The FNV-1a-128 prime (the standard constant) `0x0000000001000000_000000000000013B`,
/// as two `u64` lanes `(hi, lo)`.
const PRIME_HI: u64 = 0x0000_0000_0100_0000;
const PRIME_LO: u64 = 0x0000_0000_0000_013b;

/// The fingerprint algorithm version, recorded in artifact metadata. A change to the
/// algorithm or to any constant above must bump this — a differing version means two
/// artifacts' `SymbolId`s are not comparable.
pub const FINGERPRINT_VERSION: u32 = 1;

/// A durable, position-independent 128-bit symbol identity.
///
/// `SymbolId` supports only equality, ordering, and hashing — it is an *identity*,
/// never a number, so no arithmetic is exposed. `#[repr(C)]` fixes the lane layout
/// for artifact serialization.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolId {
    /// The high 64 bits of the fingerprint.
    pub hi: u64,
    /// The low 64 bits of the fingerprint.
    pub lo: u64,
}

impl SymbolId {
    /// Builds a `SymbolId` from already-fingerprinted lanes (deserialization, tests).
    #[inline]
    pub const fn from_parts(hi: u64, lo: u64) -> Self {
        Self { hi, lo }
    }
}

/// The declaration kind a `SymbolId` is minted for, mixed into the fingerprint so a
/// `record R` and an `enum R` in the same module never share an id.
///
/// The discriminant values are part of the fingerprint contract: changing one shifts
/// every id of that kind, so they are pinned like the hash constants (bumping
/// [`FINGERPRINT_VERSION`] when they change).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SymbolKind {
    Component = 1,
    System = 2,
    Record = 3,
    Enum = 4,
    EnumVariant = 5,
    Function = 6,
    Action = 7,
    Task = 8,
    Input = 9,
    State = 10,
    Computed = 11,
    Event = 12,
    Const = 13,
    TypeAlias = 14,
}

/// An in-progress FNV-1a-128 fold across two `u64` lanes.
///
/// The 128-bit multiply-by-prime is done with `u128` intermediates and truncated
/// back to 128 bits, matching the reference FNV-1a-128 exactly.
struct Fnv1a128 {
    hi: u64,
    lo: u64,
}

impl Fnv1a128 {
    #[inline]
    fn new() -> Self {
        Self {
            hi: OFFSET_BASIS_HI,
            lo: OFFSET_BASIS_LO,
        }
    }

    /// The current state as a single 128-bit value.
    #[inline]
    fn state(&self) -> u128 {
        ((self.hi as u128) << 64) | (self.lo as u128)
    }

    #[inline]
    fn set(&mut self, value: u128) {
        self.hi = (value >> 64) as u64;
        self.lo = value as u64;
    }

    /// FNV-1a step: `hash = (hash XOR byte) * prime`, all modulo 2^128.
    #[inline]
    fn write_byte(&mut self, byte: u8) {
        let mut hash = self.state();
        hash ^= byte as u128;
        let prime = ((PRIME_HI as u128) << 64) | (PRIME_LO as u128);
        hash = hash.wrapping_mul(prime);
        self.set(hash);
    }

    /// Folds a length-prefixed byte chunk so distinct input tuples cannot collide by
    /// concatenation (`["ab","c"]` and `["a","bc"]` fingerprint differently).
    #[inline]
    fn write_chunk(&mut self, bytes: &[u8]) {
        for b in (bytes.len() as u64).to_le_bytes() {
            self.write_byte(b);
        }
        for &b in bytes {
            self.write_byte(b);
        }
    }

    #[inline]
    fn finish(self) -> SymbolId {
        SymbolId {
            hi: self.hi,
            lo: self.lo,
        }
    }
}

/// The canonical identity of a declaration, fingerprinted into a [`SymbolId`].
///
/// `package` is the package identity string; `module_path` is the `::`-joined module
/// path; `decl_path` is the canonical dotted path to the declaration within its
/// module (e.g. `Counter.bump` for an action on a component). These are the four
/// inputs doc section 10.4.3 fixes as canonical — nothing position- or text-derived.
#[derive(Debug, Clone, Copy)]
pub struct SymbolIdentity<'a> {
    pub package: &'a str,
    pub module_path: &'a str,
    pub kind: SymbolKind,
    pub decl_path: &'a str,
}

/// Fingerprints a canonical [`SymbolIdentity`] into a [`SymbolId`].
///
/// Deterministic across runs, machines, and source reorderings: the same identity
/// tuple always yields the same id; any differing field yields a different id. This
/// is the sole way a `SymbolId` is minted from source.
pub fn fingerprint(identity: SymbolIdentity<'_>) -> SymbolId {
    let mut hash = Fnv1a128::new();
    // Version-tag the stream so a future algorithm bump can't alias an old id.
    hash.write_chunk(&FINGERPRINT_VERSION.to_le_bytes());
    hash.write_chunk(identity.package.as_bytes());
    hash.write_chunk(identity.module_path.as_bytes());
    hash.write_byte(identity.kind as u8);
    hash.write_chunk(identity.decl_path.as_bytes());
    hash.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity<'a>(module: &'a str, kind: SymbolKind, decl: &'a str) -> SymbolIdentity<'a> {
        SymbolIdentity {
            package: "app",
            module_path: module,
            kind,
            decl_path: decl,
        }
    }

    #[test]
    fn fingerprint_is_deterministic_across_calls() {
        let a = fingerprint(identity("ui::screens", SymbolKind::Component, "Counter"));
        let b = fingerprint(identity("ui::screens", SymbolKind::Component, "Counter"));
        assert_eq!(a, b, "same identity fingerprints identically");
    }

    #[test]
    fn a_different_module_path_mints_a_different_id() {
        let a = fingerprint(identity("ui::screens", SymbolKind::Component, "Counter"));
        let b = fingerprint(identity("ui::widgets", SymbolKind::Component, "Counter"));
        assert_ne!(a, b, "module path is part of identity");
    }

    #[test]
    fn a_different_kind_mints_a_different_id() {
        let r = fingerprint(identity("m", SymbolKind::Record, "R"));
        let e = fingerprint(identity("m", SymbolKind::Enum, "R"));
        assert_ne!(r, e, "declaration kind disambiguates same-named decls");
    }

    #[test]
    fn length_prefixing_prevents_concatenation_collisions() {
        // module="ab" decl="c" vs module="a" decl="bc": equal only if the joiner
        // didn't length-prefix. They must differ.
        let x = fingerprint(identity("ab", SymbolKind::Component, "c"));
        let y = fingerprint(identity("a", SymbolKind::Component, "bc"));
        assert_ne!(x, y, "length-prefixed chunks can't alias by concatenation");
    }

    #[test]
    fn known_answer_vector_pins_the_constants() {
        // Pins the algorithm + constants + input framing. If a refactor changes the
        // hash, the discriminants, the version tag, or the chunk framing, this
        // breaks — which is the point: SymbolId identity must never shift silently.
        let id = fingerprint(SymbolIdentity {
            package: "app",
            module_path: "ui::screens",
            kind: SymbolKind::Component,
            decl_path: "Counter",
        });
        assert_eq!(
            id,
            SymbolId::from_parts(KNOWN_ANSWER_HI, KNOWN_ANSWER_LO),
            "known-answer fingerprint drifted — SymbolId identity would break"
        );
    }

    // Frozen from the initial fingerprint of the identity above; the algorithm,
    // constants, discriminants, version tag, and chunk framing are all pinned
    // against these. Regenerated only on a deliberate, FINGERPRINT_VERSION-bumping
    // identity break.
    const KNOWN_ANSWER_HI: u64 = 0x4619_9c27_7ca6_cb44;
    const KNOWN_ANSWER_LO: u64 = 0x424b_a4fd_26f1_5314;
}
