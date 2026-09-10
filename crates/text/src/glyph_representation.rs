//! Adaptive glyph representation: the Coverage / MTSDF / Vector / Color retained
//! state machine and its promotion hysteresis.
//!
//! A glyph is not assigned a fixed representation lane. It starts as exact A8
//! coverage — the best answer for small, CJK, and editor text — and is promoted
//! by observed on-screen behavior (Temporal Promotion): sustained scale or
//! rotation promotes to a scalable multi-channel distance field, extreme
//! sustained zoom to a retained vector outline, and a color source resolves to
//! a color representation by what the face provides. Promotion is hysteretic so
//! transient motion does not thrash representations, and any representation can
//! fall back to exact coverage under residency pressure because coverage is
//! always correct.

/// The five glyph image representations. Which one a glyph resolves to is a
/// runtime decision (see [`RepresentationState`]), not a per-crate lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlyphImageKind {
    /// Exact single-channel coverage. The default; best for small / CJK /
    /// editor text and the always-correct fallback.
    MaskA8,
    /// Scalable multi-channel signed-distance field (sharp corners plus a true
    /// distance channel). Used for glyphs promoted under sustained transform.
    ScalableMtsdf,
    /// Retained vector outline for extreme scale / high precision; the steady
    /// state does not re-tessellate it every frame.
    OutlineVector,
    /// Premultiplied RGBA bitmap strike (for example sbix / CBDT color emoji).
    ColorRgba8,
    /// Vector color glyph (for example COLR / SVG).
    ColorVector,
}

/// The retained promotion state a glyph carries between frames: its current
/// representation, the resolution bucket in use, and the hysteresis window that
/// decides when to promote or fall back. This never recomputes per glyph per
/// frame.
#[derive(Debug, Default)]
pub struct RepresentationState {
    // TODO(TF-P3): current kind, active resolution bucket, transform-quality
    // window, and pending-promotion accounting.
}

impl RepresentationState {
    /// Resolve the representation to use this frame given the observed on-screen
    /// transform, without mutating any residency pool. Returns the kind and the
    /// resolution bucket the caller should look up or request.
    pub fn resolve(&mut self) -> GlyphImageKind {
        todo!("TF-P3: Temporal Promotion decision with hysteresis")
    }
}
