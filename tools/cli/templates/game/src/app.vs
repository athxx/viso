import viso::game::{FixedFrame, FixedUpdate, InputAction}
import viso::i18n::tr

export system Player implements FixedUpdate {
    @probe state jumps = 0

    action fixed_update(frame: FixedFrame) {
        if frame.input.pressed(InputAction::jump) {
            jumps += 1
        }
    }
}

export component App {
    view {
        Text { width: 320dp; height: 40dp; text: tr("app.hint") }
    }
}
