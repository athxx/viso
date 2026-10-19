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
//! - a pre-order node table — each node's builder [`AotNodeKind`], a folded
//!   [`AotStyle`] of layout scalars and the [`NodeLengths`] that read the
//!   environment — where a node's pre-order index is its stable identity, exactly
//!   the numbering the binding edges reference (the same `NodeKey` convention
//!   Slice O's `commit.rs` walks);
//! - a binding edge table `(StateKey, node index, DirtyClass bits)`.
//!
//! What it does **not** carry is every form of developer-only metadata the section
//! 60 rule says must be strippable: no property-name strings, no type names, no
//! source spans, no symbol string table. A binding needs only the durable
//! [`StateKey`] identity and the [`DirtyClass`](crate::dirty::DirtyClass) it
//! invalidates — `commit.rs` already
//! proves the runtime never consults a property name — so the name is dropped.
//!
//! # Wire form
//!
//! The blob is read in place (architecture §50A.6): from memory-mapped flash or
//! an mmap, with nothing of it copied into RAM.
//!
//! ```text
//! ProtocolTag (magic + wire version)  4 bytes
//! MAGIC "AOT1"                        4 bytes
//! section table (2 sections)          viso_ende::Sections
//! nodes  (8-aligned)                  one variable-width record per node, pre-order
//! edges  (8-aligned)                  one 24-byte record per edge
//! ```
//!
//! A node record is its kind byte, a varint presence mask over its style and
//! lengths, their present values, and a varint child count. An edge record is
//! the state key's two halves, the node index as a `u32` and the class byte,
//! then three zero bytes: fixed width, so [`AotView::edge`] reads edge `i` in
//! place without decoding the ones before it.
//!
//! [`AotView`] is the borrowed reader: it validates the whole blob up front —
//! header, section table, every node record, the shape of the pre-order forest
//! and every edge's node index — and then yields one [`AotRecord`] at a time,
//! decoded on the stack, so instantiating a valid view never fails halfway and
//! never materializes the tables. [`AotPackage`] is the owned form the emitter
//! builds and the development commit edits; it decodes through the same view.
//! The decoder is *bounded*: a truncated, corrupt-magic, wrong-version, or
//! otherwise malformed blob returns a [`DecodeError`] and never panics — the
//! safety precondition for loading an untrusted asset blob (section 30: a bad
//! layout is a diagnosable error, never a silent miscompile or a panic).

use alloc::boxed::Box;
use alloc::vec::Vec;

use viso_ende::{
    Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag, Section, SectionWriter, Sections,
};

use crate::env::Avoid;
use crate::layout::{Align, Inset, Justify};
use crate::length::{EdgeTerms, LengthTerms, NodeLengths};
use crate::state::StateKey;

/// The format magic after the [`ProtocolTag`].
const MAGIC: [u8; 4] = *b"AOT1";

/// The sections, in table order.
const NODES: u32 = 0;
const EDGES: u32 = 1;
const SECTIONS: u32 = 2;

/// The bytes of one edge record.
const EDGE_RECORD: usize = 24;

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

/// One node as a table keeps it: which builder call it maps to, its folded
/// layout style, its environment lengths and how many immediate children follow
/// it in the pre-order table. [`record`](Self::record) is the form a builder
/// reads.
#[derive(Debug, Clone, PartialEq)]
pub struct AotNode {
    /// The builder call this node instantiates through (`flex`/`grid`/`scroll`/`leaf`).
    pub kind: AotNodeKind,
    /// The folded compile-time-constant layout scalars.
    pub style: AotStyle,
    /// The lengths that read the environment (`px`, `sp`, `em`), which the
    /// loader binds to fold at layout; boxed, as few nodes have any.
    pub lengths: Option<Box<NodeLengths>>,
    /// The number of immediate children, whose subtrees follow this node in
    /// pre-order. A leaf has `0`. The loader reconstructs ancestry from this count
    /// in one forward pass, so no parent/sibling links are stored.
    pub child_count: u32,
}

