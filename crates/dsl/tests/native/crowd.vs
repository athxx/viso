import viso::game::{FixedUpdate, FixedFrame};

record Agent {
    pos: F64;
    vel: F64;
    mood: I64;
}

fn steer(a: Agent, target: F64, tick: I64) -> Agent {
    let pull = (target - a.pos) * 0.05;
    let mood = (a.mood * 7 + tick) % 17;
    let vel = a.vel * 0.9 + pull + (mood as F64 - 8.0) * 0.01;
    Agent { pos: a.pos + vel, vel: vel, mood: mood }
}

export system Crowd implements FixedUpdate {
    state seeds: List<I64> = [7411, 9171, 7629, 7402, 8320, 9623, 3111, 3025, 8387, 7794, 3050, 1542, 7316, 4970, 2323, 1485, 8825, 686, 9755, 6490, 7421, 2580, 245, 8656, 1034, 975, 584, 3116, 3963, 9824, 492, 7601, 5345, 7217, 9682, 3200, 8505, 3828, 4819, 8188, 75, 1392, 7492, 4557, 6664, 9031, 1363, 4161, 5165, 3762, 8403, 4735, 487, 1150, 9226, 1768, 6560, 1766, 4766, 6332, 1094, 276, 8, 3498, 3436, 857, 7700, 6151, 6511, 6877, 1196, 9277, 3252, 4420, 5519, 1427, 5098, 5449, 248, 6718, 1933, 2205, 4036, 1655, 179, 981, 7617, 7976, 2911, 9163, 3086, 7330, 8337, 3124, 2145, 6868, 6287, 1908, 6469, 6893, 3487, 7, 4420, 9711, 4983, 321, 3452, 3068, 6459, 9863, 9452, 1643, 689, 2397, 3493, 7234, 4231, 156, 5389, 4854, 6327, 1202, 1217, 1476, 3419, 9547, 3981, 254];
    state lead: Agent = Agent { pos: 0.0, vel: 0.0, mood: 3 };
    state energy = 0.0;
    state checksum = 0;

    action fixed_update(frame: FixedFrame) {
        let tick = frame.tick();
        let mut acc = 0;
        let mut e = energy;
        let mut agent = lead;
        for i in 0..128 {
            let a = seeds[i];
            let b = (a * 1103 + tick * 7 + i) % 9973;
            seeds[i] = b;
            acc = acc + b % 101;
            e = e * 0.999 + (b as F64) * 0.001;
            agent = steer(agent, (b % 64) as F64, tick + i);
        }
        checksum = (checksum + acc) % 1000003;
        energy = e;
        lead = agent;
    }
}
