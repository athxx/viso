//! A `.vs` panel showing the gauge of a native package, a widget built in
//! Rust: the view binds its `level` both ways and counts its presses.

use viso::prelude::*;

/// The app: the panel.
pub struct Panel;

impl Application for Panel {
    fn new(_cx: &mut AppCx) -> Self {
        Panel
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("panel.vs")(cx);
    }
}