/// One node as the decoder yields it: an [`AotNode`] by value, its lengths
/// inline, so reading a node allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AotRecord {
    /// The builder call this node instantiates through.
    pub kind: AotNodeKind,
    /// The folded compile-time-constant layout scalars.
    pub style: AotStyle,
    /// The lengths that read the environment.
    pub lengths: Option<NodeLengths>,
    /// The number of immediate children.
    pub child_count: u32,
}

impl AotNode {
    /// The node as a builder reads it.
    #[inline]
    pub fn record(&self) -> AotRecord {
        AotRecord {
            kind: self.kind,
            style: self.style,
            lengths: self.lengths.as_deref().copied(),
            child_count: self.child_count,
        }
    }
}

impl From<AotRecord> for AotNode {
    fn from(record: AotRecord) -> AotNode {
        AotNode {
            kind: record.kind,
            style: record.style,
            lengths: record.lengths.map(Box::new),
            child_count: record.child_count,
        }
    }
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
    /// Where a flex container packs its children along its axis, when the
    /// node placed them.
    pub justify: Option<Justify>,
    /// How a flex container places its children across its axis, when the
    /// node placed them.
    pub align: Option<Align>,
    /// The folded width, when a constant property set one.
    pub width: Option<AotLength>,
    /// The folded height, when a constant property set one.
    pub height: Option<AotLength>,
    /// The folded gap between children, when a constant property set one.
    pub gap: Option<f32>,
    /// The folded padding of a container, when a constant property set one
    /// that reads neither the environment nor the layout direction.
    pub padding: Option<Inset>,
    /// The adaptive scope the node establishes, when it is one.
    pub scope: Option<AotScope>,
    /// What the node keeps its content clear of, when it is an avoiding
    /// region.
    pub avoid: Option<Avoid>,
}

/// An adaptive scope: the node its subtree's size class classifies.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AotScope {
    /// The constant width in dp it classifies, else the width its parent
    /// gives it.
    pub basis: Option<f32>,
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
/// payload; `Fixed`/`Fill` each carry one `f32`; `Relative` carries two.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AotLength {
    /// An exact extent in dp.
    Fixed(f32),
    /// `fixed + pct × basis`, with `pct` a fraction of the basis.
    Relative { fixed: f32, pct: f32 },
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

// The style field is written as a varint presence bitmask followed by the payloads of
// the fields that are present, so an all-default style costs a single byte and only
// the rare fields past the seventh bit a second. The bits are private to this format:
// emitter and loader are the same source, so no external contract depends on them.
const STYLE_HAS_AXIS: u16 = 1 << 0;
const STYLE_HAS_WIDTH: u16 = 1 << 1;
const STYLE_HAS_HEIGHT: u16 = 1 << 2;
const STYLE_HAS_GAP: u16 = 1 << 3;
const STYLE_HAS_LENGTHS: u16 = 1 << 4;
const STYLE_IS_SCOPE: u16 = 1 << 5;
const STYLE_HAS_BASIS: u16 = 1 << 6;
const STYLE_AVOIDS: u16 = 1 << 7;
const STYLE_HAS_PADDING: u16 = 1 << 8;
const STYLE_HAS_JUSTIFY: u16 = 1 << 9;
const STYLE_HAS_ALIGN: u16 = 1 << 10;
const STYLE_BITS: u16 = (1 << 11) - 1;

// An inset is a byte naming its non-zero edges, then their values.
const EDGE_LEFT: u8 = 1 << 0;
const EDGE_TOP: u8 = 1 << 1;
const EDGE_RIGHT: u8 = 1 << 2;
const EDGE_BOTTOM: u8 = 1 << 3;

// Environment lengths are a byte naming the bound properties, then each one's
// terms as a byte naming the non-zero terms followed by their values.
const LENGTHS_WIDTH: u8 = 1 << 0;
const LENGTHS_HEIGHT: u8 = 1 << 1;
const LENGTHS_GAP: u8 = 1 << 2;
const LENGTHS_FONT_SIZE: u8 = 1 << 3;
const LENGTHS_PADDING: u8 = 1 << 4;
const TERMS_DP: u8 = 1 << 0;
const TERMS_PX: u8 = 1 << 1;
const TERMS_SP: u8 = 1 << 2;
const TERMS_EM: u8 = 1 << 3;
const TERMS_PCT: u8 = 1 << 4;

