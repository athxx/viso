//! What an inspector asks a running app about past its census: the full
//! state of chosen nodes, and what a window's last frame drew and holds.
//!
//! A [`NodeDetail`] is a node as the runtime holds it: its layout, style and
//! content properties as it resolved them, the boxes and offsets that place
//! it, the classes it is dirty for, the handlers its events reach on their
//! way, the cells whose changes invalidate it, and its span of the paint
//! stream. What those cells are called and where the bound properties are
//! written is the tool's to know, from the [`SourceTable`](super::SourceTable).
//! A [`FrameDetail`] is a window's frame: its counters, its draw batches and
//! the device memory and caches behind them.

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder};
use viso_math::Rect;
use viso_schema::{DirtyClass, Interaction, StateValue};

use super::census::{count, malformed, read_option_str, write_option_str};

/// What an inspector asks a running app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectAsk {
    /// Every window's [`Census`](super::Census).
    Census,
    /// The detail of `nodes` of window `window`, by index and generation.
    Nodes { window: u32, nodes: Vec<(u32, u32)> },
    /// What window `window`'s last frame drew and holds.
    Frame { window: u32 },
    /// The events window `window` recorded after number `after`; the first
    /// ask starts its journal.
    Events { window: u32, after: u64 },
}

/// The most nodes one [`InspectAsk::Nodes`] names.
pub const MAX_ASKED_NODES: usize = 4096;

impl Encode for InspectAsk {
    fn encode(&self, enc: &mut Encoder) {
        match self {
            InspectAsk::Census => enc.write_u8(0),
            InspectAsk::Nodes { window, nodes } => {
                enc.write_u8(1);
                enc.write_varint(u64::from(*window));
                enc.write_varint(nodes.len() as u64);
                for (index, generation) in nodes {
                    enc.write_varint(u64::from(*index));
                    enc.write_varint(u64::from(*generation));
                }
            }
            InspectAsk::Frame { window } => {
                enc.write_u8(2);
                enc.write_varint(u64::from(*window));
            }
            InspectAsk::Events { window, after } => {
                enc.write_u8(3);
                enc.write_varint(u64::from(*window));
                enc.write_varint(*after);
            }
        }
    }
}

impl Decode for InspectAsk {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(match dec.read_u8()? {
            0 => InspectAsk::Census,
            1 => {
                let window = read_u32(dec)?;
                let len = count(dec)?;
                if len > MAX_ASKED_NODES {
                    return Err(malformed(dec));
                }
                let nodes = (0..len)
                    .map(|_| Ok((read_u32(dec)?, read_u32(dec)?)))
                    .collect::<Result<_, DecodeError>>()?;
                InspectAsk::Nodes { window, nodes }
            }
            2 => InspectAsk::Frame {
                window: read_u32(dec)?,
            },
            3 => InspectAsk::Events {
                window: read_u32(dec)?,
                after: dec.read_varint()?,
            },
            _ => return Err(malformed(dec)),
        })
    }
}

/// What a runtime's inspector answers, as its hello names it. A tool asks
/// for nothing else, and reports the rest unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct InspectorCaps(u32);

impl InspectorCaps {
    pub const NONE: InspectorCaps = InspectorCaps(0);
    /// The census: every node, its box, semantics and source.
    pub const TREE: InspectorCaps = InspectorCaps(1);
    /// [`NodeDetail`]s.
    pub const NODES: InspectorCaps = InspectorCaps(1 << 1);
    /// [`FrameDetail`]s.
    pub const FRAME: InspectorCaps = InspectorCaps(1 << 2);
    /// The time the GPU spent on a frame.
    pub const GPU_TIME: InspectorCaps = InspectorCaps(1 << 3);
    /// The stream of input routes, focus moves and dirty reasons.
    pub const EVENTS: InspectorCaps = InspectorCaps(1 << 4);
    /// Every capability, with its name.
    pub const ALL: [(InspectorCaps, &'static str); 5] = [
        (InspectorCaps::TREE, "tree"),
        (InspectorCaps::NODES, "nodes"),
        (InspectorCaps::FRAME, "frame"),
        (InspectorCaps::GPU_TIME, "gpu_time"),
        (InspectorCaps::EVENTS, "events"),
    ];

    pub const fn with(self, other: InspectorCaps) -> InspectorCaps {
        InspectorCaps(self.0 | other.0)
    }

    pub const fn contains(self, other: InspectorCaps) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// The capabilities `bits` names; bits this build does not know are
    /// kept, so a newer runtime's are not mistaken for none.
    pub const fn from_bits(bits: u32) -> InspectorCaps {
        InspectorCaps(bits)
    }

    /// The names of the capabilities it has.
    pub fn names(self) -> impl Iterator<Item = &'static str> {
        InspectorCaps::ALL
            .into_iter()
            .filter(move |(cap, _)| self.contains(*cap))
            .map(|(_, name)| name)
    }
}

