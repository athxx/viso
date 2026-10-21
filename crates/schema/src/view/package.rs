//! The release form of a view with behavior: its UI package, its behavior
//! module, the tables that join them, the bytes of the assets it shows and
//! the shaders it paints with.
//!
//! # Wire form
//!
//! The blob is read in place (architecture §50A.6): a [`ProtocolTag`], the
//! magic `VPK1`, then a section table ([`viso_ende::Sections`]) of eleven
//! sections, each starting on an eight-byte boundary of the blob:
//!
//! | section   | items | content |
//! |-----------|-------|---------|
//! | ui        | —     | the [`AotPackage`] blob |
//! | behavior  | —     | the behavior module blob, empty for a view without handlers |
//! | component | —     | the component's name, UTF-8 |
//! | states    | cells | 40-byte records: key, initial value, slot, kind, tracked |
//! | handlers  | routes | 12-byte records: node, handler, route tag |
//! | controls  | controls | a `u32` node, then the encoded [`Control`] |
//! | regions   | —     | the [`ViewRegions`] blob |
//! | env       | reads | 12-byte records: slot, anchor, field |
//! | assets    | assets | per asset a `u64` id, a `u32` length, four zero bytes, the bytes, padding to eight |
//! | shaders   | shaders | per shader a `u32` length, four zero bytes, the packed brush, padding to eight |
//! | named     | nodes | 8-byte records: `NodeRef` key, node |
//!
//! Fixed-width records are read in place; the bytes of an asset and of a
//! shader start aligned, so a decoder reads them from mapped flash where they
//! lie. [`ViewPackageView`] validates the whole blob up front and reads it
//! borrowed; [`ViewPackage`] is the owned form the emitter builds and the
//! development commit and dev link carry, decoded through the same view.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use viso_ende::{
    Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag, SECTION_ALIGN, Section,
    SectionWriter, Sections,
};

use crate::aot::{AotPackage, AotView};
use crate::env::EnvField;
use crate::resource::{AssetBytes, ResourceId};
use crate::state::{StateKey, StateValue};
use crate::view::control::Control;
use crate::view::regions::ViewRegions;
use crate::view::route::Route;

/// A compiled view with behavior, as a release build embeds it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ViewPackage {
    /// The retained tree and its binding edges.
    pub ui: AotPackage,
    /// The encoded behavior module, empty for a view without handlers.
    pub behavior: Vec<u8>,
    /// The component of [`behavior`](Self::behavior) the view mounts.
    pub component: String,
    /// The view's state cells, allocated with their initial values before the
    /// tree mounts.
    pub states: Vec<ViewState>,
    /// The node handlers, grouped by node in ascending node order.
    pub handlers: Vec<ViewHandler>,
    /// The native control nodes, in ascending node order.
    pub controls: Vec<ViewControl>,
    /// The control-flow regions, mounted under the nodes of
    /// [`ui`](Self::ui).
    pub regions: ViewRegions,
    /// The `env` fields the view's static instances read.
    pub env: Vec<ViewEnv>,
    /// The static nodes the view's actions and handlers reach as a
    /// `NodeRef`, ascending by key.
    pub named: Vec<ViewNamed>,
    /// The assets the view can show, ascending by resource.
    pub assets: Vec<ViewAsset>,
    /// The view's shader table: each shader its shader views paint with, in
    /// the order their controls index it.
    pub shaders: Vec<ViewShader>,
}

/// A shader a view paints with: the packed brush its build produced, in the
/// languages its targets' backends read. The runtime loads it into a program
/// when the view mounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewShader(Arc<[u8]>);

impl ViewShader {
    /// The shader the packed brush `bytes` encodes.
    pub fn new(bytes: impl Into<Arc<[u8]>>) -> ViewShader {
        ViewShader(bytes.into())
    }

    /// The packed brush, as the package carries it.
    pub fn bytes(&self) -> &[u8] {
        &self.0
    }
}

