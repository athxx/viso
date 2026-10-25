//! The game a mounted view runs: a view whose module declares systems starts
//! a [`GameSession`] on its root — its own interpreter over the view's
//! module, linked against the standard natives with the package's
//! capabilities — which the window's frame loop advances as a
//! [`FrameClient`]: every frame runs the ticks the clock owes and then every
//! `FrameUpdate`, the keys no UI node takes reach the game, and the session
//! ends with the root.
//!
//! A reload swaps the session's build at a tick boundary
//! ([`GameSession::reload`]), the World, entities and compatible states
//! kept, or rebuilds the World in a shadow game for a World Rebuild; the
//! audio device swaps its audio systems with it. A game whose module has
//! audio systems starts them on the device the app installed
//! ([`install_game_audio`]), also when a reload adds them.
//!
//! The views of the root read the game through queries: the
//! session runs each query a view observes once a frame, after the frame's
//! ticks and hooks, keeps its value, and raises the query's revision cells as
//! the frame settles only when the value changed. A view pauses the game
//! through the [`GameControl`] it is lent, taken at the next frame.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use viso_behavior::aot::NativeCode;
use viso_behavior::game::{
    AudioLink, Committed, GameControl, GameError, InputTape, Key as GameKey, Persist, PreparedSwap,
    Rebuild, Scheduler, SystemFault,
};
use viso_behavior::native::Natives;
use viso_behavior::{Budget, Module, Value, Vm};
use viso_schema::view::GameQuery;
use viso_ui::frame_client::SceneFill;
use viso_ui::input::Key;
use viso_ui::{FrameClient, NodeId, NodeStore, StateId, StateStore, StateValue};

/// The device a session's audio systems render on, provided by the host.
pub trait GameAudioDevice {
    /// Builds `module`'s audio systems and hands them to the device in
    /// place of the running ones, with no gap in the output; the game's
    /// link stays valid.
    ///
    /// # Errors
    ///
    /// When `module`'s audio systems do not build; the running ones keep
    /// playing.
    fn reload(&mut self, module: &Module, natives: &Natives) -> Result<(), String>;

    /// Drops what the device handed back since, here rather than on its
    /// thread: called once a frame.
    fn collect(&mut self);
}

/// Starts the audio systems of a game's `module`, linked against `natives`
/// and running `native` when given, on the app's device: the device and the
/// link the game reaches them through, `None` when the module has none or the
/// target no device.
pub type GameAudioStart =
    fn(&Module, &Natives, Option<&'static NativeCode>) -> Result<Option<StartedAudio>, String>;

/// A game's audio started on a device: the device, and the link the game
/// reaches its audio systems through.
pub type StartedAudio = (Box<dyn GameAudioDevice>, AudioLink);

thread_local! {
    static AUDIO: std::cell::Cell<Option<GameAudioStart>> = const { std::cell::Cell::new(None) };
    static HISTORY: std::cell::Cell<Option<u32>> = const { std::cell::Cell::new(None) };
}

/// Installs `start` as how the games of this thread's views start their
/// audio; `None` leaves them silent.
pub fn install_game_audio(start: Option<GameAudioStart>) {
    AUDIO.with(|installed| installed.set(start));
}

/// Makes the games this thread's views start from now on keep the last
/// `seconds` of play to replay ([`Scheduler::replay_from`]); `None` keeps
/// none, as off a dev session.
pub fn keep_game_history(seconds: Option<u32>) {
    HISTORY.with(|kept| kept.set(seconds));
}

/// Why a session's build was not swapped.
#[derive(Debug)]
pub enum GameReloadError {
    /// The candidate module does not link.
    Link(String),
    /// A system faulted building the candidate's instances, its start or its
    /// smoke tick, or the physics tier does not hold; the last good runs on.
    Game(GameError),
}

impl std::fmt::Display for GameReloadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GameReloadError::Link(e) => write!(f, "the systems do not link: {e}"),
            GameReloadError::Game(GameError::Fault(fault)) => write!(
                f,
                "{} faulted at tick {}: {}",
                fault.system, fault.tick, fault.fault
            ),
            GameReloadError::Game(GameError::Physics(tier)) => write!(f, "{tier:?}"),
        }
    }
}

