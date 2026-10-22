import viso::game::{Startup, GameStart, FixedUpdate, FixedFrame, SpawnDesc, InputAction};
import viso::math::Vec3F32;

system Stand implements Startup + FixedUpdate {
    action startup(cx: GameStart) {
        cx.spawn(SpawnDesc::block(Vec3F32::new(40.0f32, 1.0f32, 40.0f32)).at(Vec3F32::new(0.0f32, -0.5f32, 0.0f32)));
        cx.spawn(SpawnDesc::player().at(Vec3F32::new(0.0f32, 0.9f32, 0.0f32)));
    }

    action fixed_update(frame: FixedFrame) {
        if frame.input.held(InputAction::jump) {
            for id in frame.world.entities() {
                frame.world.walk(id, 2.0, 0.0f32);
            }
        }
    }
}

export component Arena {
    view {
        Column {
            width: 200dp;
            height: 100dp;
            GameViewport {}
        }
    }
}