/// An asset a view can show: its resource and encoded bytes, decoded only
/// when a node shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewAsset {
    pub id: ResourceId,
    pub bytes: AssetBytes,
}

/// An `env` field a view reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewEnv {
    /// The component's state slot holding it.
    pub slot: u32,
    /// The field.
    pub field: EnvField,
    /// The pre-order index in [`ViewPackage::ui`] of the root node of the
    /// component instance that reads it, where its anchored fields resolve.
    pub anchor: u32,
}

/// A static node of [`ViewPackage::ui`] its component reaches as a `NodeRef`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewNamed {
    /// The key its `NodeRef` holds: the inlined instance whose view names it
    /// in the high 16 bits, its place among that component's named nodes in
    /// the low 16.
    pub key: u32,
    /// Its pre-order index in [`ViewPackage::ui`].
    pub node: u32,
}

/// A native control node of [`ViewPackage::ui`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewControl {
    /// The pre-order index of the node in [`ViewPackage::ui`].
    pub node: u32,
    /// Its built-in response and the entries it reads.
    pub control: Control,
}

/// A component state held in a UI cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewState {
    /// The cell's durable identity, the key the UI package's edges name.
    pub key: StateKey,
    /// The component's state slot the cell mirrors, `None` for a cell no
    /// handler writes.
    pub slot: Option<u32>,
    /// Whether the cell holds a revision of a state no cell can hold (a
    /// string, a list) rather than its value.
    pub tracked: bool,
    /// The cell's initial value.
    pub initial: StateValue,
}

/// One node handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewHandler {
    /// The pre-order index of the node in [`ViewPackage::ui`].
    pub node: u32,
    /// The event it runs on, its handler and its phase.
    pub route: Route,
}

/// The format magic after the [`ProtocolTag`].
const MAGIC: [u8; 4] = *b"VPK1";

/// The sections, in table order.
const UI: u32 = 0;
const BEHAVIOR: u32 = 1;
const COMPONENT: u32 = 2;
const STATES: u32 = 3;
const HANDLERS: u32 = 4;
const CONTROLS: u32 = 5;
const REGIONS: u32 = 6;
const ENV: u32 = 7;
const ASSETS: u32 = 8;
const SHADERS: u32 = 9;
const NAMED: u32 = 10;
const SECTIONS: u32 = 11;

/// The bytes of a state, handler and env record.
const STATE_RECORD: usize = 40;
const HANDLER_RECORD: usize = 12;
const ENV_RECORD: usize = 12;
const NAMED_RECORD: usize = 8;

/// The slot of a state record whose cell mirrors no slot.
const NO_SLOT: u32 = u32::MAX;

fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn le64(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

/// Writes a state record: the key, the value's sixteen payload bytes, the
/// slot, the value's kind, whether it is tracked, two zero bytes.
fn write_state(state: &ViewState, enc: &mut Encoder) {
    enc.write_u64(state.key.hi);
    enc.write_u64(state.key.lo);
    let (kind, payload) = match state.initial {
        StateValue::Int(n) => (0, [n as u32, 0, 0, 0]),
        StateValue::Float(x) => (1, [x.to_bits(), 0, 0, 0]),
        StateValue::Bool(b) => (2, [u32::from(b), 0, 0, 0]),
        StateValue::Color(r, g, b, a) => (3, [r, g, b, a].map(f32::to_bits)),
    };
    for word in payload {
        enc.write_u32(word);
    }
    debug_assert_ne!(state.slot, Some(NO_SLOT), "a slot below u32::MAX");
    enc.write_u32(state.slot.unwrap_or(NO_SLOT));
    enc.write_u8(kind);
    enc.write_bool(state.tracked);
    enc.write_raw(&[0; 2]);
}

/// The state record `bytes`, or `None` when it is not one [`write_state`]
/// writes.
fn state_of(bytes: &[u8]) -> Option<ViewState> {
    let word = |i: usize| le32(bytes, 16 + 4 * i);
    let unused = |from: usize| (from..4).all(|i| word(i) == 0);
    let initial = match bytes[36] {
        0 if unused(1) => StateValue::Int(word(0) as i32),
        1 if unused(1) => StateValue::Float(f32::from_bits(word(0))),
        2 if unused(1) && word(0) <= 1 => StateValue::Bool(word(0) == 1),
        3 => {
            let [r, g, b, a] = [0, 1, 2, 3].map(|i| f32::from_bits(word(i)));
            StateValue::Color(r, g, b, a)
        }
        _ => return None,
    };
    if bytes[37] > 1 || bytes[38..40] != [0; 2] {
        return None;
    }
    let slot = le32(bytes, 32);
    Some(ViewState {
        key: StateKey::from_parts(le64(bytes, 0), le64(bytes, 8)),
        slot: (slot != NO_SLOT).then_some(slot),
        tracked: bytes[37] == 1,
        initial,
    })
}

/// The handler record `bytes`: node, handler, route tag, three zero bytes.
fn handler_of(bytes: &[u8]) -> Option<ViewHandler> {
    if bytes[9..12] != [0; 3] {
        return None;
    }
    Some(ViewHandler {
        node: le32(bytes, 0),
        route: Route::from_tag(bytes[8], le32(bytes, 4))?,
    })
}

/// The named-node record `bytes`: key, node.
fn named_of(bytes: &[u8]) -> ViewNamed {
    ViewNamed {
        key: le32(bytes, 0),
        node: le32(bytes, 4),
    }
}

/// The env record `bytes`: slot, anchor, field tag, three zero bytes.
fn env_of(bytes: &[u8]) -> Option<ViewEnv> {
    if bytes[9..12] != [0; 3] {
        return None;
    }
    Some(ViewEnv {
        slot: le32(bytes, 0),
        anchor: le32(bytes, 4),
        field: EnvField::from_tag(bytes[8])?,
    })
}

/// Checks `section` holds `items` records of `width` bytes each `valid`
/// accepts.
fn check_records(
    section: &Section<'_>,
    width: usize,
    mut valid: impl FnMut(&[u8]) -> bool,
) -> Result<(), DecodeError> {
    if section.bytes.len() != section.items as usize * width {
        return Err(DecodeError::Malformed {
            offset: section.offset,
        });
    }
    for (index, record) in section.bytes.chunks_exact(width).enumerate() {
        if !valid(record) {
            return Err(DecodeError::Malformed {
                offset: section.offset + index * width,
            });
        }
    }
    Ok(())
}

/// Writes a run of aligned byte strings: each a `u32` length, four zero
/// bytes, the bytes, padding to [`SECTION_ALIGN`], after `head`.
fn write_aligned(enc: &mut Encoder, head: impl FnOnce(&mut Encoder), bytes: &[u8]) {
    head(enc);
    enc.write_u32(u32::try_from(bytes.len()).expect("an embedded payload fits a u32 length"));
    enc.write_u32(0);
    enc.write_raw(bytes);
    enc.align(SECTION_ALIGN);
}

/// Reads one aligned byte string [`write_aligned`] wrote.
fn read_aligned<'a>(dec: &mut Decoder<'a>) -> Result<&'a [u8], DecodeError> {
    let len = dec.read_u32()? as usize;
    let offset = dec.position();
    if dec.read_u32()? != 0 {
        return Err(DecodeError::Malformed { offset });
    }
    let bytes = dec.read_raw(len)?;
    dec.align(SECTION_ALIGN)?;
    Ok(bytes)
}