// A length is one discriminant byte, then the payload for the variants that carry one.
const LEN_FIXED: u8 = 0;
const LEN_FILL: u8 = 1;
const LEN_FIT: u8 = 2;
const LEN_RELATIVE: u8 = 3;

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
            AotLength::Relative { fixed, pct } => {
                enc.write_u8(LEN_RELATIVE);
                enc.write_f32(*fixed);
                enc.write_f32(*pct);
            }
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
            LEN_RELATIVE => Ok(AotLength::Relative {
                fixed: dec.read_f32()?,
                pct: dec.read_f32()?,
            }),
            _ => Err(DecodeError::Malformed { offset }),
        }
    }
}

/// The terms of a length in the order the term byte names them.
fn terms_of(terms: &LengthTerms) -> [f32; 5] {
    [terms.dp, terms.px, terms.sp, terms.em, terms.pct]
}

fn encode_terms(terms: &LengthTerms, enc: &mut Encoder) {
    let values = terms_of(terms);
    let mut mask = 0u8;
    for (bit, value) in values.iter().enumerate() {
        if *value != 0.0 {
            mask |= 1 << bit;
        }
    }
    enc.write_u8(mask);
    for value in values {
        if value != 0.0 {
            enc.write_f32(value);
        }
    }
}

fn decode_terms(dec: &mut Decoder) -> Result<LengthTerms, DecodeError> {
    let offset = dec.position();
    let mask = dec.read_u8()?;
    if mask & !(TERMS_DP | TERMS_PX | TERMS_SP | TERMS_EM | TERMS_PCT) != 0 {
        return Err(DecodeError::Malformed { offset });
    }
    let mut values = [0.0f32; 5];
    for (bit, value) in values.iter_mut().enumerate() {
        if mask & (1 << bit) != 0 {
            *value = dec.read_f32()?;
        }
    }
    let [dp, px, sp, em, pct] = values;
    Ok(LengthTerms {
        dp,
        px,
        sp,
        em,
        pct,
    })
}

fn encode_inset(inset: &Inset, enc: &mut Encoder) {
    let edges = [inset.left, inset.top, inset.right, inset.bottom];
    let mut mask = 0u8;
    for (bit, edge) in edges.iter().enumerate() {
        if *edge != 0.0 {
            mask |= 1 << bit;
        }
    }
    enc.write_u8(mask);
    for edge in edges {
        if edge != 0.0 {
            enc.write_f32(edge);
        }
    }
}

fn decode_inset(dec: &mut Decoder) -> Result<Inset, DecodeError> {
    let offset = dec.position();
    let mask = dec.read_u8()?;
    if mask & !(EDGE_LEFT | EDGE_TOP | EDGE_RIGHT | EDGE_BOTTOM) != 0 {
        return Err(DecodeError::Malformed { offset });
    }
    let mut edges = [0.0f32; 4];
    for (bit, edge) in edges.iter_mut().enumerate() {
        if mask & (1 << bit) != 0 {
            *edge = dec.read_f32()?;
        }
    }
    let [left, top, right, bottom] = edges;
    Ok(Inset {
        left,
        top,
        right,
        bottom,
    })
}

/// The bound properties of `lengths` in the order the property byte names them.
fn properties(lengths: &NodeLengths) -> [Option<LengthTerms>; 4] {
    [
        lengths.width,
        lengths.height,
        lengths.gap,
        lengths.font_size,
    ]
}

impl Encode for NodeLengths {
    fn encode(&self, enc: &mut Encoder) {
        let present = properties(self);
        let mut mask = 0u8;
        for (bit, terms) in present.iter().enumerate() {
            if terms.is_some() {
                mask |= 1 << bit;
            }
        }
        if self.padding.is_some() {
            mask |= LENGTHS_PADDING;
        }
        enc.write_u8(mask);
        for terms in present.iter().flatten() {
            encode_terms(terms, enc);
        }
        if let Some(edges) = &self.padding {
            for terms in [edges.top, edges.end, edges.bottom, edges.start] {
                encode_terms(&terms, enc);
            }
        }
    }
}

