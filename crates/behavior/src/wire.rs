//! The binary form of a [`Module`], so a release build or a macro can embed
//! compiled behavior and load it without the compiler.
//!
//! The blob is framed by the shared [`ProtocolTag`] header and encoded through
//! [`viso_ende`]. Every instruction is a one-byte opcode followed by its fields
//! at fixed width; constants carry a one-byte kind. Decoding is bounded — a
//! truncated or corrupt blob is a [`LoadError`], never a panic — and the result
//! goes through [`Module::new`], so a loaded module is verified exactly like a
//! freshly compiled one.

use std::fmt;
use std::rc::Rc;
use std::time::Duration;

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder, ProtocolTag};

use crate::game::{
    Action, InputBindings, InputSchema, Key, KeySet, MoveSource, PadButton, PadStick, TouchButton,
};
use crate::i18n::Catalog;
use crate::module::{
    Chunk, ChunkKind, Code, CollisionDelivery, Component, ComponentEffect, EffectRun, Migrator,
    Module, NativeImport, PersistSlot, ResourceLoad, SnapshotSlot, Span, StableId, System,
    VerifyError,
};
use crate::native::{Determinism, NativeId};
use crate::op::{Arith, ArithOp, DisplayKind, Num, Op};
use crate::retype::ValueSchema;
use crate::value::{Aggregate, Value};

/// Why a module blob failed to load.
#[derive(Debug, Clone, PartialEq)]
pub enum LoadError {
    /// The bytes are not a well-formed module blob.
    Decode(DecodeError),
    /// The blob decoded but its code does not verify.
    Verify(VerifyError),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Decode(e) => write!(f, "malformed behavior module: {e:?}"),
            LoadError::Verify(e) => write!(f, "behavior module does not verify: {e}"),
        }
    }
}

impl std::error::Error for LoadError {}

impl From<DecodeError> for LoadError {
    fn from(e: DecodeError) -> LoadError {
        LoadError::Decode(e)
    }
}

impl Module {
    /// The module as a self-describing blob [`Module::decode`] reads back.
    pub fn encode(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        ProtocolTag::current().encode(&mut enc);
        write_list(&mut enc, self.chunks(), write_chunk);
        write_list(&mut enc, self.components(), write_component);
        write_list(&mut enc, self.systems(), |enc, s| {
            enc.write_varint(u64::from(s.component));
            write_list(enc, &s.hooks, |enc, (hook, chunk)| {
                enc.write_u64(hook.0);
                enc.write_varint(u64::from(*chunk));
            });
            write_stable_id(enc, s.id);
            for states in [&s.snapshot, &s.locals] {
                write_list(enc, states, |enc, state| {
                    write_stable_id(enc, state.id);
                    enc.write_varint(u64::from(state.slot));
                    enc.write_u64(state.schema);
                });
            }
            write_list(enc, &s.persist, write_persist);
        });
        write_list(&mut enc, self.natives(), |enc, n| {
            enc.write_str(&n.path);
            enc.write_u64(n.signature);
            enc.write_u16(n.params);
        });
        enc.write_bool(self.input().is_some());
        if let Some(input) = self.input() {
            write_input(&mut enc, input);
        }
        enc.write_varint(u64::from(self.tick_rate()));
        enc.write_u8(match self.determinism() {
            Determinism::None => 0,
            Determinism::SameBinary => 1,
            Determinism::CrossPlatform => 2,
        });
        enc.write_u8(match self.collision_delivery() {
            CollisionDelivery::EventMajor => 0,
            CollisionDelivery::ListenerMajor => 1,
        });
        write_list(&mut enc, self.migrators(), |enc, m| {
            enc.write_str(&m.from);
            m.param.encode(enc);
            m.ret.encode(enc);
            enc.write_varint(u64::from(m.chunk));
        });
        write_list(&mut enc, self.capabilities(), |enc, c| enc.write_str(c));
        write_list(&mut enc, self.themes(), |enc, (name, chunk)| {
            enc.write_str(name);
            enc.write_varint(u64::from(*chunk));
        });
        enc.write_bool(self.catalog().is_some());
        if let Some(catalog) = self.catalog() {
            catalog.encode(&mut enc);
        }
        enc.into_bytes()
    }

    /// Loads and verifies a blob [`Module::encode`] wrote.
    ///
    /// # Errors
    ///
    /// A [`LoadError`] when the bytes are malformed, from another wire version,
    /// or hold code that does not verify.
    pub fn decode(bytes: &[u8]) -> Result<Module, LoadError> {
        let mut dec = Decoder::new(bytes);
        let offset = dec.position();
        if !ProtocolTag::decode(&mut dec)?.is_compatible() {
            return Err(DecodeError::Malformed { offset }.into());
        }
        let chunks = read_list(&mut dec, read_chunk)?;
        let components = read_list(&mut dec, read_component)?;
        let systems = read_list(&mut dec, |dec| {
            Ok(System {
                component: read_u32_varint(dec)?,
                hooks: read_list(dec, |dec| {
                    Ok((NativeId(dec.read_u64()?), read_u32_varint(dec)?))
                })?
                .into(),
                id: read_stable_id(dec)?,
                snapshot: read_list(dec, read_state)?.into(),
                locals: read_list(dec, read_state)?.into(),
                persist: read_list(dec, read_persist)?.into(),
            })
        })?;
        let natives = read_list(&mut dec, |dec| {
            Ok(NativeImport {
                path: dec.read_str()?.into(),
                signature: dec.read_u64()?,
                params: dec.read_u16()?,
            })
        })?;
        let input = if dec.read_bool()? {
            Some(read_input(&mut dec)?)
        } else {
            None
        };
        let tick_rate = read_u32_varint(&mut dec)?;
        let offset = dec.position();
        let determinism = match dec.read_u8()? {
            0 => Determinism::None,
            1 => Determinism::SameBinary,
            2 => Determinism::CrossPlatform,
            _ => return Err(DecodeError::Malformed { offset }.into()),
        };
        let offset = dec.position();
        let delivery = match dec.read_u8()? {
            0 => CollisionDelivery::EventMajor,
            1 => CollisionDelivery::ListenerMajor,
            _ => return Err(DecodeError::Malformed { offset }.into()),
        };
        let migrators = read_list(&mut dec, |dec| {
            Ok(Migrator {
                from: dec.read_str()?.into(),
                param: ValueSchema::decode(dec)?,
                ret: ValueSchema::decode(dec)?,
                chunk: read_u32_varint(dec)?,
            })
        })?;
        let capabilities = read_list(&mut dec, |dec| Ok(Box::<str>::from(dec.read_str()?)))?;
        let themes = read_list(&mut dec, |dec| {
            Ok((Box::<str>::from(dec.read_str()?), read_u32_varint(dec)?))
        })?;
        let catalog = if dec.read_bool()? {
            Some(Catalog::decode(&mut dec)?)
        } else {
            None
        };
        dec.finish()?;
        let module = Module::new(chunks, components, systems, natives)
            .and_then(|m| m.with_tick_rate(tick_rate))
            .map(|m| m.with_game_profile(determinism, delivery))
            .and_then(|m| m.with_migrators(migrators))
            .and_then(|m| m.with_themes(themes))
            .and_then(|m| m.with_catalog(catalog))
            .map_err(LoadError::Verify)?
            .with_capabilities(capabilities);
        match input {
            Some(input) => module.with_input(input).map_err(LoadError::Verify),
            None => Ok(module),
        }
    }
}