/// `fault`, raised by a system of `module`, as one line naming the system.
pub fn describe_fault(module: &Module, fault: &SystemFault) -> String {
    let name = module
        .systems()
        .get(fault.system)
        .map_or("a system", |system| &*module.layout(system.component).name);
    format!("`{name}` faulted at tick {}: {}", fault.tick, fault.fault)
}

/// What a swap kept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GameSwap {
    /// The tick the new build runs first.
    pub tick: u64,
    /// The states carried across, by stable identity and schema.
    pub carried: u32,
    /// The states that started over at their initializers.
    pub reset: u32,
    /// The characters a World Rebuild carried by stable entity key.
    pub characters: u32,
}

/// A running game.
pub struct GameSession {
    scheduler: Scheduler,
    /// The module the running build was made from.
    module: Rc<Module>,
    /// The libraries the game's builds link against.
    natives: Arc<Natives>,
    /// The module compiled to Rust the running build runs: a release build's;
    /// a reloaded build is interpreted.
    native: Option<&'static NativeCode>,
    audio: Option<Box<dyn GameAudioDevice>>,
    /// The system and code of the fault last reported, so a system faulting
    /// every tick is reported once until it faults otherwise or the build
    /// swaps.
    reported: Option<(usize, &'static str)>,
    /// The audio faults the link counted when last reported.
    audio_faults: u64,
    /// The queries the views of the root observe.
    observed: Vec<Observed>,
    /// The pause the views ask for.
    control: GameControl,
}

/// A query the views of a session observe, and what it last returned.
struct Observed {
    query: GameQuery,
    /// The system and the chunk a system's query runs, `None` for a standard
    /// query or one the running build does not declare.
    target: Option<(usize, u32)>,
    value: Value,
    /// Raised each time the value changes.
    revision: i32,
    /// Whether the value changed since the cells were last raised.
    moved: bool,
    /// Whether its last run faulted, so a query faulting every frame is
    /// reported once.
    faulted: bool,
    /// The revision cell of each view read of it.
    cells: Vec<StateId>,
}

impl std::fmt::Debug for GameSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GameSession")
            .field("tick", &self.scheduler.clock().tick())
            .field("audio", &self.audio.is_some())
            .finish_non_exhaustive()
    }
}

/// A VM over `module` linked against `natives` with the package's
/// capabilities and running `native` when given, persisting through the
/// installed store.
fn link(
    module: &Rc<Module>,
    natives: &Natives,
    native: Option<&'static NativeCode>,
) -> Result<Vm, String> {
    let grant: Vec<&str> = module.capabilities().iter().map(|c| &**c).collect();
    let mut vm = Vm::new(Rc::clone(module), Budget::default());
    vm.link(natives, &grant).map_err(|e| e.to_string())?;
    if let Some(native) = native {
        vm.install_native(native).map_err(|e| e.to_string())?;
    }
    if let Some(shared) = crate::persistence::installed() {
        vm.services_mut().insert(Persist::new(shared));
    }
    Ok(vm)
}

impl GameSession {
    /// Starts the game `module`'s systems make, running `native`, the module
    /// compiled to Rust, when given.
    ///
    /// # Errors
    ///
    /// When the module does not link or `native` was lowered from another
    /// build, or a system faults creating its instance or in the start.
    pub fn start(
        module: Rc<Module>,
        native: Option<&'static NativeCode>,
    ) -> Result<GameSession, GameReloadError> {
        GameSession::start_in(module, Natives::standard(), native)
    }

