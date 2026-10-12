import viso::game::{Startup, GameStart, FixedUpdate, FixedFrame, SpawnDesc, InputAction};
import viso::math::Vec3F32;

system Runner implements Startup + FixedUpdate {
    state steps = 0;
    state jumps = 0;

    action startup(cx: GameStart) {
        cx.spawn(SpawnDesc::player().at(Vec3F32::new(0.0f32, 1.0f32, 0.0f32)));
    }

    action fixed_update(frame: FixedFrame) {
        steps += 1;
        if frame.input.pressed(InputAction::jump) {
            jumps += 1;
        }
        for id in frame.world.entities() {
            frame.world.walk(id, 2.0, 0.0f32);
        }
    }
}

export component Arena {
    view {
        Column { width: 200dp; height: 100dp; }
    }
}
