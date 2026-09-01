//! The canonical minimal Viso application (AGENTS §1).
//!
//! Opens the default window on launch and draws its first frame without waiting
//! for any external OS expose event: the facade builds the scene, brings up the
//! GPU, and the scheduler drives the initial frame to the surface. Proves the
//! facade, prelude, and `Application` lifecycle with zero makepad runtime types.

use viso::prelude::*;

struct App;

impl Application for App {
    fn new(_cx: &mut AppCx) -> Self {
        App
    }
}

fn main() {
    viso::run::<App>();
}
