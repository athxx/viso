//! The native bridge: Rust functions and handle types the DSL calls through a
//! typed schema.
//!
//! A [`NativeLibrary`] is static Rust data: its functions ([`NativeFunction`])
//! and handle types ([`NativeType`]), each carrying the schema the compiler
//! checks calls against — the effect kind ([`NativeKind`]), the thread domain,
//! the required capabilities, the parameter and return types ([`SchemaTy`]),
//! determinism, realtime safety and budget cost — and the thunk the
//! interpreter calls. The [`native!`](crate::native!) macro derives a
//! function's parameter and return types from its Rust signature, so the schema
//! cannot drift from the code.
//!
//! A library also declares the widgets a view instantiates ([`NativeWidget`]):
//! their properties, events and slots, and the retained node each lowers to.
//!
//! A [`Natives`] registry collects libraries; the compiler resolves native
//! paths against it and a module records each native it calls as a
//! [`NativeImport`](crate::NativeImport) (path and signature hash). Linking a
//! [`Vm`](crate::Vm) to a registry resolves every import once, so a call is an
//! index into the linked table, never a string lookup.
//!
//! A thunk receives a [`NativeCx`] (the host services) and the argument values,
//! and returns a value or a [`NativeError`]; an error or a Rust panic becomes a
//! [`FaultKind::NativeFailure`](crate::FaultKind::NativeFailure) fault.

mod registry;
mod standard;
mod value;
mod widget;
mod widgets;

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::fmt;

use crate::value::Value;

pub use registry::{NativeEntry, NativeTypeEntry, NativeWidgetEntry, Natives, SchemaConflict};
pub use standard::{Clipboard, STANDARD, Stopwatch};
pub use value::{NativeHandle, NativeObject, NativeValue, Obj};
pub use widget::{
    FlexAxis, MigratableState, NativeWidget, PropertyGroup, SlotCardinality, WidgetEvent,
    WidgetNode, WidgetProperty, WidgetSlot,
};
pub use widgets::COMPONENT;

/// A native function's effect kind, which fixes the contexts that may call it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NativeKind {
    /// A read-only function, callable from pure contexts when it is
    /// deterministic.
    Fn,
    /// A synchronous side effect, callable from action contexts.
    Action,
    /// An asynchronous, cancellable side effect, callable from tasks.
    Task,
}

impl NativeKind {
    /// Its keyword.
    pub fn keyword(self) -> &'static str {
        match self {
            NativeKind::Fn => "fn",
            NativeKind::Action => "action",
            NativeKind::Task => "task",
        }
    }
}

/// Where a native function runs and a native handle lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThreadDomain {
    /// Any thread, the caller's.
    Any,
    /// The UI thread.
    Ui,
    /// A worker thread, off the UI thread.
    Worker,
}

impl ThreadDomain {
    /// Its schema name.
    pub fn name(self) -> &'static str {
        match self {
            ThreadDomain::Any => "any",
            ThreadDomain::Ui => "ui",
            ThreadDomain::Worker => "worker",
        }
    }
}

/// Who may hold a native handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Ownership {
    /// Reference-counted and freely stored: states, fields and return values
    /// may hold it.
    Shared,
    /// Valid only for the call that receives it: it may be a parameter or a
    /// local, but not stored in a state, input, event, record or constant, nor
    /// returned from a function or action.
    Borrowed,
}

impl Ownership {
    /// Its schema name.
    pub fn name(self) -> &'static str {
        match self {
            Ownership::Shared => "shared",
            Ownership::Borrowed => "borrowed",
        }
    }
}

/// A parameter or return type in a native schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SchemaTy {
    /// `()`.
    Unit,
    /// `Bool`.
    Bool,
    /// `I64`.
    I64,
    /// `F64`.
    F64,
    /// `String`.
    String,
    /// `List<T>`.
    List(&'static SchemaTy),
    /// `Option<T>`.
    Option(&'static SchemaTy),
    /// The native handle type at this path.
    Handle(&'static str),
}

impl fmt::Display for SchemaTy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SchemaTy::Unit => f.write_str("()"),
            SchemaTy::Bool => f.write_str("Bool"),
            SchemaTy::I64 => f.write_str("I64"),
            SchemaTy::F64 => f.write_str("F64"),
            SchemaTy::String => f.write_str("String"),
            SchemaTy::List(t) => write!(f, "List<{t}>"),
            SchemaTy::Option(t) => write!(f, "Option<{t}>"),
            SchemaTy::Handle(path) => f.write_str(path.rsplit("::").next().unwrap_or(path)),
        }
    }
}

/// A native function parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Param {
    /// Its name.
    pub name: &'static str,
    /// Its type.
    pub ty: SchemaTy,
}

