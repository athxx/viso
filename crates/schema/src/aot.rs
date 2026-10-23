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

use crate::env::{Avoid, LayoutDirection};
use crate::layout::{
    AbsoluteInsets, AdaptiveColumns, Align, AlignItems, Alignment2D, AutoRepeat, BoxRules,
    GridPlacement, Inset, Justify, Placement, SheetDetent, SheetEdge, Span, SpanInset,
    StackPlacement, TrackMax, TrackSizing,
};
use crate::length::{EdgeTerms, InsetTerms, LengthTerms, NodeLengths};
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
    /// What bounds the node's box beyond its size request, its margin and its
    /// padding percentage, when constant properties set any; boxed, as few
    /// nodes have any.
    pub rules: Option<Box<BoxRules>>,
    /// A grid container's tracks, gaps and cell alignment, when it authored
    /// any; boxed, as few nodes are grids.
    pub grid: Option<Box<AotGrid>>,
    /// The number of immediate children, whose subtrees follow this node in
    /// pre-order. A leaf has `0`. The loader reconstructs ancestry from this count
    /// in one forward pass, so no parent/sibling links are stored.
    pub child_count: u32,
}

/// One node as a builder reads it: its kind, style and child count by
/// value, its lengths, rules and grid — which few nodes have — read on
/// demand from where they lie, so reading a node copies little and
/// allocates nothing.
#[derive(Debug, Clone, Copy)]
pub struct AotRecord<'a> {
    /// The builder call this node instantiates through.
    pub kind: AotNodeKind,
    /// The folded compile-time-constant layout scalars.
    pub style: AotStyle,
    /// The number of immediate children.
    pub child_count: u32,
    extras: Extras<'a>,
}

/// Where a record's lengths, rules and grid lie: still encoded in a
/// validated blob, or held by an [`AotNode`].
#[derive(Debug, Clone, Copy)]
enum Extras<'a> {
    Encoded {
        lengths: Option<&'a [u8]>,
        rules: Option<&'a [u8]>,
        grid: Option<&'a [u8]>,
    },
    Held {
        lengths: Option<&'a NodeLengths>,
        rules: Option<&'a BoxRules>,
        grid: Option<&'a AotGrid>,
    },
}

/// A packaged grid container's tracks, gaps and cell alignment: what its
/// builder style takes beyond the size and padding every container has.
#[derive(Debug, Clone, PartialEq)]
pub struct AotGrid {
    /// The explicit column template.
    pub columns: Vec<TrackSizing>,
    /// The explicit row template.
    pub rows: Vec<TrackSizing>,
    /// How a row past the explicit ones is sized.
    pub auto_rows: TrackSizing,
    /// The gap between adjacent columns.
    pub column_gap: f32,
    /// The gap between adjacent rows.
    pub row_gap: f32,
    /// How a cell's content sits in its cell on the block axis.
    pub align_items: AlignItems,
    /// A column template whose count follows the grid's width.
    pub adaptive_columns: Option<AdaptiveColumns>,
}

impl Default for AotGrid {
    fn default() -> Self {
        AotGrid {
            columns: Vec::new(),
            rows: Vec::new(),
            auto_rows: TrackSizing::Auto,
            column_gap: 0.0,
            row_gap: 0.0,
            align_items: AlignItems::Stretch,
            adaptive_columns: None,
        }
    }
}

impl AotRecord<'_> {
    /// The lengths that read the environment.
    #[inline]
    pub fn lengths(&self) -> Option<NodeLengths> {
        match self.extras {
            Extras::Encoded { lengths, .. } => lengths.map(|bytes| {
                NodeLengths::decode(&mut Decoder::new(bytes)).expect("validated with the record")
            }),
            Extras::Held { lengths, .. } => lengths.copied(),
        }
    }

    /// Whether the node has lengths that read the environment.
    #[inline]
    pub fn has_lengths(&self) -> bool {
        match self.extras {
            Extras::Encoded { lengths, .. } => lengths.is_some(),
            Extras::Held { lengths, .. } => lengths.is_some(),
        }
    }

    /// The constant box rules.
    #[inline]
    pub fn rules(&self) -> Option<BoxRules> {
        match self.extras {
            Extras::Encoded { rules, .. } => rules.map(|bytes| {
                decode_rules(&mut Decoder::new(bytes)).expect("validated with the record")
            }),
            Extras::Held { rules, .. } => rules.copied(),
        }
    }

    /// The grid container's tracks, gaps and cell alignment.
    #[inline]
    pub fn grid(&self) -> Option<AotGrid> {
        match self.extras {
            Extras::Encoded { grid, .. } => grid.map(|bytes| {
                decode_grid(&mut Decoder::new(bytes)).expect("validated with the record")
            }),
            Extras::Held { grid, .. } => grid.cloned(),
        }
    }
}

impl PartialEq for AotRecord<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.style == other.style
            && self.child_count == other.child_count
            && self.lengths() == other.lengths()
            && self.rules() == other.rules()
            && self.grid() == other.grid()
    }
}

impl AotNode {
    /// The node as a builder reads it.
    #[inline]
    pub fn record(&self) -> AotRecord<'_> {
        AotRecord {
            kind: self.kind,
            style: self.style,
            child_count: self.child_count,
            extras: Extras::Held {
                lengths: self.lengths.as_deref(),
                rules: self.rules.as_deref(),
                grid: self.grid.as_deref(),
            },
        }
    }
}

impl From<AotRecord<'_>> for AotNode {
    fn from(record: AotRecord<'_>) -> AotNode {
        AotNode {
            kind: record.kind,
            style: record.style,
            lengths: record.lengths().map(Box::new),
            rules: record.rules().map(Box::new),
            grid: record.grid().map(Box::new),
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
    /// A stack → `cx.stack`.
    Stack = 4,
    /// An absolute container → `cx.absolute`.
    Absolute = 5,
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
            AotNodeKind::Flex
                | AotNodeKind::Grid
                | AotNodeKind::Scroll
                | AotNodeKind::Stack
                | AotNodeKind::Absolute
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
    /// Where a stack places a child that does not align itself, when the
    /// node set it.
    pub content_align: Option<Alignment2D>,
    /// Where a grid, stack or absolute child sits in its parent, when it
    /// placed itself.
    pub place: Option<Placement>,
    /// How a scroll viewport scrolls, when the node set it.
    pub scroll: Option<AotScroll>,
    /// What a sheet is, when the node is one.
    pub sheet: Option<AotSheet>,
}

/// What a sheet is: the edge its panel lies against, where it rests (none
/// for all of the panel), and whether a drag past its least detent
/// dismisses it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AotSheet {
    pub edge: SheetEdge,
    pub dismissible: bool,
    detents: [SheetDetent; AotSheet::MAX_DETENTS],
    count: u8,
}

