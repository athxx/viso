//! Widgets built in Rust that `.vs` views name.
//!
//! A native library declares such a widget's schema —
//! [`NativeWidget::rust`](viso_behavior::native::NativeWidget::rust), its
//! own properties and events — and its package hands the view its
//! implementation, a [`WidgetImpl`]. The node a view writes for it is a box
//! taking every node's layout, look and standard events; the implementation
//! builds its content inside that box the first time the node shows, takes
//! each own property's value as it changes, and reports its own events
//! through [`WidgetEvents`], which run the view's handlers of them.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use viso_behavior::Value;
use viso_behavior::native::NativeLibrary;
use viso_ui::{BuildCx, NodeId, StructureCx};

use crate::host::{StateCells, ViewHost};
use crate::scope::Scope;

/// A widget built in Rust, one instance per node a view writes for it.
pub trait RustWidget: 'static {
    /// Builds its content inside `node`, the box its view laid out: `cx`
    /// adds children beneath it. `events` reports its own events from the
    /// handlers it attaches.
    fn build(&mut self, cx: &mut BuildCx<'_>, node: NodeId, events: &WidgetEvents);

    /// Shows `value` for its own property at `property`, its place among
    /// the widget's own properties as its schema declares them. It is given
    /// each value its view sets, once built, and again only as it changes.
    fn set(&mut self, cx: &mut StructureCx<'_>, property: usize, value: &Value);
}

/// The implementation of a Rust widget: the name its schema declares and how
/// to make one. A package hands the view its implementations as a
/// `&'static [WidgetImpl]` beside its libraries.
#[derive(Clone, Copy)]
pub struct WidgetImpl {
    /// The widget's name, as its library declares it.
    pub name: &'static str,
    /// Makes one, for a node about to show.
    pub new: fn() -> Box<dyn RustWidget>,
}

impl std::fmt::Debug for WidgetImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WidgetImpl")
            .field("name", &self.name)
            .finish()
    }
}

/// Where a Rust widget reports its own events: the handlers its view runs
/// for each, in the scope its node mounted in.
#[derive(Clone)]
pub struct WidgetEvents(Rc<Reports>);

struct Reports {
    host: Weak<RefCell<ViewHost>>,
    routes: RefCell<Routes>,
}

/// Each handled event's place among the widget's own and its handler, and
/// the scope they run in.
struct Routes {
    handlers: Box<[(u16, u32)]>,
    scope: Scope,
}

impl WidgetEvents {
    pub(crate) fn new(host: Weak<RefCell<ViewHost>>) -> WidgetEvents {
        WidgetEvents(Rc::new(Reports {
            host,
            routes: RefCell::new(Routes {
                handlers: Box::default(),
                scope: Scope::EMPTY,
            }),
        }))
    }

    /// Routes them to `handlers` in `scope`.
    pub(crate) fn route(&self, handlers: &[(u16, u32)], scope: &Scope) {
        let mut routes = self.0.routes.borrow_mut();
        if *routes.handlers != *handlers {
            routes.handlers = handlers.into();
        }
        routes.scope = scope.clone();
    }

    /// Runs the view's handlers of own event `event` — its place among the
    /// widget's own events as its schema declares them — with `payload`, a
    /// value of the type the schema gives it. A two-way property's change
    /// event writes `payload` to the state bound to it. Whether a handler
    /// ran; none runs while the view is busy, as it is while it delivers a
    /// value to [`RustWidget::set`].
    pub fn fire(&self, cells: &mut dyn StateCells, event: usize, payload: Value) -> bool {
        let Some(host) = self.0.host.upgrade() else {
            return false;
        };
        let Ok(mut host) = host.try_borrow_mut() else {
            return false;
        };
        let routes = self.0.routes.borrow();
        let mut ran = false;
        for &(place, handler) in routes.handlers.iter() {
            if usize::from(place) == event {
                ran |= host.dispatch(handler, payload.clone(), &routes.scope, cells);
            }
        }
        ran
    }
}

/// A node's widget, built.
pub(crate) struct Built {
    /// Its implementation's name.
    pub(crate) name: &'static str,
    pub(crate) widget: Box<dyn RustWidget>,
    pub(crate) events: WidgetEvents,
    /// The value of each own property it was given last, by place.
    pub(crate) shows: Vec<(u16, Value)>,
}

/// Checks that `widgets` implement exactly the Rust widgets `libraries`
/// declare, each once: a package's own test, beside its schema check.
///
/// # Panics
///
/// Naming the first widget declared and not implemented, implemented and
/// not declared, or implemented twice.
pub fn check_widgets(libraries: &[&'static NativeLibrary], widgets: &[WidgetImpl]) {
    let declared: Vec<&str> = libraries
        .iter()
        .flat_map(|library| library.widgets)
        .filter(|widget| widget.is_rust())
        .map(|widget| widget.name)
        .collect();
    for name in &declared {
        if !widgets.iter().any(|w| w.name == *name) {
            panic!("the Rust widget `{name}` is declared but not implemented");
        }
    }
    for (at, widget) in widgets.iter().enumerate() {
        if !declared.contains(&widget.name) {
            panic!(
                "`{}` is implemented but no library declares it a Rust widget",
                widget.name
            );
        }
        if widgets[..at].iter().any(|w| w.name == widget.name) {
            panic!("`{}` is implemented twice", widget.name);
        }
    }
}