impl Decode for NodeLengths {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let offset = dec.position();
        let mask = dec.read_u8()?;
        let known = LENGTHS_WIDTH | LENGTHS_HEIGHT | LENGTHS_GAP | LENGTHS_FONT_SIZE;
        if mask & !(known | LENGTHS_PADDING) != 0 {
            return Err(DecodeError::Malformed { offset });
        }
        let mut present = [None; 4];
        for (bit, terms) in present.iter_mut().enumerate() {
            if mask & (1 << bit) != 0 {
                *terms = Some(decode_terms(dec)?);
            }
        }
        let [width, height, gap, font_size] = present;
        let padding = if mask & LENGTHS_PADDING != 0 {
            Some(EdgeTerms {
                top: decode_terms(dec)?,
                end: decode_terms(dec)?,
                bottom: decode_terms(dec)?,
                start: decode_terms(dec)?,
            })
        } else {
            None
        };
        Ok(NodeLengths {
            width,
            height,
            gap,
            font_size,
            padding,
        })
    }
}

/// Writes one node record: the kind, the style and lengths, the child count.
fn encode_record(record: &AotRecord, enc: &mut Encoder) {
    let style = &record.style;
    enc.write_u8(record.kind.as_u8());
    let mut mask = 0u16;
    if style.axis.is_some() {
        mask |= STYLE_HAS_AXIS;
    }
    if style.width.is_some() {
        mask |= STYLE_HAS_WIDTH;
    }
    if style.height.is_some() {
        mask |= STYLE_HAS_HEIGHT;
    }
    if style.gap.is_some() {
        mask |= STYLE_HAS_GAP;
    }
    if record.lengths.is_some() {
        mask |= STYLE_HAS_LENGTHS;
    }
    if let Some(scope) = style.scope {
        mask |= STYLE_IS_SCOPE;
        if scope.basis.is_some() {
            mask |= STYLE_HAS_BASIS;
        }
    }
    if style.avoid.is_some() {
        mask |= STYLE_AVOIDS;
    }
    if style.padding.is_some() {
        mask |= STYLE_HAS_PADDING;
    }
    if style.justify.is_some() {
        mask |= STYLE_HAS_JUSTIFY;
    }
    if style.align.is_some() {
        mask |= STYLE_HAS_ALIGN;
    }
    enc.write_varint(u64::from(mask));
    if let Some(axis) = style.axis {
        enc.write_u8(match axis {
            AotAxis::Row => 0,
            AotAxis::Column => 1,
        });
    }
    if let Some(width) = style.width {
        width.encode(enc);
    }
    if let Some(height) = style.height {
        height.encode(enc);
    }
    if let Some(gap) = style.gap {
        enc.write_f32(gap);
    }
    if let Some(padding) = &style.padding {
        encode_inset(padding, enc);
    }
    if let Some(lengths) = &record.lengths {
        lengths.encode(enc);
    }
    if let Some(basis) = style.scope.and_then(|scope| scope.basis) {
        enc.write_f32(basis);
    }
    if let Some(avoid) = style.avoid {
        enc.write_u8(match avoid {
            Avoid::SafeArea => 0,
            Avoid::Keyboard => 1,
        });
    }
    if let Some(justify) = style.justify {
        enc.write_u8(match justify {
            Justify::Start => 0,
            Justify::Center => 1,
            Justify::End => 2,
        });
    }
    if let Some(align) = style.align {
        enc.write_u8(match align {
            Align::Start => 0,
            Align::Center => 1,
            Align::End => 2,
            Align::Stretch => 3,
        });
    }
    enc.write_varint(u64::from(record.child_count));
}

/// Reads one byte naming a variant of `variants`, in their wire order.
fn read_variant<T: Copy>(dec: &mut Decoder<'_>, variants: &[T]) -> Result<T, DecodeError> {
    let offset = dec.position();
    let tag = dec.read_u8()?;
    variants
        .get(usize::from(tag))
        .copied()
        .ok_or(DecodeError::Malformed { offset })
}