/// A validated, borrowed view package: the blob read where it lies.
///
/// Construction validates the whole blob — the header and section table,
/// the UI package ([`AotView`]), the component name, every state, handler and
/// env record, every control and the regions, the asset and shader framing,
/// that handlers and controls ascend by node and assets by resource, and that
/// each names a node of the UI package — so a loader reading it never meets
/// an error after it starts mounting. The behavior module and the shaders are
/// other formats: their loaders check them, and a view loader does so before
/// it mounts anything.
#[derive(Debug, Clone, Copy)]
pub struct ViewPackageView<'a> {
    /// The retained tree and its binding edges.
    pub ui: AotView<'a>,
    /// The encoded behavior module, empty for a view without handlers.
    pub behavior: &'a [u8],
    /// The component of [`behavior`](Self::behavior) the view mounts.
    pub component: &'a str,
    states: Section<'a>,
    handlers: Section<'a>,
    controls: Section<'a>,
    regions: &'a [u8],
    env: Section<'a>,
    assets: Section<'a>,
    shaders: Section<'a>,
    named: Section<'a>,
}

impl<'a> ViewPackageView<'a> {
    /// Validates `blob`, which holds exactly one package.
    ///
    /// # Errors
    ///
    /// A [`DecodeError`] for anything malformed, or bytes after the package.
    pub fn new(blob: &'a [u8]) -> Result<ViewPackageView<'a>, DecodeError> {
        let (view, len) = ViewPackageView::parse(blob)?;
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
    pub fn read(dec: &mut Decoder<'a>) -> Result<ViewPackageView<'a>, DecodeError> {
        let (view, len) = ViewPackageView::parse(&dec.input()[dec.position()..])?;
        dec.read_raw(len)?;
        Ok(view)
    }

    /// The package at the start of `blob` and its length.
    fn parse(blob: &'a [u8]) -> Result<(ViewPackageView<'a>, usize), DecodeError> {
        let mut dec = Decoder::new(blob);
        if !ProtocolTag::decode(&mut dec)?.is_compatible() || *dec.read_array()? != MAGIC {
            return Err(DecodeError::Malformed { offset: 0 });
        }
        let sections = Sections::read(&mut dec, SECTIONS)?;
        let section = |index| sections.get(index);
        let component = section(COMPONENT);
        let view = ViewPackageView {
            ui: AotView::new(section(UI).bytes)?,
            behavior: section(BEHAVIOR).bytes,
            component: core::str::from_utf8(component.bytes).map_err(|_| {
                DecodeError::InvalidUtf8 {
                    offset: component.offset,
                }
            })?,
            states: section(STATES),
            handlers: section(HANDLERS),
            controls: section(CONTROLS),
            regions: section(REGIONS).bytes,
            env: section(ENV),
            assets: section(ASSETS),
            shaders: section(SHADERS),
            named: section(NAMED),
        };
        let nodes = view.ui.node_count();
        let node = |index: u32| (index as usize) < nodes;
        check_records(&view.states, STATE_RECORD, |r| state_of(r).is_some())?;
        let mut last = 0;
        check_records(&view.handlers, HANDLER_RECORD, |r| {
            handler_of(r).is_some_and(|h| {
                let ascending = h.node >= last;
                last = h.node;
                ascending && node(h.node)
            })
        })?;
        check_records(&view.env, ENV_RECORD, |r| {
            env_of(r).is_some_and(|env| node(env.anchor))
        })?;
        let mut last = None;
        check_records(&view.named, NAMED_RECORD, |r| {
            let named = named_of(r);
            let ascending = last.is_none_or(|last| last < named.key);
            last = Some(named.key);
            ascending && node(named.node)
        })?;
        let mut dec = view.controls.decoder();
        let mut last = None;
        for _ in 0..view.controls.items {
            let offset = dec.position();
            let at = dec.read_u32()?;
            Control::decode(&mut dec)?;
            if !node(at) || last.is_some_and(|last| last >= at) {
                return Err(DecodeError::Malformed { offset });
            }
            last = Some(at);
        }
        dec.finish()?;
        ViewRegions::decode_from_slice(view.regions)?;
        let mut dec = view.assets.decoder();
        let mut last = None;
        for _ in 0..view.assets.items {
            let offset = dec.position();
            let id = dec.read_u64()?;
            read_aligned(&mut dec)?;
            if last.is_some_and(|last| last >= id) {
                return Err(DecodeError::Malformed { offset });
            }
            last = Some(id);
        }
        dec.finish()?;
        let mut dec = view.shaders.decoder();
        for _ in 0..view.shaders.items {
            read_aligned(&mut dec)?;
        }
        dec.finish()?;
        Ok((view, sections.end()))
    }

    /// The state cells, allocated with their initial values before the tree
    /// mounts.
    pub fn states(&self) -> impl ExactSizeIterator<Item = ViewState> + 'a {
        let records = self.states.bytes.as_chunks::<STATE_RECORD>().0.iter();
        records.map(|r| state_of(r).expect("the view validated every state record"))
    }

    /// The node handlers, in ascending node order.
    pub fn handlers(&self) -> impl ExactSizeIterator<Item = ViewHandler> + 'a {
        let records = self.handlers.bytes.as_chunks::<HANDLER_RECORD>().0.iter();
        records.map(|r| handler_of(r).expect("the view validated every handler record"))
    }

    /// The `env` fields the view's static instances read.
    pub fn env(&self) -> impl ExactSizeIterator<Item = ViewEnv> + 'a {
        let records = self.env.bytes.as_chunks::<ENV_RECORD>().0.iter();
        records.map(|r| env_of(r).expect("the view validated every env record"))
    }

