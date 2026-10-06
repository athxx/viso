//! Rollback netcode on the Simulation tier: each player's input in a tick
//! (`frame.input_of`), a session per peer over a transport-agnostic packet
//! stream with input delay, prediction, confirmation and desync detection,
//! and two in-process peers over a lossy, delayed, reordering link that
//! converge on the game the true inputs give.

use std::rc::Rc;

use viso_behavior::game::{
    InputAction, Key, PacketError, RollbackSession, Scheduler, SessionConfig, TickInput,
};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Module, Value, Vm};
use viso_dsl::frontend::{Origin, compile_file_for};
use viso_dsl::hir::{Determinism, TargetProfile};

const DUEL: &str = r#"
import viso::game::{Startup, GameStart, FixedUpdate, CollisionListener, FixedFrame, CollisionEvent};
import viso::game::{EntityId, SpawnDesc, GameTag, InputAction, InputAxis};
import viso::game::kit::Sfx;
import viso::math::Vec3F32;

export system Duel implements Startup + FixedUpdate + CollisionListener {
    state heroes: List<EntityId> = [];
    state jumps: List<I64> = [0, 0];
    state roll = 0;
    state touches = 0;

    action startup(cx: GameStart) {
        cx.spawn(SpawnDesc::block(Vec3F32::new(40.0f32, 1.0f32, 40.0f32)).at(Vec3F32::new(0.0f32, -0.5f32, 0.0f32)));
        let a = cx.spawn(SpawnDesc::player().at(Vec3F32::new(-3.0f32, 0.9f32, 0.0f32)));
        let b = cx.spawn(SpawnDesc::player().at(Vec3F32::new(3.0f32, 0.9f32, 0.0f32)));
        heroes = [a, b];
        for i in 0..6 {
            cx.spawn(SpawnDesc::sensor(Vec3F32::new(0.6f32, 0.6f32, 0.6f32)).at(Vec3F32::new(-5.0f32 + 2.0f32 * (i as F32), 0.6f32, 0.0f32)).tag(GameTag::coin));
        }
    }

    action fixed_update(frame: FixedFrame) {
        roll = (roll * 7 + frame.world.random_range(0, 100)) % 100003;
        for p in 0..frame.players() {
            let input = frame.input_of(p);
            let id = heroes[p];
            frame.world.walk(id, input.axis(InputAxis::move_x) * 5.0, input.axis(InputAxis::move_z) * 5.0);
            if input.pressed(InputAction::jump) && frame.world.on_floor(id) {
                frame.world.jump(id, 5.0);
                jumps[p] = jumps[p] + 1;
            }
        }
        frame.kit.sound(Sfx::step);
    }

    action collision(event: CollisionEvent) {
        touches += 1;
    }
}
"#;

fn module() -> Rc<Module> {
    let origin = Origin {
        package: "app".to_owned(),
        module: vec!["main".to_owned()],
        language: None,
    };
    let profile = TargetProfile {
        determinism: Determinism::CrossPlatform,
        ..TargetProfile::default()
    };
    let compiled = compile_file_for(DUEL, &origin, Natives::standard(), profile);
    let errors: Vec<_> = compiled.errors().collect();
    assert!(errors.is_empty(), "{errors:#?}");
    Rc::new(compiled.behavior.bytecode().expect("verified bytecode"))
}

fn game(module: &Rc<Module>, seed: u64) -> Scheduler {
    let mut vm = Vm::new(module.clone(), Budget::default());
    vm.link(&Natives::standard(), &[]).expect("link");
    Scheduler::with_seed(vm, seed).expect("a started game")
}

fn state(game: &Scheduler, name: &str) -> Value {
    let module = game.vm().module();
    let component = module.component("Duel").expect("system");
    let slot = module.layout(component).state(name).expect("state");
    game.instance(0).states()[slot].clone()
}

fn ints(value: Value) -> Vec<i64> {
    match value {
        Value::List(items) => items
            .iter()
            .map(|v| v.as_int().expect("an integer"))
            .collect(),
        other => panic!("not a list: {other:?}"),
    }
}

const JUMP: u64 = 1 << InputAction::Jump as u32;

fn moving(x: f64, y: f64) -> TickInput {
    TickInput {
        axes: [x.to_bits(), y.to_bits()],
        ..TickInput::default()
    }
}