    /// [`start`](Self::start) linked against `natives`, the standard
    /// libraries and those of the native packages the module calls.
    ///
    /// # Errors
    ///
    /// As [`start`](Self::start).
    pub fn start_in(
        module: Rc<Module>,
        natives: Arc<Natives>,
        native: Option<&'static NativeCode>,
    ) -> Result<GameSession, GameReloadError> {
        let vm = link(&module, &natives, native).map_err(GameReloadError::Link)?;
        let mut scheduler =
            Scheduler::new(vm).map_err(|fault| GameReloadError::Game(GameError::Fault(fault)))?;
        // A replayed tape's patches link as the game's own build did.
        let linking = Arc::clone(&natives);
        scheduler.set_linker(Rc::new(move |module: Rc<Module>| {
            link(&module, &linking, None)
        }));
        let mut session = GameSession {
            scheduler,
            module,
            natives,
            native,
            audio: None,
            reported: None,
            audio_faults: 0,
            observed: Vec::new(),
            control: GameControl::new(),
        };
        if let Some(seconds) = HISTORY.with(std::cell::Cell::get) {
            session.scheduler.keep_history(seconds);
        }
        session.start_audio();
        Ok(session)
    }

    /// The running game.
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    /// The running game, to drive or inspect it.
    pub fn scheduler_mut(&mut self) -> &mut Scheduler {
        &mut self.scheduler
    }

    /// The module the running build was made from.
    pub fn module(&self) -> &Rc<Module> {
        &self.module
    }

    /// The value of state `state` of system `system`, as the running build
    /// holds it: what `viso game peek <session> System.state` reads.
    pub fn state(&self, system: &str, state: &str) -> Option<Value> {
        let module = self.scheduler.vm().module();
        let (index, layout) = module.systems().iter().enumerate().find_map(|(i, s)| {
            let layout = module.layout(s.component);
            (&*layout.name == system).then_some((i, layout))
        })?;
        let slot = layout.state(state)?;
        self.scheduler.instance(index).states().get(slot).cloned()
    }

    /// The control a view pauses the game through.
    pub fn control(&self) -> GameControl {
        self.control.clone()
    }

    /// Observes `query` for a view read whose revision cell is `cell`: the
    /// session runs it from now on once a frame, and raises `cell` each time
    /// its value changes. The query runs at once the first time.
    pub fn observe(&mut self, query: &GameQuery, cell: StateId) {
        let at = match self.observed.iter().position(|o| o.query == *query) {
            Some(at) => at,
            None => {
                let target = self.target(query);
                if target.is_none() && matches!(query, GameQuery::System { .. }) {
                    queue_fault(format!("the game has no query {}", query_name(query)));
                }
                self.observed.push(Observed {
                    query: query.clone(),
                    target,
                    value: Value::Nil,
                    revision: 0,
                    moved: false,
                    faulted: false,
                    cells: Vec::new(),
                });
                let at = self.observed.len() - 1;
                self.run_query(at);
                self.observed[at].moved = false;
                at
            }
        };
        let cells = &mut self.observed[at].cells;
        if !cells.contains(&cell) {
            cells.push(cell);
        }
    }

    /// What `query` last returned, `None` when no view observes it.
    pub fn query_value(&self, query: &GameQuery) -> Option<&Value> {
        self.observed
            .iter()
            .find(|o| o.query == *query)
            .map(|o| &o.value)
    }

    /// The system and chunk of `query` in the running build.
    fn target(&self, query: &GameQuery) -> Option<(usize, u32)> {
        let GameQuery::System { system, function } = query else {
            return None;
        };
        let module = self.scheduler.vm().module();
        module.systems().iter().enumerate().find_map(|(at, s)| {
            let layout = module.layout(s.component);
            (*layout.name == **system)
                .then(|| layout.member(function))
                .flatten()
                .map(|chunk| (at, chunk))
        })
    }

    /// Runs observed query `at`, keeping its value and noting a change; a
    /// fault keeps the last value.
    fn run_query(&mut self, at: usize) {
        let observed = &self.observed[at];
        let clock = self.scheduler.clock();
        let value = match (&observed.query, observed.target) {
            (GameQuery::Paused, _) => Ok(Value::bool(clock.paused())),
            (GameQuery::Tick, _) => Ok(Value::Int(clock.tick() as i64)),
            (GameQuery::System { .. }, Some((system, chunk))) => {
                self.scheduler.query(system, chunk)
            }
            (GameQuery::System { .. }, None) => return,
        };
        let observed = &mut self.observed[at];
        match value {
            Ok(value) => {
                observed.faulted = false;
                if value != observed.value {
                    observed.value = value;
                    observed.revision = observed.revision.wrapping_add(1);
                    observed.moved = true;
                }
            }
            Err(fault) => {
                if !std::mem::replace(&mut observed.faulted, true) {
                    queue_fault(format!(
                        "the query {} faulted: {fault}",
                        query_name(&observed.query)
                    ));
                }
            }
        }
    }

