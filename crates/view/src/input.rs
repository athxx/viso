//! The inputs a view mounted from Rust is given: `view!("shell.vs", nav:
//! router)`. Each value converts through [`ViewInput`]: a scalar into a cell
//! holding it, a string or wide number into a cell that never moves, and a
//! [`RouterView`] into the `Navigator<R>` the view drives, its slot tracked
//! through the navigator's own revision cell.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use viso_behavior::Value;
use viso_behavior::native::{NativeData, NativeValue, Obj};
use viso_behavior::nav::{Navigation, NavigatorRef, RouteStack};
use viso_ui::{
    BackRequest, BuildCx, Component, EventCx, NodeId, RouteEdit, RouterView, SharedRoutes, StateId,
    StateValue, structure::StructureCx,
};

/// The `.vs` types of a root input, as the Rust types a [`ViewInput`] names:
/// a value given for an input of another type does not type-check.
pub mod input_kind {
    /// `Bool`.
    pub enum Bool {}
    /// `I32`.
    pub enum I32 {}
    /// `F32`.
    pub enum F32 {}
    /// `I64`.
    pub enum I64 {}
    /// `F64`.
    pub enum F64 {}
    /// `String`.
    pub enum String {}
    /// `Navigator<R>`, whatever `R`.
    pub enum Navigator {}
}

const fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// A root input, as the view takes it: the cell its readers depend on and the
/// value its instance holds.
pub struct MountedInput {
    pub(crate) cell: StateId,
    pub(crate) value: Value,
    pub(crate) navigator: Option<Rc<dyn HostRoutes>>,
}

impl MountedInput {
    /// The cell its readers depend on.
    #[doc(hidden)]
    pub fn __cell(&self) -> StateId {
        self.cell
    }
}

/// A Rust value a view takes as a root input of the `.vs` type
/// [`Kind`](Self::Kind) names.
pub trait ViewInput {
    /// The input type it is, a marker of [`input_kind`].
    type Kind;

    /// For a `Navigator<R>`, the native path of `R`.
    const ROUTE: &'static str = "";

    /// Allocates what the view reads it through.
    fn mount(self, cx: &mut BuildCx<'_>) -> MountedInput;
}

/// A cell holding `value`, which the view reads directly.
fn held(cx: &mut BuildCx<'_>, value: StateValue, vm: Value) -> MountedInput {
    MountedInput {
        cell: cx.__macro_state(value, None),
        value: vm,
        navigator: None,
    }
}

impl ViewInput for bool {
    type Kind = input_kind::Bool;

    fn mount(self, cx: &mut BuildCx<'_>) -> MountedInput {
        held(cx, StateValue::Bool(self), Value::bool(self))
    }
}

impl ViewInput for i32 {
    type Kind = input_kind::I32;

    fn mount(self, cx: &mut BuildCx<'_>) -> MountedInput {
        held(cx, StateValue::Int(self), Value::Int(self.into()))
    }
}

impl ViewInput for f32 {
    type Kind = input_kind::F32;

    fn mount(self, cx: &mut BuildCx<'_>) -> MountedInput {
        held(cx, StateValue::Float(self), Value::Float(self.into()))
    }
}

impl ViewInput for i64 {
    type Kind = input_kind::I64;

    fn mount(self, cx: &mut BuildCx<'_>) -> MountedInput {
        held(cx, StateValue::Int(0), Value::Int(self))
    }
}

impl ViewInput for f64 {
    type Kind = input_kind::F64;

    fn mount(self, cx: &mut BuildCx<'_>) -> MountedInput {
        held(cx, StateValue::Int(0), Value::Float(self))
    }
}

impl ViewInput for String {
    type Kind = input_kind::String;

    fn mount(self, cx: &mut BuildCx<'_>) -> MountedInput {
        held(cx, StateValue::Int(0), Value::str(&self))
    }
}

impl ViewInput for &str {
    type Kind = input_kind::String;

    fn mount(self, cx: &mut BuildCx<'_>) -> MountedInput {
        held(cx, StateValue::Int(0), Value::str(self))
    }
}

/// The router view becomes the view's `Navigator<R>`; a `RouterView` node of
/// the view places it.
impl<R: NativeData + Clone + PartialEq + 'static> ViewInput for RouterView<R> {
    type Kind = input_kind::Navigator;
    const ROUTE: &'static str = R::PATH;

