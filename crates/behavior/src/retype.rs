//! Converting a value from one version of its type to another: the
//! [`Retyping`] the schemas of its old and new type give, applied to a
//! [`Value`].

use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::vec::Vec;

use crate::value::{Aggregate, Value};

pub use viso_schema::retype::*;

/// Applying a [`Retyping`] to a value.
pub trait RetypeValue {
    /// `value` converted, each new record field's default computed by
    /// `default` from its chunk. `None` when the value has no counterpart in
    /// the new type: its variant was removed, or a default did not compute.
    fn apply(&self, value: &Value, default: &mut dyn FnMut(u32) -> Option<Value>) -> Option<Value>;
}

impl RetypeValue for Retyping {
    fn apply(&self, value: &Value, default: &mut dyn FnMut(u32) -> Option<Value>) -> Option<Value> {
        convert(self, &self.root, value, default)
    }
}

fn convert(
    retyping: &Retyping,
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
            value => convert(retyping, inner, value, default),
        },
        Conversion::List(item) => match value {
            Value::List(items) => {
                let items = items
                    .iter()
                    .map(|v| convert(retyping, item, v, default))
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
                .map(|(c, v)| convert(retyping, c, v, default))
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
            let inner = convert(retyping, payload, agg.fields.first()?, default)?;
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
            let inner = convert(retyping, payload, agg.fields.first()?, default)?;
            Some(aggregate(agg.tag, Box::new([inner])))
        }
        Conversion::Named(index) => match retyping.named.get(*index as usize)? {
            Shape::Record(fields) => {
                let Value::Agg(agg) = value else {
                    return None;
                };
                let fields = convert_fields(retyping, fields, &agg.fields, default)?;
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
                let fields = convert_fields(retyping, &map.fields, old, default)?;
                Some(aggregate(map.tag, fields))
            }
        },
    }
}

fn convert_fields(
    retyping: &Retyping,
    fields: &[FieldSource],
    old: &[Value],
    default: &mut dyn FnMut(u32) -> Option<Value>,
) -> Option<Box<[Value]>> {
    fields
        .iter()
        .map(|field| match field {
            FieldSource::Old(index, c) => convert(retyping, c, old.get(*index as usize)?, default),
            FieldSource::Default(chunk) => default(*chunk),
        })
        .collect()
}

fn aggregate(tag: u32, fields: Box<[Value]>) -> Value {
    Value::Agg(Rc::new(Aggregate { tag, fields }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::module::StableId;
    use alloc::vec;

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
}