/// The Rust side of a native function: the host services and the argument
/// values, in parameter order and already checked against the schema types.
pub type Thunk = fn(&mut NativeCx<'_>, &[Value]) -> Result<Value, NativeError>;

/// A native function or handle method.
///
/// A method's first parameter is its receiver when its type is the owning
/// handle type; such a method is called as `value.method(..)`, any other as
/// `Type::method(..)`.
#[derive(Clone, Copy)]
pub struct NativeFunction {
    /// Its name within its library or type.
    pub name: &'static str,
    /// Its effect kind.
    pub kind: NativeKind,
    /// Its parameters.
    pub params: &'static [Param],
    /// Its return type.
    pub ret: SchemaTy,
    /// The capabilities a caller must be granted.
    pub capabilities: &'static [&'static str],
    /// The thread it runs on.
    pub thread: ThreadDomain,
    /// Whether equal arguments always give an equal result with no observable
    /// effect.
    pub deterministic: bool,
    /// Whether it never allocates, blocks or takes a non-realtime lock.
    pub realtime_safe: bool,
    /// The instruction-budget units a call spends.
    pub cost: u32,
    /// The Rust implementation.
    pub call: Thunk,
}

impl NativeFunction {
    /// A function on the UI thread without capabilities, costing one
    /// instruction.
    pub const fn new(
        name: &'static str,
        kind: NativeKind,
        params: &'static [Param],
        ret: SchemaTy,
        call: Thunk,
    ) -> NativeFunction {
        NativeFunction {
            name,
            kind,
            params,
            ret,
            capabilities: &[],
            thread: ThreadDomain::Ui,
            deterministic: false,
            realtime_safe: false,
            cost: 1,
            call,
        }
    }

    /// Requires `capabilities` of its callers.
    pub const fn requires(mut self, capabilities: &'static [&'static str]) -> NativeFunction {
        self.capabilities = capabilities;
        self
    }

    /// Runs it on `thread`.
    pub const fn on(mut self, thread: ThreadDomain) -> NativeFunction {
        self.thread = thread;
        self
    }

    /// Marks it deterministic, and so callable from any thread.
    pub const fn deterministic(mut self) -> NativeFunction {
        self.deterministic = true;
        self.thread = ThreadDomain::Any;
        self
    }

    /// Marks it realtime-safe.
    pub const fn realtime_safe(mut self) -> NativeFunction {
        self.realtime_safe = true;
        self
    }

    /// Makes a call spend `cost` instruction-budget units.
    pub const fn cost(mut self, cost: u32) -> NativeFunction {
        self.cost = cost;
        self
    }

    /// A hash of its kind, parameter and return types: a module compiled
    /// against one signature links only to the same signature.
    pub fn signature(&self) -> u64 {
        let mut h = Fnv::new();
        h.write(self.kind.keyword().as_bytes());
        for p in self.params {
            h.ty(&p.ty);
        }
        h.write(b"->");
        h.ty(&self.ret);
        h.finish()
    }
}

impl fmt::Debug for NativeFunction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "native {} {}(", self.kind.keyword(), self.name)?;
        for (i, p) in self.params.iter().enumerate() {
            if i > 0 {
                f.write_str(", ")?;
            }
            write!(f, "{}: {}", p.name, p.ty)?;
        }
        write!(f, ") -> {}", self.ret)
    }
}

/// A native handle type.
#[derive(Debug, Clone, Copy)]
pub struct NativeType {
    /// Its name within its library.
    pub name: &'static str,
    /// Who may hold a handle.
    pub ownership: Ownership,
    /// The thread its handles live and drop on.
    pub thread: ThreadDomain,
    /// Its methods and associated functions.
    pub methods: &'static [NativeFunction],
}

impl NativeType {
    /// A shared UI-thread type.
    pub const fn new(name: &'static str, methods: &'static [NativeFunction]) -> NativeType {
        NativeType {
            name,
            ownership: Ownership::Shared,
            thread: ThreadDomain::Ui,
            methods,
        }
    }

    /// Makes its handles borrowed.
    pub const fn borrowed(mut self) -> NativeType {
        self.ownership = Ownership::Borrowed;
        self
    }

    /// Lives on `thread`.
    pub const fn on(mut self, thread: ThreadDomain) -> NativeType {
        self.thread = thread;
        self
    }
}

/// A versioned set of native functions, handle types and widgets under one
/// module path.
#[derive(Debug, Clone, Copy)]
pub struct NativeLibrary {
    /// Its module path, such as `viso::text`.
    pub path: &'static str,
    /// Its schema version; a registry holds one version of a path.
    pub version: u32,
    /// Its functions.
    pub functions: &'static [NativeFunction],
    /// Its handle types.
    pub types: &'static [NativeType],
    /// Its widgets, which a view names by their bare type name.
    pub widgets: &'static [NativeWidget],
}