#[test]
fn each_player_reads_their_own_input() {
    let module = module();
    let mut game = game(&module, 1);
    game.set_players(2);
    // Let both land, then player 1 alone jumps.
    for _ in 0..30 {
        game.step_with(&[TickInput::default(), moving(0.0, 0.0)]);
    }
    let jump = TickInput {
        held: JUMP,
        pressed: JUMP,
        ..TickInput::default()
    };
    game.step_with(&[moving(1.0, 0.0), jump]);
    assert_eq!(ints(state(&game, "jumps")), [0, 1]);
    assert!(game.faults().is_empty(), "{:#?}", game.faults());
    // A single-player tick leaves player 1 without input.
    game.step(1);
    assert!(game.faults().is_empty(), "{:#?}", game.faults());
}

/// A deterministic stream of pseudo-random numbers.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, below: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % below
    }
}

/// A one-way link that loses a quarter of the packets and delays the rest
/// by 1 to 6 steps, reordering them.
struct Link {
    rng: Lcg,
    queue: Vec<(u64, Vec<u8>)>,
}

impl Link {
    fn new(seed: u64) -> Link {
        Link {
            rng: Lcg(seed),
            queue: Vec::new(),
        }
    }

    fn send(&mut self, step: u64, packet: Vec<u8>) {
        if self.rng.next(4) != 0 {
            let due = step + 1 + self.rng.next(6);
            self.queue.push((due, packet));
        }
    }

    fn due(&mut self, step: u64) -> Vec<Vec<u8>> {
        let (due, rest): (Vec<_>, Vec<_>) = self.queue.drain(..).partition(|p| p.0 <= step);
        self.queue = rest;
        due.into_iter().map(|p| p.1).collect()
    }
}

/// The keys `peer` holds during step `step`, and the steps it taps jump on.
fn held_keys(peer: usize, step: u64) -> Vec<Key> {
    let mut keys = Vec::new();
    match peer {
        0 => {
            if (10..60).contains(&step) {
                keys.push(Key::D);
            }
            if (70..110).contains(&step) {
                keys.push(Key::S);
            }
        }
        _ => {
            if (5..50).contains(&step) {
                keys.push(Key::A);
            }
            if (55..90).contains(&step) {
                keys.push(Key::W);
            }
        }
    }
    keys
}

fn taps(peer: usize) -> &'static [u64] {
    if peer == 0 { &[40, 95, 120] } else { &[33, 61] }
}

/// The input the latch freezes for the tick sampled at `step`.
fn expected(peer: usize, step: u64) -> TickInput {
    let mut input = TickInput::default();
    let (mut x, mut y) = (0.0, 0.0);
    for key in held_keys(peer, step) {
        match key {
            Key::D => x += 1.0,
            Key::A => x -= 1.0,
            Key::W => y += 1.0,
            Key::S => y -= 1.0,
            _ => {}
        }
    }
    input.axes = [f64::to_bits(x), f64::to_bits(y)];
    if taps(peer).contains(&step) {
        input.pressed = JUMP;
        input.released = JUMP;
    }
    input
}

/// Reports `peer`'s device input of step `step`.
fn press(session: &mut RollbackSession, peer: usize, step: u64) {
    let game = session.game_mut();
    let before = if step == 0 {
        Vec::new()
    } else {
        held_keys(peer, step - 1)
    };
    let now = held_keys(peer, step);
    for key in before.iter().filter(|k| !now.contains(k)) {
        game.key(*key, false);
    }
    for key in now.iter().filter(|k| !before.contains(k)) {
        game.key(*key, true);
    }
    if taps(peer).contains(&step) {
        game.key(Key::Space, true);
        game.key(Key::Space, false);
    }
}

