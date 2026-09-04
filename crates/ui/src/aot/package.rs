//! The compact release UI package format (architecture section 41; AGENTS 21.6,
//! 60).
//!
//! A release build lowers the same shared frontend IR that Slice N mounts through
//! builder tokens and Slice O commits into a live tree — but here into a *compact,
//! dependency-free byte blob* that an app instantiates at startup with **no DSL
//! compiler present**. That is the load-bearing exit criterion: the release path
//! carries none of the compiler's developer metadata.
//!
//! So the package deliberately holds **only** what the runtime needs to rebuild the
//! retained tree and its reactive bindings:
//!
//! - a pre-order node table — each node's builder [`AotNodeKind`] plus a folded
//!   [`AotStyle`] of layout scalars — where a node's pre-order index is its stable
//!   identity, exactly the numbering the binding edges reference (the same
//!   `NodeKey` convention Slice O's `commit.rs` walks);
//! - a binding edge table `(StateKey, node index, DirtyClass bits)`.
//!
//! What it does **not** carry is every form of developer-only metadata the section
//! 60 rule says must be strippable: no property-name strings, no type names, no
//! source spans, no symbol string table. A binding needs only the durable
//! [`StateKey`] identity and the [`DirtyClass`](crate::dirty::DirtyClass) it
//! invalidates — `commit.rs` already
//! proves the runtime never consults a property name — so the name is dropped.
//!
//! The wire form is framed by the shared [`viso_ende::ProtocolTag`] header (magic +
//! version) and encoded through [`viso_ende`], Viso's own dependency-free codec. The
//! decoder is *bounded*: a truncated, corrupt-magic, wrong-version, or otherwise
//! malformed blob returns a [`DecodeError`] and never panics — the safety precondition
//! for loading an untrusted asset blob (section 30: a bad layout is a diagnosable
//! error, never a silent miscompile or a panic).

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag};

use crate::state::StateKey;

/// A compact, self-describing release UI package: the retained node tree and its
/// reactive binding edges, stripped of all developer metadata.
///
/// Nodes are in pre-order; a node's index in [`nodes`](Self::nodes) is its stable
/// identity, and [`AotEdge::node`] references that index. Container membership is
/// carried by each node's [`AotNode::child_count`], so a single forward pass over the
/// table reconstructs the tree without back-references.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AotPackage {
    /// The pre-order node table. `nodes[i]` has pre-order identity `i`.
    pub nodes: Vec<AotNode>,
    /// The reactive binding edges, each wiring a durable state cell to a node.
    pub edges: Vec<AotEdge>,
}

/// One node in the package: which builder call it maps to, its folded layout style,
/// and how many immediate children follow it in the pre-order table.
#[derive(Debug, Clone, PartialEq)]
pub struct AotNode {
    /// The builder call this node instantiates through (`flex`/`grid`/`scroll`/`leaf`).
    pub kind: AotNodeKind,
    /// The folded compile-time-constant layout scalars.
    pub style: AotStyle,
    /// The number of immediate children, whose subtrees follow this node in
    /// pre-order. A leaf has `0`. The loader reconstructs ancestry from this count
    /// in one forward pass, so no parent/sibling links are stored.
    pub child_count: u32,
}

/// Which `viso_ui::BuildCx` builder call a packaged node instantiates through.
///
/// This is a UI-side enum, **not** imported from the DSL compiler — the release path
/// must not reference any `viso-dsl` type. It mirrors the builder-selection semantics
/// of `commit.rs::build_node`: a virtualized list has no static child region to pack
/// (its body is a control-flow region rejected before packaging), so it collapses to
/// a leaf here, exactly as the live-commit twin treats it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AotNodeKind {
    /// A flex container → `cx.flex`.
    Flex = 0,
    /// A grid container → `cx.grid`.
    Grid = 1,
    /// A scroll container → `cx.scroll`.
    Scroll = 2,
    /// A leaf primitive → `cx.leaf`.
    Leaf = 3,
}

impl AotNodeKind {
    /// The wire tag for this kind.
    #[inline]
    fn as_u8(self) -> u8 {
        self as u8
    }