    /// The static nodes the view's actions and handlers reach as a
    /// `NodeRef`, ascending by key.
    pub fn named(&self) -> impl ExactSizeIterator<Item = ViewNamed> + 'a {
        let records = self.named.bytes.as_chunks::<NAMED_RECORD>().0.iter();
        records.map(|r| named_of(r))
    }

    /// The number of native control nodes.
    pub fn control_count(&self) -> usize {
        self.controls.items as usize
    }

    /// The native control nodes, in ascending node order, each decoded as it
    /// is reached.
    pub fn controls(&self) -> impl ExactSizeIterator<Item = ViewControl> + 'a {
        let mut dec = self.controls.decoder();
        (0..self.controls.items).map(move |_| {
            let mut read = || {
                Ok::<_, DecodeError>(ViewControl {
                    node: dec.read_u32()?,
                    control: Control::decode(&mut dec)?,
                })
            };
            read().expect("the view validated every control")
        })
    }

    /// The control-flow regions.
    pub fn regions(&self) -> ViewRegions {
        ViewRegions::decode_from_slice(self.regions).expect("the view validated the regions")
    }

    /// The assets the view can show, ascending by resource, their bytes
    /// borrowed from the blob.
    pub fn assets(&self) -> impl ExactSizeIterator<Item = (ResourceId, &'a [u8])> + 'a {
        let mut dec = self.assets.decoder();
        (0..self.assets.items).map(move |_| {
            let mut read = || Ok::<_, DecodeError>((dec.read_u64()?, read_aligned(&mut dec)?));
            let (id, bytes) = read().expect("the view validated every asset");
            (ResourceId(id), bytes)
        })
    }

    /// The view's shader table, each packed brush borrowed from the blob.
    pub fn shaders(&self) -> impl ExactSizeIterator<Item = &'a [u8]> + 'a {
        let mut dec = self.shaders.decoder();
        (0..self.shaders.items)
            .map(move |_| read_aligned(&mut dec).expect("the view validated every shader"))
    }

    /// The package as owned tables, every byte it carries copied.
    pub fn to_package(&self) -> ViewPackage {
        ViewPackage {
            ui: self.ui.to_package(),
            behavior: self.behavior.to_vec(),
            component: String::from(self.component),
            states: self.states().collect(),
            handlers: self.handlers().collect(),
            controls: self.controls().collect(),
            regions: self.regions(),
            env: self.env().collect(),
            named: self.named().collect(),
            assets: self
                .assets()
                .map(|(id, bytes)| ViewAsset {
                    id,
                    bytes: AssetBytes::Shared(Arc::from(bytes)),
                })
                .collect(),
            shaders: self.shaders().map(ViewShader::new).collect(),
        }
    }
}