#[test]
fn two_peers_over_a_lossy_delayed_link_converge() {
    let module = module();
    let config = |local| SessionConfig {
        max_prediction: 32,
        hash_interval: 4,
        ..SessionConfig::new(2, local)
    };
    let mut peers = [
        RollbackSession::new(game(&module, 5), config(0)).expect("a session"),
        RollbackSession::new(game(&module, 5), config(1)).expect("a session"),
    ];
    let mut links = [Link::new(11), Link::new(23)];
    let steps = 160;
    for step in 0..steps + 60 {
        for (p, peer) in peers.iter_mut().enumerate() {
            if step < steps {
                press(peer, p, step);
            }
            assert_eq!(peer.advance(1), 1, "peer {p} stalled at step {step}");
        }
        links[0].send(step, peers[0].packet_for(1));
        links[1].send(step, peers[1].packet_for(0));
        for packet in links[0].due(step) {
            peers[1].receive(&packet).expect("a packet");
        }
        for packet in links[1].due(step) {
            peers[0].receive(&packet).expect("a packet");
        }
    }
    let delay = u64::from(config(0).input_delay);
    let ticks = steps + 60;
    for (p, peer) in peers.iter().enumerate() {
        assert!(peer.desyncs().is_empty(), "{:?}", peer.desyncs());
        assert!(peer.confirmed_tick() + 32 >= ticks, "peer {p}: {peer:?}");
        let stats = peer.stats();
        assert!(
            stats.rollbacks > 0 && stats.predicted > 0,
            "peer {p}: {stats:?}"
        );
        assert!(
            peer.game().faults().is_empty(),
            "{:#?}",
            peer.game().faults()
        );
        // One sound a tick, each delivered once however often it reran.
        let game = peer.game();
        assert_eq!(game.delivered_commands(), game.clock().tick(), "peer {p}");
        assert!(game.replayed_commands() > 0, "peer {p}");
    }

    // Both peers hashed the same confirmed ticks alike.
    let common = (0..ticks)
        .rev()
        .find(|&t| peers[0].confirmed_hash(t).is_some() && peers[1].confirmed_hash(t).is_some())
        .expect("a tick both confirmed");
    assert!(common >= steps);
    assert_eq!(
        peers[0].confirmed_hash(common),
        peers[1].confirmed_hash(common)
    );

    // And that is the game the true inputs give.
    let mut truth = game(&module, 5);
    truth.set_players(2);
    for tick in 0..common {
        let input = |peer| {
            tick.checked_sub(delay)
                .map_or_else(TickInput::default, |step| expected(peer, step))
        };
        truth.step_with(&[input(0), input(1)]);
    }
    assert_eq!(
        Some(truth.snapshot().hash()),
        peers[0].confirmed_hash(common)
    );
    assert!(
        ints(state(&truth, "jumps")).iter().all(|&j| j > 0),
        "both jumped"
    );
}

#[test]
fn a_peer_alone_stalls_past_its_prediction_window() {
    let module = module();
    let mut solo =
        RollbackSession::new(game(&module, 5), SessionConfig::new(2, 0)).expect("a session");
    let config = solo.config();
    // At most `max_prediction` ticks are owed; the rest is dropped.
    assert_eq!(solo.advance(50), config.max_prediction);
    // The remote's first `input_delay` ticks are known to be empty.
    let window = u64::from(config.input_delay + config.max_prediction);
    assert_eq!(
        u64::from(solo.advance(50)),
        window - u64::from(config.max_prediction)
    );
    assert_eq!(solo.game().clock().tick(), window);
    assert_eq!(solo.advance(1), 0);
    assert!(solo.stats().stalls > 0);

    // The remote's inputs arrive: the game runs on.
    let mut other =
        RollbackSession::new(game(&module, 5), SessionConfig::new(2, 1)).expect("a session");
    other.advance(5);
    solo.receive(&other.packet_for(0)).expect("a packet");
    assert!(solo.advance(1) > 0);
}

#[test]
fn diverged_games_are_reported_and_foreign_packets_refused() {
    let module = module();
    let config = |local| SessionConfig {
        hash_interval: 2,
        ..SessionConfig::new(2, local)
    };
    // Same build, another seed: the worlds draw other numbers.
    let mut a = RollbackSession::new(game(&module, 5), config(0)).expect("a session");
    let mut b = RollbackSession::new(game(&module, 6), config(1)).expect("a session");
    for _ in 0..20 {
        a.advance(1);
        b.advance(1);
        let (to_b, to_a) = (a.packet_for(1), b.packet_for(0));
        b.receive(&to_b).expect("a packet");
        a.receive(&to_a).expect("a packet");
    }
    assert!(!a.desyncs().is_empty() && !b.desyncs().is_empty());
    let desync = a.desyncs()[0];
    assert_eq!(desync.peer, 1);
    assert_ne!(desync.local, desync.remote);

    assert!(matches!(
        a.receive(b"nonsense"),
        Err(PacketError::Decode(_))
    ));
    let mut third =
        RollbackSession::new(game(&module, 5), SessionConfig::new(3, 2)).expect("a session");
    third.advance(1);
    // Player 2 is no player of a two-player session.
    assert_eq!(a.receive(&third.packet_for(0)), Err(PacketError::Peer(2)));
}
