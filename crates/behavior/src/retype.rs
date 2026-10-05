//! Converting a value from one version of its type to another.
//!
//! A [`ValueSchema`] describes a type as one build declares it: its
//! structure, and each record and enum it reaches by stable identity with
//! their field and variant names and the chunk computing each defaulted
//! field. The schemas of a value's old and new type give the [`Retyping`]
//! that carries the value across, by a closed matrix: an exact type keeps
//! the value; an integer converts to another integer type or to a float when
//! the new type holds the value exactly (always, for a widening); a value
//! becomes the payload of an `Option`; a container converts elementwise; a
//! record of the same declaration keeps its fields by name, takes each new
//! field's default and drops each removed one; an enum of the same
//! declaration keeps each variant by name whose payload converts. Anything
//! else does not convert.

use std::collections::HashMap;
use std::rc::Rc;

use viso_ende::{DecodeError, Decoder, Encoder};

use crate::module::StableId;
use crate::value::{Aggregate, Value};
use crate::wire::{
    malformed, read_list, read_stable_id, read_u32_varint, write_list, write_stable_id,
};

/// How a value of an old type becomes one of a new type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Conversion {
    /// The value already is one of the new type.
    Keep,
    /// An integer becomes the integer of another type, when that type holds it.
    Int(IntType),
    /// An integer becomes the float of the same value, when an `F32` (`true`)
    /// or an `F64` holds it exactly.
    ToFloat(bool),
    /// `None` stays `None`; a payload converts.
    Option(Box<Conversion>),
    /// Each item converts.
    List(Box<Conversion>),
    /// Each field of a tuple or range converts by position.
    Fields(Box<[Conversion]>),
    /// The payload of `Ok` or of `Err` converts.
    Result(Box<Conversion>, Box<Conversion>),
    /// A resource state's value (`ready`, `reloading`) or error payload
    /// converts; `idle` and `loading` stay.
    Resource(Box<Conversion>, Box<Conversion>),
    /// A record or enum converts by entry `n` of the [`Retyping`]'s table.
    Named(u32),
}

/// An integer type: its bits, and whether it is signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntType {
    pub bits: u32,
    pub signed: bool,
}

impl IntType {
    /// Whether the type holds `value`.
    fn holds(self, value: i64) -> bool {
        let value = i128::from(value);
        if self.signed {
            let bound = 1i128 << (self.bits - 1);
            (-bound..bound).contains(&value)
        } else {
            (0..1i128 << self.bits).contains(&value)
        }
    }
}

/// Where a field of a converted record or payload comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldSource {
    /// The old field at this index, converted.
    Old(u32, Conversion),
    /// The new field's default, computed by the chunk.
    Default(u32),
}

/// The variant an old enum variant becomes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantMap {
    /// The new variant's tag.
    pub tag: u32,
    /// Whether the new variant carries no payload.
    pub unit: bool,
    /// The new payload's fields.
    pub fields: Box<[FieldSource]>,
}

/// How a value of a record or enum declaration converts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// The new record's fields, in declaration order.
    Record(Box<[FieldSource]>),
    /// By old tag, the variant each old variant becomes; `None` for a variant
    /// that was removed or whose payload does not convert.
    Enum(Box<[Option<VariantMap>]>),
}

/// The conversion of one value: its root, and the record and enum
/// conversions it refers to, which may refer to each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retyping {
    pub root: Conversion,
    pub named: Vec<Shape>,
}

impl Retyping {
    /// The conversion keeping a value as it is.
    pub fn keep() -> Retyping {
        Retyping {
            root: Conversion::Keep,
            named: Vec::new(),
        }
    }

    /// How a value of `old` becomes one of `new`, or `None` when the matrix
    /// has no conversion between them.
    pub fn between(old: &ValueSchema, new: &ValueSchema) -> Option<Retyping> {
        let mut matrix = Matrix {
            old,
            new,
            named: Vec::new(),
            memo: HashMap::new(),
        };
        let root = matrix.convert(&old.root, &new.root)?;
        Some(Retyping {
            root,
            named: matrix.named,
        })
    }