impl AotSheet {
    /// The most detents a sheet rests at.
    pub const MAX_DETENTS: usize = 4;

    /// A sheet against `edge` resting at `detents`, `None` past
    /// [`MAX_DETENTS`](Self::MAX_DETENTS) of them.
    pub fn new(edge: SheetEdge, detents: &[SheetDetent], dismissible: bool) -> Option<Self> {
        if detents.len() > Self::MAX_DETENTS {
            return None;
        }
        let mut at = [SheetDetent::Full; Self::MAX_DETENTS];
        at[..detents.len()].copy_from_slice(detents);
        Some(AotSheet {
            edge,
            dismissible,
            detents: at,
            count: detents.len() as u8,
        })
    }

    /// Where it rests, least first; none for all of the panel.
    pub fn detents(&self) -> &[SheetDetent] {
        &self.detents[..usize::from(self.count)]
    }
}

impl Default for AotSheet {
    fn default() -> Self {
        AotSheet {
            edge: SheetEdge::Bottom,
            dismissible: true,
            detents: [SheetDetent::Full; Self::MAX_DETENTS],
            count: 0,
        }
    }
}

// A sheet is a byte — bits 0-1 its edge, bit 2 whether it is dismissible,
// bits 3-5 how many detents follow — then each detent a tag byte (`0` all of
// the panel, `1` a share of the surface, `2` a length), the latter two an
// `f32` after it.
const SHEET_DISMISSIBLE: u8 = 1 << 2;
const SHEET_COUNT_SHIFT: u8 = 3;

/// How a scroll viewport scrolls: whether it shows scroll bars, whether it
/// follows the end of its content, how it comes to rest on its items and
/// how far in from its edges, and whether page dots show them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AotScroll {
    pub bars: bool,
    pub tail: bool,
    pub snap: Option<AotSnap>,
    pub snap_inset: f32,
    pub dots: bool,
}

impl Default for AotScroll {
    fn default() -> Self {
        AotScroll {
            bars: true,
            tail: false,
            snap: None,
            snap_inset: 0.0,
            dots: false,
        }
    }
}

/// Where a snapping viewport's item rests: at its start, middle or end, or a
/// page at a time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AotSnap {
    Start,
    Center,
    End,
    Pages,
}

impl AotSnap {
    /// Every snap, by wire tag.
    pub const ALL: [AotSnap; 4] = [
        AotSnap::Start,
        AotSnap::Center,
        AotSnap::End,
        AotSnap::Pages,
    ];
}

// A scroll is a byte: bit 0 its bars, bit 1 its tail, bit 2 its dots, bit 3
// an inset following as an `f32`, and above them its snap, `0` for none, else
// one past its tag.
const SCROLL_BARS: u8 = 1;
const SCROLL_TAIL: u8 = 1 << 1;
const SCROLL_DOTS: u8 = 1 << 2;
const SCROLL_INSET: u8 = 1 << 3;
const SCROLL_SNAP_SHIFT: u8 = 4;

/// An adaptive scope: the node its subtree's size class classifies.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct AotScope {
    /// The constant width in dp it classifies, else the width its parent
    /// gives it.
    pub basis: Option<f32>,
    /// The direction its subtree's lines run in, else its parent's.
    pub direction: Option<LayoutDirection>,
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

/// A folded length, the compact twin of `viso_ui::layout::Length`. `Fit` and
/// the content sizes need no payload; `Fixed`/`Fill` each carry one `f32`;
/// `Relative` carries two.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AotLength {
    /// An exact extent in dp.
    Fixed(f32),
    /// `fixed + pct × basis`, with `pct` a fraction of the basis.
    Relative { fixed: f32, pct: f32 },
    /// A weighted share of leftover main-axis space.
    Fill { weight: f32 },
    /// The content's extent within the room.
    Fit,
    /// The narrowest the content lays out in.
    MinContent,
    /// The content's unconstrained extent.
    MaxContent,
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
const STYLE_HAS_AXIS: u32 = 1 << 0;
const STYLE_HAS_WIDTH: u32 = 1 << 1;
const STYLE_HAS_HEIGHT: u32 = 1 << 2;
const STYLE_HAS_GAP: u32 = 1 << 3;
const STYLE_HAS_LENGTHS: u32 = 1 << 4;
const STYLE_IS_SCOPE: u32 = 1 << 5;
const STYLE_HAS_BASIS: u32 = 1 << 6;
const STYLE_AVOIDS: u32 = 1 << 7;
const STYLE_HAS_PADDING: u32 = 1 << 8;
const STYLE_HAS_JUSTIFY: u32 = 1 << 9;
const STYLE_HAS_ALIGN: u32 = 1 << 10;
const STYLE_HAS_RULES: u32 = 1 << 11;
const STYLE_HAS_PLACE: u32 = 1 << 12;
const STYLE_HAS_GRID: u32 = 1 << 13;
const STYLE_HAS_CONTENT_ALIGN: u32 = 1 << 14;
const STYLE_HAS_DIRECTION: u32 = 1 << 15;
const STYLE_SCROLLS: u32 = 1 << 16;
const STYLE_SHEET: u32 = 1 << 17;

// A grid is a byte naming the parts it sets, then each: a template is a
// varint count of tracks; a track is a byte naming its sizing, then its
// values; an adaptive template is its mode byte, its floor and its ceiling
// as a track maximum.
const GRID_COLUMNS: u8 = 1 << 0;
const GRID_ROWS: u8 = 1 << 1;
const GRID_AUTO_ROWS: u8 = 1 << 2;
const GRID_COLUMN_GAP: u8 = 1 << 3;
const GRID_ROW_GAP: u8 = 1 << 4;
const GRID_ALIGN: u8 = 1 << 5;
const GRID_ADAPTIVE: u8 = 1 << 6;
const TRACK_FIXED: u8 = 0;
const TRACK_FR: u8 = 1;
const TRACK_AUTO: u8 = 2;
const TRACK_PERCENT: u8 = 3;
const TRACK_MINMAX: u8 = 4;
const TRACK_FIT_CONTENT: u8 = 5;
const TRACK_FLEX_MIN: u8 = 6;
const TRACK_RELATIVE: u8 = 7;
const TRACK_LENGTH: u8 = 8;

