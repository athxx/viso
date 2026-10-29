//! What a view does to the game its root runs: `game.pause()` and
//! `game.resume()` are the actions `viso::game::session` declares, which ask
//! the [`GameControl`] the view's host lends its VM. The session takes the
//! request at its next frame.

use alloc::rc::Rc;
use core::cell::Cell;

use crate::native::NativeLibrary;

/// The pause a view asks of its game, shared by the view's VM and the
/// session.
#[derive(Debug, Clone, Default)]
pub struct GameControl(Rc<Cell<Option<bool>>>);

impl GameControl {
    /// A control no view has asked anything of.
    pub fn new() -> GameControl {
        GameControl::default()
    }

    /// Asks the game to pause, or to run again; a later request replaces it.
    pub fn request(&self, paused: bool) {
        self.0.set(Some(paused));
    }

    /// The request standing, taken.
    pub fn take(&self) -> Option<bool> {
        self.0.take()
    }
}

/// The actions a view's `game` binding lowers to: without a [`GameControl`]
/// (the view runs no game) they do nothing.
pub static SESSION: NativeLibrary = NativeLibrary {
    path: "viso::game::session",
    version: 1,
    functions: &[
        crate::native!(action "pause" |cx| -> () {
            if let Ok(control) = cx.service::<GameControl>() {
                control.request(true);
            }
            Ok(())
        }),
        crate::native!(action "resume" |cx| -> () {
            if let Ok(control) = cx.service::<GameControl>() {
                control.request(false);
            }
            Ok(())
        }),
    ],
    types: &[],
    traits: &[],
    derives: &[],
    widgets: &[],
};

/// The path of the action `game.pause()` calls.
pub const PAUSE_PATH: &str = "viso::game::session::pause";

/// The path of the action `game.resume()` calls.
pub const RESUME_PATH: &str = "viso::game::session::resume";