    /// `value` converted, each new record field's default computed by
    /// `default` from its chunk. `None` when the value has no counterpart in
    /// the new type: its variant was removed, or a default did not compute.
    pub fn apply(
        &self,
        value: &Value,
        default: &mut dyn FnMut(u32) -> Option<Value>,
    ) -> Option<Value> {
        self.convert(&self.root, value, default)
    }

    fn convert(
        &self,
        conversion: &Conversion,
        value: &Value,
        default: &mut dyn FnMut(u32) -> Option<Value>,
    ) -> Option<Value> {
        match conversion {
            Conversion::Keep => Some(value.clone()),
            Conversion::Int(ty) => {
                let int = value.as_int()?;
                ty.holds(int).then_some(Value::Int(int))
            }
            Conversion::ToFloat(single) => {
                let int = value.as_int()?;
                let float = if *single {
                    f64::from(int as f32)
                } else {
                    int as f64
                };
                // The float holds the integer exactly when it converts back to
                // it; compared wider than either, as a saturating cast back
                // would compare equal at the bounds.
                (float as i128 == i128::from(int)).then_some(Value::Float(float))
            }
            Conversion::Option(inner) => match value {
                Value::Nil => Some(Value::Nil),
                value => self.convert(inner, value, default),
            },
            Conversion::List(item) => match value {
                Value::List(items) => {
                    let items = items
                        .iter()
                        .map(|v| self.convert(item, v, default))
                        .collect::<Option<Vec<_>>>()?;
                    Some(Value::List(Rc::new(items)))
                }
                _ => None,
            },
            Conversion::Fields(fields) => {
                let Value::Agg(agg) = value else {
                    return None;
                };
                if agg.fields.len() != fields.len() {
                    return None;
                }
                let fields = fields
                    .iter()
                    .zip(agg.fields.iter())
                    .map(|(c, v)| self.convert(c, v, default))
                    .collect::<Option<Box<[_]>>>()?;
                Some(aggregate(agg.tag, fields))
            }
            Conversion::Result(ok, err) => {
                let Value::Agg(agg) = value else {
                    return None;
                };
                let payload = match agg.tag {
                    0 => ok,
                    _ => err,
                };
                let inner = self.convert(payload, agg.fields.first()?, default)?;
                Some(aggregate(agg.tag, Box::new([inner])))
            }
            Conversion::Resource(value_of, error_of) => {
                let Value::Agg(agg) = value else {
                    return Some(value.clone());
                };
                let payload = match agg.tag {
                    3 => error_of,
                    _ => value_of,
                };
                let inner = self.convert(payload, agg.fields.first()?, default)?;
                Some(aggregate(agg.tag, Box::new([inner])))
            }
            Conversion::Named(index) => match self.named.get(*index as usize)? {
                Shape::Record(fields) => {
                    let Value::Agg(agg) = value else {
                        return None;
                    };
                    let fields = self.fields(fields, &agg.fields, default)?;
                    Some(aggregate(agg.tag, fields))
                }
                Shape::Enum(variants) => {
                    let (tag, old) = match value {
                        Value::Int(tag) => (u32::try_from(*tag).ok()?, &[][..]),
                        Value::Agg(agg) => (agg.tag, &agg.fields[..]),
                        _ => return None,
                    };
                    let map = variants.get(tag as usize)?.as_ref()?;
                    if map.unit {
                        return Some(Value::Int(i64::from(map.tag)));
                    }
                    let fields = self.fields(&map.fields, old, default)?;
                    Some(aggregate(map.tag, fields))
                }
            },
        }
    }

    fn fields(
        &self,
        fields: &[FieldSource],
        old: &[Value],
        default: &mut dyn FnMut(u32) -> Option<Value>,
    ) -> Option<Box<[Value]>> {
        fields
            .iter()
            .map(|field| match field {
                FieldSource::Old(index, c) => self.convert(c, old.get(*index as usize)?, default),
                FieldSource::Default(chunk) => default(*chunk),
            })
            .collect()
    }
}