/// The largest count a decoder preallocates for, so a corrupt length cannot
/// request a huge buffer before the bytes run out.
const MAX_PREALLOC: u64 = 4096;

pub(crate) fn write_list<T>(
    enc: &mut Encoder,
    items: &[T],
    mut write: impl FnMut(&mut Encoder, &T),
) {
    enc.write_varint(items.len() as u64);
    for item in items {
        write(enc, item);
    }
}

pub(crate) fn read_list<'a, T>(
    dec: &mut Decoder<'a>,
    mut read: impl FnMut(&mut Decoder<'a>) -> Result<T, DecodeError>,
) -> Result<Vec<T>, DecodeError> {
    let n = dec.read_varint()?;
    let mut items = Vec::with_capacity(n.min(MAX_PREALLOC) as usize);
    for _ in 0..n {
        items.push(read(dec)?);
    }
    Ok(items)
}

pub(crate) fn malformed(dec: &Decoder<'_>) -> DecodeError {
    DecodeError::Malformed {
        offset: dec.position(),
    }
}

pub(crate) fn write_stable_id(enc: &mut Encoder, id: StableId) {
    enc.write_u64(id.hi);
    enc.write_u64(id.lo);
}

pub(crate) fn read_stable_id(dec: &mut Decoder<'_>) -> Result<StableId, DecodeError> {
    Ok(StableId {
        hi: dec.read_u64()?,
        lo: dec.read_u64()?,
    })
}

fn read_state(dec: &mut Decoder<'_>) -> Result<SnapshotSlot, DecodeError> {
    Ok(SnapshotSlot {
        id: read_stable_id(dec)?,
        slot: read_u32_varint(dec)?,
        schema: dec.read_u64()?,
    })
}

fn write_names(enc: &mut Encoder, names: &[Box<str>]) {
    write_list(enc, names, |enc, n| enc.write_str(n));
}

fn read_names(dec: &mut Decoder<'_>) -> Result<Box<[Box<str>]>, DecodeError> {
    Ok(read_list(dec, |dec| dec.read_str().map(Into::into))?.into())
}

fn write_refs(enc: &mut Encoder, refs: &[Option<u32>]) {
    write_list(enc, refs, |enc, r| match r {
        None => enc.write_varint(0),
        Some(c) => enc.write_varint(u64::from(*c) + 1),
    });
}

fn read_refs(dec: &mut Decoder<'_>) -> Result<Box<[Option<u32>]>, DecodeError> {
    let refs = read_list(dec, |dec| match dec.read_varint()? {
        0 => Ok(None),
        n => u32::try_from(n - 1).map(Some).map_err(|_| malformed(dec)),
    })?;
    Ok(refs.into())
}

fn write_input(enc: &mut Encoder, input: &InputSchema) {
    enc.write_str(&input.name);
    write_list(enc, &input.actions, |enc, a| enc.write_str(a));
    let b = &input.bindings;
    write_list(enc, &b.keys, |enc, (key, action)| {
        enc.write_u8(*key as u8);
        enc.write_varint(u64::from(action.0));
    });
    write_list(enc, &b.pads, |enc, (button, action)| {
        enc.write_u8(*button as u8);
        enc.write_varint(u64::from(action.0));
    });
    write_list(enc, &b.touches, |enc, (button, action)| {
        enc.write_u8(*button as u8);
        enc.write_varint(u64::from(action.0));
    });
    enc.write_bool(b.move_axes.is_some());
    if let Some(source) = b.move_axes {
        let k = source.keys;
        for key in [k.up, k.left, k.down, k.right] {
            enc.write_u8(key as u8);
        }
        enc.write_u8(source.stick as u8);
    }
    enc.write_f64(b.dead_zone);
}

