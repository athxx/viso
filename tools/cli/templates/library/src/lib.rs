//! Viso components for other apps to mount.

use viso::prelude::*;

/// Mounts the counter: a count and the button that raises it.
pub fn counter(cx: &mut BuildCx<'_>) {
    viso::view!("counter.vs")(cx);
}