fn aggregate(tag: u32, fields: Box<[Value]>) -> Value {
    Value::Agg(Rc::new(Aggregate { tag, fields }))
}

/// A type as one build declares it: its structure, each record and enum it
/// reaches in [`decls`](Self::decls).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueSchema {
    pub root: TypeDesc,
    pub decls: Box<[TypeDecl]>,
}

/// The structure of a type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeDesc {
    Int(IntType),
    F32,
    F64,
    Unit,
    /// Any other scalar, by its name: two of the same name hold the same
    /// values.
    Plain(Box<str>),
    /// A native type, by its identity.
    Native(u64),
    Tuple(Box<[TypeDesc]>),
    List(Box<TypeDesc>),
    Option(Box<TypeDesc>),
    Result(Box<TypeDesc>, Box<TypeDesc>),
    /// `ResourceState<T, E>`.
    Resource(Box<TypeDesc>, Box<TypeDesc>),
    Range(Box<TypeDesc>),
    RangeInclusive(Box<TypeDesc>),
    Fn(Box<[TypeDesc]>, Box<TypeDesc>),
    /// The record or enum at this index of the schema's declarations.
    Named(u32),
}

/// A record or enum declaration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDecl {
    /// Its stable identity, which pairs it with its other versions.
    pub id: StableId,
    pub name: Box<str>,
    pub body: DeclBody,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclBody {
    Record(Box<[FieldDesc]>),
    Enum(Box<[VariantDesc]>),
    /// A nominal type whose structure the schema does not describe: equal to
    /// itself, converting to nothing.
    Opaque,
}

/// A field of a record or record payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDesc {
    pub name: Box<str>,
    pub ty: TypeDesc,
    /// The chunk computing its default, when it has one.
    pub default: Option<u32>,
}

/// An enum variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantDesc {
    pub name: Box<str>,
    pub payload: PayloadDesc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PayloadDesc {
    Unit,
    Tuple(Box<[TypeDesc]>),
    Record(Box<[FieldDesc]>),
}

impl ValueSchema {
    /// Whether `other` describes the same type: each declaration paired by
    /// identity and compared field by field and variant by variant, defaults
    /// aside.
    pub fn same(&self, other: &ValueSchema) -> bool {
        Exact {
            a: self,
            b: other,
            visiting: Vec::new(),
        }
        .equal(&self.root, &other.root)
    }