    /// The kind for a wire tag, or `None` for an unknown tag (a corrupt blob).
    #[inline]
    fn from_u8(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(AotNodeKind::Flex),
            1 => Some(AotNodeKind::Grid),
            2 => Some(AotNodeKind::Scroll),
            3 => Some(AotNodeKind::Leaf),
            _ => None,
        }
    }

    /// Whether this kind hosts a child region the loader descends into.
    #[inline]
    pub fn is_container(self) -> bool {
        matches!(
            self,
            AotNodeKind::Flex | AotNodeKind::Grid | AotNodeKind::Scroll
        )
    }
}

/// The folded layout scalars a packaged node carries, mirroring the DSL `StyleIr`
/// semantics but in a UI-side compact form (the release path imports no DSL type).
///
/// Each optional dimension is a folded [`AotLength`]; `None` means the node authored
/// no value and takes the runtime builder default — the same "unset ⇒ default"
/// meaning `commit.rs`'s `flex_style`/`scroll_style`/`leaf_style` apply.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AotStyle {
    /// The container arrangement axis, when the node fixed one.
    pub axis: Option<AotAxis>,
    /// The folded width, when a constant property set one.
    pub width: Option<AotLength>,
    /// The folded height, when a constant property set one.
    pub height: Option<AotLength>,
    /// The folded gap between children, when a constant property set one.
    pub gap: Option<f32>,
}

/// A folded layout axis, the compact twin of `viso_ui::layout::Axis`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AotAxis {
    /// Children flow left-to-right.
    Row = 0,
    /// Children flow top-to-bottom.
    Column = 1,
}

/// A folded length, the compact twin of `viso_ui::layout::Length`. `Fit` needs no
/// payload; `Fixed`/`Fill` each carry one `f32`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AotLength {
    /// A hard pixel length.
    Fixed(f32),
    /// A weighted share of leftover main-axis space.
    Fill { weight: f32 },
    /// Sized to the measured natural size.
    Fit,
}

/// One reactive binding edge: a durable state cell, the node it drives, and the
/// invalidation classes a write to it marks.
///
/// The state cell is named by its durable [`StateKey`] identity — the layout-twin of
/// the compiler's `SymbolId` — so the loader binds by identity without any DSL type.
/// The `class` is the raw [`DirtyClass`](crate::dirty::DirtyClass) bit set; the
/// loader rebuilds a `DirtyClass`
/// from named constants rather than reinterpreting the byte, so a future layout
/// divergence is a compile error, not a silent miscompile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AotEdge {
    /// The durable identity of the reactive source cell.
    pub state: StateKey,
    /// The pre-order index of the node this edge drives.
    pub node: u32,
    /// The raw [`DirtyClass`](crate::dirty::DirtyClass) bit set a write to the
    /// source marks on `node`.
    pub class: u8,
}

// The style field is written as a small presence bitmask followed by the payloads of
// the fields that are present, so an all-default style costs a single byte. The bits
// are private to this format: emitter and loader are the same source, so no external
// contract depends on them.
const STYLE_HAS_AXIS: u8 = 1 << 0;
const STYLE_HAS_WIDTH: u8 = 1 << 1;
const STYLE_HAS_HEIGHT: u8 = 1 << 2;
const STYLE_HAS_GAP: u8 = 1 << 3;

// A length is one discriminant byte, then the payload for the variants that carry one.
const LEN_FIXED: u8 = 0;
const LEN_FILL: u8 = 1;
const LEN_FIT: u8 = 2;

impl Encode for AotLength {
    fn encode(&self, enc: &mut Encoder) {
        match self {
            AotLength::Fixed(px) => {
                enc.write_u8(LEN_FIXED);
                enc.write_f32(*px);
            }
            AotLength::Fill { weight } => {
                enc.write_u8(LEN_FILL);
                enc.write_f32(*weight);
            }
            AotLength::Fit => enc.write_u8(LEN_FIT),
        }
    }
}

impl Decode for AotLength {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let offset = dec.position();
        match dec.read_u8()? {
            LEN_FIXED => Ok(AotLength::Fixed(dec.read_f32()?)),
            LEN_FILL => Ok(AotLength::Fill {
                weight: dec.read_f32()?,
            }),
            LEN_FIT => Ok(AotLength::Fit),
            _ => Err(DecodeError::Malformed { offset }),
        }
    }
}