/// The nodes an [`InspectAsk::Nodes`] named that are live, in its order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeDetails {
    pub nodes: Vec<NodeDetail>,
}

/// One node as the runtime holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeDetail {
    pub index: u32,
    pub generation: u32,
    /// Its properties as resolved, by group: `layout`, `style`, `content`.
    pub groups: Vec<PropertyGroup>,
    /// Its box in window coordinates, scrolling and offsets applied.
    pub world: Rect,
    /// Its unscrolled layout box, in its parent's coordinates.
    pub bounds: Rect,
    /// The nodes from the root down to it whose offset or scroll moves it.
    pub chain: Vec<ChainLink>,
    /// The classes it is dirty for, as [`DirtyClass`] bits.
    pub dirty: u8,
    /// The interaction states it is in, a bit per [`Interaction`] tag.
    pub states: u8,
    /// The nodes from the root down to it that an event reaching it passes:
    /// those with a handler, or taking focus.
    pub route: Vec<RouteStop>,
    /// The cells whose change invalidates it.
    pub bindings: Vec<NodeBinding>,
    /// Its span of the window's paint stream, its subtree's included, and
    /// the kinds of primitive it draws itself.
    pub paint: PaintSpan,
}

/// Named properties of one aspect of a node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyGroup {
    pub name: String,
    pub properties: Vec<(String, String)>,
}

/// A node above (or at) the inspected one that moves it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChainLink {
    pub index: u32,
    pub generation: u32,
    /// Its offset, by a transform or an animation.
    pub translate: [f32; 2],
    /// How far its content is scrolled.
    pub scroll: [f32; 2],
}

/// A node an event passes on its way to the inspected one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteStop {
    pub index: u32,
    pub generation: u32,
    /// Whether it handles pointer events.
    pub pointer: bool,
    /// Whether it handles key events.
    pub key: bool,
    /// Whether it takes focus.
    pub focusable: bool,
    /// Whether it turned input off for itself and its subtree.
    pub disabled: bool,
}

/// A cell that invalidates the node when it changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeBinding {
    /// The cell, by index and generation.
    pub cell: (u32, u32),
    /// Its durable identity when the compiler declared it: what a source
    /// table names it by.
    pub key: Option<(u64, u64)>,
    /// The classes its change marks, as [`DirtyClass`] bits.
    pub class: u8,
    /// Whether the edge is a dynamic one.
    pub dynamic: bool,
    /// The cell's current value, as source would spell it.
    pub value: String,
}

/// A node's span of the paint stream.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PaintSpan {
    pub start: u32,
    pub len: u32,
    /// The primitives it draws itself, by kind, in the order it draws them.
    pub own: Vec<(String, u32)>,
    /// The draw batches its own primitives land in, in submission order, each
    /// with the number of its draws there; a culled primitive or a marker
    /// draws nothing.
    pub batches: Vec<(u32, u32)>,
}

/// What a window's last frame drew and what it holds to draw it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrameDetail {
    pub window: u32,
    /// The frame's counters, by name.
    pub counters: Vec<(String, u64)>,
    /// The draw batches, in submission order.
    pub batches: Vec<BatchDetail>,
    /// The device memory and caches behind them.
    pub resources: Vec<ResourceDetail>,
    /// What its present redrew of the surface (§30.3).
    pub damage: FrameDamage,
}

/// What a frame's present redrew of the surface, in viewport units.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum FrameDamage {
    /// Nothing: no frame was presented, or nothing on the surface changed.
    #[default]
    None,
    /// The whole surface.
    Whole,
    /// Only these rects, disjoint.
    Rects(Vec<Rect>),
}

/// One draw command of a frame.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchDetail {
    /// The pipeline it draws through.
    pub pipeline: String,
    /// Its geometry range in its buffer: instances, or indices for meshes.
    pub range: (u32, u32),
    /// Its clip, `None` when unclipped.
    pub clip: Option<Rect>,
    /// Whether it draws into an offscreen pass.
    pub offscreen: bool,
    /// Why it costs what it costs.
    pub cost: String,
}

/// A resource or cache: what it holds, and how much.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceDetail {
    /// `gpu`, `cache` or `asset`.
    pub group: String,
    pub name: String,
    pub count: u64,
    /// Its bytes, when they are known.
    pub bytes: Option<u64>,
    /// The most it may hold, when bounded.
    pub budget: Option<u64>,
    /// What else there is to say of it: an asset's state and size.
    pub note: Option<String>,
}