/// Reads one node record [`encode_record`] wrote.
fn decode_record(dec: &mut Decoder<'_>) -> Result<AotRecord, DecodeError> {
    let kind = read_variant(
        dec,
        &[
            AotNodeKind::Flex,
            AotNodeKind::Grid,
            AotNodeKind::Scroll,
            AotNodeKind::Leaf,
        ],
    )?;
    let offset = dec.position();
    let mask = u16::try_from(dec.read_varint()?)
        .ok()
        .filter(|mask| mask & !STYLE_BITS == 0)
        .ok_or(DecodeError::Malformed { offset })?;
    if mask & STYLE_HAS_BASIS != 0 && mask & STYLE_IS_SCOPE == 0 {
        return Err(DecodeError::Malformed { offset });
    }
    let has = |bit: u16| mask & bit != 0;
    let axis = if has(STYLE_HAS_AXIS) {
        Some(read_variant(dec, &[AotAxis::Row, AotAxis::Column])?)
    } else {
        None
    };
    let width = if has(STYLE_HAS_WIDTH) {
        Some(AotLength::decode(dec)?)
    } else {
        None
    };
    let height = if has(STYLE_HAS_HEIGHT) {
        Some(AotLength::decode(dec)?)
    } else {
        None
    };
    let gap = if has(STYLE_HAS_GAP) {
        Some(dec.read_f32()?)
    } else {
        None
    };
    let padding = if has(STYLE_HAS_PADDING) {
        Some(decode_inset(dec)?)
    } else {
        None
    };
    let lengths = if has(STYLE_HAS_LENGTHS) {
        Some(NodeLengths::decode(dec)?)
    } else {
        None
    };
    let scope = if has(STYLE_IS_SCOPE) {
        let basis = if has(STYLE_HAS_BASIS) {
            Some(dec.read_f32()?)
        } else {
            None
        };
        Some(AotScope { basis })
    } else {
        None
    };
    let avoid = if has(STYLE_AVOIDS) {
        Some(read_variant(dec, &[Avoid::SafeArea, Avoid::Keyboard])?)
    } else {
        None
    };
    let justify = if has(STYLE_HAS_JUSTIFY) {
        Some(read_variant(
            dec,
            &[Justify::Start, Justify::Center, Justify::End],
        )?)
    } else {
        None
    };
    let align = if has(STYLE_HAS_ALIGN) {
        Some(read_variant(
            dec,
            &[Align::Start, Align::Center, Align::End, Align::Stretch],
        )?)
    } else {
        None
    };
    let offset = dec.position();
    let child_count =
        u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })?;
    Ok(AotRecord {
        kind,
        style: AotStyle {
            axis,
            justify,
            align,
            width,
            height,
            gap,
            padding,
            scope,
            avoid,
        },
        lengths,
        child_count,
    })
}

impl Encode for AotRecord {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        encode_record(self, enc);
    }
}

impl Decode for AotRecord {
    #[inline]
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        decode_record(dec)
    }
}

impl Encode for AotNode {
    fn encode(&self, enc: &mut Encoder) {
        encode_record(&self.record(), enc);
    }
}

impl Decode for AotNode {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        decode_record(dec).map(AotNode::from)
    }
}

/// Writes one 24-byte edge record.
fn encode_edge(edge: &AotEdge, enc: &mut Encoder) {
    enc.write_u64(edge.state.hi);
    enc.write_u64(edge.state.lo);
    enc.write_u32(edge.node);
    enc.write_u8(edge.class);
    enc.write_raw(&[0; 3]);
}

/// Reads the edge record `bytes`, which [`AotView::new`] validated.
fn edge_of(bytes: &[u8]) -> AotEdge {
    let word = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    AotEdge {
        state: StateKey::from_parts(word(0), word(8)),
        node: u32::from_le_bytes(bytes[16..20].try_into().unwrap()),
        class: bytes[20],
    }
}

/// A validated, borrowed release UI package: the blob read where it lies.
///
/// Construction checks everything a loader relies on, so reading a view never
/// fails and instantiating one never stops halfway: the header, the section
/// table, every node record, that the pre-order forest closes (a container's
/// subtrees fit the table, a leaf has no children), and that every edge names
/// a node of the table. Nothing is decoded ahead: [`nodes`](Self::nodes)
/// decodes one record at a time on the stack, and [`edge`](Self::edge) reads
/// a fixed-width record in place.
#[derive(Debug, Clone, Copy)]
pub struct AotView<'a> {
    nodes: Section<'a>,
    edges: Section<'a>,
}