    fn mount(mut self, cx: &mut BuildCx<'_>) -> MountedInput {
        let shared = self
            .share(cx)
            .expect("a view with behavior builds with states");
        let routes = Rc::new(Routes {
            shared,
            changed: Cell::new(false),
            outlet: RefCell::new(Some(self)),
        });
        MountedInput {
            cell: routes.shared.revision(),
            value: Obj::new(NavigatorRef(routes.clone())).into_value(),
            navigator: Some(routes),
        }
    }
}

/// Mounts `value` as a root input of the `.vs` type `K` names; a value of
/// another type does not type-check.
#[doc(hidden)]
pub fn __mount_input<K, T: ViewInput<Kind = K>>(cx: &mut BuildCx<'_>, value: T) -> MountedInput {
    value.mount(cx)
}

/// The route type of a `Navigator<R>` input of `view!`.
#[doc(hidden)]
pub trait __Route {
    /// The native path of `R`.
    const PATH: &'static str;
}

/// Mounts `value` as a root `Navigator<R>` input, `E` naming `R`; a router
/// view over routes of another type does not build.
#[doc(hidden)]
pub fn __mount_navigator<E: __Route, T: ViewInput<Kind = input_kind::Navigator>>(
    cx: &mut BuildCx<'_>,
    value: T,
) -> MountedInput {
    const {
        assert!(
            same(T::ROUTE.as_bytes(), E::PATH.as_bytes()),
            "a navigator input of the view is given a router view over routes of another type"
        );
    }
    value.mount(cx)
}

/// What a `RouterView` node asks before a system back action pops.
pub(crate) type BackSink = Box<dyn FnMut(&mut EventCx<'_>, &mut BackRequest)>;

/// A navigator input as the view's host sees it.
pub(crate) trait HostRoutes {
    /// Whether a navigation changed the stack since the last call.
    fn take_changed(&self) -> bool;

    /// Whether `value` is a handle to this navigator.
    fn is(&self, value: &Value) -> bool;

    /// Builds the router view under `parent`, asking `on_back` before a back
    /// action pops; whether it was there to build, which it is once.
    fn place(&self, cx: &mut StructureCx<'_>, parent: NodeId, on_back: BackSink) -> bool;
}

/// A router view a view drives: its back stack, and the view until a
/// `RouterView` node places it.
struct Routes<R> {
    shared: SharedRoutes<R>,
    changed: Cell<bool>,
    outlet: RefCell<Option<RouterView<R>>>,
}

impl<R: NativeValue + Clone + PartialEq + 'static> RouteStack for Routes<R> {
    fn current(&self) -> Option<Value> {
        self.shared.current().map(NativeValue::into_value)
    }

    fn depth(&self) -> usize {
        self.shared.depth()
    }

    fn navigate(&self, navigation: Navigation) -> bool {
        let route = |value: &Value| R::from_value(value);
        let edit = match &navigation {
            Navigation::Push(value) => route(value).map(RouteEdit::Push),
            Navigation::Replace(value) => route(value).map(RouteEdit::Replace),
            Navigation::Pop => Some(RouteEdit::Pop),
            Navigation::Reset(value) => route(value).map(RouteEdit::Reset),
        };
        let changed = edit.is_some_and(|edit| self.shared.edit(edit));
        if changed {
            self.changed.set(true);
        }
        changed
    }
}

impl<R: NativeValue + Clone + PartialEq + 'static> HostRoutes for Routes<R> {
    fn take_changed(&self) -> bool {
        self.changed.replace(false)
    }

    fn is(&self, value: &Value) -> bool {
        let Value::Handle(handle) = value else {
            return false;
        };
        handle
            .get::<NavigatorRef>()
            .is_some_and(|nav| std::ptr::addr_eq(Rc::as_ptr(&nav.0), self as *const Self))
    }

    fn place(&self, cx: &mut StructureCx<'_>, parent: NodeId, mut on_back: BackSink) -> bool {
        let Some(view) = self.outlet.borrow_mut().take() else {
            return false;
        };
        let view = view.on_back_requested(move |ev, request| on_back(ev, request));
        let mut build =
            BuildCx::with_parent(cx.store, cx.states, cx.bindings, parent).with_lists(cx.lists);
        Component::build(&view, &mut build);
        true
    }
}