    /// Every chunk the schema names: its field defaults.
    pub fn chunks(&self) -> impl Iterator<Item = u32> + '_ {
        self.decls.iter().flat_map(|decl| {
            let fields: Box<dyn Iterator<Item = &FieldDesc>> = match &decl.body {
                DeclBody::Record(fields) => Box::new(fields.iter()),
                DeclBody::Enum(variants) => {
                    Box::new(variants.iter().flat_map(|v| match &v.payload {
                        PayloadDesc::Record(fields) => &fields[..],
                        _ => &[],
                    }))
                }
                DeclBody::Opaque => Box::new([].iter()),
            };
            fields.filter_map(|f| f.default)
        })
    }

    /// Writes the schema to `enc`.
    pub fn encode(&self, enc: &mut Encoder) {
        write_desc(enc, &self.root);
        write_list(enc, &self.decls, |enc, decl| {
            write_stable_id(enc, decl.id);
            enc.write_str(&decl.name);
            match &decl.body {
                DeclBody::Record(fields) => {
                    enc.write_u8(0);
                    write_fields(enc, fields);
                }
                DeclBody::Enum(variants) => {
                    enc.write_u8(1);
                    write_list(enc, variants, |enc, v| {
                        enc.write_str(&v.name);
                        match &v.payload {
                            PayloadDesc::Unit => enc.write_u8(0),
                            PayloadDesc::Tuple(items) => {
                                enc.write_u8(1);
                                write_list(enc, items, write_desc);
                            }
                            PayloadDesc::Record(fields) => {
                                enc.write_u8(2);
                                write_fields(enc, fields);
                            }
                        }
                    });
                }
                DeclBody::Opaque => enc.write_u8(2),
            }
        });
    }

    /// Reads a schema [`encode`](Self::encode) wrote.
    ///
    /// # Errors
    ///
    /// A malformed schema: truncated, nested too deep, or naming a
    /// declaration it lacks.
    pub fn decode(dec: &mut Decoder<'_>) -> Result<ValueSchema, DecodeError> {
        let root = read_desc(dec, 0)?;
        let decls: Box<[TypeDecl]> = read_list(dec, |dec| {
            let id = read_stable_id(dec)?;
            let name = dec.read_str()?.into();
            let body = match dec.read_u8()? {
                0 => DeclBody::Record(read_fields(dec)?),
                1 => DeclBody::Enum(
                    read_list(dec, |dec| {
                        let name = dec.read_str()?.into();
                        let payload = match dec.read_u8()? {
                            0 => PayloadDesc::Unit,
                            1 => PayloadDesc::Tuple(read_list(dec, |d| read_desc(d, 1))?.into()),
                            2 => PayloadDesc::Record(read_fields(dec)?),
                            _ => return Err(malformed(dec)),
                        };
                        Ok(VariantDesc { name, payload })
                    })?
                    .into(),
                ),
                2 => DeclBody::Opaque,
                _ => return Err(malformed(dec)),
            };
            Ok(TypeDecl { id, name, body })
        })?
        .into();
        let schema = ValueSchema { root, decls };
        if schema.names_within() {
            Ok(schema)
        } else {
            Err(malformed(dec))
        }
    }

    /// Whether every declaration the schema names is one of its own.
    fn names_within(&self) -> bool {
        let n = self.decls.len() as u32;
        let fields = |fields: &[FieldDesc]| fields.iter().all(|f| within(&f.ty, n));
        within(&self.root, n)
            && self.decls.iter().all(|decl| match &decl.body {
                DeclBody::Record(fs) => fields(fs),
                DeclBody::Enum(variants) => variants.iter().all(|v| match &v.payload {
                    PayloadDesc::Unit => true,
                    PayloadDesc::Tuple(items) => items.iter().all(|t| within(t, n)),
                    PayloadDesc::Record(fs) => fields(fs),
                }),
                DeclBody::Opaque => true,
            })
    }
}

fn within(desc: &TypeDesc, n: u32) -> bool {
    match desc {
        TypeDesc::Named(i) => *i < n,
        TypeDesc::Tuple(items) => items.iter().all(|t| within(t, n)),
        TypeDesc::Fn(params, ret) => params.iter().all(|t| within(t, n)) && within(ret, n),
        TypeDesc::List(t)
        | TypeDesc::Option(t)
        | TypeDesc::Range(t)
        | TypeDesc::RangeInclusive(t) => within(t, n),
        TypeDesc::Result(a, b) | TypeDesc::Resource(a, b) => within(a, n) && within(b, n),
        _ => true,
    }
}

/// The deepest type nesting a decoder follows.
const MAX_DEPTH: u32 = 64;

fn write_desc(enc: &mut Encoder, desc: &TypeDesc) {
    let one = |enc: &mut Encoder, tag: u8, inner: &TypeDesc| {
        enc.write_u8(tag);
        write_desc(enc, inner);
    };
    match desc {
        TypeDesc::Int(ty) => {
            enc.write_u8(0);
            enc.write_u8(ty.bits as u8);
            enc.write_bool(ty.signed);
        }
        TypeDesc::F32 => enc.write_u8(1),
        TypeDesc::F64 => enc.write_u8(2),
        TypeDesc::Unit => enc.write_u8(3),
        TypeDesc::Plain(name) => {
            enc.write_u8(4);
            enc.write_str(name);
        }
        TypeDesc::Native(id) => {
            enc.write_u8(5);
            enc.write_u64(*id);
        }
        TypeDesc::Tuple(items) => {
            enc.write_u8(6);
            write_list(enc, items, write_desc);
        }
        TypeDesc::List(t) => one(enc, 7, t),
        TypeDesc::Option(t) => one(enc, 8, t),
        TypeDesc::Result(a, b) => {
            one(enc, 9, a);
            write_desc(enc, b);
        }
        TypeDesc::Range(t) => one(enc, 10, t),
        TypeDesc::RangeInclusive(t) => one(enc, 11, t),
        TypeDesc::Fn(params, ret) => {
            enc.write_u8(12);
            write_list(enc, params, write_desc);
            write_desc(enc, ret);
        }
        TypeDesc::Named(index) => {
            enc.write_u8(13);
            enc.write_varint(u64::from(*index));
        }
        TypeDesc::Resource(a, b) => {
            one(enc, 14, a);
            write_desc(enc, b);
        }
    }
}

