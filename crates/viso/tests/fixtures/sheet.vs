import viso::game::quick::{QuickGame, QuickStart, QuickFrame}
import viso::game::{EntityId, SpawnDesc, GameTag}
import viso::game::kit::{CameraRig, Terrain, Prefab, Particle, Model}
import viso::math::Vec3F32

export system Arena implements QuickGame {
    state ticks = 0

    action start(cx: QuickStart) {
        cx.kit.terrain(Terrain::hills(48.0, 2.0).seed(5))
        let h = cx.spawn(SpawnDesc::prefab(Prefab::hero).at(Vec3F32::new(0.0f32, 0.9f32, 0.0f32)).tag(GameTag::player))
        let m = cx.spawn(SpawnDesc::prefab(Prefab::monster).at(Vec3F32::new(5.0f32, 0.8f32, -2.0f32)))
        cx.kit.chase(m, GameTag::player, 20.0, 2.0)
        cx.kit.emit(m, Particle::smoke)
        cx.spawn(SpawnDesc::prefab(Prefab::tree).at(Vec3F32::new(-4.0f32, 2.0f32, -5.0f32)))
        cx.spawn(SpawnDesc::prefab(Prefab::car).at(Vec3F32::new(-3.0f32, 0.6f32, 2.0f32)))
        cx.spawn(SpawnDesc::prefab(Prefab::coin).at(Vec3F32::new(2.0f32, 1.0f32, 2.0f32)).model(Model::coin))
        cx.kit.camera(CameraRig::third_person(h).distance(6.0))
    }

    action fixed(frame: QuickFrame) {
        ticks += 1
        if ticks == 12 {
            frame.kit.burst(Particle::confetti, Vec3F32::new(0.0f32, 1.5f32, 0.0f32), 40)
        }
        frame.kit.debug_box(Vec3F32::new(2.0f32, 1.0f32, 2.0f32), Vec3F32::new(1.2f32, 1.2f32, 1.2f32))
    }
}
