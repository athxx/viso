//! A clip from `assets/`, named with `asset("…")` and embedded, played
//! through the platform's own player — decoded in hardware, its sound kept
//! in step — under the framework's controls. The buttons below steer the
//! same two-way `playing` the controls write back.

use viso::prelude::*;

struct Player;

impl Application for Player {
    fn new(_cx: &mut AppCx) -> Self {
        Player
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("player.vs")(cx);
    }
}

fn main() {
    viso::run::<Player>();
}