/// Everything of one window past its census, as a headless run writes it:
/// the detail of each of its nodes, and its last frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WindowDetail {
    pub nodes: NodeDetails,
    pub frame: FrameDetail,
}

impl WindowDetail {
    /// Its wire form.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }

    /// The detail `bytes` spell.
    ///
    /// # Errors
    ///
    /// Bytes that are not a window's detail.
    pub fn from_bytes(bytes: &[u8]) -> Result<WindowDetail, DecodeError> {
        WindowDetail::decode_from_slice(bytes)
    }
}

impl Encode for WindowDetail {
    fn encode(&self, enc: &mut Encoder) {
        self.nodes.encode(enc);
        self.frame.encode(enc);
    }
}

impl Decode for WindowDetail {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        Ok(WindowDetail {
            nodes: NodeDetails::decode(dec)?,
            frame: FrameDetail::decode(dec)?,
        })
    }
}

/// A cell's value as source would spell it.
pub fn spell_value(value: StateValue) -> String {
    match value {
        StateValue::Int(v) => v.to_string(),
        StateValue::Float(v) => format!("{v:?}"),
        StateValue::Bool(v) => v.to_string(),
        StateValue::Color(r, g, b, a) => {
            let byte = |c: f32| (c.clamp(0.0, 1.0) * 255.0).round() as u8;
            format!(
                "#{:02x}{:02x}{:02x}{:02x}",
                byte(r),
                byte(g),
                byte(b),
                byte(a)
            )
        }
    }
}

/// [`DirtyClass`] bits by name, lowercase.
pub fn dirty_names(bits: u8) -> impl Iterator<Item = &'static str> {
    [
        (DirtyClass::STRUCTURE, "structure"),
        (DirtyClass::STYLE, "style"),
        (DirtyClass::MEASURE, "measure"),
        (DirtyClass::LAYOUT, "layout"),
        (DirtyClass::TRANSFORM, "transform"),
        (DirtyClass::PAINT, "paint"),
        (DirtyClass::HIT_TEST, "hit_test"),
        (DirtyClass::SEMANTICS, "semantics"),
    ]
    .into_iter()
    .filter(move |(class, _)| DirtyClass::from_bits(bits).contains(*class))
    .map(|(_, name)| name)
}

/// [`Interaction`] bits by name.
pub fn state_names(bits: u8) -> impl Iterator<Item = &'static str> {
    Interaction::ALL
        .into_iter()
        .filter(move |state| bits & 1 << state.tag() != 0)
        .map(|state| match state {
            Interaction::Hovered => "hovered",
            Interaction::Pressed => "pressed",
            Interaction::Focused => "focused",
            Interaction::FocusVisible => "focus_visible",
            Interaction::Selected => "selected",
            Interaction::Dragging => "dragging",
        })
}

fn read_u32(dec: &mut Decoder<'_>) -> Result<u32, DecodeError> {
    u32::try_from(dec.read_varint()?).map_err(|_| malformed(dec))
}

fn write_rect(enc: &mut Encoder, rect: Rect) {
    for v in [rect.x, rect.y, rect.w, rect.h] {
        enc.write_f32(v);
    }
}

fn read_rect(dec: &mut Decoder<'_>) -> Result<Rect, DecodeError> {
    Ok(Rect {
        x: dec.read_f32()?,
        y: dec.read_f32()?,
        w: dec.read_f32()?,
        h: dec.read_f32()?,
    })
}

fn write_pairs(enc: &mut Encoder, pairs: &[(String, String)]) {
    enc.write_varint(pairs.len() as u64);
    for (name, value) in pairs {
        enc.write_str(name);
        enc.write_str(value);
    }
}

fn read_pairs(dec: &mut Decoder<'_>) -> Result<Vec<(String, String)>, DecodeError> {
    (0..count(dec)?)
        .map(|_| Ok((dec.read_str()?.to_owned(), dec.read_str()?.to_owned())))
        .collect()
}

fn write_option_u64(enc: &mut Encoder, value: Option<u64>) {
    enc.write_varint(value.map_or(0, |v| v.saturating_add(1)));
}

fn read_option_u64(dec: &mut Decoder<'_>) -> Result<Option<u64>, DecodeError> {
    Ok(dec.read_varint()?.checked_sub(1))
}

impl NodeDetails {
    /// Its wire form.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }

    /// The details `bytes` spell.
    ///
    /// # Errors
    ///
    /// Bytes that are not node details.
    pub fn from_bytes(bytes: &[u8]) -> Result<NodeDetails, DecodeError> {
        NodeDetails::decode_from_slice(bytes)
    }
}

impl Encode for NodeDetails {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_varint(self.nodes.len() as u64);
        for node in &self.nodes {
            node.encode(enc);
        }
    }
}

impl Decode for NodeDetails {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let nodes = (0..count(dec)?)
            .map(|_| NodeDetail::decode(dec))
            .collect::<Result<_, _>>()?;
        Ok(NodeDetails { nodes })
    }
}

impl Encode for NodeDetail {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_varint(u64::from(self.index));
        enc.write_varint(u64::from(self.generation));
        enc.write_varint(self.groups.len() as u64);
        for group in &self.groups {
            enc.write_str(&group.name);
            write_pairs(enc, &group.properties);
        }
        write_rect(enc, self.world);
        write_rect(enc, self.bounds);
        enc.write_varint(self.chain.len() as u64);
        for link in &self.chain {
            enc.write_varint(u64::from(link.index));
            enc.write_varint(u64::from(link.generation));
            for v in link.translate.into_iter().chain(link.scroll) {
                enc.write_f32(v);
            }
        }
        enc.write_u8(self.dirty);
        enc.write_u8(self.states);
        enc.write_varint(self.route.len() as u64);
        for stop in &self.route {
            enc.write_varint(u64::from(stop.index));
            enc.write_varint(u64::from(stop.generation));
            enc.write_u8(
                u8::from(stop.pointer)
                    | u8::from(stop.key) << 1
                    | u8::from(stop.focusable) << 2
                    | u8::from(stop.disabled) << 3,
            );
        }
        enc.write_varint(self.bindings.len() as u64);
        for binding in &self.bindings {
            enc.write_varint(u64::from(binding.cell.0));
            enc.write_varint(u64::from(binding.cell.1));
            match binding.key {
                Some((hi, lo)) => {
                    enc.write_u8(1);
                    enc.write_u64(hi);
                    enc.write_u64(lo);
                }
                None => enc.write_u8(0),
            }
            enc.write_u8(binding.class);
            enc.write_bool(binding.dynamic);
            enc.write_str(&binding.value);
        }
        enc.write_varint(u64::from(self.paint.start));
        enc.write_varint(u64::from(self.paint.len));
        enc.write_varint(self.paint.own.len() as u64);
        for (kind, n) in &self.paint.own {
            enc.write_str(kind);
            enc.write_varint(u64::from(*n));
        }
        enc.write_varint(self.paint.batches.len() as u64);
        for &(batch, n) in &self.paint.batches {
            enc.write_varint(u64::from(batch));
            enc.write_varint(u64::from(n));
        }
    }
}

impl Decode for NodeDetail {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let index = read_u32(dec)?;
        let generation = read_u32(dec)?;
        let groups = (0..count(dec)?)
            .map(|_| {
                Ok(PropertyGroup {
                    name: dec.read_str()?.to_owned(),
                    properties: read_pairs(dec)?,
                })
            })
            .collect::<Result<_, DecodeError>>()?;
        let world = read_rect(dec)?;
        let bounds = read_rect(dec)?;
        let chain = (0..count(dec)?)
            .map(|_| {
                Ok(ChainLink {
                    index: read_u32(dec)?,
                    generation: read_u32(dec)?,
                    translate: [dec.read_f32()?, dec.read_f32()?],
                    scroll: [dec.read_f32()?, dec.read_f32()?],
                })
            })
            .collect::<Result<_, DecodeError>>()?;
        let dirty = dec.read_u8()?;
        let states = dec.read_u8()?;
        if states >> Interaction::ALL.len() != 0 {
            return Err(malformed(dec));
        }
        let route = (0..count(dec)?)
            .map(|_| {
                let index = read_u32(dec)?;
                let generation = read_u32(dec)?;
                let flags = dec.read_u8()?;
                if flags > 0b1111 {
                    return Err(malformed(dec));
                }
                Ok(RouteStop {
                    index,
                    generation,
                    pointer: flags & 1 != 0,
                    key: flags & 2 != 0,
                    focusable: flags & 4 != 0,
                    disabled: flags & 8 != 0,
                })
            })
            .collect::<Result<_, DecodeError>>()?;
        let bindings = (0..count(dec)?)
            .map(|_| {
                let cell = (read_u32(dec)?, read_u32(dec)?);
                let key = match dec.read_u8()? {
                    0 => None,
                    1 => Some((dec.read_u64()?, dec.read_u64()?)),
                    _ => return Err(malformed(dec)),
                };
                Ok(NodeBinding {
                    cell,
                    key,
                    class: dec.read_u8()?,
                    dynamic: dec.read_bool()?,
                    value: dec.read_str()?.to_owned(),
                })
            })
            .collect::<Result<_, DecodeError>>()?;
        let start = read_u32(dec)?;
        let len = read_u32(dec)?;
        let own = (0..count(dec)?)
            .map(|_| Ok((dec.read_str()?.to_owned(), read_u32(dec)?)))
            .collect::<Result<_, DecodeError>>()?;
        let batches = (0..count(dec)?)
            .map(|_| Ok((read_u32(dec)?, read_u32(dec)?)))
            .collect::<Result<_, DecodeError>>()?;
        Ok(NodeDetail {
            index,
            generation,
            groups,
            world,
            bounds,
            chain,
            dirty,
            states,
            route,
            bindings,
            paint: PaintSpan {
                start,
                len,
                own,
                batches,
            },
        })
    }
}