    /// Runs every observed query, after a frame.
    fn refresh(&mut self) {
        for at in 0..self.observed.len() {
            self.run_query(at);
        }
    }

    /// Raises the cells of each query whose value changed, dropping the cells
    /// of reads gone.
    fn raise(&mut self, states: &mut StateStore) {
        for observed in &mut self.observed {
            if !std::mem::take(&mut observed.moved) {
                continue;
            }
            let revision = StateValue::Int(observed.revision);
            observed.cells.retain(|&cell| states.set(cell, revision));
        }
        self.observed.retain(|o| !o.cells.is_empty());
    }

    /// Points each observed query at the running build's code, as a swap
    /// that changed it needs: a query the build no longer declares keeps its
    /// last value and is reported once.
    fn retarget(&mut self) {
        for at in 0..self.observed.len() {
            let target = self.target(&self.observed[at].query);
            let observed = &mut self.observed[at];
            if target.is_none() && observed.target.is_some() {
                queue_fault(format!(
                    "the game no longer has the query {}",
                    query_name(&observed.query)
                ));
            }
            observed.target = target;
        }
    }

    /// Restarts the World — fresh instances, the start run again on the
    /// game's seed, the clock at tick 0 — and records the input from there,
    /// so the tape replays the run: what `viso game record` asks.
    ///
    /// # Errors
    ///
    /// The fault of a system that could not be created or of the start: the
    /// game runs on as it was, unrecorded.
    pub fn record(&mut self) -> Result<(), SystemFault> {
        self.scheduler.rebuild_world(Rebuild::Fresh)?;
        self.scheduler.record();
        self.reported = None;
        Ok(())
    }

    /// Stops recording and returns the tape through the last tick run;
    /// `None` when not recording.
    pub fn stop_recording(&mut self) -> Option<InputTape> {
        self.scheduler.stop_recording()
    }

    /// Starts the module's audio systems on the installed device, if it has
    /// any and the app installed one; why they did not start is queued for
    /// [`take_game_faults`].
    fn start_audio(&mut self) {
        let Some(start) = AUDIO.with(std::cell::Cell::get) else {
            return;
        };
        match start(&self.module, &self.natives, self.native) {
            Ok(Some((device, link))) => self.attach_audio(link, device),
            Ok(None) => {}
            Err(why) => queue_fault(format!("the game's audio did not start: {why}")),
        }
    }

    /// Lets the systems talk to the audio systems `device` renders through
    /// `link`.
    pub fn attach_audio(&mut self, link: AudioLink, device: Box<dyn GameAudioDevice>) {
        self.scheduler.attach_audio(link);
        self.audio = Some(device);
    }

    /// Whether an audio device is attached.
    pub fn has_audio(&self) -> bool {
        self.audio.is_some()
    }

    /// The faults the systems raised since the frame loop last reported
    /// them to [`take_game_faults`].
    pub fn take_faults(&mut self) -> Vec<SystemFault> {
        self.scheduler.take_faults()
    }

    /// Queues the faults raised since the last frame for
    /// [`take_game_faults`], a fault repeating the last one reported left
    /// out.
    fn report_faults(&mut self) {
        if let Some(audio) = &mut self.audio {
            audio.collect();
        }
        if let Some(link) = self.scheduler.audio() {
            let status = link.status();
            if status.faults() != self.audio_faults {
                self.audio_faults = status.faults();
                if let Some(fault) = status.take_fault() {
                    let module = self.scheduler.vm().module();
                    let name = module
                        .systems()
                        .get(fault.system)
                        .map_or("an audio system", |s| &*module.layout(s.component).name);
                    queue_fault(format!(
                        "`{name}` faulted on the audio thread: {}",
                        fault.fault
                    ));
                }
            }
        }
        let faults = self.scheduler.take_faults();
        for fault in &faults {
            let key = (fault.system, fault.code);
            if self.reported != Some(key) {
                self.reported = Some(key);
                queue_fault(describe_fault(self.scheduler.vm().module(), fault));
            }
        }
    }