impl ViewPackage {
    /// Writes the package's blob: the header, the section table and the
    /// eleven sections.
    fn write(&self, enc: &mut Encoder) {
        let count = |n: usize| u32::try_from(n).expect("a package table fits u32 counts");
        ProtocolTag::current().encode(enc);
        enc.write_raw(&MAGIC);
        let mut sections = SectionWriter::new(enc, SECTIONS);
        sections.section(enc, |enc| {
            self.ui.encode(enc);
            0
        });
        sections.section(enc, |enc| {
            enc.write_raw(&self.behavior);
            0
        });
        sections.section(enc, |enc| {
            enc.write_raw(self.component.as_bytes());
            0
        });
        sections.section(enc, |enc| {
            self.states.iter().for_each(|state| write_state(state, enc));
            count(self.states.len())
        });
        sections.section(enc, |enc| {
            for handler in &self.handlers {
                enc.write_u32(handler.node);
                enc.write_u32(handler.route.handler);
                enc.write_u8(handler.route.tag());
                enc.write_raw(&[0; 3]);
            }
            count(self.handlers.len())
        });
        sections.section(enc, |enc| {
            for control in &self.controls {
                enc.write_u32(control.node);
                control.control.encode(enc);
            }
            count(self.controls.len())
        });
        sections.section(enc, |enc| {
            self.regions.encode(enc);
            0
        });
        sections.section(enc, |enc| {
            for env in &self.env {
                enc.write_u32(env.slot);
                enc.write_u32(env.anchor);
                enc.write_u8(env.field.tag());
                enc.write_raw(&[0; 3]);
            }
            count(self.env.len())
        });
        sections.section(enc, |enc| {
            for asset in &self.assets {
                write_aligned(enc, |enc| enc.write_u64(asset.id.0), &asset.bytes);
            }
            count(self.assets.len())
        });
        sections.section(enc, |enc| {
            for shader in &self.shaders {
                write_aligned(enc, |_| {}, shader.bytes());
            }
            count(self.shaders.len())
        });
        sections.section(enc, |enc| {
            for named in &self.named {
                enc.write_u32(named.key);
                enc.write_u32(named.node);
            }
            count(self.named.len())
        });
        sections.finish();
    }
}

impl Encode for ViewPackage {
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

impl Decode for ViewPackage {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        ViewPackageView::read(dec).map(|view| view.to_package())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aot::{AotNode, AotNodeKind, AotStyle};
    use alloc::vec;

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

    fn package() -> ViewPackage {
        ViewPackage {
            ui: AotPackage {
                nodes: vec![
                    AotNode {
                        kind: AotNodeKind::Flex,
                        child_count: 1,
                        ..leaf()
                    },
                    leaf(),
                ],
                edges: Vec::new(),
            },
            behavior: vec![1, 2, 3],
            component: "Counter".into(),
            states: vec![
                ViewState {
                    key: StateKey::from_parts(7, 9),
                    slot: Some(0),
                    tracked: false,
                    initial: StateValue::Int(-3),
                },
                ViewState {
                    key: StateKey::from_parts(8, 1),
                    slot: None,
                    tracked: true,
                    initial: StateValue::Color(0.25, 0.5, 0.75, 1.0),
                },
                ViewState {
                    key: StateKey::from_parts(8, 2),
                    slot: Some(4),
                    tracked: false,
                    initial: StateValue::Bool(true),
                },
            ],
            handlers: vec![ViewHandler {
                node: 1,
                route: Route {
                    event: crate::view::EventRoute::Click,
                    handler: 0,
                    capture: true,
                },
            }],
            controls: Vec::new(),
            regions: ViewRegions::default(),
            env: vec![ViewEnv {
                slot: 1,
                field: EnvField::SizeClass,
                anchor: 0,
            }],
            named: vec![
                ViewNamed { key: 0, node: 1 },
                ViewNamed {
                    key: 1 << 16,
                    node: 0,
                },
            ],
            assets: vec![
                ViewAsset {
                    id: ResourceId(3),
                    bytes: AssetBytes::Static(b"\x89PNG"),
                },
                ViewAsset {
                    id: ResourceId(9),
                    bytes: AssetBytes::Shared(Arc::from(&b"<svg/>"[..])),
                },
            ],
            shaders: vec![ViewShader::new(&b"brush"[..])],
        }
    }