fn read_input(dec: &mut Decoder<'_>) -> Result<InputSchema, DecodeError> {
    fn variant<T>(dec: &mut Decoder<'_>, of: fn(i64) -> Option<T>) -> Result<T, DecodeError> {
        let i = dec.read_u8()?;
        of(i64::from(i)).ok_or_else(|| malformed(dec))
    }
    let name = dec.read_str()?.into();
    let actions = read_list(dec, |dec| Ok(dec.read_str()?.into()))?.into();
    let keys = read_list(dec, |dec| {
        Ok((
            variant(dec, Key::from_index)?,
            Action(read_u32_varint(dec)?),
        ))
    })?;
    let pads = read_list(dec, |dec| {
        Ok((
            variant(dec, PadButton::from_index)?,
            Action(read_u32_varint(dec)?),
        ))
    })?;
    let touches = read_list(dec, |dec| {
        Ok((
            variant(dec, TouchButton::from_index)?,
            Action(read_u32_varint(dec)?),
        ))
    })?;
    let move_axes = if dec.read_bool()? {
        let keys = KeySet {
            up: variant(dec, Key::from_index)?,
            left: variant(dec, Key::from_index)?,
            down: variant(dec, Key::from_index)?,
            right: variant(dec, Key::from_index)?,
        };
        let stick = variant(dec, PadStick::from_index)?;
        Some(MoveSource { keys, stick })
    } else {
        None
    };
    let dead_zone = dec.read_f64()?;
    Ok(InputSchema {
        name,
        actions,
        bindings: InputBindings {
            keys,
            pads,
            touches,
            move_axes,
            dead_zone,
        },
    })
}

pub(crate) fn read_u32_varint(dec: &mut Decoder<'_>) -> Result<u32, DecodeError> {
    let n = dec.read_varint()?;
    u32::try_from(n).map_err(|_| malformed(dec))
}

fn write_component(enc: &mut Encoder, c: &Component) {
    enc.write_str(&c.name);
    write_names(enc, &c.states);
    write_names(enc, &c.inputs);
    write_names(enc, &c.events);
    write_refs(enc, &c.state_inits);
    write_refs(enc, &c.input_defaults);
    write_list(enc, &c.members, |enc, (name, chunk)| {
        enc.write_str(name);
        enc.write_varint(u64::from(*chunk));
    });
    write_list(enc, &c.handlers, |enc, chunk| {
        enc.write_varint(u64::from(*chunk))
    });
    write_list(enc, &c.effects, |enc, e| e.encode(enc));
    write_list(enc, &c.persist, write_persist);
}

fn write_persist(enc: &mut Encoder, state: &PersistSlot) {
    enc.write_str(&state.key);
    enc.write_varint(u64::from(state.slot));
    state.schema.encode(enc);
    enc.write_str(&state.spelling);
}

fn read_persist(dec: &mut Decoder<'_>) -> Result<PersistSlot, DecodeError> {
    Ok(PersistSlot {
        key: dec.read_str()?.into(),
        slot: read_u32_varint(dec)?,
        schema: ValueSchema::decode(dec)?,
        spelling: dec.read_str()?.into(),
    })
}

impl Encode for ComponentEffect {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_varint(self.deps.map_or(0, |d| u64::from(d) + 1));
        enc.write_varint(u64::from(self.body));
        enc.write_u8(match self.run {
            EffectRun::Mount => 0,
            EffectRun::Change => 1,
            EffectRun::MountAndChange => 2,
        });
        match &self.resource {
            None => enc.write_u8(0),
            Some(load) => {
                enc.write_u8(1);
                enc.write_varint(u64::from(load.state));
                enc.write_varint(u64::from(load.write));
                write_duration(enc, load.debounce);
                write_duration(enc, load.cache_for);
                enc.write_bool(load.cache_errors);
                enc.write_bool(load.keep_latest);
            }
        }
    }
}

/// An optional duration: `0` for none, else its nanoseconds plus one.
fn write_duration(enc: &mut Encoder, duration: Option<Duration>) {
    let nanos = duration.map_or(0, |d| {
        u64::try_from(d.as_nanos()).unwrap_or(u64::MAX - 1) + 1
    });
    enc.write_varint(nanos);
}

fn read_duration(dec: &mut Decoder<'_>) -> Result<Option<Duration>, DecodeError> {
    Ok(dec.read_varint()?.checked_sub(1).map(Duration::from_nanos))
}

fn read_resource(dec: &mut Decoder<'_>) -> Result<ResourceLoad, DecodeError> {
    Ok(ResourceLoad {
        state: read_u32_varint(dec)?,
        write: read_u32_varint(dec)?,
        debounce: read_duration(dec)?,
        cache_for: read_duration(dec)?,
        cache_errors: dec.read_bool()?,
        keep_latest: dec.read_bool()?,
    })
}

impl Decode for ComponentEffect {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let deps = read_u32_varint(dec)?.checked_sub(1);
        let body = read_u32_varint(dec)?;
        let offset = dec.position();
        let run = match dec.read_u8()? {
            0 => EffectRun::Mount,
            1 => EffectRun::Change,
            2 => EffectRun::MountAndChange,
            _ => return Err(DecodeError::Malformed { offset }),
        };
        let resource = match dec.read_u8()? {
            0 => None,
            1 => Some(read_resource(dec)?),
            _ => return Err(DecodeError::Malformed { offset }),
        };
        Ok(ComponentEffect {
            deps,
            body,
            run,
            resource,
        })
    }
}

fn read_component(dec: &mut Decoder<'_>) -> Result<Component, DecodeError> {
    Ok(Component {
        name: dec.read_str()?.into(),
        states: read_names(dec)?,
        inputs: read_names(dec)?,
        events: read_names(dec)?,
        state_inits: read_refs(dec)?,
        input_defaults: read_refs(dec)?,
        members: read_list(dec, |dec| {
            Ok((dec.read_str()?.into(), read_u32_varint(dec)?))
        })?
        .into(),
        handlers: read_list(dec, read_u32_varint)?.into(),
        effects: read_list(dec, ComponentEffect::decode)?.into(),
        persist: read_list(dec, read_persist)?.into(),
    })
}

const CHUNK_KINDS: [ChunkKind; 12] = [
    ChunkKind::Fn,
    ChunkKind::Action,
    ChunkKind::Closure,
    ChunkKind::Computed,
    ChunkKind::StateInit,
    ChunkKind::InputDefault,
    ChunkKind::Const,
    ChunkKind::FieldDefault,
    ChunkKind::Handler,
    ChunkKind::RegionEntry,
    ChunkKind::Effect,
    ChunkKind::Task,
];