fn read_desc(dec: &mut Decoder<'_>, depth: u32) -> Result<TypeDesc, DecodeError> {
    if depth > MAX_DEPTH {
        return Err(malformed(dec));
    }
    let nested = |dec: &mut Decoder<'_>| read_desc(dec, depth + 1).map(Box::new);
    Ok(match dec.read_u8()? {
        0 => {
            let bits = u32::from(dec.read_u8()?);
            if ![8, 16, 32, 64].contains(&bits) {
                return Err(malformed(dec));
            }
            TypeDesc::Int(IntType {
                bits,
                signed: dec.read_bool()?,
            })
        }
        1 => TypeDesc::F32,
        2 => TypeDesc::F64,
        3 => TypeDesc::Unit,
        4 => TypeDesc::Plain(dec.read_str()?.into()),
        5 => TypeDesc::Native(dec.read_u64()?),
        6 => TypeDesc::Tuple(read_list(dec, |d| read_desc(d, depth + 1))?.into()),
        7 => TypeDesc::List(nested(dec)?),
        8 => TypeDesc::Option(nested(dec)?),
        9 => TypeDesc::Result(nested(dec)?, nested(dec)?),
        10 => TypeDesc::Range(nested(dec)?),
        11 => TypeDesc::RangeInclusive(nested(dec)?),
        12 => TypeDesc::Fn(
            read_list(dec, |d| read_desc(d, depth + 1))?.into(),
            nested(dec)?,
        ),
        13 => TypeDesc::Named(read_u32_varint(dec)?),
        14 => TypeDesc::Resource(nested(dec)?, nested(dec)?),
        _ => return Err(malformed(dec)),
    })
}

fn write_fields(enc: &mut Encoder, fields: &[FieldDesc]) {
    write_list(enc, fields, |enc, f| {
        enc.write_str(&f.name);
        write_desc(enc, &f.ty);
        enc.write_bool(f.default.is_some());
        if let Some(chunk) = f.default {
            enc.write_varint(u64::from(chunk));
        }
    });
}

fn read_fields(dec: &mut Decoder<'_>) -> Result<Box<[FieldDesc]>, DecodeError> {
    Ok(read_list(dec, |dec| {
        let name = dec.read_str()?.into();
        let ty = read_desc(dec, 1)?;
        let default = if dec.read_bool()? {
            Some(read_u32_varint(dec)?)
        } else {
            None
        };
        Ok(FieldDesc { name, ty, default })
    })?
    .into())
}

/// Whether two schemas' types are the same.
struct Exact<'a> {
    a: &'a ValueSchema,
    b: &'a ValueSchema,
    /// The declarations being compared, assumed equal while their own fields
    /// are: a recursive type is equal unless something it holds differs.
    visiting: Vec<StableId>,
}

