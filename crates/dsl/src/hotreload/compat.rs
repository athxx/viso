//! State-type compatibility — whether a kept state's live value carries into
//! its recompiled type, and the conversion that carries it. Pure, like the
//! stages before the commit: it compares the two candidates' declarations and
//! returns plain data the commit applies to the live values.
//!
//! The matrix is closed: an exact type keeps the value; an integer converts to
//! another integer type or to a float when the new type holds the live value
//! exactly (always, for a widening); a value becomes the
//! payload of an `Option`; a container converts elementwise; a record of the
//! same declaration keeps its fields by name, takes each new field's default
//! and drops each removed one; an enum of the same declaration keeps each
//! variant by name whose payload converts. Anything else does not convert:
//! the state is reset to its new initializer, unless a `@migrate` function
//! converts from its old type.

use std::collections::HashMap;
use std::rc::Rc;

use viso_behavior::{Aggregate, Value};

use crate::hir::{FieldInfo, Ty, TypeSchemas, VariantInfo, VariantPayload};
use crate::hotreload::migrate::{MigrationPlan, Retype, StateAction};
use crate::hotreload::plan::CandidatePlan;
use crate::resolve::SymbolId;

/// How a value of a state's old type becomes one of its new type.
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
    /// The new field's default, computed by the behavior chunk.
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

/// The conversion of one state's value: its root, and the record and enum
/// conversions it refers to, which may refer to each other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Retyping {
    pub root: Conversion,
    pub named: Vec<Shape>,
}

impl Retyping {
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

/// Refines each kept state of `migration` against the two candidates' types:
/// a state whose type and cell form are unchanged stays
/// [`Keep`](StateAction::Keep); one whose value converts becomes
/// [`Convert`](StateAction::Convert); any other becomes
/// [`Reset`](StateAction::Reset). Each refined state gets a [`Retype`].
pub fn retype(last_good: &CandidatePlan, candidate: &CandidatePlan, migration: &mut MigrationPlan) {
    for state in &mut migration.states {
        if state.action != StateAction::Keep {
            continue;
        }
        let symbol = state.symbol;
        let (Some((old, _)), Some((new, at))) =
            (last_good.declaration(symbol), candidate.declaration(symbol))
        else {
            continue;
        };
        let held = last_good.initial(symbol).is_some();
        let same_form = held == candidate.initial(symbol).is_some();
        if matches!(old, Ty::Unknown) || matches!(new, Ty::Unknown) {
            continue;
        }
        let exact = Exact {
            old: &last_good.schemas,
            new: &candidate.schemas,
            visiting: Vec::new(),
        }
        .equal(old, new);
        if exact && same_form {
            continue;
        }
        let conversion = if exact {
            Some(Retyping {
                root: Conversion::Keep,
                named: Vec::new(),
            })
        } else {
            Matrix::new(last_good, candidate).retyping(old, new)
        };
        state.action = if conversion.is_some() {
            StateAction::Convert
        } else {
            StateAction::Reset
        };
        let name = candidate
            .sources
            .iter()
            .position(|s| *s == symbol)
            .map_or_else(String::new, |i| candidate.source_names[i].clone());
        migration.retypes.push(Retype {
            symbol,
            name,
            conversion,
            from: last_good.schemas.describe(old),
            to: candidate.schemas.describe(new),
            at,
            from_slot: slot_of(last_good, symbol),
            held,
        });
    }
}

/// The behavior state slot of `symbol` in `plan`.
fn slot_of(plan: &CandidatePlan, symbol: SymbolId) -> Option<u32> {
    let view = plan.view.as_ref()?;
    view.slots
        .iter()
        .find(|&&(s, _)| s == symbol)
        .map(|&(_, slot)| slot)
}

/// Whether two types are the same, each nominal type's declaration
/// compared field by field and variant by variant.
struct Exact<'a> {
    old: &'a TypeSchemas,
    new: &'a TypeSchemas,
    /// The declarations being compared, assumed equal while their own fields
    /// are: a recursive type is equal unless something it holds differs.
    visiting: Vec<SymbolId>,
}

