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

pub use viso_behavior::Value;
pub use viso_behavior::native::schema::check;
pub use viso_macros::NativeData;

pub use viso_behavior::native::{
    Determinism, NativeCx, NativeData, NativeError, NativeFunction, NativeFuture, NativeHandle,
    NativeKind, NativeLibrary, NativeObject, NativeType, NativeValue, Obj, Ownership, Param,
    SchemaTy, Services, ThreadDomain, Ticks, VariantFields,
};

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
