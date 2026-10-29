//! The standard widget library `viso::widgets`, as a native library: the widget
//! table schema data declares (`viso_schema::widgets`).

use super::NativeLibrary;

pub use viso_schema::widgets::COMPONENT;

/// The standard widget library.
pub static WIDGETS: NativeLibrary = NativeLibrary {
    path: "viso::widgets",
    version: 1,
    functions: &[],
    types: &[],
    traits: &[],
    derives: &[],
    widgets: viso_schema::widgets::STANDARD,
};
