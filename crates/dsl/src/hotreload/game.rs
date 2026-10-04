//! The reload tier of a game build (Viso_DSL_1.0.md section 110): which
//! layer a running game swaps a new build in at, chosen by comparing the
//! last-good build's Behavior IR with the candidate's by stable identity.
//!
//! Functions pair by declaration identity and compare instruction by
//! instruction, a call by its callee's identity, a closure by its body and a
//! native call by the path and signature it was checked against, so moving
//! code or editing elsewhere changes nothing. A hook's behavior changed when
//! a function it reaches through calls and closures did, or when the action
//! bound to it did; each hook's phase gives the change its tier:
//!
//! | Change                                               | Tier             |
//! | ---------------------------------------------------- | ---------------- |
//! | the code a `FrameUpdate` reaches, a `@local` state   | Presentation     |
//! | the code a fixed or collision hook reaches, the       | Logic            |
//! | `InputMap`, the tick rate, the system order           |                  |
//! | a Simulation state added, removed, retyped or with a | Logic with State |
//! | new initializer                                      | Migration        |
//! | the code a start hook reaches                        | World Rebuild    |
//!
//! The highest tier any change needs is the build's. Shader edits reload
//! through their own pipeline, outside this comparison. [`swap`] applies a
//! candidate to a running game at its tier.

use std::collections::HashMap;
use std::rc::Rc;

use viso_behavior::Vm;
use viso_behavior::game::quick::QUICK_START;
use viso_behavior::game::{FRAME_UPDATE, Rebuild, Restored, STARTUP, Scheduler, SystemFault};
use viso_behavior::native::{NativeId, Natives};

use crate::behavior::ir::{FuncId, Function, FunctionKind, Inst, Program, SystemLayout};
use crate::diag::{Diagnostic, Related};
use crate::resolve::SymbolId;
use crate::syntax::TextRange;

/// The layer a running game swaps a build in at, from the least disruptive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ReloadTier {
    /// Nothing the game runs changed.
    Unchanged,
    /// Only Presentation code or `@local` states: swapped at a frame
    /// boundary, the Simulation untouched.
    Presentation,
    /// Simulation code, the input map or the fixed step: swapped at a tick
    /// boundary over the same world, states, tick and random state.
    Logic,
    /// [`Logic`](Self::Logic), and a Simulation state was added, removed,
    /// retyped or given a new initializer: states carry by identity and
    /// schema, the rest take their initializers.
    LogicMigration,
    /// The start changed: the world is built again from tick 0.
    WorldRebuild,
}

impl ReloadTier {
    /// The spelling of diagnostics and the `dev` event.
    pub fn as_str(self) -> &'static str {
        match self {
            ReloadTier::Unchanged => "unchanged",
            ReloadTier::Presentation => "presentation-only",
            ReloadTier::Logic => "logic-only",
            ReloadTier::LogicMigration => "logic+state-migration",
            ReloadTier::WorldRebuild => "world-rebuild",
        }
    }
}

/// What changed between two builds of a game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GameChangeKind {
    /// Code a start hook reaches, or the hook's binding.
    Start,
    /// Code a fixed or collision hook reaches, or the hook's binding.
    Simulation,
    /// Code a `FrameUpdate` reaches, or the hook's binding.
    Presentation,
    /// A Simulation state appeared.
    StateAdded,
    /// A Simulation state disappeared.
    StateRemoved,
    /// A Simulation state's type changed: its value does not carry.
    StateRetyped,
    /// A Simulation state's initializer, or code it reaches, changed.
    StateInitializer,
    /// A `@local` state appeared, disappeared, or changed type or initializer.
    LocalState,
    /// The input map.
    InputMap,
    /// The fixed step's tick rate.
    TickRate,
    /// The order the systems run in.
    SystemOrder,
}