    /// Links `module` against `natives` into a VM for
    /// [`reload`](Self::reload), touching nothing live: what a reload stages
    /// when its patch arrives.
    ///
    /// # Errors
    ///
    /// When the module does not link.
    pub fn stage(module: &Rc<Module>, natives: &Natives) -> Result<Vm, String> {
        link(module, natives, None)
    }

    /// The libraries the game's builds link against.
    pub fn natives(&self) -> &Arc<Natives> {
        &self.natives
    }

    /// Swaps in `vm`, staged from `module`, at the tick boundary the session
    /// is at: a Logic or Presentation reload keeps the World, entities,
    /// clock and compatible states; `rebuild` runs a World Rebuild in a
    /// shadow game, carrying characters as `keep` says. On any failure the
    /// running game is unchanged. The audio systems swap with the build.
    ///
    /// # Errors
    ///
    /// The candidate's fault, or its physics tier.
    pub fn reload(
        &mut self,
        module: Rc<Module>,
        vm: Vm,
        rebuild: Option<Rebuild>,
    ) -> Result<GameSwap, GameReloadError> {
        let prepared = self.prepare(vm, rebuild)?;
        Ok(self.commit(module, prepared, 0))
    }

    /// The first half of [`reload`](Self::reload): proves the swap, the
    /// session not advancing until [`commit`](Self::commit) or
    /// [`abort`](Self::abort) settles it, so a revision swapping several
    /// sessions swaps every one or none.
    ///
    /// # Errors
    ///
    /// As [`reload`](Self::reload): the running game is unchanged.
    pub fn prepare(
        &mut self,
        vm: Vm,
        rebuild: Option<Rebuild>,
    ) -> Result<PreparedSwap, GameReloadError> {
        match rebuild {
            Some(keep) => self.scheduler.prepare_rebuild(vm, keep),
            None => self.scheduler.prepare_reload(vm),
        }
        .map_err(GameReloadError::Game)
    }

    /// Commits the swap [`prepare`](Self::prepare) proved from `module` as
    /// dev revision `revision` (0 off a dev session), the audio systems
    /// swapping with it; a recording tape records it.
    pub fn commit(
        &mut self,
        module: Rc<Module>,
        prepared: PreparedSwap,
        revision: u64,
    ) -> GameSwap {
        let swap = match self.scheduler.commit_swap(prepared, revision) {
            Committed::Rebuilt { characters } => GameSwap {
                tick: self.scheduler.clock().tick(),
                characters,
                ..GameSwap::default()
            },
            Committed::Reloaded(restored) => GameSwap {
                tick: self.scheduler.clock().tick(),
                carried: restored.states + restored.locals,
                reset: restored.mismatched + restored.missing,
                characters: 0,
            },
        };
        self.module = module;
        self.native = None;
        self.reported = None;
        self.retarget();
        match &mut self.audio {
            // The new systems run; audio that does not build keeps the old
            // sound rather than failing the swap.
            Some(audio) => {
                if let Err(why) = audio.reload(&self.module, &self.natives) {
                    queue_fault(format!("the new audio systems did not build: {why}"));
                }
            }
            // A reload that adds audio systems starts them.
            None => self.start_audio(),
        }
        swap
    }

    /// Puts back the build [`prepare`](Self::prepare) displaced.
    pub fn abort(&mut self, prepared: PreparedSwap) {
        self.scheduler.abort_swap(prepared);
    }
}

/// The frame client a mount registers: the shared session, so the dev
/// runtime reaches it typed through its owner, and the last frame's wall
/// delta its viewports' rigs ease by.
struct GameClient(Rc<RefCell<GameSession>>, f32);