fn write_chunk(enc: &mut Encoder, c: &Chunk) {
    enc.write_str(&c.name);
    let kind = CHUNK_KINDS.iter().position(|k| *k == c.kind).unwrap_or(0);
    enc.write_u8(kind as u8);
    enc.write_varint(u64::from(c.module));
    enc.write_u16(c.params);
    enc.write_u16(c.regs);
    write_list(enc, &c.captures, |enc, r| enc.write_u16(*r));
    match &c.body {
        Err(reason) => {
            enc.write_u8(0);
            enc.write_str(reason);
        }
        Ok(code) => {
            enc.write_u8(1);
            write_list(enc, &code.ops, write_op);
            write_list(enc, &code.consts, write_value);
            write_list(enc, &code.ext, |enc, w| enc.write_varint(u64::from(*w)));
            write_list(enc, &code.spans, |enc, s| {
                enc.write_varint(u64::from(s.start));
                enc.write_varint(u64::from(s.end));
            });
        }
    }
}

fn read_chunk(dec: &mut Decoder<'_>) -> Result<Chunk, DecodeError> {
    let name = dec.read_str()?.into();
    let kind = *CHUNK_KINDS
        .get(usize::from(dec.read_u8()?))
        .ok_or_else(|| malformed(dec))?;
    let module = read_u32_varint(dec)?;
    let params = dec.read_u16()?;
    let regs = dec.read_u16()?;
    let captures = read_list(dec, Decoder::read_u16)?.into();
    let body = match dec.read_u8()? {
        0 => Err(dec.read_str()?.into()),
        1 => Ok(Code {
            ops: read_list(dec, read_op)?.into(),
            consts: read_list(dec, read_value)?.into(),
            ext: read_list(dec, read_u32_varint)?.into(),
            spans: read_list(dec, |dec| {
                Ok(Span {
                    start: read_u32_varint(dec)?,
                    end: read_u32_varint(dec)?,
                })
            })?
            .into(),
        }),
        _ => return Err(malformed(dec)),
    };
    Ok(Chunk {
        name,
        kind,
        module,
        params,
        regs,
        captures,
        body,
    })
}

/// Constant kinds. Closures and native handles never appear in a constant
/// pool ([`Module::new`] rejects them), so they have no wire form.
const VALUE_NIL: u8 = 0;
const VALUE_INT: u8 = 1;
const VALUE_FLOAT: u8 = 2;
const VALUE_STR: u8 = 3;
const VALUE_LIST: u8 = 4;
const VALUE_AGG: u8 = 5;

/// The deepest constant nesting a decoder follows.
const MAX_VALUE_DEPTH: u32 = 64;

/// `v`, with every NaN the one quiet NaN: x86 and Arm produce NaNs of
/// different signs and payloads, which a canonical blob does not keep.
pub(crate) fn canonical_f64(v: f64) -> f64 {
    if v.is_nan() { f64::NAN } else { v }
}

/// The bits of `v`, with every NaN the one quiet NaN (see [`canonical_f64`]).
pub(crate) fn canonical_f32_bits(v: f32) -> u32 {
    if v.is_nan() {
        f32::NAN.to_bits()
    } else {
        v.to_bits()
    }
}

pub(crate) fn write_value(enc: &mut Encoder, value: &Value) {
    match value {
        Value::Nil | Value::Closure(_) | Value::Handle(_) => enc.write_u8(VALUE_NIL),
        Value::Int(i) => {
            enc.write_u8(VALUE_INT);
            enc.write_varint_signed(*i);
        }
        Value::Float(f) => {
            enc.write_u8(VALUE_FLOAT);
            enc.write_f64(canonical_f64(*f));
        }
        Value::Str(s) => {
            enc.write_u8(VALUE_STR);
            enc.write_str(s);
        }
        Value::List(items) => {
            enc.write_u8(VALUE_LIST);
            write_list(enc, items, write_value);
        }
        Value::Agg(agg) => {
            enc.write_u8(VALUE_AGG);
            enc.write_varint(u64::from(agg.tag));
            write_list(enc, &agg.fields, write_value);
        }
    }
}

pub(crate) fn read_value(dec: &mut Decoder<'_>) -> Result<Value, DecodeError> {
    read_nested_value(dec, 0)
}

fn read_nested_value(dec: &mut Decoder<'_>, depth: u32) -> Result<Value, DecodeError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(malformed(dec));
    }
    let nested = |dec: &mut Decoder<'_>| read_nested_value(dec, depth + 1);
    Ok(match dec.read_u8()? {
        VALUE_NIL => Value::Nil,
        VALUE_INT => Value::Int(dec.read_varint_signed()?),
        VALUE_FLOAT => Value::Float(dec.read_f64()?),
        VALUE_STR => Value::Str(Rc::new(dec.read_str()?.to_owned())),
        VALUE_LIST => Value::List(Rc::new(read_list(dec, nested)?)),
        VALUE_AGG => {
            let tag = read_u32_varint(dec)?;
            let fields = read_list(dec, nested)?.into();
            Value::Agg(Rc::new(Aggregate { tag, fields }))
        }
        _ => return Err(malformed(dec)),
    })
}

const NUMS: [Num; 10] = [
    Num::I8,
    Num::I16,
    Num::I32,
    Num::I64,
    Num::U8,
    Num::U16,
    Num::U32,
    Num::U64,
    Num::F32,
    Num::F64,
];

const ARITHS: [Arith; 12] = [
    Arith::Add,
    Arith::Sub,
    Arith::Mul,
    Arith::Div,
    Arith::Rem,
    Arith::And,
    Arith::Or,
    Arith::Xor,
    Arith::Shl,
    Arith::Shr,
    Arith::Lt,
    Arith::Le,
];

const DISPLAY_KINDS: [DisplayKind; 7] = [
    DisplayKind::Bool,
    DisplayKind::Signed,
    DisplayKind::Unsigned,
    DisplayKind::F32,
    DisplayKind::F64,
    DisplayKind::Char,
    DisplayKind::Str,
];