impl Exact<'_> {
    fn equal(&mut self, x: &TypeDesc, y: &TypeDesc) -> bool {
        use TypeDesc::*;
        match (x, y) {
            (Named(i), Named(j)) => self.declaration(*i, *j),
            (Tuple(xs), Tuple(ys)) => self.all(xs, ys),
            (Fn(xp, xr), Fn(yp, yr)) => self.all(xp, yp) && self.equal(xr, yr),
            (List(x), List(y))
            | (Option(x), Option(y))
            | (Range(x), Range(y))
            | (RangeInclusive(x), RangeInclusive(y)) => self.equal(x, y),
            (Result(xt, xe), Result(yt, ye)) | (Resource(xt, xe), Resource(yt, ye)) => {
                self.equal(xt, yt) && self.equal(xe, ye)
            }
            (Int(_) | F32 | F64 | Unit | Plain(_) | Native(_), _) => x == y,
            _ => false,
        }
    }

    fn all(&mut self, xs: &[TypeDesc], ys: &[TypeDesc]) -> bool {
        xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.equal(x, y))
    }

    fn fields(&mut self, xs: &[FieldDesc], ys: &[FieldDesc]) -> bool {
        xs.len() == ys.len()
            && xs
                .iter()
                .zip(ys)
                .all(|(x, y)| x.name == y.name && self.equal(&x.ty, &y.ty))
    }

    fn declaration(&mut self, i: u32, j: u32) -> bool {
        let (a, b) = (self.a, self.b);
        let (Some(x), Some(y)) = (a.decls.get(i as usize), b.decls.get(j as usize)) else {
            return false;
        };
        if x.id != y.id {
            return false;
        }
        if self.visiting.contains(&x.id) {
            return true;
        }
        self.visiting.push(x.id);
        let equal = match (&x.body, &y.body) {
            (DeclBody::Record(xs), DeclBody::Record(ys)) => self.fields(xs, ys),
            (DeclBody::Enum(xs), DeclBody::Enum(ys)) => {
                xs.len() == ys.len()
                    && xs
                        .iter()
                        .zip(ys.iter())
                        .all(|(v, w)| v.name == w.name && self.payload(&v.payload, &w.payload))
            }
            (DeclBody::Opaque, DeclBody::Opaque) => true,
            _ => false,
        };
        self.visiting.pop();
        equal
    }

    fn payload(&mut self, x: &PayloadDesc, y: &PayloadDesc) -> bool {
        match (x, y) {
            (PayloadDesc::Unit, PayloadDesc::Unit) => true,
            (PayloadDesc::Tuple(xs), PayloadDesc::Tuple(ys)) => self.all(xs, ys),
            (PayloadDesc::Record(xs), PayloadDesc::Record(ys)) => self.fields(xs, ys),
            _ => false,
        }
    }
}

/// The compatibility matrix from one schema into another, building one
/// value's [`Retyping`].
struct Matrix<'a> {
    old: &'a ValueSchema,
    new: &'a ValueSchema,
    named: Vec<Shape>,
    /// Each old declaration's table entry, `None` once it is known not to
    /// convert.
    memo: HashMap<u32, Option<u32>>,
}

