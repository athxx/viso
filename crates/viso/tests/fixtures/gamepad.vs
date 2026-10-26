import viso::game::{FixedUpdate, FixedFrame, InputAction};

system Pad implements FixedUpdate {
    state jumps = 0;
    state landings = 0;
    state right = 0;

    action fixed_update(frame: FixedFrame) {
        if frame.input.pressed(InputAction::jump) {
            jumps += 1;
            frame.kit.rumble(1.0, 0.5, 0.2);
        }
        if frame.input.released(InputAction::jump) {
            landings += 1;
        }
        if frame.input.move_axes().x > 0.5 {
            right += 1;
        }
    }
}

export component Arena {
    view {
        Column {
            if env.input.gamepad_available {
                Text { text: "pad"; }
            } else {
                Text { text: "keys"; }
            }
        }
    }
}