impl FrameDetail {
    /// Its wire form.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode_to_vec()
    }

    /// The frame `bytes` spell.
    ///
    /// # Errors
    ///
    /// Bytes that are not a frame detail.
    pub fn from_bytes(bytes: &[u8]) -> Result<FrameDetail, DecodeError> {
        FrameDetail::decode_from_slice(bytes)
    }
}

impl Encode for FrameDetail {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_varint(u64::from(self.window));
        enc.write_varint(self.counters.len() as u64);
        for (name, value) in &self.counters {
            enc.write_str(name);
            enc.write_varint(*value);
        }
        enc.write_varint(self.batches.len() as u64);
        for batch in &self.batches {
            enc.write_str(&batch.pipeline);
            enc.write_varint(u64::from(batch.range.0));
            enc.write_varint(u64::from(batch.range.1));
            match batch.clip {
                Some(clip) => {
                    enc.write_u8(1);
                    write_rect(enc, clip);
                }
                None => enc.write_u8(0),
            }
            enc.write_bool(batch.offscreen);
            enc.write_str(&batch.cost);
        }
        enc.write_varint(self.resources.len() as u64);
        for resource in &self.resources {
            enc.write_str(&resource.group);
            enc.write_str(&resource.name);
            enc.write_varint(resource.count);
            write_option_u64(enc, resource.bytes);
            write_option_u64(enc, resource.budget);
            write_option_str(enc, resource.note.as_deref());
        }
        match &self.damage {
            FrameDamage::None => enc.write_u8(0),
            FrameDamage::Whole => enc.write_u8(1),
            FrameDamage::Rects(rects) => {
                enc.write_u8(2);
                enc.write_varint(rects.len() as u64);
                for &r in rects {
                    write_rect(enc, r);
                }
            }
        }
    }
}

impl Decode for FrameDetail {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let window = read_u32(dec)?;
        let counters = (0..count(dec)?)
            .map(|_| Ok((dec.read_str()?.to_owned(), dec.read_varint()?)))
            .collect::<Result<_, DecodeError>>()?;
        let batches = (0..count(dec)?)
            .map(|_| {
                let pipeline = dec.read_str()?.to_owned();
                let range = (read_u32(dec)?, read_u32(dec)?);
                let clip = match dec.read_u8()? {
                    0 => None,
                    1 => Some(read_rect(dec)?),
                    _ => return Err(malformed(dec)),
                };
                Ok(BatchDetail {
                    pipeline,
                    range,
                    clip,
                    offscreen: dec.read_bool()?,
                    cost: dec.read_str()?.to_owned(),
                })
            })
            .collect::<Result<_, DecodeError>>()?;
        let resources = (0..count(dec)?)
            .map(|_| {
                Ok(ResourceDetail {
                    group: dec.read_str()?.to_owned(),
                    name: dec.read_str()?.to_owned(),
                    count: dec.read_varint()?,
                    bytes: read_option_u64(dec)?,
                    budget: read_option_u64(dec)?,
                    note: read_option_str(dec)?,
                })
            })
            .collect::<Result<_, DecodeError>>()?;
        let damage = match dec.read_u8()? {
            0 => FrameDamage::None,
            1 => FrameDamage::Whole,
            2 => FrameDamage::Rects(
                (0..count(dec)?)
                    .map(|_| read_rect(dec))
                    .collect::<Result<_, DecodeError>>()?,
            ),
            _ => return Err(malformed(dec)),
        };
        Ok(FrameDetail {
            window,
            counters,
            batches,
            resources,
            damage,
        })
    }
}