impl Matrix<'_> {
    fn convert(&mut self, old: &TypeDesc, new: &TypeDesc) -> Option<Conversion> {
        use TypeDesc::*;
        if let Some(conversion) = widen(old, new) {
            return Some(conversion);
        }
        match (old, new) {
            (Named(i), Named(j)) => self.declaration(*i, *j).map(Conversion::Named),
            (Option(x), Option(y)) => Some(Conversion::Option(Box::new(self.convert(x, y)?))),
            (List(x), List(y)) => Some(Conversion::List(Box::new(self.convert(x, y)?))),
            (Tuple(xs), Tuple(ys)) if xs.len() == ys.len() => {
                let fields = xs
                    .iter()
                    .zip(ys.iter())
                    .map(|(x, y)| self.convert(x, y))
                    .collect::<std::option::Option<Box<[_]>>>()?;
                Some(Conversion::Fields(fields))
            }
            (Range(x), Range(y)) | (RangeInclusive(x), RangeInclusive(y)) => {
                let bound = self.convert(x, y)?;
                Some(Conversion::Fields(Box::new([bound.clone(), bound])))
            }
            (Result(xt, xe), Result(yt, ye)) => Some(Conversion::Result(
                Box::new(self.convert(xt, yt)?),
                Box::new(self.convert(xe, ye)?),
            )),
            (Resource(xt, xe), Resource(yt, ye)) => Some(Conversion::Resource(
                Box::new(self.convert(xt, yt)?),
                Box::new(self.convert(xe, ye)?),
            )),
            (old, Option(y)) if !matches!(old, Option(_) | Unit) => self.convert(old, y),
            _ => None,
        }
    }

    /// The table entry converting values of old declaration `i` into new
    /// declaration `j`.
    fn declaration(&mut self, i: u32, j: u32) -> Option<u32> {
        let (old, new) = (self.old, self.new);
        let (x, y) = (old.decls.get(i as usize)?, new.decls.get(j as usize)?);
        if x.id != y.id {
            return None;
        }
        if let Some(&entry) = self.memo.get(&i) {
            return entry;
        }
        // The entry is reserved before its fields convert, so a recursive
        // reference converts by it; if the declaration then does not convert,
        // every entry made since is discarded with it.
        let index = self.named.len() as u32;
        self.named.push(Shape::Record(Box::new([])));
        self.memo.insert(i, Some(index));
        let shape = match (&x.body, &y.body) {
            (DeclBody::Record(xs), DeclBody::Record(ys)) => self.record(xs, ys).map(Shape::Record),
            (DeclBody::Enum(xs), DeclBody::Enum(ys)) => Some(Shape::Enum(self.variants(xs, ys))),
            _ => None,
        };
        match shape {
            Some(shape) => {
                self.named[index as usize] = shape;
                Some(index)
            }
            None => {
                self.named.truncate(index as usize);
                self.memo
                    .retain(|_, entry| entry.is_none_or(|entry| entry < index));
                self.memo.insert(i, None);
                None
            }
        }
    }

    fn record(&mut self, old: &[FieldDesc], new: &[FieldDesc]) -> Option<Box<[FieldSource]>> {
        new.iter()
            .map(
                |field| match old.iter().position(|f| f.name == field.name) {
                    Some(from) => Some(FieldSource::Old(
                        from as u32,
                        self.convert(&old[from].ty, &field.ty)?,
                    )),
                    None => field.default.map(FieldSource::Default),
                },
            )
            .collect()
    }

    fn variants(&mut self, old: &[VariantDesc], new: &[VariantDesc]) -> Box<[Option<VariantMap>]> {
        old.iter()
            .map(|variant| {
                let tag = new.iter().position(|v| v.name == variant.name)?;
                let fields: Box<[FieldSource]> = match (&variant.payload, &new[tag].payload) {
                    (PayloadDesc::Unit, PayloadDesc::Unit) => Box::new([]),
                    (PayloadDesc::Tuple(xs), PayloadDesc::Tuple(ys)) if xs.len() == ys.len() => xs
                        .iter()
                        .zip(ys.iter())
                        .enumerate()
                        .map(|(i, (x, y))| Some(FieldSource::Old(i as u32, self.convert(x, y)?)))
                        .collect::<Option<_>>()?,
                    (PayloadDesc::Record(xs), PayloadDesc::Record(ys)) => ys
                        .iter()
                        .map(|field| {
                            let from = xs.iter().position(|f| f.name == field.name)?;
                            Some(FieldSource::Old(
                                from as u32,
                                self.convert(&xs[from].ty, &field.ty)?,
                            ))
                        })
                        .collect::<Option<_>>()?,
                    _ => return None,
                };
                Some(VariantMap {
                    tag: tag as u32,
                    unit: matches!(new[tag].payload, PayloadDesc::Unit),
                    fields,
                })
            })
            .collect()
    }
}

