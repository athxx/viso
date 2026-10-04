//! The `@probe` states of a system (Viso_DSL_1.0.md section 110.5): what a
//! headless game test traces every tick, and how each value is written to
//! the JSON trace.
//!
//! A probe's [`ProbeShape`] comes from the state's type, so the trace shows
//! a `Bool` as `true`, a unit enum variant by name and a record by field
//! names, where the runtime value alone holds only integers and fields. A
//! type the shape does not describe (a native value such as `Vec3F32`, a
//! recursive record) is written by structure: an aggregate as the array of
//! its fields.

use viso_behavior::Value;
use viso_ende::JsonWriter;

/// A Simulation state a game test traces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    /// The state's name.
    pub name: String,
    /// Its slot in the system's layout.
    pub slot: u32,
    /// How its values are written.
    pub shape: ProbeShape,
}

/// How a probed value is written to JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeShape {
    /// `true` or `false`.
    Bool,
    /// A signed integer.
    Signed,
    /// An unsigned integer, whose 64-bit pattern is unsigned.
    Unsigned,
    /// A float or dimensional scalar.
    Float,
    /// A `Char`, as a one-character string.
    Char,
    /// A `String`.
    Str,
    /// `null`.
    Unit,
    /// An enum: a unit variant as its name, a payload variant as an object
    /// of its name and its payload.
    Enum(Vec<ProbeVariant>),
    /// A record, as an object of its fields.
    Record(Vec<(String, ProbeShape)>),
    /// A tuple, as an array.
    Tuple(Vec<ProbeShape>),
    /// A list, as an array.
    List(Box<ProbeShape>),
    /// `null` for `None`, the value for `Some`.
    Option(Box<ProbeShape>),
    /// By structure.
    Value,
}

/// A variant of a probed enum.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeVariant {
    pub name: String,
    pub payload: ProbePayload,
}

/// What a probed enum variant carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbePayload {
    Unit,
    /// Written as an array.
    Tuple(Vec<ProbeShape>),
    /// Written as an object.
    Record(Vec<(String, ProbeShape)>),
}

impl ProbeShape {
    /// Writes `value`, a value of this shape, to `json`; a value that does
    /// not fit the shape is written by structure.
    pub fn write(&self, value: &Value, json: &mut JsonWriter) {
        match (self, value) {
            (ProbeShape::Bool, Value::Int(v)) => json.bool(*v != 0),
            (ProbeShape::Signed, Value::Int(v)) => json.int(*v),
            (ProbeShape::Unsigned, Value::Int(v)) => json.uint(*v as u64),
            (ProbeShape::Float, Value::Float(v)) => json.number(*v),
            (ProbeShape::Char, Value::Int(v)) => {
                let c = u32::try_from(*v).ok().and_then(char::from_u32);
                json.string(
                    c.unwrap_or(char::REPLACEMENT_CHARACTER)
                        .encode_utf8(&mut [0; 4]),
                );
            }
            (ProbeShape::Str, Value::Str(s)) => json.string(s),
            (ProbeShape::Unit, _) | (ProbeShape::Option(_), Value::Nil) => json.null(),
            (ProbeShape::Option(inner), value) => inner.write(value, json),
            (ProbeShape::Enum(variants), Value::Int(tag)) => {
                match usize::try_from(*tag).ok().and_then(|t| variants.get(t)) {
                    Some(variant) => json.string(&variant.name),
                    None => json.int(*tag),
                }
            }
            (ProbeShape::Enum(variants), Value::Agg(agg)) => {
                let Some(variant) = variants.get(agg.tag as usize) else {
                    return structure(value, json);
                };
                json.begin_object();
                json.name(&variant.name);
                match &variant.payload {
                    ProbePayload::Unit => json.null(),
                    ProbePayload::Tuple(items) => array(items.iter(), &agg.fields, json),
                    ProbePayload::Record(fields) => object(fields, &agg.fields, json),
                }
                json.end_object();
            }
            (ProbeShape::Record(fields), Value::Agg(agg)) if fields.len() == agg.fields.len() => {
                object(fields, &agg.fields, json);
            }
            (ProbeShape::Tuple(items), Value::Agg(agg)) if items.len() == agg.fields.len() => {
                array(items.iter(), &agg.fields, json);
            }
            (ProbeShape::List(item), Value::List(items)) => {
                array(std::iter::repeat(&**item), items, json);
            }
            _ => structure(value, json),
        }
    }
}

fn array<'s>(
    shapes: impl Iterator<Item = &'s ProbeShape>,
    values: &[Value],
    json: &mut JsonWriter,
) {
    json.begin_array();
    for (shape, value) in shapes.zip(values) {
        shape.write(value, json);
    }
    json.end_array();
}

fn object(fields: &[(String, ProbeShape)], values: &[Value], json: &mut JsonWriter) {
    json.begin_object();
    for ((name, shape), value) in fields.iter().zip(values) {
        json.name(name);
        shape.write(value, json);
    }
    json.end_object();
}

/// `value` by structure alone.
fn structure(value: &Value, json: &mut JsonWriter) {
    match value {
        Value::Int(v) => json.int(*v),
        Value::Float(v) => json.number(*v),
        Value::Str(s) => json.string(s),
        Value::List(items) => {
            json.begin_array();
            items.iter().for_each(|v| structure(v, json));
            json.end_array();
        }
        Value::Agg(agg) => {
            json.begin_array();
            agg.fields.iter().for_each(|v| structure(v, json));
            json.end_array();
        }
        Value::Nil | Value::Closure(_) | Value::Handle(_) => json.null(),
    }
}