fn read_num(dec: &mut Decoder<'_>) -> Result<Num, DecodeError> {
    let i = dec.read_u8()?;
    NUMS.get(usize::from(i))
        .copied()
        .ok_or_else(|| malformed(dec))
}

/// The one-byte opcodes, in [`Op`] declaration order.
mod opcode {
    pub const CONST: u8 = 0;
    pub const INT: u8 = 1;
    pub const NIL: u8 = 2;
    pub const MOVE: u8 = 3;
    pub const LOAD_STATE: u8 = 4;
    pub const STORE_STATE: u8 = 5;
    pub const LOAD_INPUT: u8 = 6;
    pub const ADD_I64: u8 = 7;
    pub const SUB_I64: u8 = 8;
    pub const MUL_I64: u8 = 9;
    pub const LT_I64: u8 = 10;
    pub const LE_I64: u8 = 11;
    pub const ADD_F64: u8 = 12;
    pub const SUB_F64: u8 = 13;
    pub const MUL_F64: u8 = 14;
    pub const DIV_F64: u8 = 15;
    pub const LT_F64: u8 = 16;
    pub const LE_F64: u8 = 17;
    pub const ARITH: u8 = 18;
    pub const EQ: u8 = 19;
    pub const NE: u8 = 20;
    pub const NEG: u8 = 21;
    pub const NOT: u8 = 22;
    pub const BIT_NOT: u8 = 23;
    pub const CAST: u8 = 24;
    pub const CALL: u8 = 25;
    pub const CALL_VALUE: u8 = 26;
    pub const NATIVE: u8 = 27;
    pub const CLOSURE: u8 = 28;
    pub const MAKE: u8 = 29;
    pub const LIST: u8 = 30;
    pub const CONCAT: u8 = 31;
    pub const FIELD: u8 = 32;
    pub const INDEX: u8 = 33;
    pub const SET_PATH: u8 = 34;
    pub const LEN: u8 = 35;
    pub const TAG: u8 = 36;
    pub const IS_NIL: u8 = 37;
    pub const JUMP: u8 = 38;
    pub const JUMP_IF: u8 = 39;
    pub const JUMP_UNLESS: u8 = 40;
    pub const SWITCH: u8 = 41;
    pub const RETURN: u8 = 42;
    pub const EMIT: u8 = 43;
    pub const DISPLAY: u8 = 44;
    pub const DISPLAY_DIM: u8 = 45;
    pub const UNREACHABLE: u8 = 46;
    pub const START: u8 = 47;
    pub const TRANSLATE: u8 = 48;
    pub const PUSH: u8 = 49;
    pub const INSERT: u8 = 50;
    pub const REMOVE: u8 = 51;
    pub const TRUNCATE: u8 = 52;
}

