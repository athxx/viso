import viso::game::{Startup, GameStart, FixedUpdate, PrePhysics, PostPhysics, CollisionListener};
import viso::game::{FixedFrame, CollisionEvent, EntityId, SpawnDesc, GameTag, InputAction, InputAxis};
import viso::game::kit::{Prefab, Sfx};
import viso::math::Vec3F32;

record Stats {
    hp: I64;
    armor: I64;
    speed: F64;
}

enum Mood {
    Calm;
    Angry(I64);
    Fleeing(F64, I64);
}

fn hurt(s: Stats, hit: I64) -> Stats {
    let taken = if hit > s.armor { hit - s.armor } else { 1 };
    Stats { hp: s.hp - taken, armor: s.armor, speed: s.speed * 0.9 }
}

fn mood_of(hp: I64, tick: I64) -> Mood {
    if hp < 20 {
        Mood::Fleeing(1.5 + (tick % 7) as F64, hp)
    } else if tick % 5 == 0 {
        Mood::Angry(tick % 13)
    } else {
        Mood::Calm
    }
}

fn weight(m: Mood) -> I64 {
    match m {
        Mood::Calm => 1,
        Mood::Angry(n) => 2 + n,
        Mood::Fleeing(f, hp) => (f * 10.0) as I64 - hp,
    }
}

fn total(xs: List<I64>) -> I64 {
    let mut t = 0;
    for x in xs {
        t += x;
    }
    t
}

export system Level implements Startup {
    state spawned = 0;

    action startup(cx: GameStart) {
        cx.spawn(SpawnDesc::block(Vec3F32::new(60.0f32, 1.0f32, 60.0f32)).at(Vec3F32::new(0.0f32, -0.5f32, 0.0f32)));
        cx.spawn(SpawnDesc::prefab(Prefab::hero).at(Vec3F32::new(0.0f32, 0.9f32, 0.0f32)).tag(GameTag::player));
        for i in 0..8 {
            let x = 2.0f32 + 1.5f32 * (i as F32);
            cx.spawn(SpawnDesc::prefab(Prefab::coin).at(Vec3F32::new(x, 0.6f32, 0.0f32)).tag(GameTag::coin));
            spawned += 1;
        }
        for i in 0..4 {
            let x = 6.0f32 + 3.0f32 * (i as F32);
            let m = cx.spawn(SpawnDesc::prefab(Prefab::monster).at(Vec3F32::new(x, 0.8f32, 3.0f32)).tag(GameTag::enemy));
            if i % 2 == 0 {
                cx.kit.chase(m, GameTag::player, 20.0, 3.0);
            } else {
                cx.kit.wander(m, 3.0, 1.0, 400ms);
            }
            spawned += 1;
        }
    }
}

@after(Level)
export system Controls implements FixedUpdate {
    state jumps = 0;
    state steps: List<I64> = [0, 0, 0, 0];

    action fixed_update(frame: FixedFrame) {
        for id in frame.world.query(GameTag::player) {
            let mx = frame.input.axis(InputAxis::move_x);
            frame.world.walk(id, mx * 5.0, frame.input.axis(InputAxis::move_z) * 5.0);
            if frame.input.pressed(InputAction::jump) && frame.world.on_floor(id) {
                frame.world.jump(id, 5.0);
                jumps += 1;
            }
            let lane = frame.tick() % 4;
            steps[lane] = steps[lane] + (mx * 3.0) as I64 + 1;
        }
    }
}

export system Brain implements PrePhysics {
    state stats: Stats = Stats { hp: 100, armor: 2, speed: 3.0 };
    state pressure = 0;
    state roll = 0;
    state heading = 0.0f32;

    action pre_physics(frame: FixedFrame) {
        let tick = frame.tick();
        let mood = mood_of(stats.hp, tick);
        pressure += weight(mood);
        roll = (roll * 31 + frame.world.random_range(0, 1000)) % 1000003;
        let scale = |x: I64| x * 3 + pressure % 11;
        for hero in frame.world.query(GameTag::player) {
            let at = frame.world.position(hero);
            heading = heading * 0.5f32 + at.x * 0.25f32 - at.z;
            for m in frame.world.query(GameTag::enemy) {
                let p = frame.world.position(m);
                let (dx, dz) = (p.x - at.x, p.z - at.z);
                if dx * dx + dz * dz < 4.0f32 {
                    stats = hurt(stats, scale(2));
                }
            }
        }
        if stats.hp < 0 {
            stats.hp = 100;
            stats.speed = stats.speed + 1.0;
        }
    }
}

export system Combat implements CollisionListener {
    state score = 0;
    state hits = 0;
    state log = "";

    action collision(event: CollisionEvent) {
        for hero in event.world.query(GameTag::player) {
            match event.other_of(hero) {
                Option::Some(other) => {
                    if event.world.has_tag(other, GameTag::coin) {
                        score += 1;
                        event.world.remove(other);
                        event.kit.sound(Sfx::coin);
                        log = format("coin {s} at {t}", s: score, t: event.tick());
                    } else if event.world.has_tag(other, GameTag::enemy) {
                        hits += 1;
                        event.kit.sound(Sfx::hurt);
                        log = log + "!";
                    }
                },
                Option::None => {},
            }
        }
    }
}

export system Ledger implements PostPhysics {
    state ticks = 0;
    state history: List<I64> = [];
    state sum = 0;
    state blown = 0;
    state fell = 0;

    action post_physics(frame: FixedFrame) {
        ticks += 1;
        let tick = frame.tick();
        let mut row = [tick % 3, tick % 5, tick % 7];
        row[1] = row[0] + row[2];
        sum = sum + total(row);
        if tick % 10 == 0 {
            history = [sum, ticks, total(history)];
        }
        for id in frame.world.entities() {
            if frame.world.position(id).y < -10.0f32 {
                frame.world.remove(id);
                fell += 1;
            }
        }
        if tick % 97 == 96 {
            blown += 1;
            let xs = [1];
            sum = xs[tick];
        }
    }
}

export system Crowd implements FixedUpdate {
    state agents: List<I64> = [4185, 5874, 8684, 475, 7628, 4080, 849, 2569, 1854, 6091, 7685, 4039, 6238, 8908, 1670, 9403, 4085, 214, 3550, 6687, 4579, 2983, 6380, 2614, 1178, 2273, 7288, 2075, 2166, 29, 87, 3431];
    state energy = 0.0;
    state checksum = 0;

    action fixed_update(frame: FixedFrame) {
        let tick = frame.tick();
        let mut acc = 0;
        let mut e = energy;
        for i in 0..32 {
            let a = agents[i];
            let b = (a * 1103 + tick * 7 + i) % 9973;
            agents[i] = b;
            acc = acc + b % 101;
            e = e * 0.999 + (b as F64) * 0.001;
        }
        checksum = (checksum + acc) % 1000003;
        energy = e;
    }
}
