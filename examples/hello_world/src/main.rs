//! The canonical minimal Viso application (AGENTS §1).
//!
//! Phase 0: this compiles and runs against the contract skeleton. It opens no
//! window yet — that arrives in Phase 1 — but proves the facade, prelude, and
//! `Application` lifecycle exist with zero makepad runtime types involved.

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