impl FrameClient for GameClient {
    fn frame(&mut self, dt: f64) {
        let mut session = self.0.borrow_mut();
        if let Some(paused) = session.control.take() {
            session.scheduler.clock_mut().set_paused(paused);
        }
        session.scheduler.frame(dt);
        session.report_faults();
        session.refresh();
        self.1 = dt as f32;
    }

    fn fill_scenes(&mut self, owner: NodeId, scenes: &mut SceneFill<'_>) {
        let session = self.0.borrow();
        let dt = self.1;
        scenes.under(owner, |target| {
            crate::world_scene::fill_viewport(target, &session.scheduler, dt)
        });
    }

    fn settle(&mut self, states: &mut StateStore) {
        self.0.borrow_mut().raise(states);
    }

    fn key(&mut self, key: Key, pressed: bool) -> bool {
        let Some(key) = game_key(key) else {
            return false;
        };
        self.0.borrow_mut().scheduler.key(key, pressed);
        true
    }

    fn release_input(&mut self) {
        self.0.borrow_mut().scheduler.release_input();
    }

    fn wants_frame(&self) -> bool {
        !self.0.borrow().scheduler.clock().paused()
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// The game key of a keyboard key, by its position on a US layout.
fn game_key(key: Key) -> Option<GameKey> {
    use GameKey as G;
    Some(match key {
        Key::A => G::A,
        Key::B => G::B,
        Key::C => G::C,
        Key::D => G::D,
        Key::E => G::E,
        Key::F => G::F,
        Key::G => G::G,
        Key::H => G::H,
        Key::I => G::I,
        Key::J => G::J,
        Key::K => G::K,
        Key::L => G::L,
        Key::M => G::M,
        Key::N => G::N,
        Key::O => G::O,
        Key::P => G::P,
        Key::Q => G::Q,
        Key::R => G::R,
        Key::S => G::S,
        Key::T => G::T,
        Key::U => G::U,
        Key::V => G::V,
        Key::W => G::W,
        Key::X => G::X,
        Key::Y => G::Y,
        Key::Z => G::Z,
        Key::Digit0 => G::Digit0,
        Key::Digit1 => G::Digit1,
        Key::Digit2 => G::Digit2,
        Key::Digit3 => G::Digit3,
        Key::Digit4 => G::Digit4,
        Key::Digit5 => G::Digit5,
        Key::Digit6 => G::Digit6,
        Key::Digit7 => G::Digit7,
        Key::Digit8 => G::Digit8,
        Key::Digit9 => G::Digit9,
        Key::Space => G::Space,
        Key::Enter => G::Enter,
        Key::Escape => G::Escape,
        Key::Tab => G::Tab,
        Key::Backspace => G::Backspace,
        Key::Up => G::ArrowUp,
        Key::Down => G::ArrowDown,
        Key::Left => G::ArrowLeft,
        Key::Right => G::ArrowRight,
        Key::ShiftLeft => G::ShiftLeft,
        Key::ShiftRight => G::ShiftRight,
        Key::ControlLeft => G::ControlLeft,
        Key::ControlRight => G::ControlRight,
        Key::AltLeft => G::AltLeft,
        Key::AltRight => G::AltRight,
        _ => return None,
    })
}

/// How `query` is spelled in a view.
fn query_name(query: &GameQuery) -> String {
    match query {
        GameQuery::Paused => "`game.is_paused()`".into(),
        GameQuery::Tick => "`game.tick()`".into(),
        GameQuery::System { system, function } => format!("`{system}.{function}`"),
    }
}

/// The session `owner` runs, if it runs one.
pub fn game_session(store: &NodeStore, owner: NodeId) -> Option<Rc<RefCell<GameSession>>> {
    let mut client = store.frame_client(owner)?.try_borrow_mut().ok()?;
    let client = client.as_any_mut().downcast_mut::<GameClient>()?;
    Some(Rc::clone(&client.0))
}

/// The most fault lines queued between two frames; the rest are counted.
const MAX_QUEUED_FAULTS: usize = 64;

#[derive(Default)]
struct FaultQueue {
    lines: Vec<String>,
    dropped: u32,
}

thread_local! {
    static FAULTS: RefCell<FaultQueue> = RefCell::default();
}

fn queue_fault(line: String) {
    FAULTS.with(|queue| {
        let mut queue = queue.borrow_mut();
        if queue.lines.len() < MAX_QUEUED_FAULTS {
            queue.lines.push(line);
        } else {
            queue.dropped += 1;
        }
    });
}

/// The faults the games of this thread raised since the last call, one line
/// each naming the system — a game that did not start, and a system that
/// faulted while it ran, reported once while it keeps faulting the same way
/// — for the frame loop to log.
pub fn take_game_faults() -> Vec<String> {
    FAULTS.with(|queue| {
        let mut queue = queue.borrow_mut();
        let mut lines = std::mem::take(&mut queue.lines);
        let dropped = std::mem::take(&mut queue.dropped);
        if dropped > 0 {
            lines.push(format!("{dropped} more game faults were not reported"));
        }
        lines
    })
}

/// [`mount_game`] for a mount that has no caller to answer: a start that
/// faults is queued for [`take_game_faults`].
pub(crate) fn mount_game_queued(
    store: &mut NodeStore,
    module: &Rc<Module>,
    natives: &Arc<Natives>,
    native: Option<&'static NativeCode>,
    owner: NodeId,
) {
    if let Err(error) = mount_game(store, module, natives, native, owner) {
        let why = match &error {
            GameReloadError::Game(GameError::Fault(fault)) => describe_fault(module, fault),
            error => error.to_string(),
        };
        queue_fault(format!("the game did not start: {why}"));
    }
}

/// Starts the game `host`'s module makes on `root` unless `root` runs one
/// already, and lets the view read it: what a view reading the game does
/// once its slots are linked, before its regions and values first evaluate,
/// so they read the game's values from the first frame.
pub fn mount_view_game(store: &mut NodeStore, host: &Rc<RefCell<crate::ViewHost>>, root: NodeId) {
    let Ok(mut view) = host.try_borrow_mut() else {
        return;
    };
    if !view.reads_game() {
        return;
    }
    mount_game_queued(store, view.module(), view.natives(), view.native(), root);
    if let Some(session) = game_session(store, root) {
        view.attach_game(&session);
    }
}

/// Links the game slots of an embedded view's host once its tree is built,
/// then starts the game on `root` and lets the view read it: each read is a
/// slot and its query, a tag (`0` paused, `1` tick, `2` a system's) with the
/// system's and function's names.
#[doc(hidden)]
pub fn __link_game(
    cx: &mut viso_ui::BuildCx<'_>,
    host: &Rc<RefCell<crate::ViewHost>>,
    root: NodeId,
    reads: &[(usize, u8, &str, &str)],
) {
    cx.structure(|cx| {
        if let Ok(mut view) = host.try_borrow_mut() {
            for &(slot, tag, system, function) in reads {
                let query = match tag {
                    0 => GameQuery::Paused,
                    1 => GameQuery::Tick,
                    _ => GameQuery::System {
                        system: system.into(),
                        function: function.into(),
                    },
                };
                view.link_game(slot, query, cx.states);
            }
        }
        mount_view_game(cx.store, host, root);
    });
}

/// Starts the game `module`'s systems make on `owner`, linked against
/// `natives` and running `native` when given, unless `owner` runs one already, or stops the one it runs when
/// `module` declares no system. A start that faults leaves `owner` without a
/// game and returns why.
pub fn mount_game(
    store: &mut NodeStore,
    module: &Rc<Module>,
    natives: &Arc<Natives>,
    native: Option<&'static NativeCode>,
    owner: NodeId,
) -> Result<(), GameReloadError> {
    if module.systems().is_empty() {
        if store.frame_client(owner).is_some() {
            store.remove_frame_client(owner);
        }
        return Ok(());
    }
    if store.frame_client(owner).is_some() {
        return Ok(());
    }
    let session = GameSession::start_in(Rc::clone(module), Arc::clone(natives), native)?;
    let session = Rc::new(RefCell::new(session));
    store.add_frame_client(owner, Rc::new(RefCell::new(GameClient(session, 0.0))));
    Ok(())
}
