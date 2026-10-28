//! Native libraries: the Rust functions and types `.vs` code calls.
//!
//! An app's own natives (`api::profile`) live in a crate of their own in
//! its workspace, a native package depending on `viso` alone: its libraries
//! are statics, each function a [`native!`] whose Rust signature is its
//! schema; its manifest names them and their schema beside it, and a test
//! keeps the schema in step ([`check`]).
//!
//! ```toml
//! [dependencies]
//! viso = { version = "1", default-features = false }
//!
//! [dev-dependencies]
//! viso = "1"
//!
//! [package.metadata.viso.natives]
//! schema = "natives.json"
//! library = "NATIVES"
//! ```
//!
//! It takes `viso` without its default features, so it picks none for the
//! app; its own tests build with them.
//!
//! ```ignore
//! use viso::native::{NativeLibrary, native};
//!
//! pub static NATIVES: &[&NativeLibrary] = &[&API];
//!
//! static API: NativeLibrary = NativeLibrary {
//!     path: "api",
//!     version: 1,
//!     functions: &[native!(fn "double" |_cx, n: i64| -> i64 { Ok(n * 2) })],
//!     types: &[],
//!     traits: &[],
//!     derives: &[],
//!     widgets: &[],
//! };
//!
//! #[test]
//! fn schema() {
//!     viso::native::check(NATIVES, "natives.json");
//! }
//! ```
//!
//! The app depends on the package, and its views import the library as
//! they import the standard ones (`import api::double;`).
//!
//! The app's own package may declare libraries the same way, for types of
//! its own its views read (its route enum): its library target holds
//! `pub static NATIVES`, and its views, its binary and its tests name them.
//!
//! A record a native returns or takes is a Rust struct deriving
//! [`NativeData`](macro@NativeData), its type in the library's `types`;
//! `.vs` reads its fields as a record's, and a failure a native reports is a
//! `Result`, which `.vs` matches:
//!
//! ```ignore
//! #[derive(NativeData)]
//! #[native(path = "api::Profile")]
//! pub struct Profile {
//!     pub name: String,
//! }
//!
//! static API: NativeLibrary = NativeLibrary {
//!     path: "api",
//!     version: 1,
//!     functions: &[native!(task "profile" async |cx, id: i64| -> Result<Profile, String> {
//!         let client = cx.service::<viso_net::Client>()?.clone();
//!         Ok(async move { Ok(fetch(&client, id).await) })
//!     })
//!     .requires(&["network.http"])],
//!     types: &[Profile::TYPE],
//!     traits: &[],
//!     derives: &[],
//!     widgets: &[],
//! };
//! ```
//!
//! A widget built in Rust is a [`NativeWidget::rust`] in a library's
//! `widgets`, its own properties and events its schema, and a
//! [`RustWidget`] implementing it, which the package hands its views in a
//! `pub static WIDGETS: &[WidgetImpl]` its manifest names
//! (`widgets = "WIDGETS"`). A view writes it as any node, with every node's
//! layout, look and standard events besides its own; the widget builds its
//! content inside that box, takes its own properties' values as they change
//! ([`RustWidget::set`]) and reports its own events
//! ([`WidgetEvents::fire`]). A two-way property's change event,
//! `<property>_changed`, carries the new value, which a `bind` writes back:
//!
//! ```ignore
//! static PROPS: [WidgetProperty; 1] = [WidgetProperty::new("level", "F32").two_way()];
//! static EVENTS: [WidgetEvent; 1] = [WidgetEvent::new("level_changed", "F32")];
//!
//! static DIALS: NativeLibrary = NativeLibrary {
//!     path: "dials",
//!     widgets: &[NativeWidget::rust("Gauge").properties(&[&PROPS]).events(&[&EVENTS])],
//!     ..
//! };
//!
//! pub static WIDGETS: &[WidgetImpl] = &[WidgetImpl {
//!     name: "Gauge",
//!     new: || Box::new(Gauge::default()),
//! }];
//!
//! #[test]
//! fn widgets() {
//!     viso::native::check_widgets(NATIVES, WIDGETS);
//! }
//! ```

pub use viso_behavior::Value;
pub use viso_behavior::native::schema::check;
pub use viso_macros::NativeData;

pub use viso_view::{RustWidget, StateCells, WidgetEvents, WidgetImpl, check_widgets};

pub use viso_behavior::native::{
    Determinism, NativeCx, NativeData, NativeError, NativeFunction, NativeFuture, NativeHandle,
    NativeKind, NativeLibrary, NativeObject, NativeType, NativeValue, Obj, Ownership, Param,
    SchemaTy, Services, ThreadDomain, Ticks, VariantFields,
};
pub use viso_behavior::native::{NativeWidget, WidgetEvent, WidgetProperty};

/// A [`NativeFunction`] from a Rust closure-like body, its parameter and
/// return types the schema: `native!(fn "double" |_cx, n: i64| -> i64 { .. })`,
/// `fn` read-only, `action` an effect, `task` awaited.
pub use crate::__native as native;

#[doc(hidden)]
#[macro_export]
macro_rules! __native {
    ($($body:tt)*) => {
        $crate::__private::viso_behavior::native!($($body)*)
    };
}