impl Exact<'_> {
    fn equal(&mut self, old: &Ty, new: &Ty) -> bool {
        match (old, new) {
            (Ty::Named(a), Ty::Named(b)) => a == b && self.declaration(*a),
            (Ty::Tuple(xs), Ty::Tuple(ys)) => self.all(xs, ys),
            (Ty::Fn(xp, xr), Ty::Fn(yp, yr)) => self.all(xp, yp) && self.equal(xr, yr),
            (Ty::List(x), Ty::List(y))
            | (Ty::Option(x), Ty::Option(y))
            | (Ty::Range(x), Ty::Range(y))
            | (Ty::RangeInclusive(x), Ty::RangeInclusive(y)) => self.equal(x, y),
            (Ty::Result(xt, xe), Ty::Result(yt, ye)) => self.equal(xt, yt) && self.equal(xe, ye),
            _ => old == new,
        }
    }

    fn all(&mut self, xs: &[Ty], ys: &[Ty]) -> bool {
        xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| self.equal(x, y))
    }

    fn fields(&mut self, xs: &[FieldInfo], ys: &[FieldInfo]) -> bool {
        xs.len() == ys.len()
            && xs
                .iter()
                .zip(ys)
                .all(|(x, y)| x.name == y.name && self.equal(&x.ty, &y.ty))
    }

    fn declaration(&mut self, symbol: SymbolId) -> bool {
        if self.visiting.contains(&symbol) {
            return true;
        }
        self.visiting.push(symbol);
        let (old, new) = (self.old, self.new);
        let equal = match (old.records.get(&symbol), new.records.get(&symbol)) {
            (Some(x), Some(y)) => self.fields(x, y),
            (None, None) => match (old.enums.get(&symbol), new.enums.get(&symbol)) {
                (Some(x), Some(y)) => {
                    x.len() == y.len()
                        && x.iter()
                            .zip(y)
                            .all(|(a, b)| a.name == b.name && self.payload(&a.payload, &b.payload))
                }
                (None, None) => true,
                _ => false,
            },
            _ => false,
        };
        self.visiting.pop();
        equal
    }

    fn payload(&mut self, old: &VariantPayload, new: &VariantPayload) -> bool {
        match (old, new) {
            (VariantPayload::Unit, VariantPayload::Unit) => true,
            (VariantPayload::Tuple(xs), VariantPayload::Tuple(ys)) => self.all(xs, ys),
            (VariantPayload::Record(xs), VariantPayload::Record(ys)) => self.fields(xs, ys),
            _ => false,
        }
    }
}

/// The compatibility matrix over two candidates, building one state's
/// [`Retyping`].
struct Matrix<'a> {
    last_good: &'a CandidatePlan,
    candidate: &'a CandidatePlan,
    named: Vec<Shape>,
    /// Each declaration's table entry, `None` once it is known not to convert.
    memo: HashMap<SymbolId, Option<u32>>,
}

impl<'a> Matrix<'a> {
    fn new(last_good: &'a CandidatePlan, candidate: &'a CandidatePlan) -> Self {
        Matrix {
            last_good,
            candidate,
            named: Vec::new(),
            memo: HashMap::new(),
        }
    }

    fn retyping(mut self, old: &Ty, new: &Ty) -> Option<Retyping> {
        let root = self.convert(old, new)?;
        Some(Retyping {
            root,
            named: self.named,
        })
    }

    fn convert(&mut self, old: &Ty, new: &Ty) -> Option<Conversion> {
        if let Some(conversion) = widen(old, new) {
            return Some(conversion);
        }
        match (old, new) {
            (Ty::Named(a), Ty::Named(b)) if a == b => self.declaration(*a).map(Conversion::Named),
            (Ty::Option(x), Ty::Option(y)) => {
                Some(Conversion::Option(Box::new(self.convert(x, y)?)))
            }
            (Ty::List(x), Ty::List(y)) => Some(Conversion::List(Box::new(self.convert(x, y)?))),
            (Ty::Tuple(xs), Ty::Tuple(ys)) if xs.len() == ys.len() => {
                let fields = xs
                    .iter()
                    .zip(ys)
                    .map(|(x, y)| self.convert(x, y))
                    .collect::<Option<Box<[_]>>>()?;
                Some(Conversion::Fields(fields))
            }
            (Ty::Range(x), Ty::Range(y)) | (Ty::RangeInclusive(x), Ty::RangeInclusive(y)) => {
                let bound = self.convert(x, y)?;
                Some(Conversion::Fields(Box::new([bound.clone(), bound])))
            }
            (Ty::Result(xt, xe), Ty::Result(yt, ye)) => Some(Conversion::Result(
                Box::new(self.convert(xt, yt)?),
                Box::new(self.convert(xe, ye)?),
            )),
            (old, Ty::Option(y)) if !matches!(old, Ty::Option(_) | Ty::Unit) => {
                self.convert(old, y)
            }
            (Ty::Fn(..), _) | (_, Ty::Fn(..)) => None,
            _ => None,
        }
    }

