//! The device's gamepads reach a running game through the frame loop: each
//! frame polls them, a pad's buttons and stick drive the game's actions and
//! move vector, a pad that goes away releases what it held, and
//! `env.input.gamepad_available` follows the connections.

use std::cell::Cell;
use std::time::Duration;

use viso::prelude::*;
use viso::ui::NodeId;
use viso_behavior::Value;
use viso_platform::backend::headless::HeadlessGamepads;
use viso_platform::{GamepadButton, GamepadState};
use viso_view::game_session;

thread_local! {
    static ROOT: Cell<Option<NodeId>> = const { Cell::new(None) };
}

struct Game;

impl Application for Game {
    fn new(_cx: &mut AppCx) -> Self {
        Game
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let root = viso::view!("fixtures/gamepad.vs")(cx).id();
        ROOT.with(|r| r.set(Some(root)));
    }
}

fn south(held: bool) -> GamepadState {
    let mut state = GamepadState::default();
    state.set(GamepadButton::South, held);
    state
}

fn run(pads: &HeadlessGamepads, frames: u32) -> viso::__test_support::DrivenApp<Game> {
    viso::__test_support::drive_gamepad_frames::<Game>(
        Vec::new(),
        pads.clone(),
        Duration::from_secs_f64(1.0 / 60.0),
        frames,
    )
}

fn state(app: &viso::__test_support::DrivenApp<Game>, name: &str) -> Option<Value> {
    let root = ROOT.with(Cell::get).expect("built");
    let session = game_session(app.store(), root).expect("the view runs its game");
    let session = session.borrow();
    session.state("Pad", name)
}

#[test]
fn a_pad_presses_the_games_actions_and_moves_it() {
    let pads = HeadlessGamepads::default();
    let pad = pads.connect("Test Pad");
    pads.set(pad, south(true));
    pads.set(pad, south(false));
    pads.set(
        pad,
        GamepadState {
            left_stick: [1.0, 0.0],
            ..GamepadState::default()
        },
    );
    pads.idle();
    pads.idle();
    pads.set(
        pad,
        GamepadState {
            left_stick: [0.05, 0.0],
            ..GamepadState::default()
        },
    );
    let app = run(&pads, 12);
    assert_eq!(pads.pending(), 0, "one step a frame");
    assert_eq!(state(&app, "jumps"), Some(Value::Int(1)));
    assert_eq!(state(&app, "landings"), Some(Value::Int(1)));
    assert_eq!(
        state(&app, "right"),
        Some(Value::Int(3)),
        "the stick held right for three ticks, then rested"
    );
    assert!(app.states().env().environment().input.gamepad_available);
}

#[test]
fn a_pad_that_goes_away_releases_what_it_held_and_is_no_longer_available() {
    let pads = HeadlessGamepads::default();
    let first = pads.connect("First");
    let second = pads.connect("Second");
    pads.set(first, south(true));
    pads.set(second, south(true));
    pads.disconnect(first);
    pads.idle();
    pads.disconnect(second);
    let app = run(&pads, 12);
    assert_eq!(
        state(&app, "jumps"),
        Some(Value::Int(1)),
        "both pads hold one action"
    );
    assert_eq!(
        state(&app, "landings"),
        Some(Value::Int(1)),
        "released only when the last pad holding it went"
    );
    assert!(!app.states().env().environment().input.gamepad_available);
}