impl<'a> AotView<'a> {
    /// Validates `blob`, which holds exactly one package.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] for anything malformed, or bytes after the package.
    pub fn new(blob: &'a [u8]) -> Result<AotView<'a>, DecodeError> {
        let (view, len) = AotView::parse(blob)?;
        if len != blob.len() {
            return Err(DecodeError::TrailingBytes {
                remaining: blob.len() - len,
            });
        }
        Ok(view)
    }

    /// Validates the package at `dec`'s position and moves past it.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] for anything malformed.
    pub fn read(dec: &mut Decoder<'a>) -> Result<AotView<'a>, DecodeError> {
        let (view, len) = AotView::parse(&dec.input()[dec.position()..])?;
        dec.read_raw(len)?;
        Ok(view)
    }

    /// The package at the start of `blob` and its length.
    fn parse(blob: &'a [u8]) -> Result<(AotView<'a>, usize), DecodeError> {
        let mut dec = Decoder::new(blob);
        let offset = dec.position();
        if !ProtocolTag::decode(&mut dec)?.is_compatible() || *dec.read_array()? != MAGIC {
            return Err(DecodeError::Malformed { offset });
        }
        let sections = Sections::read(&mut dec, SECTIONS)?;
        let view = AotView {
            nodes: sections.get(NODES),
            edges: sections.get(EDGES),
        };
        let nodes = view.nodes.items as usize;
        let mut dec = view.nodes.decoder();
        // The nodes still owed to the subtrees open at this point of the
        // pre-order walk: each node takes one, a container adds its children,
        // and the forest closes when none is owed at the end.
        let mut owed = 0u64;
        for _ in 0..nodes {
            let offset = dec.position();
            let record = decode_record(&mut dec)?;
            if !record.kind.is_container() && record.child_count != 0 {
                return Err(DecodeError::Malformed { offset });
            }
            owed = owed.saturating_sub(1) + u64::from(record.child_count);
        }
        dec.finish()?;
        if owed != 0 {
            return Err(DecodeError::Malformed {
                offset: view.nodes.offset,
            });
        }
        let edges = view.edges.items as usize;
        if view.edges.bytes.len() != edges * EDGE_RECORD {
            return Err(DecodeError::Malformed {
                offset: view.edges.offset,
            });
        }
        for (index, record) in view
            .edges
            .bytes
            .as_chunks::<EDGE_RECORD>()
            .0
            .iter()
            .enumerate()
        {
            let at = view.edges.offset + index * EDGE_RECORD;
            if record[21..] != [0; 3] || edge_of(record).node as usize >= nodes {
                return Err(DecodeError::Malformed { offset: at });
            }
        }
        Ok((view, sections.end()))
    }

    /// The number of nodes.
    #[inline]
    pub fn node_count(&self) -> usize {
        self.nodes.items as usize
    }

    /// The nodes, in pre-order.
    #[inline]
    pub fn nodes(&self) -> AotNodes<'a> {
        AotNodes {
            dec: self.nodes.decoder(),
            left: self.nodes.items,
        }
    }

    /// The number of edges.
    #[inline]
    pub fn edge_count(&self) -> usize {
        self.edges.items as usize
    }

    /// Edge `index`.
    ///
    /// # Panics
    ///
    /// If `index` is out of range.
    #[inline]
    pub fn edge(&self, index: usize) -> AotEdge {
        edge_of(&self.edges.bytes[index * EDGE_RECORD..][..EDGE_RECORD])
    }

    /// The edges, in table order.
    #[inline]
    pub fn edges(&self) -> impl ExactSizeIterator<Item = AotEdge> + 'a {
        self.edges
            .bytes
            .as_chunks::<EDGE_RECORD>()
            .0
            .iter()
            .map(|r| edge_of(r))
    }

    /// The package as owned tables.
    pub fn to_package(&self) -> AotPackage {
        AotPackage {
            nodes: self.nodes().map(AotNode::from).collect(),
            edges: self.edges().collect(),
        }
    }
}