// A placement is a byte naming the parent it is for, then a byte naming its
// set parts, then each. A grid cell's are varints, a span left out being
// `1`; a stack's are its alignment byte and its layer as a signed varint; an
// absolute child's are the spans of its pinned edges, top, end, bottom,
// start.
const PLACE_GRID: u8 = 0;
const PLACE_STACK: u8 = 1;
const PLACE_ABSOLUTE: u8 = 2;
const PLACE_COLUMN: u8 = 1 << 0;
const PLACE_ROW: u8 = 1 << 1;
const PLACE_COLUMN_SPAN: u8 = 1 << 2;
const PLACE_ROW_SPAN: u8 = 1 << 3;
const PLACE_ALIGN: u8 = 1 << 0;
const PLACE_LAYER: u8 = 1 << 1;

// Box rules are a byte naming the rules present, then each: a span is a byte
// naming its non-zero parts, then them; a margin is four spans; a padding
// percentage is an inset.
const RULE_MIN_WIDTH: u8 = 1 << 0;
const RULE_MAX_WIDTH: u8 = 1 << 1;
const RULE_MIN_HEIGHT: u8 = 1 << 2;
const RULE_MAX_HEIGHT: u8 = 1 << 3;
const RULE_MARGIN: u8 = 1 << 4;
const RULE_PADDING_PCT: u8 = 1 << 5;
const SPAN_FIXED: u8 = 1 << 0;
const SPAN_PCT: u8 = 1 << 1;

// An inset is a byte naming its non-zero edges, then their values.
const EDGE_LEFT: u8 = 1 << 0;
const EDGE_TOP: u8 = 1 << 1;
const EDGE_RIGHT: u8 = 1 << 2;
const EDGE_BOTTOM: u8 = 1 << 3;

// Environment lengths are a varint naming the bound properties, then each
// one's terms as a byte naming the non-zero terms followed by their values; an
// edge set is four of them, top, end, bottom, start.
const LENGTHS_PADDING: u16 = 1 << 8;
const LENGTHS_MARGIN: u16 = 1 << 9;
const LENGTHS_ROW_GAP: u16 = 1 << 10;
const LENGTHS_INSETS: u16 = 1 << 11;
const LENGTHS_CORNER_RADIUS: u16 = 1 << 12;
const LENGTHS_BITS: u16 = (1 << 13) - 1;
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
const LEN_MIN_CONTENT: u8 = 4;
const LEN_MAX_CONTENT: u8 = 5;

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
            AotLength::MinContent => enc.write_u8(LEN_MIN_CONTENT),
            AotLength::MaxContent => enc.write_u8(LEN_MAX_CONTENT),
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
            LEN_MIN_CONTENT => Ok(AotLength::MinContent),
            LEN_MAX_CONTENT => Ok(AotLength::MaxContent),
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

