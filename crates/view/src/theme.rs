//! The `theme` a view reads: a value of the prelude's `Theme` record, which
//! the state store publishes as the `theme` field of its environment. Setting
//! it raises that field's cell, so exactly the bindings that read `theme`
//! re-evaluate. A store without one publishes [`default_theme`].

use std::rc::Rc;

use viso_behavior::Value;
pub use viso_behavior::theme::default_theme;
use viso_ui::StateStore;
use viso_ui::adaptive::{AdaptiveEnv, ThemeValue};

/// Makes `theme`, a value of the prelude's `Theme` record (such as
/// [`ViewHost::theme`](crate::ViewHost::theme) evaluates), the theme every
/// view over `states` reads.
pub fn set_theme(states: &mut StateStore, theme: Value) {
    states.set_theme(ThemeValue(Rc::new(theme)));
}

/// The theme `env` publishes.
pub(crate) fn current_theme(env: &AdaptiveEnv) -> Value {
    env.theme()
        .and_then(|theme| theme.0.downcast_ref::<Value>())
        .cloned()
        .unwrap_or_else(default_theme)
}
