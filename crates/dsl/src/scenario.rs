//! Headless game scenarios (Viso_DSL_1.0.md section 110.5; Viso_CLI.md
//! section 22.3): a package's systems run on an input tape for a number of
//! frames, each tick's `@probe` states traced as JSON and checked against the
//! scenario's expectations, the run ending with its snapshot hash and entity
//! snapshot.
//!
//! A scenario is the text form of an input tape plus expectation lines,
//! each checked after its tick ran:
//!
//! ```text
//! seed 1234
//! 30: press Jump
//! 45: expect Player.on_floor == false
//! 600: expect Player.score == 3
//! ```
//!
//! The right side is the probe's JSON value: `3`, `true`, `"Run"`,
//! `{"hp":3}`. A frame runs one tick, then every `FrameUpdate`.

use std::fmt;

pub use viso_behavior::game::DEFAULT_SEED;
use viso_behavior::game::kit::Model;
use viso_behavior::game::{BodyKind, InputTape, Scheduler, SystemFault, TapeError};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Vm};
use viso_ende::JsonWriter;

use crate::behavior::Program;
use crate::behavior::probe::Probe;

/// A tape and what its run should show.
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub tape: InputTape,
    pub expectations: Vec<Expectation>,
}

/// A probe's value after a tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expectation {
    /// The 1-based line of the scenario it is written on.
    pub line: u32,
    pub tick: u64,
    /// `System.state`.
    pub probe: String,
    /// The value, as JSON.
    pub value: String,
}

impl Scenario {
    /// A scenario of no input and no expectations, for a run seeded with
    /// `seed` at `tick_rate`.
    pub fn empty(seed: u64, tick_rate: u32) -> Scenario {
        Scenario {
            tape: InputTape::new(seed, tick_rate),
            expectations: Vec::new(),
        }
    }

    /// Parses a scenario, for a run seeded with `seed` at `tick_rate` unless
    /// its header says otherwise.
    ///
    /// # Errors
    ///
    /// The first line that is neither a tape directive nor an expectation.
    pub fn parse(text: &str, seed: u64, tick_rate: u32) -> Result<Scenario, TapeError> {
        let mut expectations = Vec::new();
        let mut tape = String::with_capacity(text.len());
        for (n, line) in text.lines().enumerate() {
            let code = line.split('#').next().unwrap_or("");
            match code.split_once(':') {
                Some((tick, rest)) if rest.trim_start().starts_with("expect ") => {
                    let line = n as u32 + 1;
                    let fail = |message: String| TapeError {
                        line: Some(line),
                        message,
                    };
                    let tick = tick.trim();
                    let tick = tick
                        .parse()
                        .map_err(|_| fail(format!("`{tick}` is not a tick")))?;
                    let rest = rest.trim_start().trim_start_matches("expect ");
                    let (probe, value) = rest.split_once("==").ok_or_else(|| {
                        fail(format!(
                            "expected `expect System.state == value`, found `expect {}`",
                            rest.trim()
                        ))
                    })?;
                    expectations.push(Expectation {
                        line,
                        tick,
                        probe: probe.trim().to_owned(),
                        value: value.trim().to_owned(),
                    });
                }
                _ => tape.push_str(line),
            }
            tape.push('\n');
        }
        Ok(Scenario {
            tape: InputTape::parse(&tape, seed, tick_rate)?,
            expectations,
        })
    }
}

/// A tape file's tape, binary or text.
///
/// # Errors
///
/// When the bytes are neither a binary tape nor a text one.
pub fn read_tape(bytes: &[u8], seed: u64, tick_rate: u32) -> Result<InputTape, TapeError> {
    if InputTape::is_binary(bytes) {
        return InputTape::decode(bytes).map_err(|error| TapeError {
            line: None,
            message: format!("a malformed tape: {error}"),
        });
    }
    let text = std::str::from_utf8(bytes).map_err(|_| TapeError {
        line: None,
        message: "a tape is a binary tape or UTF-8 text".to_owned(),
    })?;
    InputTape::parse(text, seed, tick_rate)
}

/// Why a scenario did not run.
#[derive(Debug)]
pub enum RunError {
    /// The package's code does not verify or link.
    Build(String),
    /// The tape does not fit the build.
    Tape(TapeError),
    /// A system could not be created or the start faulted.
    Start(SystemFault),
    /// An expectation names no `@probe` state.
    UnknownProbe(Expectation),
}

impl fmt::Display for RunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunError::Build(message) => f.write_str(message),
            RunError::Tape(error) => write!(f, "{error}"),
            RunError::Start(fault) => write!(f, "the game did not start: {}", fault.fault),
            RunError::UnknownProbe(e) => {
                write!(f, "line {}: `{}` is not a `@probe` state", e.line, e.probe)
            }
        }
    }
}

impl std::error::Error for RunError {}

/// An expectation the run did not meet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub expectation: Expectation,
    /// The probe's value after the tick, or `None` when the run ended first.
    pub actual: Option<String>,
}