/// A stable numeric identity of a native function or type: a hash of its full
/// path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NativeId(pub u64);

impl NativeId {
    /// The identity of the native at `path`.
    pub fn of(path: &str) -> NativeId {
        let mut h = Fnv::new();
        h.write(path.as_bytes());
        NativeId(h.finish())
    }
}

/// 64-bit FNV-1a.
struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
    }

    fn ty(&mut self, ty: &SchemaTy) {
        self.write(ty.to_string().as_bytes());
        if let SchemaTy::Handle(path) = ty {
            self.write(path.as_bytes());
        }
        self.write(b",");
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// Why a native function failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeError {
    /// A description.
    pub message: String,
}

impl NativeError {
    /// An error described by `message`.
    pub fn new(message: impl Into<String>) -> NativeError {
        NativeError {
            message: message.into(),
        }
    }
}

impl fmt::Display for NativeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for NativeError {}

/// Host services by type: the clipboard, a clock, a platform bridge. The host
/// installs them on a [`Vm`](crate::Vm); natives look them up by type.
#[derive(Default)]
pub struct Services {
    map: HashMap<TypeId, Box<dyn Any>>,
}

impl Services {
    /// Installs `service`, replacing any of its type.
    pub fn insert<T: Any>(&mut self, service: T) {
        self.map.insert(TypeId::of::<T>(), Box::new(service));
    }

    /// The service of type `T`.
    pub fn get_mut<T: Any>(&mut self) -> Option<&mut T> {
        self.map.get_mut(&TypeId::of::<T>())?.downcast_mut()
    }

    /// Removes and returns the service of type `T`.
    pub fn remove<T: Any>(&mut self) -> Option<T> {
        self.map
            .remove(&TypeId::of::<T>())
            .and_then(|s| s.downcast().ok())
            .map(|s| *s)
    }
}

impl fmt::Debug for Services {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Services({})", self.map.len())
    }
}

/// What a native function sees of its host.
pub struct NativeCx<'a> {
    services: &'a mut Services,
}

impl<'a> NativeCx<'a> {
    /// A context over `services`.
    pub fn new(services: &'a mut Services) -> NativeCx<'a> {
        NativeCx { services }
    }

    /// The host service of type `T`, or an error naming the missing service.
    pub fn service<T: Any>(&mut self) -> Result<&mut T, NativeError> {
        self.services.get_mut().ok_or_else(|| {
            NativeError::new(format!(
                "the host provides no `{}` service",
                std::any::type_name::<T>()
            ))
        })
    }
}

/// A [`NativeFunction`] from a Rust signature and body.
///
/// ```ignore
/// native!(fn "upper" |_cx, text: String| -> String { Ok(text.to_uppercase()) })
///     .deterministic()
/// ```
///
/// The kind is `fn`, `action` or `task`; the first closure parameter names the
/// [`NativeCx`]; every other parameter's type implements [`NativeValue`], as
/// does the return type; the body returns `Result<Ret, NativeError>`.
#[macro_export]
macro_rules! native {
    ($kind:tt $name:literal |$cx:ident $(, $arg:ident : $ty:ty)* $(,)?| -> $ret:ty $body:block) => {{
        fn thunk(
            cx: &mut $crate::native::NativeCx<'_>,
            args: &[$crate::Value],
        ) -> ::core::result::Result<$crate::Value, $crate::native::NativeError> {
            #[allow(unused_variables, unused_mut)]
            let mut args = args.iter();
            $(
                let $arg = args
                    .next()
                    .and_then(<$ty as $crate::native::NativeValue>::from_value)
                    .ok_or_else(|| $crate::native::NativeError::new(concat!(
                        "argument `", stringify!($arg), "` does not match its schema type"
                    )))?;
            )*
            #[allow(unused_variables)]
            fn body(
                $cx: &mut $crate::native::NativeCx<'_>,
                $($arg: $ty),*
            ) -> ::core::result::Result<$ret, $crate::native::NativeError> $body
            body(cx, $($arg),*).map($crate::native::NativeValue::into_value)
        }
        $crate::native::NativeFunction::new(
            $name,
            $crate::native!(@kind $kind),
            &[$($crate::native::Param {
                name: stringify!($arg),
                ty: <$ty as $crate::native::NativeValue>::TY,
            }),*],
            <$ret as $crate::native::NativeValue>::TY,
            thunk,
        )
    }};
    (@kind fn) => { $crate::native::NativeKind::Fn };
    (@kind action) => { $crate::native::NativeKind::Action };
    (@kind task) => { $crate::native::NativeKind::Task };
}

#[cfg(test)]
mod tests;