impl Encode for AotStyle {
    fn encode(&self, enc: &mut Encoder) {
        let mut mask = 0u8;
        if self.axis.is_some() {
            mask |= STYLE_HAS_AXIS;
        }
        if self.width.is_some() {
            mask |= STYLE_HAS_WIDTH;
        }
        if self.height.is_some() {
            mask |= STYLE_HAS_HEIGHT;
        }
        if self.gap.is_some() {
            mask |= STYLE_HAS_GAP;
        }
        enc.write_u8(mask);
        if let Some(axis) = self.axis {
            enc.write_u8(match axis {
                AotAxis::Row => 0,
                AotAxis::Column => 1,
            });
        }
        if let Some(width) = self.width {
            width.encode(enc);
        }
        if let Some(height) = self.height {
            height.encode(enc);
        }
        if let Some(gap) = self.gap {
            enc.write_f32(gap);
        }
    }
}

impl Decode for AotStyle {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let mask = dec.read_u8()?;
        let axis = if mask & STYLE_HAS_AXIS != 0 {
            let offset = dec.position();
            Some(match dec.read_u8()? {
                0 => AotAxis::Row,
                1 => AotAxis::Column,
                _ => return Err(DecodeError::Malformed { offset }),
            })
        } else {
            None
        };
        let width = if mask & STYLE_HAS_WIDTH != 0 {
            Some(AotLength::decode(dec)?)
        } else {
            None
        };
        let height = if mask & STYLE_HAS_HEIGHT != 0 {
            Some(AotLength::decode(dec)?)
        } else {
            None
        };
        let gap = if mask & STYLE_HAS_GAP != 0 {
            Some(dec.read_f32()?)
        } else {
            None
        };
        Ok(AotStyle {
            axis,
            width,
            height,
            gap,
        })
    }
}

impl Encode for AotNode {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u8(self.kind.as_u8());
        self.style.encode(enc);
        enc.write_varint(self.child_count as u64);
    }
}

impl Decode for AotNode {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let kind_offset = dec.position();
        let kind = AotNodeKind::from_u8(dec.read_u8()?).ok_or(DecodeError::Malformed {
            offset: kind_offset,
        })?;
        let style = AotStyle::decode(dec)?;
        let count_offset = dec.position();
        let child_count =
            u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed {
                offset: count_offset,
            })?;
        Ok(AotNode {
            kind,
            style,
            child_count,
        })
    }
}

impl Encode for AotEdge {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_u64(self.state.hi);
        enc.write_u64(self.state.lo);
        enc.write_varint(self.node as u64);
        enc.write_u8(self.class);
    }
}

impl Decode for AotEdge {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let hi = dec.read_u64()?;
        let lo = dec.read_u64()?;
        let node_offset = dec.position();
        let node = u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed {
            offset: node_offset,
        })?;
        let class = dec.read_u8()?;
        Ok(AotEdge {
            state: StateKey::from_parts(hi, lo),
            node,
            class,
        })
    }
}

impl Encode for AotPackage {
    fn encode(&self, enc: &mut Encoder) {
        // The shared framing header (magic + wire version): reused verbatim, not
        // hand-rolled, so every Viso blob carries the same self-describing prefix.
        ProtocolTag::current().encode(enc);
        enc.write_varint(self.nodes.len() as u64);
        for node in &self.nodes {
            node.encode(enc);
        }
        enc.write_varint(self.edges.len() as u64);
        for edge in &self.edges {
            edge.encode(enc);
        }
    }
}

impl Decode for AotPackage {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        // Reject a bad magic or an incompatible wire version before reading any
        // payload — a malformed asset blob is a diagnosable error, never a panic.
        let tag_offset = dec.position();
        let tag = ProtocolTag::decode(dec)?;
        if !tag.is_compatible() {
            return Err(DecodeError::Malformed { offset: tag_offset });
        }
        let node_count = dec.read_varint()?;
        let mut nodes = Vec::with_capacity(bounded_capacity(node_count));
        for _ in 0..node_count {
            nodes.push(AotNode::decode(dec)?);
        }
        let edge_count = dec.read_varint()?;
        let mut edges = Vec::with_capacity(bounded_capacity(edge_count));
        for _ in 0..edge_count {
            edges.push(AotEdge::decode(dec)?);
        }
        Ok(AotPackage { nodes, edges })
    }
}