impl GameChangeKind {
    /// The tier a change of this kind needs.
    pub fn tier(self) -> ReloadTier {
        match self {
            GameChangeKind::Presentation | GameChangeKind::LocalState => ReloadTier::Presentation,
            GameChangeKind::Simulation
            | GameChangeKind::InputMap
            | GameChangeKind::TickRate
            | GameChangeKind::SystemOrder => ReloadTier::Logic,
            GameChangeKind::StateAdded
            | GameChangeKind::StateRemoved
            | GameChangeKind::StateRetyped
            | GameChangeKind::StateInitializer => ReloadTier::LogicMigration,
            GameChangeKind::Start => ReloadTier::WorldRebuild,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            GameChangeKind::Start => "start code changed",
            GameChangeKind::Simulation => "simulation code changed",
            GameChangeKind::Presentation => "presentation code changed",
            GameChangeKind::StateAdded => "simulation state added",
            GameChangeKind::StateRemoved => "simulation state removed",
            GameChangeKind::StateRetyped => "simulation state retyped",
            GameChangeKind::StateInitializer => "simulation state initializer changed",
            GameChangeKind::LocalState => "`@local` state changed",
            GameChangeKind::InputMap => "input map changed",
            GameChangeKind::TickRate => "tick rate changed",
            GameChangeKind::SystemOrder => "system order changed",
        }
    }
}

/// One change, and where the candidate holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameChange {
    pub kind: GameChangeKind,
    /// The function, hook (`System.hook`) or state (`System.state`) changed.
    pub name: String,
    /// The candidate's module index and range, when it still has the code.
    pub at: Option<(usize, TextRange)>,
}

/// A candidate's reload tier and the changes that need it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameReload {
    /// The highest tier of `changes`; [`ReloadTier::Unchanged`] without any.
    pub tier: ReloadTier,
    /// Every change, each once, in system order, highest tier first.
    pub changes: Vec<GameChange>,
}

/// The code a reload diagnostic carries (Viso_DSL_1.0.md appendix C).
pub const RELOAD_TIER: &str = "E5103";

impl GameReload {
    /// The note a reload reports its tier with: its primary span the first
    /// change of the deciding tier, every other change a related span of its
    /// own. `None` when nothing changed.
    pub fn diagnostic(&self) -> Option<Diagnostic> {
        let first = self.changes.first()?;
        let range = |change: &GameChange| change.at.map_or(TextRange::empty(0.into()), |a| a.1);
        let mut diagnostic = Diagnostic::note(
            RELOAD_TIER,
            range(first),
            format!(
                "game reload: {} ({}: `{}`)",
                self.tier.as_str(),
                first.kind.describe(),
                first.name
            ),
        );
        for change in &self.changes[1..] {
            let label = format!("{}: `{}`", change.kind.describe(), change.name);
            match change.at {
                Some((_, at)) => diagnostic.related.push(Related::new(at, label)),
                None => diagnostic.notes.push(label),
            }
        }
        Some(diagnostic)
    }
}

/// The reload tier of `candidate` over the running `last_good`.
pub fn classify(last_good: &Program, candidate: &Program) -> GameReload {
    let mut cx = Compare {
        old: Side::new(last_good),
        new: Side::new(candidate),
        changed: vec![None; candidate.functions.len()],
        changes: Vec::new(),
    };
    if last_good.input != candidate.input {
        cx.change(GameChangeKind::InputMap, "InputMap".to_owned(), None);
    }
    if last_good.tick_rate != candidate.tick_rate {
        cx.change(GameChangeKind::TickRate, "tick_rate".to_owned(), None);
    }
    let kept = |a: &Program, b: &Program| -> Vec<SymbolId> {
        a.systems
            .iter()
            .map(|s| s.symbol)
            .filter(|&id| b.systems.iter().any(|s| s.symbol == id))
            .collect()
    };
    if kept(last_good, candidate) != kept(candidate, last_good) {
        cx.change(GameChangeKind::SystemOrder, "systems".to_owned(), None);
    }
    for system in &candidate.systems {
        let old = last_good.systems.iter().find(|s| s.symbol == system.symbol);
        cx.system(old, Some(system));
    }
    for system in &last_good.systems {
        if !candidate.systems.iter().any(|s| s.symbol == system.symbol) {
            cx.system(Some(system), None);
        }
    }
    let mut changes = cx.changes;
    changes.sort_by_key(|c| std::cmp::Reverse(c.kind.tier()));
    GameReload {
        tier: changes
            .first()
            .map_or(ReloadTier::Unchanged, |c| c.kind.tier()),
        changes,
    }
}

/// What [`swap`] did to the running game.
#[derive(Debug, Clone, PartialEq)]
pub struct Swapped {
    /// The tier and changes it swapped by.
    pub reload: GameReload,
    /// What a logic or presentation reload carried; `None` for a rebuild or
    /// an unchanged build.
    pub restored: Option<Restored>,
    /// The characters a World Rebuild carried by stable key.
    pub carried: u32,
}