    /// The offset in `bytes` of section `index`.
    fn section_offset(bytes: &[u8], index: u32) -> usize {
        let mut dec = Decoder::new(bytes);
        dec.read_raw(8).unwrap();
        Sections::read(&mut dec, SECTIONS)
            .unwrap()
            .get(index)
            .offset
    }

    #[test]
    fn a_package_round_trips() {
        let package = package();
        let bytes = package.encode_to_vec();
        assert_eq!(ViewPackage::decode_from_slice(&bytes), Ok(package.clone()));
        let empty = ViewPackage::default();
        assert_eq!(
            ViewPackage::decode_from_slice(&empty.encode_to_vec()),
            Ok(empty)
        );
        for end in 0..bytes.len() {
            assert!(ViewPackage::decode_from_slice(&bytes[..end]).is_err());
        }
    }

    #[test]
    fn the_view_borrows_the_blob() {
        let bytes = package().encode_to_vec();
        let view = ViewPackageView::new(&bytes).unwrap();
        let inside = |slice: &[u8]| bytes.as_ptr_range().contains(&slice.as_ptr());
        assert!(inside(view.behavior) && inside(view.component.as_bytes()));
        for (_, asset) in view.assets() {
            assert!(inside(asset));
            let at = asset.as_ptr().addr() - bytes.as_ptr().addr();
            assert_eq!(at % SECTION_ALIGN, 0, "an asset's bytes start aligned");
        }
        assert!(view.shaders().all(inside));
        assert_eq!(view.ui.node_count(), 2);
    }

    #[test]
    fn a_bad_record_is_rejected() {
        let bytes = package().encode_to_vec();
        let rejects = |section: u32, at: usize, value: u8| {
            let mut bad = bytes.clone();
            bad[section_offset(&bytes, section) + at] = value;
            ViewPackageView::new(&bad).is_err()
        };
        assert!(rejects(HANDLERS, 8, 99), "an event that does not exist");
        assert!(rejects(HANDLERS, 0, 2), "a handler on a missing node");
        assert!(rejects(HANDLERS, 11, 1), "a handler record's padding");
        assert!(rejects(STATES, 36, 4), "a value kind that does not exist");
        assert!(rejects(STATES, 37, 2), "a tracked flag that is no bool");
        assert!(rejects(STATES, 20, 1), "an int's unused payload");
        assert!(rejects(ENV, 8, 200), "an env field that does not exist");
        assert!(rejects(ASSETS, 12, 1), "an asset's reserved word");
        assert!(
            rejects(COMPONENT, 0, 0xff),
            "a component name that is no UTF-8"
        );

        let mut reordered = package();
        reordered.assets.swap(0, 1);
        assert!(ViewPackageView::new(&reordered.encode_to_vec()).is_err());
        let mut handlers = package();
        handlers.ui.nodes[0].child_count = 2;
        handlers.ui.nodes.push(leaf());
        handlers.handlers.insert(0, handlers.handlers[0]);
        handlers.handlers[0].node = 2;
        assert!(ViewPackageView::new(&handlers.encode_to_vec()).is_err());
    }
}