    /// The table entry converting values of the declaration `symbol`.
    fn declaration(&mut self, symbol: SymbolId) -> Option<u32> {
        if let Some(&entry) = self.memo.get(&symbol) {
            return entry;
        }
        // The entry is reserved before its fields convert, so a recursive
        // reference converts by it; if the declaration then does not convert,
        // every entry made since is discarded with it.
        let index = self.named.len() as u32;
        self.named.push(Shape::Record(Box::new([])));
        self.memo.insert(symbol, Some(index));
        let (last_good, candidate) = (self.last_good, self.candidate);
        let (old, new) = (&last_good.schemas, &candidate.schemas);
        let shape = match (old.records.get(&symbol), new.records.get(&symbol)) {
            (Some(x), Some(y)) => self.record(symbol, x, y).map(Shape::Record),
            (None, None) => match (old.enums.get(&symbol), new.enums.get(&symbol)) {
                (Some(x), Some(y)) => Some(Shape::Enum(self.variants(x, y))),
                _ => None,
            },
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
                self.memo.insert(symbol, None);
                None
            }
        }
    }

    fn record(
        &mut self,
        symbol: SymbolId,
        old: &[FieldInfo],
        new: &[FieldInfo],
    ) -> Option<Box<[FieldSource]>> {
        new.iter()
            .enumerate()
            .map(
                |(index, field)| match old.iter().position(|f| f.name == field.name) {
                    Some(from) => Some(FieldSource::Old(
                        from as u32,
                        self.convert(&old[from].ty, &field.ty)?,
                    )),
                    None if field.has_default => {
                        let func = self.candidate.field_default(symbol, index as u32)?;
                        Some(FieldSource::Default(func.0))
                    }
                    None => None,
                },
            )
            .collect()
    }

    fn variants(&mut self, old: &[VariantInfo], new: &[VariantInfo]) -> Box<[Option<VariantMap>]> {
        old.iter()
            .map(|variant| {
                let tag = new.iter().position(|v| v.name == variant.name)?;
                let fields: Box<[FieldSource]> = match (&variant.payload, &new[tag].payload) {
                    (VariantPayload::Unit, VariantPayload::Unit) => Box::new([]),
                    (VariantPayload::Tuple(xs), VariantPayload::Tuple(ys))
                        if xs.len() == ys.len() =>
                    {
                        xs.iter()
                            .zip(ys)
                            .enumerate()
                            .map(|(i, (x, y))| {
                                Some(FieldSource::Old(i as u32, self.convert(x, y)?))
                            })
                            .collect::<Option<_>>()?
                    }
                    (VariantPayload::Record(xs), VariantPayload::Record(ys)) => ys
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
                    unit: matches!(new[tag].payload, VariantPayload::Unit),
                    fields,
                })
            })
            .collect()
    }
}

/// The conversion of a scalar `old` into the scalar `new`, if any: `Keep` when
/// `new` holds each of `old`'s values, a checked conversion when it holds some.
fn widen(old: &Ty, new: &Ty) -> Option<Conversion> {
    use Ty::*;
    let int = |ty: &Ty| {
        let (bits, signed) = match ty {
            I8 => (8, true),
            I16 => (16, true),
            I32 => (32, true),
            I64 => (64, true),
            U8 => (8, false),
            U16 => (16, false),
            U32 => (32, false),
            U64 => (64, false),
            _ => return None,
        };
        Some(IntType { bits, signed })
    };
    if old == new && !matches!(old, Named(_) | Option(_) | List(_) | Tuple(_) | Result(..)) {
        return match old {
            Fn(..) | Range(_) | RangeInclusive(_) => None,
            _ => Some(Conversion::Keep),
        };
    }
    match (int(old), int(new), new) {
        (Some(a), Some(b), _) => {
            let widens = match (a.signed, b.signed) {
                (true, true) | (false, false) => b.bits >= a.bits,
                (false, true) => b.bits > a.bits,
                (true, false) => false,
            };
            Some(if widens {
                Conversion::Keep
            } else {
                Conversion::Int(b)
            })
        }
        (Some(_), None, F32) => Some(Conversion::ToFloat(true)),
        (Some(_), None, F64) => Some(Conversion::ToFloat(false)),
        _ => matches!((old, new), (F32, F64)).then_some(Conversion::Keep),
    }
}
