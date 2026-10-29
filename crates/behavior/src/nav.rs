//! `viso::nav`: the `Navigator<R>` a view drives, the back stack of a router
//! the host built in Rust over routes of type `R`.
//!
//! The handle holds a [`RouteStack`], which the host implements over its
//! router. Reads (`current`, `can_pop`, `depth`) answer from the stack;
//! navigations (`push`, `replace`, `pop`, `reset`) edit it at once and leave
//! the host to raise the revision its readers depend on when the call
//! returns. Routes cross as values, typed by the receiver's type argument.

use alloc::rc::Rc;
use core::fmt;

use crate::native::{
    NativeError, NativeFunction, NativeLibrary, NativeObject, NativeType, NativeValue, Obj,
    SchemaTy,
};
use crate::value::Value;

/// The path of the navigator type.
pub const NAVIGATOR_PATH: &str = "viso::nav::Navigator";

/// A navigation a view asks of a [`RouteStack`].
#[derive(Debug, Clone, PartialEq)]
pub enum Navigation {
    /// Shows the route over the current page, which keeps its state below.
    Push(Value),
    /// Shows the route in place of the current page.
    Replace(Value),
    /// Reveals the page below the current one; the root page stays.
    Pop,
    /// Makes the route the only page, as a deep link does.
    Reset(Value),
}

/// The back stack behind a navigator handle.
pub trait RouteStack {
    /// The route on top; `None` once the router is gone.
    fn current(&self) -> Option<Value>;

    /// How many routes the stack holds; 0 once the router is gone.
    fn depth(&self) -> usize;

    /// Applies `navigation` to the stack; whether it changed it. A route
    /// that is not of the router's type changes nothing.
    fn navigate(&self, navigation: Navigation) -> bool;
}

/// The object behind a `viso::nav::Navigator` handle.
#[derive(Clone)]
pub struct NavigatorRef(pub Rc<dyn RouteStack>);

impl fmt::Debug for NavigatorRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NavigatorRef")
    }
}

impl NativeObject for NavigatorRef {
    const PATH: &'static str = NAVIGATOR_PATH;
}

/// A route: a value of the navigator's type argument, which the stack
/// converts.
pub struct Route(pub Value);

impl NativeValue for Route {
    const TY: SchemaTy = SchemaTy::Param(0);

    fn from_value(value: &Value) -> Option<Route> {
        Some(Route(value.clone()))
    }

    fn into_value(self) -> Value {
        self.0
    }
}

static METHODS: [NativeFunction; 7] = [
    crate::native!(fn "current" |_cx, nav: Obj<NavigatorRef>| -> Route {
        nav.0
            .current()
            .map(Route)
            .ok_or_else(|| NativeError::new("the navigator's router is gone"))
    }),
    crate::native!(fn "can_pop" |_cx, nav: Obj<NavigatorRef>| -> bool { Ok(nav.0.depth() > 1) }),
    crate::native!(fn "depth" |_cx, nav: Obj<NavigatorRef>| -> i64 {
        Ok(i64::try_from(nav.0.depth()).unwrap_or(i64::MAX))
    }),
    crate::native!(action "push" |_cx, nav: Obj<NavigatorRef>, route: Route| -> () {
        nav.0.navigate(Navigation::Push(route.0));
        Ok(())
    }),
    crate::native!(action "replace" |_cx, nav: Obj<NavigatorRef>, route: Route| -> () {
        nav.0.navigate(Navigation::Replace(route.0));
        Ok(())
    }),
    crate::native!(action "pop" |_cx, nav: Obj<NavigatorRef>| -> bool {
        Ok(nav.0.navigate(Navigation::Pop))
    }),
    crate::native!(action "reset" |_cx, nav: Obj<NavigatorRef>, route: Route| -> () {
        nav.0.navigate(Navigation::Reset(route.0));
        Ok(())
    }),
];

/// The navigator type, generic over its route type `R`.
pub static NAV: NativeLibrary = NativeLibrary {
    path: "viso::nav",
    version: 1,
    functions: &[],
    types: &[NativeType::new("Navigator", &METHODS).generic(&["R"])],
    traits: &[],
    derives: &[],
    widgets: &[],
};
