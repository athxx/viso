import viso::game::quick::{QuickGame, QuickStart, QuickFrame};
import viso::game::{InputAction, InputAxis};

export system Runner implements QuickGame {
    state x = 0.0;
    state jumps = 0;

    action start(cx: QuickStart) {
        x = 0.0;
    }

    action fixed(frame: QuickFrame) {
        x = x + frame.input.axis(InputAxis::move_x) * frame.dt();
        if frame.input.pressed(InputAction::jump) {
            jumps += 1;
        }
    }
}