pub(crate) fn encode_terms(terms: &LengthTerms, enc: &mut Encoder) {
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

pub(crate) fn decode_terms(dec: &mut Decoder) -> Result<LengthTerms, DecodeError> {
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

/// The bound single lengths of `lengths` in the order the property mask names
/// them.
fn properties(lengths: &NodeLengths) -> [Option<LengthTerms>; 8] {
    [
        lengths.width,
        lengths.height,
        lengths.gap,
        lengths.font_size,
        lengths.min_width,
        lengths.max_width,
        lengths.min_height,
        lengths.max_height,
    ]
}

fn encode_edges(edges: &EdgeTerms, enc: &mut Encoder) {
    for terms in [edges.top, edges.end, edges.bottom, edges.start] {
        encode_terms(&terms, enc);
    }
}

fn decode_edges(dec: &mut Decoder) -> Result<EdgeTerms, DecodeError> {
    Ok(EdgeTerms {
        top: decode_terms(dec)?,
        end: decode_terms(dec)?,
        bottom: decode_terms(dec)?,
        start: decode_terms(dec)?,
    })
}

impl Encode for NodeLengths {
    fn encode(&self, enc: &mut Encoder) {
        let present = properties(self);
        let mut mask = 0u16;
        for (bit, terms) in present.iter().enumerate() {
            if terms.is_some() {
                mask |= 1 << bit;
            }
        }
        if self.padding.is_some() {
            mask |= LENGTHS_PADDING;
        }
        if self.margin.is_some() {
            mask |= LENGTHS_MARGIN;
        }
        if self.row_gap.is_some() {
            mask |= LENGTHS_ROW_GAP;
        }
        if self.insets.is_some() {
            mask |= LENGTHS_INSETS;
        }
        if self.corner_radius.is_some() {
            mask |= LENGTHS_CORNER_RADIUS;
        }
        enc.write_varint(u64::from(mask));
        for terms in present.iter().flatten() {
            encode_terms(terms, enc);
        }
        if let Some(edges) = &self.padding {
            encode_edges(edges, enc);
        }
        if let Some(edges) = &self.margin {
            encode_edges(edges, enc);
        }
        if let Some(terms) = &self.row_gap {
            encode_terms(terms, enc);
        }
        if let Some(insets) = &self.insets {
            let edges = [insets.top, insets.end, insets.bottom, insets.start];
            let mut mask = 0u8;
            for (bit, edge) in edges.iter().enumerate() {
                if edge.is_some() {
                    mask |= 1 << bit;
                }
            }
            enc.write_u8(mask);
            for terms in edges.iter().flatten() {
                encode_terms(terms, enc);
            }
        }
        if let Some(terms) = &self.corner_radius {
            encode_terms(terms, enc);
        }
    }
}

impl Decode for NodeLengths {
    fn decode(dec: &mut Decoder) -> Result<Self, DecodeError> {
        let offset = dec.position();
        let mask = u16::try_from(dec.read_varint()?)
            .ok()
            .filter(|mask| mask & !LENGTHS_BITS == 0)
            .ok_or(DecodeError::Malformed { offset })?;
        let mut present = [None; 8];
        for (bit, terms) in present.iter_mut().enumerate() {
            if mask & (1 << bit) != 0 {
                *terms = Some(decode_terms(dec)?);
            }
        }
        let [
            width,
            height,
            gap,
            font_size,
            min_width,
            max_width,
            min_height,
            max_height,
        ] = present;
        let padding = if mask & LENGTHS_PADDING != 0 {
            Some(decode_edges(dec)?)
        } else {
            None
        };
        let margin = if mask & LENGTHS_MARGIN != 0 {
            Some(decode_edges(dec)?)
        } else {
            None
        };
        let row_gap = if mask & LENGTHS_ROW_GAP != 0 {
            Some(decode_terms(dec)?)
        } else {
            None
        };
        let insets = if mask & LENGTHS_INSETS != 0 {
            let offset = dec.position();
            let edges = dec.read_u8()?;
            if edges & !0b1111 != 0 {
                return Err(DecodeError::Malformed { offset });
            }
            let mut read = |bit: u8| -> Result<Option<LengthTerms>, DecodeError> {
                if edges & (1 << bit) != 0 {
                    decode_terms(dec).map(Some)
                } else {
                    Ok(None)
                }
            };
            Some(InsetTerms {
                top: read(0)?,
                end: read(1)?,
                bottom: read(2)?,
                start: read(3)?,
            })
        } else {
            None
        };
        let corner_radius = if mask & LENGTHS_CORNER_RADIUS != 0 {
            Some(decode_terms(dec)?)
        } else {
            None
        };
        Ok(NodeLengths {
            width,
            height,
            gap,
            font_size,
            padding,
            min_width,
            max_width,
            min_height,
            max_height,
            margin,
            row_gap,
            insets,
            corner_radius,
        })
    }
}

fn encode_span(span: Span, enc: &mut Encoder) {
    let mut mask = 0u8;
    if span.fixed != 0.0 {
        mask |= SPAN_FIXED;
    }
    if span.pct != 0.0 {
        mask |= SPAN_PCT;
    }
    enc.write_u8(mask);
    if span.fixed != 0.0 {
        enc.write_f32(span.fixed);
    }
    if span.pct != 0.0 {
        enc.write_f32(span.pct);
    }
}

fn decode_span(dec: &mut Decoder) -> Result<Span, DecodeError> {
    let offset = dec.position();
    let mask = dec.read_u8()?;
    if mask & !(SPAN_FIXED | SPAN_PCT) != 0 {
        return Err(DecodeError::Malformed { offset });
    }
    let fixed = if mask & SPAN_FIXED != 0 {
        dec.read_f32()?
    } else {
        0.0
    };
    let pct = if mask & SPAN_PCT != 0 {
        dec.read_f32()?
    } else {
        0.0
    };
    Ok(Span { fixed, pct })
}

fn encode_rules(rules: &BoxRules, enc: &mut Encoder) {
    let spans = [
        (RULE_MIN_WIDTH, rules.min_width, Span::ZERO),
        (RULE_MAX_WIDTH, rules.max_width, Span::UNBOUNDED),
        (RULE_MIN_HEIGHT, rules.min_height, Span::ZERO),
        (RULE_MAX_HEIGHT, rules.max_height, Span::UNBOUNDED),
    ];
    let mut mask = 0u8;
    for (bit, span, none) in spans {
        if span != none {
            mask |= bit;
        }
    }
    if rules.margin != SpanInset::ZERO {
        mask |= RULE_MARGIN;
    }
    if rules.padding_pct != Inset::default() {
        mask |= RULE_PADDING_PCT;
    }
    enc.write_u8(mask);
    for (bit, span, _) in spans {
        if mask & bit != 0 {
            encode_span(span, enc);
        }
    }
    if mask & RULE_MARGIN != 0 {
        let m = rules.margin;
        for span in [m.left, m.top, m.right, m.bottom] {
            encode_span(span, enc);
        }
    }
    if mask & RULE_PADDING_PCT != 0 {
        encode_inset(&rules.padding_pct, enc);
    }
}

fn decode_rules(dec: &mut Decoder) -> Result<BoxRules, DecodeError> {
    let offset = dec.position();
    let mask = dec.read_u8()?;
    let known = RULE_MIN_WIDTH
        | RULE_MAX_WIDTH
        | RULE_MIN_HEIGHT
        | RULE_MAX_HEIGHT
        | RULE_MARGIN
        | RULE_PADDING_PCT;
    if mask & !known != 0 {
        return Err(DecodeError::Malformed { offset });
    }
    let mut rules = BoxRules::NONE;
    let span = |dec: &mut Decoder, bit: u8, none: Span| -> Result<Span, DecodeError> {
        if mask & bit != 0 {
            decode_span(dec)
        } else {
            Ok(none)
        }
    };
    rules.min_width = span(dec, RULE_MIN_WIDTH, Span::ZERO)?;
    rules.max_width = span(dec, RULE_MAX_WIDTH, Span::UNBOUNDED)?;
    rules.min_height = span(dec, RULE_MIN_HEIGHT, Span::ZERO)?;
    rules.max_height = span(dec, RULE_MAX_HEIGHT, Span::UNBOUNDED)?;
    if mask & RULE_MARGIN != 0 {
        rules.margin = SpanInset {
            left: decode_span(dec)?,
            top: decode_span(dec)?,
            right: decode_span(dec)?,
            bottom: decode_span(dec)?,
        };
    }
    if mask & RULE_PADDING_PCT != 0 {
        rules.padding_pct = decode_inset(dec)?;
    }
    Ok(rules)
}

fn encode_track(track: TrackSizing, enc: &mut Encoder) {
    let (tag, values): (u8, &[f32]) = match &track {
        TrackSizing::Fixed(v) => (TRACK_FIXED, core::slice::from_ref(v)),
        TrackSizing::Fr(v) => (TRACK_FR, core::slice::from_ref(v)),
        TrackSizing::Auto => (TRACK_AUTO, &[]),
        TrackSizing::Percent(v) => (TRACK_PERCENT, core::slice::from_ref(v)),
        TrackSizing::Minmax(min, max) => return encode_pair(TRACK_MINMAX, *min, *max, enc),
        TrackSizing::FitContent(v) => (TRACK_FIT_CONTENT, core::slice::from_ref(v)),
        TrackSizing::FlexMin(min, fr) => return encode_pair(TRACK_FLEX_MIN, *min, *fr, enc),
        TrackSizing::Relative { fixed, pct } => {
            return encode_pair(TRACK_RELATIVE, *fixed, *pct, enc);
        }
        TrackSizing::Length(terms) => {
            enc.write_u8(TRACK_LENGTH);
            for term in [terms.dp, terms.px, terms.sp, terms.em, terms.pct] {
                enc.write_f32(term);
            }
            return;
        }
    };
    enc.write_u8(tag);
    for value in values {
        enc.write_f32(*value);
    }
}

fn encode_pair(tag: u8, a: f32, b: f32, enc: &mut Encoder) {
    enc.write_u8(tag);
    enc.write_f32(a);
    enc.write_f32(b);
}

fn decode_track(dec: &mut Decoder) -> Result<TrackSizing, DecodeError> {
    let offset = dec.position();
    Ok(match dec.read_u8()? {
        TRACK_FIXED => TrackSizing::Fixed(dec.read_f32()?),
        TRACK_FR => TrackSizing::Fr(dec.read_f32()?),
        TRACK_AUTO => TrackSizing::Auto,
        TRACK_PERCENT => TrackSizing::Percent(dec.read_f32()?),
        TRACK_MINMAX => TrackSizing::Minmax(dec.read_f32()?, dec.read_f32()?),
        TRACK_FIT_CONTENT => TrackSizing::FitContent(dec.read_f32()?),
        TRACK_FLEX_MIN => TrackSizing::FlexMin(dec.read_f32()?, dec.read_f32()?),
        TRACK_RELATIVE => TrackSizing::Relative {
            fixed: dec.read_f32()?,
            pct: dec.read_f32()?,
        },
        TRACK_LENGTH => TrackSizing::Length(crate::length::LengthTerms {
            dp: dec.read_f32()?,
            px: dec.read_f32()?,
            sp: dec.read_f32()?,
            em: dec.read_f32()?,
            pct: dec.read_f32()?,
        }),
        _ => return Err(DecodeError::Malformed { offset }),
    })
}

fn encode_tracks(tracks: &[TrackSizing], enc: &mut Encoder) {
    enc.write_varint(tracks.len() as u64);
    for track in tracks {
        encode_track(*track, enc);
    }
}

/// Reads a template, handing each track to `push`. A count is at most a
/// `u16` and never more than the bytes left, each track taking one.
fn decode_tracks(dec: &mut Decoder, mut push: impl FnMut(TrackSizing)) -> Result<(), DecodeError> {
    let offset = dec.position();
    let count = dec.read_varint()?;
    if count > u64::from(u16::MAX) || count > dec.remaining() as u64 {
        return Err(DecodeError::Malformed { offset });
    }
    for _ in 0..count {
        push(decode_track(dec)?);
    }
    Ok(())
}

const ALIGN_ITEMS: [AlignItems; 5] = [
    AlignItems::Stretch,
    AlignItems::Start,
    AlignItems::Center,
    AlignItems::End,
    AlignItems::Baseline,
];

fn encode_grid(grid: &AotGrid, enc: &mut Encoder) {
    let defaults = AotGrid::default();
    let mut mask = 0u8;
    if !grid.columns.is_empty() {
        mask |= GRID_COLUMNS;
    }
    if !grid.rows.is_empty() {
        mask |= GRID_ROWS;
    }
    if grid.auto_rows != defaults.auto_rows {
        mask |= GRID_AUTO_ROWS;
    }
    if grid.column_gap != 0.0 {
        mask |= GRID_COLUMN_GAP;
    }
    if grid.row_gap != 0.0 {
        mask |= GRID_ROW_GAP;
    }
    if grid.align_items != defaults.align_items {
        mask |= GRID_ALIGN;
    }
    if grid.adaptive_columns.is_some() {
        mask |= GRID_ADAPTIVE;
    }
    enc.write_u8(mask);
    if mask & GRID_COLUMNS != 0 {
        encode_tracks(&grid.columns, enc);
    }
    if mask & GRID_ROWS != 0 {
        encode_tracks(&grid.rows, enc);
    }
    if mask & GRID_AUTO_ROWS != 0 {
        encode_track(grid.auto_rows, enc);
    }
    if mask & GRID_COLUMN_GAP != 0 {
        enc.write_f32(grid.column_gap);
    }
    if mask & GRID_ROW_GAP != 0 {
        enc.write_f32(grid.row_gap);
    }
    if mask & GRID_ALIGN != 0 {
        let at = ALIGN_ITEMS.iter().position(|a| *a == grid.align_items);
        enc.write_u8(at.unwrap_or(0) as u8);
    }
    if let Some(adaptive) = grid.adaptive_columns {
        enc.write_u8(match adaptive.mode {
            AutoRepeat::Fill => 0,
            AutoRepeat::Fit => 1,
        });
        enc.write_f32(adaptive.min);
        let (tag, value) = match adaptive.max {
            TrackMax::Px(px) => (0, px),
            TrackMax::Fr(fr) => (1, fr),
        };
        enc.write_u8(tag);
        enc.write_f32(value);
    }
}

/// Reads a grid, its templates handed to `columns` and `rows` instead of
/// kept.
fn read_grid(
    dec: &mut Decoder,
    columns: impl FnMut(TrackSizing),
    rows: impl FnMut(TrackSizing),
) -> Result<AotGrid, DecodeError> {
    let offset = dec.position();
    let mask = dec.read_u8()?;
    let known = GRID_COLUMNS
        | GRID_ROWS
        | GRID_AUTO_ROWS
        | GRID_COLUMN_GAP
        | GRID_ROW_GAP
        | GRID_ALIGN
        | GRID_ADAPTIVE;
    if mask & !known != 0 {
        return Err(DecodeError::Malformed { offset });
    }
    let mut grid = AotGrid::default();
    if mask & GRID_COLUMNS != 0 {
        decode_tracks(dec, columns)?;
    }
    if mask & GRID_ROWS != 0 {
        decode_tracks(dec, rows)?;
    }
    if mask & GRID_AUTO_ROWS != 0 {
        grid.auto_rows = decode_track(dec)?;
    }
    if mask & GRID_COLUMN_GAP != 0 {
        grid.column_gap = dec.read_f32()?;
    }
    if mask & GRID_ROW_GAP != 0 {
        grid.row_gap = dec.read_f32()?;
    }
    if mask & GRID_ALIGN != 0 {
        grid.align_items = read_variant(dec, &ALIGN_ITEMS)?;
    }
    if mask & GRID_ADAPTIVE != 0 {
        let mode = read_variant(dec, &[AutoRepeat::Fill, AutoRepeat::Fit])?;
        let min = dec.read_f32()?;
        let offset = dec.position();
        let max = match dec.read_u8()? {
            0 => TrackMax::Px(dec.read_f32()?),
            1 => TrackMax::Fr(dec.read_f32()?),
            _ => return Err(DecodeError::Malformed { offset }),
        };
        grid.adaptive_columns = Some(AdaptiveColumns { mode, min, max });
    }
    Ok(grid)
}

fn decode_grid(dec: &mut Decoder) -> Result<AotGrid, DecodeError> {
    let (mut columns, mut rows) = (Vec::new(), Vec::new());
    let mut grid = read_grid(dec, |t| columns.push(t), |t| rows.push(t))?;
    grid.columns = columns;
    grid.rows = rows;
    Ok(grid)
}

fn encode_place(place: Placement, enc: &mut Encoder) {
    match place {
        Placement::Grid(grid) => {
            enc.write_u8(PLACE_GRID);
            encode_cell(grid, enc);
        }
        Placement::Stack(stack) => {
            enc.write_u8(PLACE_STACK);
            let mut mask = 0u8;
            if stack.align.is_some() {
                mask |= PLACE_ALIGN;
            }
            if stack.layer != 0 {
                mask |= PLACE_LAYER;
            }
            enc.write_u8(mask);
            if let Some(align) = stack.align {
                enc.write_u8(align as u8);
            }
            if stack.layer != 0 {
                enc.write_varint_signed(i64::from(stack.layer));
            }
        }
        Placement::Absolute(insets) => {
            enc.write_u8(PLACE_ABSOLUTE);
            let edges = [insets.top, insets.end, insets.bottom, insets.start];
            let mut mask = 0u8;
            for (bit, edge) in edges.iter().enumerate() {
                if edge.is_some() {
                    mask |= 1 << bit;
                }
            }
            enc.write_u8(mask);
            for span in edges.into_iter().flatten() {
                encode_span(span, enc);
            }
        }
    }
}

fn decode_place(dec: &mut Decoder) -> Result<Placement, DecodeError> {
    let offset = dec.position();
    match dec.read_u8()? {
        PLACE_GRID => decode_cell(dec).map(Placement::Grid),
        PLACE_STACK => {
            let offset = dec.position();
            let mask = dec.read_u8()?;
            if mask & !(PLACE_ALIGN | PLACE_LAYER) != 0 {
                return Err(DecodeError::Malformed { offset });
            }
            let align = if mask & PLACE_ALIGN != 0 {
                Some(read_variant(dec, &Alignment2D::ALL)?)
            } else {
                None
            };
            let layer = if mask & PLACE_LAYER != 0 {
                let offset = dec.position();
                i32::try_from(dec.read_varint_signed()?)
                    .map_err(|_| DecodeError::Malformed { offset })?
            } else {
                0
            };
            Ok(Placement::Stack(StackPlacement { align, layer }))
        }
        PLACE_ABSOLUTE => {
            let offset = dec.position();
            let mask = dec.read_u8()?;
            if mask & !0b1111 != 0 {
                return Err(DecodeError::Malformed { offset });
            }
            let mut read = |bit: u8| -> Result<Option<Span>, DecodeError> {
                if mask & (1 << bit) != 0 {
                    decode_span(dec).map(Some)
                } else {
                    Ok(None)
                }
            };
            Ok(Placement::Absolute(AbsoluteInsets {
                top: read(0)?,
                end: read(1)?,
                bottom: read(2)?,
                start: read(3)?,
            }))
        }
        _ => Err(DecodeError::Malformed { offset }),
    }
}

fn encode_cell(place: GridPlacement, enc: &mut Encoder) {
    let mut mask = 0u8;
    if place.column.is_some() {
        mask |= PLACE_COLUMN;
    }
    if place.row.is_some() {
        mask |= PLACE_ROW;
    }
    if place.column_span != 1 {
        mask |= PLACE_COLUMN_SPAN;
    }
    if place.row_span != 1 {
        mask |= PLACE_ROW_SPAN;
    }
    enc.write_u8(mask);
    for value in [place.column, place.row].into_iter().flatten() {
        enc.write_varint(u64::from(value));
    }
    for (bit, span) in [
        (PLACE_COLUMN_SPAN, place.column_span),
        (PLACE_ROW_SPAN, place.row_span),
    ] {
        if mask & bit != 0 {
            enc.write_varint(u64::from(span));
        }
    }
}

fn decode_cell(dec: &mut Decoder) -> Result<GridPlacement, DecodeError> {
    let offset = dec.position();
    let mask = dec.read_u8()?;
    if mask & !(PLACE_COLUMN | PLACE_ROW | PLACE_COLUMN_SPAN | PLACE_ROW_SPAN) != 0 {
        return Err(DecodeError::Malformed { offset });
    }
    let mut read = |bit: u8, least: u16| -> Result<Option<u16>, DecodeError> {
        if mask & bit == 0 {
            return Ok(None);
        }
        let offset = dec.position();
        u16::try_from(dec.read_varint()?)
            .ok()
            .filter(|v| *v >= least)
            .map(Some)
            .ok_or(DecodeError::Malformed { offset })
    };
    Ok(GridPlacement {
        column: read(PLACE_COLUMN, 0)?,
        row: read(PLACE_ROW, 0)?,
        column_span: read(PLACE_COLUMN_SPAN, 1)?.unwrap_or(1),
        row_span: read(PLACE_ROW_SPAN, 1)?.unwrap_or(1),
    })
}

/// Writes one node record: the kind, the style and lengths, the child count.
fn encode_record(record: &AotRecord<'_>, enc: &mut Encoder) {
    let (lengths, rules, grid) = (record.lengths(), record.rules(), record.grid());
    let style = &record.style;
    enc.write_u8(record.kind.as_u8());
    let mut mask = 0u32;
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
    if lengths.is_some() {
        mask |= STYLE_HAS_LENGTHS;
    }
    if let Some(scope) = style.scope {
        mask |= STYLE_IS_SCOPE;
        if scope.basis.is_some() {
            mask |= STYLE_HAS_BASIS;
        }
        if scope.direction.is_some() {
            mask |= STYLE_HAS_DIRECTION;
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
    if rules.is_some() {
        mask |= STYLE_HAS_RULES;
    }
    if style.place.is_some() {
        mask |= STYLE_HAS_PLACE;
    }
    if grid.is_some() {
        mask |= STYLE_HAS_GRID;
    }
    if style.content_align.is_some() {
        mask |= STYLE_HAS_CONTENT_ALIGN;
    }
    if style.scroll.is_some() {
        mask |= STYLE_SCROLLS;
    }
    if style.sheet.is_some() {
        mask |= STYLE_SHEET;
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
    if let Some(lengths) = &lengths {
        lengths.encode(enc);
    }
    if let Some(basis) = style.scope.and_then(|scope| scope.basis) {
        enc.write_f32(basis);
    }
    if let Some(direction) = style.scope.and_then(|scope| scope.direction) {
        enc.write_u8(match direction {
            LayoutDirection::Ltr => 0,
            LayoutDirection::Rtl => 1,
        });
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
    if let Some(rules) = &rules {
        encode_rules(rules, enc);
    }
    if let Some(place) = style.place {
        encode_place(place, enc);
    }
    if let Some(grid) = &grid {
        encode_grid(grid, enc);
    }
    if let Some(align) = style.content_align {
        enc.write_u8(align as u8);
    }
    if let Some(scroll) = style.scroll {
        let snap = scroll.snap.map_or(0, |snap| snap as u8 + 1);
        let mut bits = snap << SCROLL_SNAP_SHIFT;
        for (on, bit) in [
            (scroll.bars, SCROLL_BARS),
            (scroll.tail, SCROLL_TAIL),
            (scroll.dots, SCROLL_DOTS),
            (scroll.snap_inset != 0.0, SCROLL_INSET),
        ] {
            if on {
                bits |= bit;
            }
        }
        enc.write_u8(bits);
        if scroll.snap_inset != 0.0 {
            enc.write_f32(scroll.snap_inset);
        }
    }
    if let Some(sheet) = style.sheet {
        let mut bits = sheet.edge as u8 | (sheet.count << SHEET_COUNT_SHIFT);
        if sheet.dismissible {
            bits |= SHEET_DISMISSIBLE;
        }
        enc.write_u8(bits);
        for detent in sheet.detents() {
            match *detent {
                SheetDetent::Full => enc.write_u8(0),
                SheetDetent::Fraction(f) => {
                    enc.write_u8(1);
                    enc.write_f32(f);
                }
                SheetDetent::Fixed(v) => {
                    enc.write_u8(2);
                    enc.write_f32(v);
                }
            }
        }
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
fn decode_record<'a>(dec: &mut Decoder<'a>) -> Result<AotRecord<'a>, DecodeError> {
    let kind = read_variant(
        dec,
        &[
            AotNodeKind::Flex,
            AotNodeKind::Grid,
            AotNodeKind::Scroll,
            AotNodeKind::Leaf,
            AotNodeKind::Stack,
            AotNodeKind::Absolute,
        ],
    )?;
    let offset = dec.position();
    let mask = u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset })?;
    if mask & (STYLE_HAS_BASIS | STYLE_HAS_DIRECTION) != 0 && mask & STYLE_IS_SCOPE == 0
        || mask >= STYLE_SHEET << 1
    {
        return Err(DecodeError::Malformed { offset });
    }
    let has = |bit: u32| mask & bit != 0;
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
        Some(read_validated(dec, |dec| {
            NodeLengths::decode(dec).map(drop)
        })?)
    } else {
        None
    };
    let scope = if has(STYLE_IS_SCOPE) {
        let basis = if has(STYLE_HAS_BASIS) {
            Some(dec.read_f32()?)
        } else {
            None
        };
        let direction = if has(STYLE_HAS_DIRECTION) {
            Some(read_variant(
                dec,
                &[LayoutDirection::Ltr, LayoutDirection::Rtl],
            )?)
        } else {
            None
        };
        Some(AotScope { basis, direction })
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
    let rules = if has(STYLE_HAS_RULES) {
        Some(read_validated(dec, |dec| decode_rules(dec).map(drop))?)
    } else {
        None
    };
    let place = if has(STYLE_HAS_PLACE) {
        Some(decode_place(dec)?)
    } else {
        None
    };
    let grid = if has(STYLE_HAS_GRID) {
        Some(read_validated(dec, |dec| {
            read_grid(dec, drop, drop).map(drop)
        })?)
    } else {
        None
    };
    let content_align = if has(STYLE_HAS_CONTENT_ALIGN) {
        Some(read_variant(dec, &Alignment2D::ALL)?)
    } else {
        None
    };
    let scroll = if has(STYLE_SCROLLS) {
        let offset = dec.position();
        let bits = dec.read_u8()?;
        let snap = match (bits >> SCROLL_SNAP_SHIFT) as usize {
            0 => None,
            tag => Some(
                *AotSnap::ALL
                    .get(tag - 1)
                    .ok_or(DecodeError::Malformed { offset })?,
            ),
        };
        let snap_inset = if bits & SCROLL_INSET != 0 {
            dec.read_f32()?
        } else {
            0.0
        };
        Some(AotScroll {
            bars: bits & SCROLL_BARS != 0,
            tail: bits & SCROLL_TAIL != 0,
            snap,
            snap_inset,
            dots: bits & SCROLL_DOTS != 0,
        })
    } else {
        None
    };
    let sheet = if has(STYLE_SHEET) {
        let offset = dec.position();
        let bits = dec.read_u8()?;
        let edge = SheetEdge::ALL[usize::from(bits & 3)];
        let count = usize::from(bits >> SHEET_COUNT_SHIFT);
        if count > AotSheet::MAX_DETENTS {
            return Err(DecodeError::Malformed { offset });
        }
        let mut detents = [SheetDetent::Full; AotSheet::MAX_DETENTS];
        for detent in &mut detents[..count] {
            let offset = dec.position();
            *detent = match dec.read_u8()? {
                0 => SheetDetent::Full,
                1 => SheetDetent::Fraction(dec.read_f32()?),
                2 => SheetDetent::Fixed(dec.read_f32()?),
                _ => return Err(DecodeError::Malformed { offset }),
            };
        }
        AotSheet::new(edge, &detents[..count], bits & SHEET_DISMISSIBLE != 0)
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
            content_align,
            place,
            scroll,
            sheet,
        },
        child_count,
        extras: Extras::Encoded {
            lengths,
            rules,
            grid,
        },
    })
}

/// The bytes `read` validates at `dec`'s position, past which it moves.
fn read_validated<'a>(
    dec: &mut Decoder<'a>,
    read: impl FnOnce(&mut Decoder<'a>) -> Result<(), DecodeError>,
) -> Result<&'a [u8], DecodeError> {
    let start = dec.position();
    read(dec)?;
    Ok(&dec.input()[start..dec.position()])
}

impl Encode for AotRecord<'_> {
    #[inline]
    fn encode(&self, enc: &mut Encoder) {
        encode_record(self, enc);
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

impl<'a> Iterator for AotNodes<'a> {
    type Item = AotRecord<'a>;

    #[inline]
    fn next(&mut self) -> Option<AotRecord<'a>> {
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
                    kind: AotNodeKind::Grid,
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
                        scope: Some(AotScope {
                            basis: Some(600.0),
                            direction: Some(LayoutDirection::Rtl),
                        }),
                        avoid: None,
                        content_align: Some(Alignment2D::BottomEnd),
                        scroll: Some(AotScroll {
                            bars: false,
                            tail: true,
                            snap: Some(AotSnap::Pages),
                            snap_inset: 12.0,
                            dots: true,
                        }),
                        sheet: AotSheet::new(
                            SheetEdge::Trailing,
                            &[
                                SheetDetent::Fraction(0.5),
                                SheetDetent::Fixed(120.0),
                                SheetDetent::Full,
                            ],
                            false,
                        ),
                        place: None,
                    },
                    lengths: None,
                    rules: Some(Box::new(BoxRules {
                        min_width: Span::dp(40.0),
                        max_height: Span::pct(80.0),
                        margin: SpanInset {
                            left: Span::dp(-4.0),
                            bottom: Span {
                                fixed: 2.0,
                                pct: 0.1,
                            },
                            ..SpanInset::ZERO
                        },
                        padding_pct: Inset {
                            top: 0.05,
                            ..Inset::default()
                        },
                        ..BoxRules::NONE
                    })),
                    grid: Some(Box::new(AotGrid {
                        columns: vec![
                            TrackSizing::Fixed(80.0),
                            TrackSizing::Fr(2.0),
                            TrackSizing::Auto,
                            TrackSizing::Percent(0.25),
                            TrackSizing::Minmax(10.0, 90.0),
                            TrackSizing::FitContent(120.0),
                            TrackSizing::FlexMin(40.0, 1.5),
                            TrackSizing::Relative {
                                fixed: 12.0,
                                pct: 0.25,
                            },
                            TrackSizing::Length(crate::length::LengthTerms {
                                sp: 4.0,
                                em: 2.0,
                                pct: 0.1,
                                ..crate::length::LengthTerms::ZERO
                            }),
                        ],
                        rows: vec![TrackSizing::Fixed(24.0)],
                        auto_rows: TrackSizing::Fr(1.0),
                        column_gap: 6.0,
                        row_gap: 3.0,
                        align_items: AlignItems::Baseline,
                        adaptive_columns: Some(AdaptiveColumns {
                            mode: AutoRepeat::Fit,
                            min: 100.0,
                            max: TrackMax::Px(160.0),
                        }),
                    })),
                    child_count: 3,
                },
                AotNode {
                    kind: AotNodeKind::Scroll,
                    style: AotStyle {
                        axis: Some(AotAxis::Row),
                        width: Some(AotLength::Fill { weight: 2.0 }),
                        height: Some(AotLength::Fit),
                        avoid: Some(Avoid::Keyboard),
                        place: Some(Placement::Stack(StackPlacement {
                            align: Some(Alignment2D::Center),
                            layer: -3,
                        })),
                        ..AotStyle::default()
                    },
                    lengths: Some(Box::new(NodeLengths {
                        width: Some(LengthTerms::pct(50.0) - LengthTerms::px(1.0)),
                        font_size: Some(LengthTerms::em(1.25)),
                        padding: Some(EdgeTerms {
                            start: LengthTerms::sp(3.0),
                            ..EdgeTerms::all(LengthTerms::dp(1.0))
                        }),
                        max_height: Some(LengthTerms::em(20.0)),
                        row_gap: Some(LengthTerms::sp(4.0)),
                        margin: Some(EdgeTerms {
                            end: LengthTerms::px(2.0),
                            ..EdgeTerms::default()
                        }),
                        insets: Some(InsetTerms {
                            start: Some(LengthTerms::em(1.0)),
                            bottom: Some(LengthTerms::pct(10.0)),
                            ..InsetTerms::default()
                        }),
                        corner_radius: Some(LengthTerms::sp(2.0) + LengthTerms::pct(10.0)),
                        ..NodeLengths::default()
                    })),
                    rules: None,
                    grid: None,
                    child_count: 0,
                },
                AotNode {
                    kind: AotNodeKind::Leaf,
                    style: AotStyle {
                        place: Some(Placement::Grid(GridPlacement {
                            column: Some(3),
                            row: None,
                            column_span: 2,
                            row_span: 300,
                        })),
                        ..AotStyle::default()
                    },
                    lengths: None,
                    rules: None,
                    grid: None,
                    child_count: 0,
                },
                AotNode {
                    kind: AotNodeKind::Absolute,
                    style: AotStyle {
                        place: Some(Placement::Absolute(AbsoluteInsets {
                            top: Some(Span::dp(4.0)),
                            end: Some(Span {
                                fixed: -2.0,
                                pct: 0.5,
                            }),
                            ..AbsoluteInsets::NONE
                        })),
                        ..AotStyle::default()
                    },
                    lengths: None,
                    rules: None,
                    grid: None,
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
            rules: None,
            grid: None,
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
        assert_eq!(view.node_count(), 4);
        let records: Vec<AotRecord> = view.nodes().collect();
        let owned: Vec<AotRecord> = pkg.nodes.iter().map(AotNode::record).collect();
        assert_eq!(records, owned);
        assert_eq!(view.nodes().len(), 4);
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
    fn a_zero_span_an_unknown_track_or_an_overlong_template_is_rejected() {
        let encoded = |node: AotNode| {
            AotPackage {
                nodes: vec![node],
                edges: Vec::new(),
            }
            .encode_to_vec()
        };
        // The kind byte and the two-byte style mask come first.
        let at = NODE_SECTION + 3;
        let placed = encoded(AotNode {
            style: AotStyle {
                place: Some(Placement::Grid(GridPlacement {
                    column_span: 2,
                    ..GridPlacement::default()
                })),
                ..AotStyle::default()
            },
            ..leaf()
        });
        assert!(AotView::new(&placed).is_ok());
        assert_eq!(placed[at..at + 3], [PLACE_GRID, PLACE_COLUMN_SPAN, 2]);
        let mut bad = placed.clone();
        bad[at + 2] = 0;
        assert!(AotView::new(&bad).is_err(), "a zero span");
        let mut bad = placed.clone();
        bad[at] = 3;
        assert!(AotView::new(&bad).is_err(), "an unknown parent");
        let aligned = encoded(AotNode {
            style: AotStyle {
                place: Some(Placement::Stack(StackPlacement {
                    align: Some(Alignment2D::Top),
                    layer: 0,
                })),
                ..AotStyle::default()
            },
            ..leaf()
        });
        assert_eq!(aligned[at..at + 3], [PLACE_STACK, PLACE_ALIGN, 1]);
        let mut bad = aligned.clone();
        bad[at + 2] = 9;
        assert!(AotView::new(&bad).is_err(), "an unknown alignment");
        let grid = encoded(AotNode {
            kind: AotNodeKind::Grid,
            grid: Some(Box::new(AotGrid {
                columns: vec![TrackSizing::Auto],
                ..AotGrid::default()
            })),
            ..leaf()
        });
        assert!(AotView::new(&grid).is_ok());
        assert_eq!(grid[at..at + 3], [GRID_COLUMNS, 1, TRACK_AUTO]);
        let mut bad = grid.clone();
        bad[at + 2] = 0x7e;
        assert!(AotView::new(&bad).is_err(), "an unknown track");
        let mut bad = grid.clone();
        bad[at + 1] = 0x7f;
        assert!(AotView::new(&bad).is_err(), "more tracks than bytes");
    }

    #[test]
    fn an_edge_to_a_missing_node_is_rejected() {
        let mut pkg = sample_package();
        pkg.edges[0].node = 4;
        assert!(AotView::new(&pkg.encode_to_vec()).is_err());
    }
}
