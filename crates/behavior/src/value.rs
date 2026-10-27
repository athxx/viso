//! Runtime values.

use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::borrow::Borrow;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::ops::Deref;

use crate::native::NativeHandle;

/// A runtime value: 16 bytes, cheap to clone (heap values are shared and
/// copied on write).
///
/// Each source type has one representation:
///
/// | Source type                                   | Value                  |
/// |-----------------------------------------------|------------------------|
/// | integers (64-bit pattern, signed ones sign-extended) | `Int`           |
/// | `Bool` (`0`/`1`), `Char` (scalar value), `Color` (RGBA8) | `Int`       |
/// | `()` (`0`), a unit enum variant (its tag)     | `Int`                  |
/// | floats, single-unit lengths, `Duration`, `Angle`, `Frequency` | `Float` |
/// | `MixedLength`: its `dp`, `px`, `sp`, `em` and `%` terms | `Agg` of five `Float`s |
/// | `String`                                      | `Str`                  |
/// | `List<T>`                                     | `List`                 |
/// | record, tuple, range, payload enum variant    | `Agg`                  |
/// | closure                                       | `Closure`              |
/// | native handle                                 | `Handle`               |
/// | `None` / `Some(x)`                            | `Nil` / `x`            |
#[derive(Clone, Default)]
pub enum Value {
    /// `None`.
    #[default]
    Nil,
    /// An integer-represented value.
    Int(i64),
    /// A float-represented value; an `F32` holds a value exactly representable
    /// as `f32`.
    Float(f64),
    /// A `String`.
    Str(Rc<Text>),
    /// A `List`.
    List(Rc<Vec<Value>>),
    /// An aggregate.
    Agg(Rc<Aggregate>),
    /// A closure.
    Closure(Rc<Closure>),
    /// A native handle.
    Handle(Rc<NativeHandle>),
}

const _: () = assert!(core::mem::size_of::<Value>() == 16);

/// A record, tuple, range or payload enum variant.
#[derive(Clone, Debug, PartialEq)]
pub struct Aggregate {
    /// The enum variant index; `0` for a record, tuple or range.
    pub tag: u32,
    /// The fields, in declaration order.
    pub fields: Box<[Value]>,
}

/// A closure: a chunk and the values it captured when created.
#[derive(Clone, Debug, PartialEq)]
pub struct Closure {
    /// The chunk it runs.
    pub func: u32,
    /// The captured values, in capture order.
    pub captures: Box<[Value]>,
}

impl Value {
    /// A record of `fields`, in declaration order.
    pub fn record(fields: impl Into<Box<[Value]>>) -> Value {
        Value::Agg(Rc::new(Aggregate {
            tag: 0,
            fields: fields.into(),
        }))
    }

    /// The payload variant `tag` of `fields`, in declaration order.
    pub fn variant(tag: u32, fields: impl Into<Box<[Value]>>) -> Value {
        Value::Agg(Rc::new(Aggregate {
            tag,
            fields: fields.into(),
        }))
    }

    /// The fields of a record, tuple or payload variant.
    pub fn fields(&self) -> Option<&[Value]> {
        match self {
            Value::Agg(agg) => Some(&agg.fields),
            _ => None,
        }
    }

    /// A `Bool`.
    #[inline]
    pub fn bool(b: bool) -> Value {
        Value::Int(i64::from(b))
    }

    /// A `String`.
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(Rc::new(Text::Owned(s.into())))
    }

    /// The integer, if this is `Int`.
    #[inline]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(v) => Some(*v),
            _ => None,
        }
    }

    /// The float, if this is `Float`.
    #[inline]
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            _ => None,
        }
    }

    /// The text, if this is `Str`.
    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Whether this is `Nil`.
    #[inline]
    pub fn is_nil(&self) -> bool {
        matches!(self, Value::Nil)
    }

    /// The heap bytes a fresh copy of this value's top level occupies; what an
    /// allocation of it charges against the memory budget.
    pub(crate) fn heap_bytes(&self) -> u64 {
        const HEADER: u64 = 16;
        let slot = core::mem::size_of::<Value>() as u64;
        match self {
            Value::Nil | Value::Int(_) | Value::Float(_) => 0,
            Value::Str(s) => HEADER + s.len() as u64,
            Value::List(l) => HEADER + l.len() as u64 * slot,
            Value::Agg(a) => HEADER + 8 + a.fields.len() as u64 * slot,
            Value::Closure(c) => HEADER + 8 + c.captures.len() as u64 * slot,
            Value::Handle(_) => HEADER + slot,
        }
    }
}

/// Structural equality; floats compare by IEEE rules (`NaN != NaN`), closures
/// and handles by identity.
impl PartialEq for Value {
    fn eq(&self, other: &Value) -> bool {
        match (self, other) {
            (Value::Nil, Value::Nil) => true,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::List(a), Value::List(b)) => a == b,
            (Value::Agg(a), Value::Agg(b)) => a == b,
            (Value::Closure(a), Value::Closure(b)) => Rc::ptr_eq(a, b),
            (Value::Handle(a), Value::Handle(b)) => Rc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Nil => f.write_str("nil"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v:?}"),
            Value::Str(s) => write!(f, "{:?}", s.as_str()),
            Value::List(l) => f.debug_list().entries(l.iter()).finish(),
            Value::Agg(a) => {
                write!(f, "#{}", a.tag)?;
                let mut t = f.debug_tuple("");
                for field in a.fields.iter() {
                    t.field(field);
                }
                t.finish()
            }
            Value::Closure(c) => write!(f, "closure fn#{}", c.func),
            Value::Handle(h) => write!(f, "{h:?}"),
        }
    }
}

/// Text a module carries — a name, a string constant — or a `String` value
/// holds: owned, or borrowed from the `'static` blob a release build embeds,
/// so loading an embedded module copies none of its text
/// ([`Module::decode_static`](crate::Module::decode_static)).
///
/// It reads, compares, orders and hashes as the `str` it holds, whichever way
/// it holds it.
#[derive(Clone)]
pub enum Text {
    /// Borrowed from a `'static` blob.
    Static(&'static str),
    /// Owned.
    Owned(String),
}

impl Text {
    /// The text.
    #[inline]
    pub fn as_str(&self) -> &str {
        match self {
            Text::Static(s) => s,
            Text::Owned(s) => s,
        }
    }
}

impl Default for Text {
    fn default() -> Text {
        Text::Static("")
    }
}

impl Deref for Text {
    type Target = str;

    #[inline]
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl AsRef<str> for Text {
    #[inline]
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl Borrow<str> for Text {
    #[inline]
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl From<&str> for Text {
    fn from(s: &str) -> Text {
        Text::Owned(s.to_owned())
    }
}

impl From<String> for Text {
    fn from(s: String) -> Text {
        Text::Owned(s)
    }
}

impl From<Box<str>> for Text {
    fn from(s: Box<str>) -> Text {
        Text::Owned(s.into())
    }
}

impl From<&String> for Text {
    fn from(s: &String) -> Text {
        Text::Owned(s.clone())
    }
}

impl PartialEq for Text {
    #[inline]
    fn eq(&self, other: &Text) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for Text {}

impl PartialEq<str> for Text {
    #[inline]
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl PartialEq<&str> for Text {
    #[inline]
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == *other
    }
}

impl PartialOrd for Text {
    #[inline]
    fn partial_cmp(&self, other: &Text) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Text {
    #[inline]
    fn cmp(&self, other: &Text) -> core::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl Hash for Text {
    #[inline]
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl fmt::Debug for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for Text {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