/// Why [`swap`] left the running game as it was.
#[derive(Debug)]
pub enum SwapError {
    /// The candidate's code does not verify or link.
    Build(String),
    /// The candidate's systems could not be created, or its start or smoke
    /// tick faulted.
    Fault(SystemFault),
}

impl std::fmt::Display for SwapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SwapError::Build(message) => f.write_str(message),
            SwapError::Fault(fault) => write!(f, "{}", fault.fault),
        }
    }
}

impl std::error::Error for SwapError {}

/// Swaps `candidate` into `game`, which runs `last_good`, at the tier its
/// changes need: nothing for an unchanged build, a reload between frames
/// for a Presentation or Logic change (states carried by identity and
/// schema), and for a start change a World Rebuild in a shadow game that
/// keeps what `keep` says. The candidate links against `natives` with
/// `capabilities` and runs on the game's budget.
///
/// # Errors
///
/// When the candidate does not verify or link, or its systems, start or
/// smoke tick fault: `game` is left as it was.
pub fn swap(
    game: &mut Scheduler,
    last_good: &Program,
    candidate: &Program,
    natives: &Natives,
    capabilities: &[&str],
    keep: Rebuild,
) -> Result<Swapped, SwapError> {
    let reload = classify(last_good, candidate);
    if reload.tier == ReloadTier::Unchanged {
        return Ok(Swapped {
            reload,
            restored: None,
            carried: 0,
        });
    }
    let module = candidate
        .bytecode()
        .map_err(|error| SwapError::Build(format!("the candidate does not verify: {error}")))?;
    let mut vm = Vm::new(Rc::new(module), game.budget());
    vm.link(natives, capabilities)
        .map_err(|error| SwapError::Build(format!("the candidate does not link: {error}")))?;
    let (restored, carried) = if reload.tier == ReloadTier::WorldRebuild {
        (None, game.rebuild(vm, keep).map_err(SwapError::Fault)?)
    } else {
        (Some(game.reload(vm).map_err(SwapError::Fault)?), 0)
    };
    Ok(Swapped {
        reload,
        restored,
        carried,
    })
}

/// The phase a hook runs in: a start hook, `FrameUpdate`, or Simulation
/// for the fixed and collision hooks and any this runtime does not schedule.
fn phase(hook: NativeId) -> GameChangeKind {
    if hook == STARTUP || hook == QUICK_START {
        GameChangeKind::Start
    } else if hook == FRAME_UPDATE {
        GameChangeKind::Presentation
    } else {
        GameChangeKind::Simulation
    }
}

/// A function's identity across builds: its declaration, kind and name (a
/// record's field defaults share the record's declaration).
type Key<'p> = (SymbolId, FunctionKind, &'p str);

/// One build, its named functions by identity, and the component each
/// member function belongs to.
struct Side<'p> {
    program: &'p Program,
    by_key: HashMap<Key<'p>, FuncId>,
    owner: Vec<Option<u32>>,
}

impl<'p> Side<'p> {
    fn new(program: &'p Program) -> Self {
        let by_key = program
            .functions
            .iter()
            .enumerate()
            .filter_map(|(i, f)| Some((key(f)?, FuncId(i as u32))))
            .collect();
        let mut owner = vec![None; program.functions.len()];
        for (i, c) in program.components.iter().enumerate() {
            let members = c.members.iter().map(|m| Some(m.1));
            let inits = c.state_inits.iter().chain(&c.input_defaults).copied();
            for func in members.chain(inits).flatten() {
                owner[func.0 as usize] = Some(i as u32);
            }
        }
        Side {
            program,
            by_key,
            owner,
        }
    }

    fn function(&self, id: FuncId) -> &'p Function {
        self.program.function(id)
    }

    /// The name of state `slot` of component `owner`.
    fn state(&self, owner: Option<u32>, slot: u32) -> Option<&'p str> {
        let component = &self.program.components[owner? as usize];
        component.states.get(slot as usize).map(String::as_str)
    }
}

/// The component a function of each build reads the states of.
type Owners = (Option<u32>, Option<u32>);

fn key(function: &Function) -> Option<Key<'_>> {
    let symbol = function.symbol?;
    (function.kind != FunctionKind::Closure).then_some((symbol, function.kind, &function.name))
}

struct Compare<'p> {
    old: Side<'p>,
    new: Side<'p>,
    /// Whether each candidate function differs from its last-good
    /// counterpart, once compared.
    changed: Vec<Option<bool>>,
    changes: Vec<GameChange>,
}

