//! The `theme` a view reads: a value of the prelude's `Theme` record, which
//! the state store publishes as the `theme` field of its environment. Setting
//! it raises that field's cell, so exactly the bindings that read `theme`
//! re-evaluate. A store without one publishes [`default_theme`].

use std::rc::Rc;

use viso_behavior::Value;
pub use viso_behavior::theme::{default_dark_theme, default_theme};
use viso_ui::adaptive::{AdaptiveEnv, ThemeValue};
use viso_ui::{FontFamily, NodeStore, Palette, StateStore};

use crate::text_style::family;

/// Makes `theme`, a value of the prelude's `Theme` record (such as
/// [`ViewHost::theme`](crate::ViewHost::theme) evaluates), the theme every
/// view over `states` reads, and its typography the root font size and the
/// default family of the text in `store`.
pub fn set_theme(states: &mut StateStore, store: &mut NodeStore, theme: Value) {
    let (base_size, family) = typography(&theme);
    store.set_typography(base_size, family);
    if let Some(palette) = palette(&theme) {
        store.set_palette(palette);
    }
    states.set_theme(ThemeValue(Rc::new(theme)));
}

/// The palette of a theme's `colors`, the controls' colors; `None` for a
/// value of the wrong shape, which only a compiler bug makes.
pub fn palette(theme: &Value) -> Option<Palette> {
    let Value::Agg(theme) = theme else {
        return None;
    };
    let Value::Agg(colors) = theme.fields.first()? else {
        return None;
    };
    let mut srgb = [0u32; 14];
    for (at, color) in srgb.iter_mut().zip(colors.fields.iter()) {
        *at = u32::try_from(color.as_int()?).ok()?;
    }
    (colors.fields.len() == 14).then(|| Palette::from_srgb(srgb))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_themes_give_the_stores_their_default_palettes() {
        assert_eq!(palette(&default_theme()), Some(Palette::light()));
        assert_eq!(palette(&default_dark_theme()), Some(Palette::dark()));
    }
}