/// What a scenario's run showed.
#[derive(Debug)]
pub struct GameRun {
    /// The ticks run.
    pub ticks: u64,
    pub seed: u64,
    /// The hash of the build run.
    pub build: u64,
    /// The hash of the final snapshot.
    pub snapshot_hash: u64,
    /// The entities at the end, in allocation order.
    pub entities: Vec<EntityState>,
    pub failures: Vec<Failure>,
    /// The faults the systems raised, in order.
    pub faults: Vec<SystemFault>,
}

/// An entity at the end of a run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EntityState {
    pub index: u32,
    pub generation: u32,
    /// `character`, `block` or `sensor`.
    pub kind: &'static str,
    /// One bit per tag variant.
    pub tags: u64,
    /// What it is drawn as: a `viso::game::kit::Model` variant.
    pub model: &'static str,
    pub position: [f32; 3],
    pub half_extents: [f32; 3],
}

/// Runs `program`'s systems on `scenario` for `frames` frames, a world seeded
/// with the tape's seed, calling `trace` after each tick with the tick and a
/// JSON object of every `@probe` state by `System.state`.
///
/// # Errors
///
/// When the build does not verify or link, the tape does not fit it, the
/// start faults or an expectation names no probe.
pub fn run(
    program: &Program,
    scenario: &Scenario,
    frames: u64,
    mut trace: impl FnMut(u64, &str),
) -> Result<GameRun, RunError> {
    let probes: Vec<(usize, String, &Probe)> = program
        .systems
        .iter()
        .enumerate()
        .flat_map(|(i, system)| {
            let name = &program.components[system.component as usize].name;
            system
                .probes
                .iter()
                .map(move |p| (i, format!("{name}.{}", p.name), p))
        })
        .collect();
    if let Some(e) = scenario
        .expectations
        .iter()
        .find(|e| !probes.iter().any(|p| p.1 == e.probe))
    {
        return Err(RunError::UnknownProbe(e.clone()));
    }
    let module = program
        .bytecode()
        .map_err(|error| RunError::Build(format!("the package's code does not verify: {error}")))?;
    let mut vm = Vm::new(std::rc::Rc::new(module), Budget::default());
    vm.link(&Natives::standard(), &[])
        .map_err(|error| RunError::Build(format!("the package's natives do not link: {error}")))?;
    let mut game = Scheduler::with_seed(vm, scenario.tape.seed).map_err(RunError::Start)?;
    game.play(scenario.tape.clone()).map_err(RunError::Tape)?;

    let mut pending: Vec<&Expectation> = scenario.expectations.iter().collect();
    pending.sort_by_key(|e| e.tick);
    let mut pending = pending.into_iter().peekable();
    let mut failures = Vec::new();
    let mut faults = Vec::new();
    let value =
        |game: &Scheduler, (system, _, probe): &(usize, String, &Probe), json: &mut JsonWriter| {
            let state = &game.instance(*system).states()[probe.slot as usize];
            probe.shape.write(state, json);
        };
    for tick in 0..frames {
        game.step(1);
        game.frame(0.0);
        faults.append(&mut game.take_faults());
        let mut json = JsonWriter::new();
        json.begin_object();
        for probe in &probes {
            json.name(&probe.1);
            value(&game, probe, &mut json);
        }
        json.end_object();
        trace(tick, json.as_str());
        while let Some(e) = pending.next_if(|e| e.tick <= tick) {
            let actual = (e.tick == tick).then(|| {
                let mut json = JsonWriter::new();
                if let Some(probe) = probes.iter().find(|p| p.1 == e.probe) {
                    value(&game, probe, &mut json);
                }
                json.into_string()
            });
            if actual.as_deref().is_none_or(|a| !same_json(a, &e.value)) {
                failures.push(Failure {
                    expectation: e.clone(),
                    actual,
                });
            }
        }
    }
    failures.extend(pending.map(|e| Failure {
        expectation: e.clone(),
        actual: None,
    }));
    let mut extracted = Vec::new();
    game.world().extract(1.0, &mut extracted);
    let entities = extracted
        .iter()
        .map(|e| EntityState {
            index: e.id.index(),
            generation: e.id.generation(),
            kind: match e.kind {
                BodyKind::Character => "character",
                BodyKind::Block => "block",
                BodyKind::Sensor => "sensor",
            },
            tags: e.tags,
            model: Model::VARIANTS[e.model.resolve(e.kind) as usize],
            position: e.position.to_array(),
            half_extents: e.half_extents.to_array(),
        })
        .collect();
    Ok(GameRun {
        ticks: frames,
        seed: scenario.tape.seed,
        build: game.build(),
        snapshot_hash: game.snapshot().hash(),
        entities,
        failures,
        faults,
    })
}

/// Whether two JSON texts say the same: equal outside whitespace between
/// tokens, or equal numbers.
fn same_json(a: &str, b: &str) -> bool {
    if let (Ok(x), Ok(y)) = (a.parse::<f64>(), b.parse::<f64>()) {
        return x == y;
    }
    compact(a) == compact(b)
}

fn compact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let (mut quoted, mut escaped) = (false, false);
    for c in text.chars() {
        if quoted {
            out.push(c);
            match c {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => quoted = false,
                _ => {}
            }
        } else if !c.is_whitespace() {
            quoted = c == '"';
            out.push(c);
        }
    }
    out
}