impl<'p> Compare<'p> {
    fn change(&mut self, kind: GameChangeKind, name: String, at: Option<(usize, TextRange)>) {
        if !self
            .changes
            .iter()
            .any(|c| c.kind == kind && c.name == name)
        {
            self.changes.push(GameChange { kind, name, at });
        }
    }

    /// Compares a system of each build, either missing when the other build
    /// added or removed it.
    fn system(&mut self, old: Option<&'p SystemLayout>, new: Option<&'p SystemLayout>) {
        let component = |side: &Side<'p>, s: Option<&'p SystemLayout>| {
            s.map(|s| &side.program.components[s.component as usize])
        };
        let (old_layout, new_layout) = (component(&self.old, old), component(&self.new, new));
        let name = new_layout.or(old_layout).map_or("", |c| c.name.as_str());
        let hooks = |s: Option<&'p SystemLayout>| s.map_or(&[][..], |s| &s.hooks[..]);
        let (old_hooks, new_hooks) = (hooks(old), hooks(new));
        for &(hook, func) in new_hooks {
            let kind = phase(hook);
            let bound = old_hooks.iter().find(|h| h.0 == hook).map(|h| h.1);
            let same =
                bound.is_some_and(|b| key(self.old.function(b)) == key(self.new.function(func)));
            if !same {
                let label = format!("{name}.{}", hook_name(self.new.function(func)));
                let at = self.at(func);
                self.change(kind, label, at);
            }
            self.reach(func, kind);
        }
        for &(hook, func) in old_hooks {
            if !new_hooks.iter().any(|h| h.0 == hook) {
                let label = format!("{name}.{}", hook_name(self.old.function(func)));
                self.change(phase(hook), label, None);
            }
        }
        let states = |s: Option<&'p SystemLayout>, local: bool| {
            s.map_or(&[][..], |s| {
                if local {
                    &s.locals[..]
                } else {
                    &s.snapshot[..]
                }
            })
        };
        for local in [false, true] {
            let (old_states, new_states) = (states(old, local), states(new, local));
            for &(id, slot, schema) in new_states {
                let state = new_layout.map_or("", |c| c.states[slot as usize].as_str());
                let label = format!("{name}.{state}");
                let init = new_layout.and_then(|c| c.state_inits[slot as usize]);
                let at = init.and_then(|f| self.at(f));
                let Some(&(_, old_slot, old_schema)) = old_states.iter().find(|s| s.0 == id) else {
                    let kind = if local {
                        GameChangeKind::LocalState
                    } else {
                        GameChangeKind::StateAdded
                    };
                    self.change(kind, label, at);
                    continue;
                };
                let retyped = old_schema != schema;
                let old_init = old_layout.and_then(|c| c.state_inits[old_slot as usize]);
                let reinit = match (old_init, init) {
                    (Some(a), Some(b)) => {
                        key(self.old.function(a)) != key(self.new.function(b))
                            || self.reaches_change(b)
                    }
                    (None, None) => false,
                    _ => true,
                };
                let kind = match (local, retyped, reinit) {
                    (_, false, false) => continue,
                    (true, ..) => GameChangeKind::LocalState,
                    (false, true, _) => GameChangeKind::StateRetyped,
                    (false, false, true) => GameChangeKind::StateInitializer,
                };
                self.change(kind, label, at);
            }
            for &(id, slot, _) in old_states {
                if !new_states.iter().any(|s| s.0 == id) {
                    let state = old_layout.map_or("", |c| c.states[slot as usize].as_str());
                    let kind = if local {
                        GameChangeKind::LocalState
                    } else {
                        GameChangeKind::StateRemoved
                    };
                    self.change(kind, format!("{name}.{state}"), None);
                }
            }
        }
    }

    /// Reports, under `kind`, every changed named function `root` reaches.
    fn reach(&mut self, root: FuncId, kind: GameChangeKind) {
        for func in self.reachable(root) {
            if self.differs(func) {
                let at = self.at(func);
                self.change(kind, self.new.function(func).name.clone(), at);
            }
        }
    }

    /// Whether any named function `root` reaches changed.
    fn reaches_change(&mut self, root: FuncId) -> bool {
        self.reachable(root)
            .into_iter()
            .any(|func| self.differs(func))
    }

    /// Every candidate function `root` reaches through calls and closures,
    /// itself first.
    fn reachable(&self, root: FuncId) -> Vec<FuncId> {
        let mut seen = vec![root];
        let mut next = 0;
        while let Some(&func) = seen.get(next) {
            next += 1;
            let Ok(body) = &self.new.function(func).body else {
                continue;
            };
            for inst in &body.insts {
                if let Inst::Call { func, .. } | Inst::Closure { func, .. } = inst
                    && !seen.contains(func)
                {
                    seen.push(*func);
                }
            }
        }
        seen
    }

    /// Whether the candidate's named function `func` differs from the
    /// last-good function of its identity, or has none; a closure counts as
    /// part of the function creating it.
    fn differs(&mut self, func: FuncId) -> bool {
        if let Some(known) = self.changed[func.0 as usize] {
            return known;
        }
        let differs = match key(self.new.function(func)) {
            Some(key) => match self.old.by_key.get(&key) {
                Some(&old) => {
                    let owners = (
                        self.old.owner[old.0 as usize],
                        self.new.owner[func.0 as usize],
                    );
                    !self.same_function(old, func, owners)
                }
                None => true,
            },
            None => false,
        };
        self.changed[func.0 as usize] = Some(differs);
        differs
    }

    /// Whether two functions run the same code: calls compared by callee
    /// identity, closures by body, and state accesses by the name of the
    /// state of `owners` they access.
    fn same_function(&self, old: FuncId, new: FuncId, owners: Owners) -> bool {
        let (a, b) = (self.old.function(old), self.new.function(new));
        if a.kind != b.kind || a.params != b.params || a.captures != b.captures {
            return false;
        }
        match (&a.body, &b.body) {
            (Ok(x), Ok(y)) => {
                x.regs == y.regs
                    && x.insts.len() == y.insts.len()
                    && x.insts
                        .iter()
                        .zip(&y.insts)
                        .all(|(i, j)| self.same_inst(i, j, owners))
            }
            (Err(x), Err(y)) => x.reason == y.reason,
            _ => false,
        }
    }

    fn same_inst(&self, a: &Inst, b: &Inst, owners: Owners) -> bool {
        let same_state =
            |x: u32, y: u32| match (self.old.state(owners.0, x), self.new.state(owners.1, y)) {
                (Some(x), Some(y)) => x == y,
                _ => x == y,
            };
        match (a, b) {
            (
                Inst::LoadState { dst, slot },
                Inst::LoadState {
                    dst: dst2,
                    slot: slot2,
                },
            ) => dst == dst2 && same_state(*slot, *slot2),
            (
                Inst::StoreState { slot, src },
                Inst::StoreState {
                    slot: slot2,
                    src: src2,
                },
            ) => src == src2 && same_state(*slot, *slot2),
            (
                Inst::Call { dst, func, args },
                Inst::Call {
                    dst: dst2,
                    func: func2,
                    args: args2,
                },
            ) => {
                let (x, y) = (self.old.function(*func), self.new.function(*func2));
                dst == dst2
                    && args == args2
                    && match (key(x), key(y)) {
                        (Some(x), Some(y)) => x == y,
                        (None, None) => self.same_function(*func, *func2, owners),
                        _ => false,
                    }
            }
            (
                Inst::Closure {
                    dst,
                    func,
                    captures,
                },
                Inst::Closure {
                    dst: dst2,
                    func: func2,
                    captures: captures2,
                },
            ) => dst == dst2 && captures == captures2 && self.same_function(*func, *func2, owners),
            (
                Inst::Native { dst, import, args },
                Inst::Native {
                    dst: dst2,
                    import: import2,
                    args: args2,
                },
            ) => {
                let x = &self.old.program.natives[*import as usize];
                let y = &self.new.program.natives[*import2 as usize];
                dst == dst2 && args == args2 && x.path == y.path && x.signature == y.signature
            }
            _ => a == b,
        }
    }

    /// Where the candidate function `func` is: its module and the range its
    /// body spans.
    fn at(&self, func: FuncId) -> Option<(usize, TextRange)> {
        let function = self.new.function(func);
        let spans = &function.body.as_ref().ok()?.spans;
        let start = spans.iter().map(|s| s.start()).min()?;
        let end = spans.iter().map(|s| s.end()).max()?;
        Some((function.module, TextRange::new(start, end)))
    }
}

/// The member name of a hook's action (`Player.fixed_update` names
/// `fixed_update`).
fn hook_name(function: &Function) -> &str {
    function
        .name
        .rsplit_once('.')
        .map_or(function.name.as_str(), |n| n.1)
}