fn write_op(enc: &mut Encoder, op: &Op) {
    use opcode::*;
    let regs = |enc: &mut Encoder, code: u8, rs: &[u16]| {
        enc.write_u8(code);
        for r in rs {
            enc.write_u16(*r);
        }
    };
    match *op {
        Op::Const { dst, index } => {
            regs(enc, CONST, &[dst]);
            enc.write_u32(index);
        }
        Op::Int { dst, value } => {
            regs(enc, INT, &[dst]);
            enc.write_i32(value);
        }
        Op::Nil { dst } => regs(enc, NIL, &[dst]),
        Op::Move { dst, src } => regs(enc, MOVE, &[dst, src]),
        Op::LoadState { dst, slot } => {
            regs(enc, LOAD_STATE, &[dst]);
            enc.write_u32(slot);
        }
        Op::StoreState { src, slot } => {
            regs(enc, STORE_STATE, &[src]);
            enc.write_u32(slot);
        }
        Op::LoadInput { dst, slot } => {
            regs(enc, LOAD_INPUT, &[dst]);
            enc.write_u32(slot);
        }
        Op::AddI64 { dst, a, b } => regs(enc, ADD_I64, &[dst, a, b]),
        Op::SubI64 { dst, a, b } => regs(enc, SUB_I64, &[dst, a, b]),
        Op::MulI64 { dst, a, b } => regs(enc, MUL_I64, &[dst, a, b]),
        Op::LtI64 { dst, a, b } => regs(enc, LT_I64, &[dst, a, b]),
        Op::LeI64 { dst, a, b } => regs(enc, LE_I64, &[dst, a, b]),
        Op::AddF64 { dst, a, b } => regs(enc, ADD_F64, &[dst, a, b]),
        Op::SubF64 { dst, a, b } => regs(enc, SUB_F64, &[dst, a, b]),
        Op::MulF64 { dst, a, b } => regs(enc, MUL_F64, &[dst, a, b]),
        Op::DivF64 { dst, a, b } => regs(enc, DIV_F64, &[dst, a, b]),
        Op::LtF64 { dst, a, b } => regs(enc, LT_F64, &[dst, a, b]),
        Op::LeF64 { dst, a, b } => regs(enc, LE_F64, &[dst, a, b]),
        Op::Arith { op, dst, a, b } => {
            regs(enc, ARITH, &[dst, a, b]);
            enc.write_u8(op.op() as u8);
            enc.write_u8(op.num() as u8);
        }
        Op::Eq { dst, a, b } => regs(enc, EQ, &[dst, a, b]),
        Op::Ne { dst, a, b } => regs(enc, NE, &[dst, a, b]),
        Op::Neg { num, dst, src } => {
            regs(enc, NEG, &[dst, src]);
            enc.write_u8(num as u8);
        }
        Op::Not { dst, src } => regs(enc, NOT, &[dst, src]),
        Op::BitNot { num, dst, src } => {
            regs(enc, BIT_NOT, &[dst, src]);
            enc.write_u8(num as u8);
        }
        Op::Cast { from, to, dst, src } => {
            regs(enc, CAST, &[dst, src]);
            enc.write_u8(from as u8);
            enc.write_u8(to as u8);
        }
        Op::Call { dst, ext } => {
            regs(enc, CALL, &[dst]);
            enc.write_u32(ext);
        }
        Op::CallValue { dst, ext } => {
            regs(enc, CALL_VALUE, &[dst]);
            enc.write_u32(ext);
        }
        Op::Native { dst, ext } => {
            regs(enc, NATIVE, &[dst]);
            enc.write_u32(ext);
        }
        Op::Closure { dst, ext } => {
            regs(enc, CLOSURE, &[dst]);
            enc.write_u32(ext);
        }
        Op::Make { dst, ext } => {
            regs(enc, MAKE, &[dst]);
            enc.write_u32(ext);
        }
        Op::List { dst, ext } => {
            regs(enc, LIST, &[dst]);
            enc.write_u32(ext);
        }
        Op::Concat { dst, ext } => {
            regs(enc, CONCAT, &[dst]);
            enc.write_u32(ext);
        }
        Op::Translate { dst, ext } => {
            regs(enc, TRANSLATE, &[dst]);
            enc.write_u32(ext);
        }
        Op::Field { dst, src, index } => regs(enc, FIELD, &[dst, src, index]),
        Op::Index { dst, list, index } => regs(enc, INDEX, &[dst, list, index]),
        Op::SetPath { root, ext } => {
            regs(enc, SET_PATH, &[root]);
            enc.write_u32(ext);
        }
        Op::Len { dst, src } => regs(enc, LEN, &[dst, src]),
        Op::Push { list, item } => regs(enc, PUSH, &[list, item]),
        Op::Insert { list, index, item } => regs(enc, INSERT, &[list, index, item]),
        Op::Remove { dst, list, index } => regs(enc, REMOVE, &[dst, list, index]),
        Op::Truncate { list, len } => regs(enc, TRUNCATE, &[list, len]),
        Op::Tag { dst, src } => regs(enc, TAG, &[dst, src]),
        Op::IsNil { dst, src } => regs(enc, IS_NIL, &[dst, src]),
        Op::Jump { target } => {
            enc.write_u8(JUMP);
            enc.write_u32(target);
        }
        Op::JumpIf { cond, target } => {
            regs(enc, JUMP_IF, &[cond]);
            enc.write_u32(target);
        }
        Op::JumpUnless { cond, target } => {
            regs(enc, JUMP_UNLESS, &[cond]);
            enc.write_u32(target);
        }
        Op::Switch { src, ext } => {
            regs(enc, SWITCH, &[src]);
            enc.write_u32(ext);
        }
        Op::Return { src } => regs(enc, RETURN, &[src]),
        Op::Emit { ext } => {
            enc.write_u8(EMIT);
            enc.write_u32(ext);
        }
        Op::Start { ext } => {
            enc.write_u8(START);
            enc.write_u32(ext);
        }
        Op::Display { kind, dst, src } => {
            regs(enc, DISPLAY, &[dst, src]);
            enc.write_u8(kind as u8);
        }
        Op::DisplayDim { dst, src, suffix } => regs(enc, DISPLAY_DIM, &[dst, src, suffix]),
        Op::Unreachable => enc.write_u8(UNREACHABLE),
    }
}