/// The nodes of an [`AotView`], in pre-order, each decoded as it is reached.
#[derive(Debug, Clone)]
pub struct AotNodes<'a> {
    dec: Decoder<'a>,
    left: u32,
}

impl Iterator for AotNodes<'_> {
    type Item = AotRecord;

    #[inline]
    fn next(&mut self) -> Option<AotRecord> {
        self.left = self.left.checked_sub(1)?;
        Some(decode_record(&mut self.dec).expect("the view validated every node record"))
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.left as usize, Some(self.left as usize))
    }
}

impl ExactSizeIterator for AotNodes<'_> {}

impl AotPackage {
    /// Writes the package's blob: the header, the section table and the
    /// node and edge sections.
    fn write(&self, enc: &mut Encoder) {
        let narrow = |n: usize| u32::try_from(n).expect("a package table fits u32 counts");
        ProtocolTag::current().encode(enc);
        enc.write_raw(&MAGIC);
        let mut sections = SectionWriter::new(enc, SECTIONS);
        sections.section(enc, |enc| {
            for node in &self.nodes {
                node.encode(enc);
            }
            narrow(self.nodes.len())
        });
        sections.section(enc, |enc| {
            for edge in &self.edges {
                encode_edge(edge, enc);
            }
            narrow(self.edges.len())
        });
        sections.finish();
    }
}

impl Encode for AotPackage {
    /// The package as a self-contained blob: its section offsets are relative
    /// to its own first byte, wherever the stream puts it.
    fn encode(&self, enc: &mut Encoder) {
        let mut blob = Encoder::new();
        self.write(&mut blob);
        enc.write_raw(blob.as_bytes());
    }

    fn encode_to_vec(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        self.write(&mut enc);
        enc.into_bytes()
    }
}

impl Decode for AotPackage {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        AotView::read(dec).map(|view| view.to_package())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dirty::DirtyClass;
    use alloc::vec;
    use viso_ende::{Decode, Encode};

    /// Where the node section starts: the eight header bytes and a two-entry
    /// section table, aligned.
    const NODE_SECTION: usize = 40;

