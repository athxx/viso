//! Conversions between Rust values and [`Value`]s at the native boundary, and
//! native handles.

use std::any::Any;
use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::rc::Rc;

use super::SchemaTy;
use crate::value::Value;

/// A Rust type a native parameter or return value may have: its schema type
/// and its conversions to and from the value representation.
pub trait NativeValue: Sized {
    /// Its schema type.
    const TY: SchemaTy;

    /// The Rust value of `value`, or `None` if it does not have this type.
    fn from_value(value: &Value) -> Option<Self>;

    /// The value of `self`.
    fn into_value(self) -> Value;
}

impl NativeValue for () {
    const TY: SchemaTy = SchemaTy::Unit;

    fn from_value(value: &Value) -> Option<()> {
        value.as_int().map(drop)
    }

    fn into_value(self) -> Value {
        Value::Int(0)
    }
}

impl NativeValue for bool {
    const TY: SchemaTy = SchemaTy::Bool;

    fn from_value(value: &Value) -> Option<bool> {
        value.as_int().map(|v| v != 0)
    }

    fn into_value(self) -> Value {
        Value::bool(self)
    }
}

impl NativeValue for i64 {
    const TY: SchemaTy = SchemaTy::I64;

    fn from_value(value: &Value) -> Option<i64> {
        value.as_int()
    }

    fn into_value(self) -> Value {
        Value::Int(self)
    }
}

impl NativeValue for f64 {
    const TY: SchemaTy = SchemaTy::F64;

    fn from_value(value: &Value) -> Option<f64> {
        value.as_float()
    }

    fn into_value(self) -> Value {
        Value::Float(self)
    }
}

impl NativeValue for String {
    const TY: SchemaTy = SchemaTy::String;

    fn from_value(value: &Value) -> Option<String> {
        value.as_str().map(str::to_owned)
    }

    fn into_value(self) -> Value {
        Value::Str(Rc::new(self))
    }
}

impl<T: NativeValue> NativeValue for Vec<T> {
    const TY: SchemaTy = SchemaTy::List(&T::TY);

    fn from_value(value: &Value) -> Option<Vec<T>> {
        match value {
            Value::List(items) => items.iter().map(T::from_value).collect(),
            _ => None,
        }
    }

    fn into_value(self) -> Value {
        Value::List(Rc::new(self.into_iter().map(T::into_value).collect()))
    }
}

/// `None` is `Nil` and `Some(x)` is `x`, so an `Option` of an `Option` is not a
/// schema type.
impl<T: NativeValue> NativeValue for Option<T> {
    const TY: SchemaTy = SchemaTy::Option(&T::TY);

    fn from_value(value: &Value) -> Option<Option<T>> {
        if value.is_nil() {
            Some(None)
        } else {
            T::from_value(value).map(Some)
        }
    }

    fn into_value(self) -> Value {
        self.map_or(Value::Nil, T::into_value)
    }
}

/// A Rust type that native handles hold.
pub trait NativeObject: Any {
    /// The full path of its [`NativeType`](super::NativeType), such as
    /// `viso::time::Stopwatch`.
    const PATH: &'static str;
}

/// A native handle: a Rust object behind a DSL value, shared by reference and
/// compared by identity. It is created, used and dropped on the thread of the
/// interpreter holding it.
pub struct NativeHandle {
    path: &'static str,
    object: Box<dyn Any>,
}

impl NativeHandle {
    /// The full path of its type.
    pub fn path(&self) -> &'static str {
        self.path
    }

    /// The object, if it is a `T`.
    pub fn get<T: NativeObject>(&self) -> Option<&T> {
        self.object.downcast_ref()
    }
}

impl fmt::Debug for NativeHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "handle {}", self.path)
    }
}

/// A native parameter or return value holding a handle to a `T`.
pub struct Obj<T: NativeObject> {
    handle: Rc<NativeHandle>,
    object: PhantomData<T>,
}

impl<T: NativeObject> Obj<T> {
    /// A new handle to `object`.
    pub fn new(object: T) -> Obj<T> {
        Obj {
            handle: Rc::new(NativeHandle {
                path: T::PATH,
                object: Box::new(object),
            }),
            object: PhantomData,
        }
    }
}

impl<T: NativeObject> Deref for Obj<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // `from_value` and `new` only build an `Obj<T>` over a `T`.
        self.handle.get().expect("a handle of its own type")
    }
}

impl<T: NativeObject> NativeValue for Obj<T> {
    const TY: SchemaTy = SchemaTy::Handle(T::PATH);

    fn from_value(value: &Value) -> Option<Obj<T>> {
        match value {
            Value::Handle(handle) if handle.get::<T>().is_some() => Some(Obj {
                handle: Rc::clone(handle),
                object: PhantomData,
            }),
            _ => None,
        }
    }

    fn into_value(self) -> Value {
        Value::Handle(self.handle)
    }
}