/// A pre-allocation hint that never trusts a length prefix past a sane ceiling: a
/// corrupt blob claiming a huge count must not reserve gigabytes before the decode
/// fails on the truncated body. The real bound is the input length — each element
/// costs at least one byte — so this only caps the *initial* reservation; the `Vec`
/// still grows to the true count if the bytes are actually there.
#[inline]
fn bounded_capacity(count: u64) -> usize {
    const MAX_PREALLOC: u64 = 4096;
    count.min(MAX_PREALLOC) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dirty::DirtyClass;
    use viso_ende::{Decode, Encode};

    fn sample_package() -> AotPackage {
        AotPackage {
            nodes: vec![
                AotNode {
                    kind: AotNodeKind::Flex,
                    style: AotStyle {
                        axis: Some(AotAxis::Column),
                        width: Some(AotLength::Fixed(320.0)),
                        height: Some(AotLength::Fill { weight: 2.0 }),
                        gap: Some(8.0),
                    },
                    child_count: 2,
                },
                AotNode {
                    kind: AotNodeKind::Scroll,
                    style: AotStyle {
                        axis: Some(AotAxis::Row),
                        width: None,
                        height: Some(AotLength::Fit),
                        gap: None,
                    },
                    child_count: 0,
                },
                AotNode {
                    kind: AotNodeKind::Leaf,
                    style: AotStyle::default(),
                    child_count: 0,
                },
            ],
            edges: vec![
                AotEdge {
                    state: StateKey::from_parts(0x1122_3344_5566_7788, 0x99aa_bbcc_ddee_ff00),
                    node: 2,
                    class: DirtyClass::MEASURE.bits()
                        | DirtyClass::LAYOUT.bits()
                        | DirtyClass::PAINT.bits(),
                },
                AotEdge {
                    state: StateKey::from_parts(1, 2),
                    node: 1,
                    class: DirtyClass::PAINT.bits(),
                },
            ],
        }
    }

    #[test]
    fn round_trips_every_field() {
        let pkg = sample_package();
        let bytes = pkg.encode_to_vec();
        let back = AotPackage::decode_from_slice(&bytes).expect("well-formed blob decodes");
        assert_eq!(pkg, back, "every field survives the round trip");
    }

    #[test]
    fn empty_package_round_trips() {
        let pkg = AotPackage::default();
        let bytes = pkg.encode_to_vec();
        let back = AotPackage::decode_from_slice(&bytes).expect("empty blob decodes");
        assert_eq!(pkg, back);
    }

    #[test]
    fn truncated_blob_is_an_error_not_a_panic() {
        let bytes = sample_package().encode_to_vec();
        // Every proper prefix must fail cleanly: the bounded decoder either runs out
        // of input or rejects the trailing-bytes check, but it never panics.
        for cut in 0..bytes.len() {
            let res = AotPackage::decode_from_slice(&bytes[..cut]);
            assert!(res.is_err(), "truncation at {cut} must be a DecodeError");
        }
    }

    #[test]
    fn corrupt_magic_is_rejected() {
        let mut bytes = sample_package().encode_to_vec();
        bytes[0] ^= 0xff; // clobber the first magic byte
        let res = AotPackage::decode_from_slice(&bytes);
        assert!(res.is_err(), "a bad magic must be rejected, not panic");
    }

    #[test]
    fn wrong_version_is_rejected() {
        let mut bytes = sample_package().encode_to_vec();
        // The version follows the two magic bytes; bump it past any compatible value.
        bytes[2] = bytes[2].wrapping_add(0x7f);
        bytes[3] = bytes[3].wrapping_add(0x7f);
        let res = AotPackage::decode_from_slice(&bytes);
        assert!(
            res.is_err(),
            "an incompatible wire version must be rejected"
        );
    }

    #[test]
    fn unknown_node_kind_tag_is_rejected() {
        // Hand-build a one-node blob and corrupt the node-kind tag.
        let pkg = AotPackage {
            nodes: vec![AotNode {
                kind: AotNodeKind::Leaf,
                style: AotStyle::default(),
                child_count: 0,
            }],
            edges: Vec::new(),
        };
        let mut bytes = pkg.encode_to_vec();
        // Header is MAGIC(2) + version(2) + node_count varint(1 byte for `1`), then
        // the node-kind tag. Flip it to an out-of-range value.
        let kind_offset = 2 + 2 + 1;
        bytes[kind_offset] = 0xfe;
        let res = AotPackage::decode_from_slice(&bytes);
        assert!(res.is_err(), "an unknown node kind must be rejected");
    }
}
