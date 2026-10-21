//! The `theme` a view reads: a value of the prelude's `Theme` record, which
//! the state store publishes as the `theme` field of its environment. Setting
//! it raises that field's cell, so exactly the bindings that read `theme`
//! re-evaluate. A store without one publishes [`default_theme`].

use std::rc::Rc;

use viso_behavior::Value;
pub use viso_behavior::theme::default_theme;
use viso_ui::adaptive::{AdaptiveEnv, ThemeValue};
use viso_ui::{FontFamily, NodeStore, StateStore};

use crate::text_style::family;

/// Makes `theme`, a value of the prelude's `Theme` record (such as
/// [`ViewHost::theme`](crate::ViewHost::theme) evaluates), the theme every
/// view over `states` reads, and its typography the root font size and the
/// default family of the text in `store`.
pub fn set_theme(states: &mut StateStore, store: &mut NodeStore, theme: Value) {
    let (base_size, family) = typography(&theme);
    store.set_typography(base_size, family);
    states.set_theme(ThemeValue(Rc::new(theme)));
}

/// A theme's `typography.base_size` and `typography.family`; a value of the
/// wrong shape, which only a compiler bug makes, reads the schema defaults.
fn typography(theme: &Value) -> (f32, FontFamily) {
    fn field(value: &Value, at: usize) -> Option<&Value> {
        match value {
            Value::Agg(agg) => agg.fields.get(at),
            _ => None,
        }
    }
    let scale = field(theme, 1);
    let base_size = scale
        .and_then(|scale| field(scale, 0)?.as_float())
        .map_or(14.0, |v| v as f32);
    let family = scale
        .and_then(|scale| field(scale, 4))
        .map_or(FontFamily::SystemUi, family);
    (base_size, family)
}

/// The theme `env` publishes.
pub(crate) fn current_theme(env: &AdaptiveEnv) -> Value {
    env.theme()
        .and_then(|theme| theme.0.downcast_ref::<Value>())
        .cloned()
        .unwrap_or_else(default_theme)
}