/// The conversion of a scalar `old` into the scalar `new`, if any: `Keep` when
/// `new` holds each of `old`'s values, a checked conversion when it holds some.
fn widen(old: &TypeDesc, new: &TypeDesc) -> Option<Conversion> {
    use TypeDesc::*;
    match (old, new) {
        (Int(a), Int(b)) => {
            let widens = match (a.signed, b.signed) {
                (true, true) | (false, false) => b.bits >= a.bits,
                (false, true) => b.bits > a.bits,
                (true, false) => false,
            };
            Some(if widens {
                Conversion::Keep
            } else {
                Conversion::Int(*b)
            })
        }
        (Int(_), F32) => Some(Conversion::ToFloat(true)),
        (Int(_), F64) => Some(Conversion::ToFloat(false)),
        (F32, F32 | F64) | (F64, F64) | (Unit, Unit) => Some(Conversion::Keep),
        (Plain(a), Plain(b)) if a == b => Some(Conversion::Keep),
        (Native(a), Native(b)) if a == b => Some(Conversion::Keep),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const I32: TypeDesc = TypeDesc::Int(IntType {
        bits: 32,
        signed: true,
    });
    const U8: TypeDesc = TypeDesc::Int(IntType {
        bits: 8,
        signed: false,
    });

    fn record(fields: Vec<FieldDesc>) -> ValueSchema {
        ValueSchema {
            root: TypeDesc::Named(0),
            decls: Box::new([TypeDecl {
                id: StableId { hi: 1, lo: 2 },
                name: "Save".into(),
                body: DeclBody::Record(fields.into()),
            }]),
        }
    }

    fn field(name: &str, ty: TypeDesc, default: Option<u32>) -> FieldDesc {
        FieldDesc {
            name: name.into(),
            ty,
            default,
        }
    }

    fn agg(fields: Vec<Value>) -> Value {
        aggregate(0, fields.into())
    }

    #[test]
    fn a_record_keeps_fields_by_name_widens_and_takes_new_defaults() {
        let old = record(vec![field("best", U8, None), field("gone", I32, None)]);
        let new = record(vec![
            field("level", I32, Some(7)),
            field("best", TypeDesc::F64, None),
        ]);
        assert!(!old.same(&new));
        let retyping = Retyping::between(&old, &new).expect("converts");
        let value = agg(vec![Value::Int(200), Value::Int(-1)]);
        let converted = retyping.apply(&value, &mut |chunk| Some(Value::Int(i64::from(chunk) * 2)));
        assert_eq!(
            converted,
            Some(agg(vec![Value::Int(14), Value::Float(200.0)]))
        );
        let narrowed = Retyping::between(&new, &old);
        assert!(narrowed.is_none(), "`gone` has no default");
    }

    #[test]
    fn a_narrowing_converts_only_the_values_it_holds() {
        let wide = ValueSchema {
            root: I32,
            decls: Box::new([]),
        };
        let narrow = ValueSchema {
            root: U8,
            decls: Box::new([]),
        };
        let retyping = Retyping::between(&wide, &narrow).expect("checked");
        let none = &mut |_| None;
        assert_eq!(
            retyping.apply(&Value::Int(255), none),
            Some(Value::Int(255))
        );
        assert_eq!(retyping.apply(&Value::Int(256), none), None);
    }

    #[test]
    fn a_schema_round_trips_and_a_corrupt_one_is_an_error() {
        let schema = record(vec![
            field("best", TypeDesc::Option(Box::new(I32)), Some(3)),
            field(
                "names",
                TypeDesc::List(Box::new(TypeDesc::Plain("String".into()))),
                None,
            ),
        ]);
        let mut enc = Encoder::new();
        schema.encode(&mut enc);
        let bytes = enc.into_bytes();
        let back = ValueSchema::decode(&mut Decoder::new(&bytes)).expect("decodes");
        assert_eq!(back, schema);
        assert!(back.same(&schema));
        assert_eq!(back.chunks().collect::<Vec<_>>(), [3]);
        for cut in 0..bytes.len() {
            assert!(ValueSchema::decode(&mut Decoder::new(&bytes[..cut])).is_err());
        }
        let dangling = ValueSchema {
            root: TypeDesc::Named(4),
            decls: Box::new([]),
        };
        let mut enc = Encoder::new();
        dangling.encode(&mut enc);
        let bytes = enc.into_bytes();
        assert!(ValueSchema::decode(&mut Decoder::new(&bytes)).is_err());
    }
}
