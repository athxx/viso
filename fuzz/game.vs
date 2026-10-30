import viso::game::{FixedUpdate, FixedFrame, InputAction, InputAxis}

export system Tally implements FixedUpdate {
    state jumps = 0
    state drift = 0.0

    action fixed_update(frame: FixedFrame) {
        for p in 0..frame.players() {
            let input = frame.input_of(p)
            if input.pressed(InputAction::jump) {
                jumps += 1
            }
            drift = drift + input.axis(InputAxis::move_x)
        }
    }
}