fn read_op(dec: &mut Decoder<'_>) -> Result<Op, DecodeError> {
    use opcode::*;
    let code = dec.read_u8()?;
    let r = |dec: &mut Decoder<'_>| dec.read_u16();
    let w = |dec: &mut Decoder<'_>| dec.read_u32();
    Ok(match code {
        CONST => Op::Const {
            dst: r(dec)?,
            index: w(dec)?,
        },
        INT => Op::Int {
            dst: r(dec)?,
            value: dec.read_i32()?,
        },
        NIL => Op::Nil { dst: r(dec)? },
        MOVE => Op::Move {
            dst: r(dec)?,
            src: r(dec)?,
        },
        LOAD_STATE => Op::LoadState {
            dst: r(dec)?,
            slot: w(dec)?,
        },
        STORE_STATE => Op::StoreState {
            src: r(dec)?,
            slot: w(dec)?,
        },
        LOAD_INPUT => Op::LoadInput {
            dst: r(dec)?,
            slot: w(dec)?,
        },
        ADD_I64..=LE_F64 | EQ | NE => {
            let (dst, a, b) = (r(dec)?, r(dec)?, r(dec)?);
            match code {
                ADD_I64 => Op::AddI64 { dst, a, b },
                SUB_I64 => Op::SubI64 { dst, a, b },
                MUL_I64 => Op::MulI64 { dst, a, b },
                LT_I64 => Op::LtI64 { dst, a, b },
                LE_I64 => Op::LeI64 { dst, a, b },
                ADD_F64 => Op::AddF64 { dst, a, b },
                SUB_F64 => Op::SubF64 { dst, a, b },
                MUL_F64 => Op::MulF64 { dst, a, b },
                DIV_F64 => Op::DivF64 { dst, a, b },
                LT_F64 => Op::LtF64 { dst, a, b },
                LE_F64 => Op::LeF64 { dst, a, b },
                EQ => Op::Eq { dst, a, b },
                _ => Op::Ne { dst, a, b },
            }
        }
        ARITH => {
            let (dst, a, b) = (r(dec)?, r(dec)?, r(dec)?);
            let op = dec.read_u8()?;
            let op = *ARITHS.get(usize::from(op)).ok_or_else(|| malformed(dec))?;
            let num = read_num(dec)?;
            Op::Arith {
                op: ArithOp::new(op, num),
                dst,
                a,
                b,
            }
        }
        NEG => Op::Neg {
            dst: r(dec)?,
            src: r(dec)?,
            num: read_num(dec)?,
        },
        NOT => Op::Not {
            dst: r(dec)?,
            src: r(dec)?,
        },
        BIT_NOT => Op::BitNot {
            dst: r(dec)?,
            src: r(dec)?,
            num: read_num(dec)?,
        },
        CAST => Op::Cast {
            dst: r(dec)?,
            src: r(dec)?,
            from: read_num(dec)?,
            to: read_num(dec)?,
        },
        CALL => Op::Call {
            dst: r(dec)?,
            ext: w(dec)?,
        },
        CALL_VALUE => Op::CallValue {
            dst: r(dec)?,
            ext: w(dec)?,
        },
        NATIVE => Op::Native {
            dst: r(dec)?,
            ext: w(dec)?,
        },
        CLOSURE => Op::Closure {
            dst: r(dec)?,
            ext: w(dec)?,
        },
        MAKE => Op::Make {
            dst: r(dec)?,
            ext: w(dec)?,
        },
        LIST => Op::List {
            dst: r(dec)?,
            ext: w(dec)?,
        },
        CONCAT => Op::Concat {
            dst: r(dec)?,
            ext: w(dec)?,
        },
        TRANSLATE => Op::Translate {
            dst: r(dec)?,
            ext: w(dec)?,
        },
        FIELD => Op::Field {
            dst: r(dec)?,
            src: r(dec)?,
            index: r(dec)?,
        },
        INDEX => Op::Index {
            dst: r(dec)?,
            list: r(dec)?,
            index: r(dec)?,
        },
        SET_PATH => Op::SetPath {
            root: r(dec)?,
            ext: w(dec)?,
        },
        LEN => Op::Len {
            dst: r(dec)?,
            src: r(dec)?,
        },
        PUSH => Op::Push {
            list: r(dec)?,
            item: r(dec)?,
        },
        INSERT => Op::Insert {
            list: r(dec)?,
            index: r(dec)?,
            item: r(dec)?,
        },
        REMOVE => Op::Remove {
            dst: r(dec)?,
            list: r(dec)?,
            index: r(dec)?,
        },
        TRUNCATE => Op::Truncate {
            list: r(dec)?,
            len: r(dec)?,
        },
        TAG => Op::Tag {
            dst: r(dec)?,
            src: r(dec)?,
        },
        IS_NIL => Op::IsNil {
            dst: r(dec)?,
            src: r(dec)?,
        },
        JUMP => Op::Jump { target: w(dec)? },
        JUMP_IF => Op::JumpIf {
            cond: r(dec)?,
            target: w(dec)?,
        },
        JUMP_UNLESS => Op::JumpUnless {
            cond: r(dec)?,
            target: w(dec)?,
        },
        SWITCH => Op::Switch {
            src: r(dec)?,
            ext: w(dec)?,
        },
        RETURN => Op::Return { src: r(dec)? },
        EMIT => Op::Emit { ext: w(dec)? },
        START => Op::Start { ext: w(dec)? },
        DISPLAY => {
            let (dst, src) = (r(dec)?, r(dec)?);
            let kind = dec.read_u8()?;
            let kind = *DISPLAY_KINDS
                .get(usize::from(kind))
                .ok_or_else(|| malformed(dec))?;
            Op::Display { kind, dst, src }
        }
        DISPLAY_DIM => Op::DisplayDim {
            dst: r(dec)?,
            src: r(dec)?,
            suffix: r(dec)?,
        },
        UNREACHABLE => Op::Unreachable,
        _ => return Err(malformed(dec)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Module {
        let ops = vec![
            Op::LoadState { dst: 1, slot: 0 },
            Op::Int { dst: 2, value: -7 },
            Op::Arith {
                op: ArithOp::new(Arith::Rem, Num::U32),
                dst: 1,
                a: 1,
                b: 2,
            },
            Op::Const { dst: 2, index: 0 },
            Op::Display {
                kind: DisplayKind::F32,
                dst: 3,
                src: 2,
            },
            Op::Cast {
                from: Num::I8,
                to: Num::F64,
                dst: 3,
                src: 1,
            },
            Op::StoreState { src: 1, slot: 0 },
            Op::Return { src: 1 },
        ];
        let spans = (0..ops.len() as u32)
            .map(|i| Span {
                start: i,
                end: i + 1,
            })
            .collect();
        let handler = Chunk {
            name: "Counter.on_click".into(),
            kind: ChunkKind::Handler,
            module: 0,
            params: 1,
            regs: 4,
            captures: Box::new([]),
            body: Ok(Code {
                ops: ops.into(),
                consts: Box::new([
                    Value::Float(1.5),
                    Value::str("x"),
                    Value::List(Rc::new(vec![Value::Nil, Value::Int(i64::MIN)])),
                    Value::Agg(Rc::new(Aggregate {
                        tag: 3,
                        fields: Box::new([Value::Int(1)]),
                    })),
                ]),
                ext: Box::new([]),
                spans,
            }),
        };
        let missing = Chunk {
            name: "f".into(),
            kind: ChunkKind::Fn,
            module: 1,
            params: 0,
            regs: 1,
            captures: Box::new([]),
            body: Err("unsupported".into()),
        };
        let tick = Chunk {
            name: "Counter.tick".into(),
            kind: ChunkKind::Action,
            ..missing.clone()
        };
        let dark = Chunk {
            name: "Dark".into(),
            kind: ChunkKind::Const,
            ..missing.clone()
        };
        let component = Component {
            name: "Counter".into(),
            states: Box::new(["count".into()]),
            state_inits: Box::new([None]),
            handlers: Box::new([0]),
            members: Box::new([("f".into(), 1), ("tick".into(), 2)]),
            ..Component::default()
        };
        let systems = vec![System {
            component: 0,
            hooks: Box::new([(NativeId(0x0123_4567_89ab_cdef), 2)]),
            id: StableId { hi: 7, lo: 9 },
            snapshot: Box::new([SnapshotSlot {
                id: StableId { hi: 1, lo: 2 },
                slot: 0,
                schema: 0xfeed,
            }]),
            locals: Box::new([]),
            persist: Box::new([PersistSlot {
                key: "best".into(),
                slot: 0,
                schema: ValueSchema {
                    root: crate::retype::TypeDesc::Plain("Bool".into()),
                    decls: Box::new([]),
                },
                spelling: "Bool".into(),
            }]),
        }];
        let natives = vec![NativeImport {
            path: "viso::text::upper".into(),
            signature: 0xdead_beef,
            params: 1,
        }];
        Module::new(
            vec![handler, missing, tick, dark],
            vec![component],
            systems,
            natives,
        )
        .unwrap()
        .with_tick_rate(30)
        .unwrap()
        .with_capabilities(["storage.persist", "network.http"])
        .with_themes(vec![("Dark".into(), 3)])
        .unwrap()
    }

    #[test]
    fn every_nan_encodes_alike() {
        let encode = |v: f64| {
            let mut enc = Encoder::new();
            write_value(&mut enc, &Value::Float(v));
            enc.into_bytes()
        };
        let x86 = f64::from_bits(0xfff8_0000_0000_0000);
        let payload = f64::from_bits(0x7ff8_0000_0000_beef);
        assert_eq!(encode(x86), encode(f64::NAN));
        assert_eq!(encode(payload), encode(f64::NAN));
        assert_ne!(encode(-0.0), encode(0.0), "signed zeros stay distinct");
        assert_eq!(
            canonical_f32_bits(f32::from_bits(0xffc0_0001)),
            f32::NAN.to_bits()
        );
    }

    #[test]
    fn a_module_round_trips() {
        let module = sample();
        let decoded = Module::decode(&module.encode()).unwrap();
        assert_eq!(decoded.tick_rate(), 30);
        assert_eq!(
            decoded.capabilities(),
            [Box::from("network.http"), Box::from("storage.persist")]
        );
        assert_eq!(decoded.theme("Dark"), Some(3));
        assert_eq!(decoded.theme("Light"), None);
        assert_eq!(decoded, module);
    }

    #[test]
    fn a_system_snapshots_its_own_states_in_identity_order() {
        let module = sample();
        let rebuild = |states: Vec<SnapshotSlot>| {
            let mut systems = module.systems().to_vec();
            systems[0].snapshot = states.into();
            Module::new(
                module.chunks().to_vec(),
                module.components().to_vec(),
                systems,
                module.natives().to_vec(),
            )
        };
        let slot = |lo, slot| SnapshotSlot {
            id: StableId { hi: 0, lo },
            slot,
            schema: 0,
        };
        assert!(rebuild(vec![slot(1, 0)]).is_ok());
        assert!(rebuild(vec![slot(1, 1)]).is_err(), "a missing state");
        assert!(
            rebuild(vec![slot(2, 0), slot(1, 0)]).is_err(),
            "out of order"
        );
        assert!(module.clone().with_tick_rate(0).is_err());
        let local = |states: Vec<SnapshotSlot>| {
            let mut systems = module.systems().to_vec();
            systems[0].snapshot = Box::new([]);
            systems[0].locals = states.into();
            Module::new(
                module.chunks().to_vec(),
                module.components().to_vec(),
                systems,
                module.natives().to_vec(),
            )
        };
        let carried = local(vec![slot(1, 0)]).unwrap();
        assert_eq!(Module::decode(&carried.encode()).unwrap(), carried);
        assert!(local(vec![slot(1, 1)]).is_err(), "a missing local");
        assert!(local(vec![slot(2, 0), slot(1, 0)]).is_err());
    }

    #[test]
    fn every_truncation_is_an_error_not_a_panic() {
        let bytes = sample().encode();
        for len in 0..bytes.len() {
            assert!(Module::decode(&bytes[..len]).is_err(), "prefix {len}");
        }
    }

    #[test]
    fn a_theme_names_one_constant_chunk() {
        let module = || sample().with_themes(Vec::new()).unwrap();
        assert!(module().with_themes(vec![("Bad".into(), 1)]).is_err());
        assert!(module().with_themes(vec![("Gone".into(), 9)]).is_err());
        let twice = vec![("Dark".into(), 3), ("Dark".into(), 3)];
        assert!(module().with_themes(twice).is_err());
    }

    #[test]
    fn a_blob_whose_code_does_not_verify_is_rejected() {
        let mut module = sample();
        let mut bytes = module.encode();
        // Point the handler at a chunk that does not exist.
        module = Module::decode(&bytes).unwrap();
        let mut components = module.components().to_vec();
        components[0].handlers = Box::new([9]);
        let chunks = module.chunks().to_vec();
        let bad = Module {
            chunks: chunks.into(),
            components: components.into(),
            systems: module.systems().to_vec().into(),
            natives: module.natives().to_vec().into(),
            input: None,
            tick_rate: module.tick_rate(),
            determinism: module.determinism(),
            collision_delivery: module.collision_delivery(),
            migrators: Box::new([]),
            capabilities: Box::new([]),
            themes: Box::new([]),
            catalog: None,
        };
        bytes = bad.encode();
        assert!(matches!(Module::decode(&bytes), Err(LoadError::Verify(_))));
    }

    #[test]
    fn a_system_hook_must_be_an_action_of_its_component() {
        let module = sample();
        let with = |hooks: Box<[(NativeId, u32)]>| {
            Module::new(
                module.chunks().to_vec(),
                module.components().to_vec(),
                vec![System {
                    component: 0,
                    hooks,
                    id: StableId::default(),
                    snapshot: Box::new([]),
                    locals: Box::new([]),
                    persist: Box::new([]),
                }],
                module.natives().to_vec(),
            )
        };
        // `f` is a member but a `fn`; chunk 0 is a handler, no member.
        assert!(with(Box::new([(NativeId(1), 1)])).is_err());
        assert!(with(Box::new([(NativeId(1), 0)])).is_err());
        assert!(with(Box::new([(NativeId(1), 2), (NativeId(1), 2)])).is_err());
        assert!(with(Box::new([(NativeId(1), 2)])).is_ok());
    }
}