    fn sample_package() -> AotPackage {
        AotPackage {
            nodes: vec![
                AotNode {
                    kind: AotNodeKind::Flex,
                    style: AotStyle {
                        axis: Some(AotAxis::Column),
                        justify: Some(Justify::End),
                        align: Some(Align::Stretch),
                        width: Some(AotLength::Fixed(320.0)),
                        height: Some(AotLength::Relative {
                            fixed: -16.0,
                            pct: 1.0,
                        }),
                        gap: Some(8.0),
                        padding: Some(Inset {
                            left: 4.0,
                            top: 2.0,
                            ..Inset::default()
                        }),
                        scope: Some(AotScope { basis: Some(600.0) }),
                        avoid: None,
                    },
                    lengths: None,
                    child_count: 2,
                },
                AotNode {
                    kind: AotNodeKind::Scroll,
                    style: AotStyle {
                        axis: Some(AotAxis::Row),
                        width: Some(AotLength::Fill { weight: 2.0 }),
                        height: Some(AotLength::Fit),
                        avoid: Some(Avoid::Keyboard),
                        ..AotStyle::default()
                    },
                    lengths: Some(Box::new(NodeLengths {
                        width: Some(LengthTerms::pct(50.0) - LengthTerms::px(1.0)),
                        font_size: Some(LengthTerms::em(1.25)),
                        padding: Some(EdgeTerms {
                            start: LengthTerms::sp(3.0),
                            ..EdgeTerms::all(LengthTerms::dp(1.0))
                        }),
                        ..NodeLengths::default()
                    })),
                    child_count: 0,
                },
                AotNode {
                    kind: AotNodeKind::Leaf,
                    style: AotStyle::default(),
                    lengths: None,
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

    fn leaf() -> AotNode {
        AotNode {
            kind: AotNodeKind::Leaf,
            style: AotStyle::default(),
            lengths: None,
            child_count: 0,
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
    fn the_view_reads_the_tables_in_place() {
        let pkg = sample_package();
        let bytes = pkg.encode_to_vec();
        let view = AotView::new(&bytes).expect("valid");
        assert_eq!(view.node_count(), 3);
        let records: Vec<AotRecord> = view.nodes().collect();
        let owned: Vec<AotRecord> = pkg.nodes.iter().map(AotNode::record).collect();
        assert_eq!(records, owned);
        assert_eq!(view.nodes().len(), 3);
        assert_eq!(view.edge_count(), 2);
        assert_eq!(view.edge(1), pkg.edges[1]);
        assert!(view.edges().eq(pkg.edges.iter().copied()));
        // Both sections start on an eight-byte boundary of the blob.
        assert_eq!(view.nodes.offset, NODE_SECTION);
        assert_eq!(view.edges.offset % viso_ende::SECTION_ALIGN, 0);
    }

    #[test]
    fn a_package_nests_in_a_stream() {
        let pkg = sample_package();
        let mut enc = Encoder::new();
        enc.write_u8(0xab);
        pkg.encode(&mut enc);
        enc.write_u8(0xcd);
        let bytes = enc.into_bytes();
        let mut dec = Decoder::new(&bytes);
        assert_eq!(dec.read_u8(), Ok(0xab));
        assert_eq!(AotPackage::decode(&mut dec), Ok(pkg));
        assert_eq!(dec.read_u8(), Ok(0xcd));
        dec.finish().unwrap();
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
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(AotView::new(&longer).is_err(), "trailing bytes");
    }

    #[test]
    fn corrupt_magic_is_rejected() {
        for at in [0, 4] {
            let mut bytes = sample_package().encode_to_vec();
            bytes[at] ^= 0xff;
            let res = AotPackage::decode_from_slice(&bytes);
            assert!(res.is_err(), "a bad magic must be rejected, not panic");
        }
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
    fn a_broken_section_table_or_padding_is_rejected() {
        let bytes = sample_package().encode_to_vec();
        let table = 8;
        // Another section count.
        let mut bad = bytes.clone();
        bad[table] = 3;
        assert!(AotView::new(&bad).is_err());
        // The node section moved off its aligned place.
        let mut bad = bytes.clone();
        bad[table + 4] += 1;
        assert!(AotView::new(&bad).is_err());
        // A stray byte in the padding after the table.
        let mut bad = bytes.clone();
        bad[NODE_SECTION - 1] = 1;
        assert!(AotView::new(&bad).is_err());
        // A stray byte in an edge record's padding.
        let view = AotView::new(&bytes).unwrap();
        let mut bad = bytes.clone();
        bad[view.edges.offset + 23] = 1;
        assert!(AotView::new(&bad).is_err());
    }

    #[test]
    fn unknown_node_kind_tag_is_rejected() {
        let pkg = AotPackage {
            nodes: vec![leaf()],
            edges: Vec::new(),
        };
        let mut bytes = pkg.encode_to_vec();
        bytes[NODE_SECTION] = 0xfe;
        let res = AotPackage::decode_from_slice(&bytes);
        assert!(res.is_err(), "an unknown node kind must be rejected");
    }

    #[test]
    fn a_forest_that_does_not_close_is_rejected() {
        let container = |child_count| AotNode {
            kind: AotNodeKind::Flex,
            child_count,
            ..leaf()
        };
        let valid = |nodes: Vec<AotNode>| {
            AotView::new(
                &AotPackage {
                    nodes,
                    edges: Vec::new(),
                }
                .encode_to_vec(),
            )
            .is_ok()
        };
        assert!(valid(vec![container(2), leaf(), leaf()]));
        assert!(valid(vec![container(1), container(0), leaf()]), "two roots");
        assert!(!valid(vec![container(2), leaf()]), "a child missing");
        assert!(
            !valid(vec![
                AotNode {
                    child_count: 1,
                    ..leaf()
                },
                leaf()
            ]),
            "a leaf with a child"
        );
    }

    #[test]
    fn an_edge_to_a_missing_node_is_rejected() {
        let mut pkg = sample_package();
        pkg.edges[0].node = 3;
        assert!(AotView::new(&pkg.encode_to_vec()).is_err());
    }
}
