import viso::game::{Startup, FixedUpdate, GameStart, FixedFrame, InputAction, InputAxis};

export system Movement implements Startup + FixedUpdate {
    state x = 0.0;

    action startup(cx: GameStart) {
        x = 0.0;
    }

    action fixed_update(frame: FixedFrame) {
        x = x + frame.input.axis(InputAxis::move_x) * frame.dt();
    }
}

@after(Movement)
export system Jumping implements FixedUpdate {
    state jumps = 0;

    action fixed_update(frame: FixedFrame) {
        if frame.input.pressed(InputAction::jump) {
            jumps += 1;
        }
    }
}
