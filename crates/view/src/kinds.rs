//! Each kind of control as what delivering to a node of the kind runs beyond
//! what every node's delivery does: building the parts the framework draws
//! inside it, and showing its state. A view `view!` embeds links the kinds
//! it holds alone, so an app links the code of just the controls it uses;
//! a view loaded at run time, or reloaded, links [`ALL`].
#![allow(non_upper_case_globals)]

use crate::control::ControlKind;
use crate::values::{self, Deliver};

/// What delivering to a node of one kind of control runs.
pub struct Kind {
    pub(crate) kind: ControlKind,
    /// Builds the parts inside it, before its look shows.
    pub(crate) parts: Option<Deliver>,
    /// Shows its state, once its look has.
    pub(crate) state: Option<Deliver>,
}

macro_rules! kinds {
    ($($kind:ident: $parts:expr, $state:expr;)*) => {
        $(
            #[doc = concat!("A `", stringify!($kind), "`.")]
            pub static $kind: Kind = Kind {
                kind: ControlKind::$kind,
                parts: $parts,
                state: $state,
            };
        )*
        /// Every kind.
        pub static ALL: &[&Kind] = &[$(&$kind),*];
    };
}

kinds! {
    Plain: None, None;
    Glass: None, None;
    Dialog: None, None;
    Sheet: None, None;
    Toast: None, None;
    Rust: None, None;
    ColorPicker: Some(values::picker_parts), None;
    ComboBox: Some(values::combo_parts), None;
    SearchField: Some(values::search_parts), None;
    Router: Some(values::router_parts), None;
    Video: Some(values::video_parts), Some(values::video);
    Viewer: None, Some(values::viewer);
    SwipeRow: None, Some(values::swipe_row);
    DragPreview: None, Some(values::drag_preview);
    SwipeActions: None, Some(values::swipe_actions);
    ReorderHandle: None, Some(values::reorder_handle);
    Pages: None, Some(values::pages);
    Viewport: None, Some(values::viewport);
    Shader: None, Some(values::shader);
    Asset: None, Some(values::asset);
    Camera: None, Some(values::camera);
    Label: None, Some(values::label);
    Button: None, Some(values::label);
    TextInput: None, Some(values::text_input);
    Number: None, Some(values::number);
    Check: None, Some(values::check);
    Switch: None, Some(values::check);
    Chip: None, Some(values::chip);
    Stepper: None, Some(values::stepper);
    Slider: None, Some(values::slider);
    Knob: None, Some(values::slider);
    Progress: None, Some(values::progress);
    Ring: None, Some(values::progress);
    Spinner: None, Some(values::spinner);
    Badge: None, Some(values::badge);
    Avatar: None, Some(values::avatar);
    Select: None, Some(values::select_state);
    Segmented: None, Some(values::segmented);
    Disclosure: None, Some(values::disclosure);
    Shortcut: None, Some(values::shortcut);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_is_listed_once() {
        for kind in ControlKind::ALL {
            let listed = ALL.iter().filter(|k| k.kind == kind).count();
            assert_eq!(listed, 1, "{kind:?}");
        }
        assert_eq!(ALL.len(), ControlKind::ALL.len());
    }
}
