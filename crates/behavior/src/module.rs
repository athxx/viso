//! Chunks, component layouts, systems and the verified module.

use std::fmt;
use std::rc::Rc;
use std::time::Duration;

use crate::game::InputSchema;
use crate::i18n::Catalog;
use crate::native::{Determinism, NativeId};
use crate::op::Op;
use crate::retype::ValueSchema;
use crate::value::Value;

/// A byte range in a chunk's source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Span {
    /// The first byte.
    pub start: u32,
    /// One past the last byte.
    pub end: u32,
}

/// What a chunk runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkKind {
    /// A `fn`.
    Fn,
    /// An `action`.
    Action,
    /// A closure expression.
    Closure,
    /// A `computed` value.
    Computed,
    /// A `state` initializer.
    StateInit,
    /// An `input` default.
    InputDefault,
    /// A `const` value.
    Const,
    /// A record field default.
    FieldDefault,
    /// A view event handler: the payload arrives in `r0`, then the bindings of
    /// every enclosing `for`/`match` region the handler reads.
    Handler,
    /// A view region's pure entry (an arm choice, an iterable, a key): the
    /// bindings of every enclosing `for`/`match` region arrive first, then the
    /// entry's subject when it takes one.
    RegionEntry,
    /// An `effect` body: one transaction over the bindings of every enclosing
    /// `for`/`match` region, returning its cleanup closure, or `Nil`.
    Effect,
    /// A `task`: runs on a fiber that suspends at each task native it awaits.
    Task,
}

/// How a named task slot treats a `start` while a task it started still runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskPolicy {
    /// Cancels the running task and starts the new one.
    KeepLatest,
    /// Ignores the new start.
    DropNew,
    /// Starts the new task once every earlier one finished.
    Queue,
    /// Runs up to this many at once, queueing the rest.
    Parallel(u32),
}

impl TaskPolicy {
    /// The policy as one operand word; `parallel(n)` runs at least one.
    pub fn word(self) -> u32 {
        match self {
            TaskPolicy::KeepLatest => 0,
            TaskPolicy::DropNew => 1,
            TaskPolicy::Queue => 2,
            TaskPolicy::Parallel(n) => n.max(1).saturating_add(2),
        }
    }

    /// The policy of operand word `word`.
    pub fn from_word(word: u32) -> TaskPolicy {
        match word {
            0 => TaskPolicy::KeepLatest,
            1 => TaskPolicy::DropNew,
            2 => TaskPolicy::Queue,
            n => TaskPolicy::Parallel(n - 2),
        }
    }
}

/// When an `effect` runs, after the commit that mounts its component and the
/// commits that change its dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EffectRun {
    /// Once, after the mount.
    Mount,
    /// After each commit that changes a dependency's value.
    Change,
    /// After the mount and after each commit that changes a dependency's value.
    MountAndChange,
}

/// An `effect` of a component's view: its entries in the handler table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentEffect {
    /// The region entry computing the dependency values as a list; `None` for
    /// an effect without dependencies.
    pub deps: Option<u32>,
    /// The [`ChunkKind::Effect`] body.
    pub body: u32,
    /// When it runs.
    pub run: EffectRun,
    /// What it loads, for a `resource`: its dependencies are then the key and
    /// its body starts the loader.
    pub resource: Option<ResourceLoad>,
}

/// How a `resource` loads: the effect carrying it runs on mount and on each
/// key change, and its body starts the loader task without handlers; the view
/// host moves the state through `loading`, `ready`, `error` and `reloading`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLoad {
    /// The region entry reading the resource's state.
    pub state: u32,
    /// The [`ChunkKind::Handler`] writing its payload to the state.
    pub write: u32,
    /// How long a key must hold before its loader starts.
    pub debounce: Option<Duration>,
    /// How long a loaded value stays cached under its key.
    pub cache_for: Option<Duration>,
    /// Whether an error is cached as a value is.
    pub cache_errors: bool,
    /// Whether a key change cancels the load in flight.
    pub keep_latest: bool,
}

/// A runnable body.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Code {
    /// The instructions.
    pub ops: Box<[Op]>,
    /// The constant pool [`Op::Const`] and [`Op::DisplayDim`] index.
    pub consts: Box<[Value]>,
    /// The operand table variable-operand instructions index.
    pub ext: Box<[u32]>,
    /// The source span of each instruction, index-parallel to `ops`.
    pub spans: Box<[Span]>,
}

/// One function of a module.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// The declared name (`Component.member` for a component member).
    pub name: Box<str>,
    /// What it runs.
    pub kind: ChunkKind,
    /// The index of the source file it is declared in.
    pub module: u32,
    /// The number of arguments, arriving in `r0..`.
    pub params: u16,
    /// The frame size.
    pub regs: u16,
    /// The registers a closure's captured values arrive in, in capture order.
    pub captures: Box<[u16]>,
    /// The body, or why the compiler could not produce one; calling a chunk
    /// without a body faults.
    pub body: Result<Code, Box<str>>,
}

/// A component's runtime layout.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Component {
    /// Its name.
    pub name: Box<str>,
    /// The state names, by slot.
    pub states: Box<[Box<str>]>,
    /// The input names, by slot.
    pub inputs: Box<[Box<str>]>,
    /// The event names, by index.
    pub events: Box<[Box<str>]>,
    /// Each state slot's initializer chunk; a slot without one starts `Nil`.
    pub state_inits: Box<[Option<u32>]>,
    /// Each input slot's default chunk; an input without one is required.
    pub input_defaults: Box<[Option<u32>]>,
    /// The `fn`/`action`/`computed` members, by name.
    pub members: Box<[(Box<str>, u32)]>,
    /// The view's handler table, in source order: its event handlers, which a
    /// view node names by index, its regions' entries, and the initializers of
    /// the states region content keeps.
    pub handlers: Box<[u32]>,
    /// The effects that mount with the view, in source order; those of an
    /// instance region content mounts mount with that content instead.
    pub effects: Box<[ComponentEffect]>,
    /// Its `@persist` states, in declaration order: those a view host of the
    /// component loads before it mounts and stores as they change.
    pub persist: Box<[PersistSlot]>,
}

impl Component {
    /// The member chunk named `name`.
    pub fn member(&self, name: &str) -> Option<u32> {
        self.members.iter().find(|(n, _)| &**n == name).map(|m| m.1)
    }

    /// The state slot named `name`.
    pub fn state(&self, name: &str) -> Option<usize> {
        self.states.iter().position(|s| &**s == name)
    }

    /// The input slot named `name`.
    pub fn input(&self, name: &str) -> Option<usize> {
        self.inputs.iter().position(|s| &**s == name)
    }
}

/// A native function a module calls: the path it was compiled against and
/// the hash of the schema signature it was checked with. Linking resolves it
/// against a registry once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeImport {
    /// The function's full path, such as `viso::text::upper`.
    pub path: Box<str>,
    /// The [`NativeFunction::signature`](crate::native::NativeFunction::signature)
    /// it was compiled against.
    pub signature: u64,
    /// The number of arguments a call passes.
    pub params: u16,
}

/// The durable identity of a declaration: the 128-bit fingerprint of its
/// package, module, kind and path, unchanged by edits elsewhere and by
/// source reordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct StableId {
    /// The high 64 bits.
    pub hi: u64,
    /// The low 64 bits.
    pub lo: u64,
}

/// A Simulation state of a system that a game snapshot captures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotSlot {
    /// The state's stable identity.
    pub id: StableId,
    /// Its slot in the system's layout.
    pub slot: u32,
    /// A hash of its type's schema: a snapshot value restores only into a
    /// state of the same schema.
    pub schema: u64,
}

/// A `system`: a component a scheduler drives through the hooks of the
/// traits it implements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct System {
    /// Its component layout.
    pub component: u32,
    /// Its hooks, in the order its traits declare them: each the identity of
    /// a trait hook ([`NativeId::of`] `viso::game::FixedUpdate::fixed_update`)
    /// and the member action implementing it.
    pub hooks: Box<[(NativeId, u32)]>,
    /// Its stable identity.
    pub id: StableId,
    /// Its Simulation states, by ascending stable identity; its `@local`
    /// states are not among them.
    pub snapshot: Box<[SnapshotSlot]>,
    /// Its `@local` states, by ascending stable identity: no snapshot holds
    /// them, but a logic reload carries them to the build that kept them.
    pub locals: Box<[SnapshotSlot]>,
    /// Its `@persist` states, in declaration order.
    pub persist: Box<[PersistSlot]>,
}

/// A `@persist` state of a system: stored under its key, the value a run
/// left loaded before the next run's start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistSlot {
    /// Its key, unique in the module.
    pub key: Box<str>,
    /// Its slot in the system's layout.
    pub slot: u32,
    /// Its type, which a value stored by a build of another type converts
    /// into.
    pub schema: ValueSchema,
    /// Its type as source spells it, which a `@migrate(from:)` names.
    pub spelling: Box<str>,
}

/// A `@migrate` function: it carries a value of the type spelled `from`, once
/// converted into its parameter, into its return type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migrator {
    pub from: Box<str>,
    pub param: ValueSchema,
    pub ret: ValueSchema,
    /// The function's chunk.
    pub chunk: u32,
}

impl System {
    /// The action implementing hook `hook`.
    pub fn hook(&self, hook: NativeId) -> Option<u32> {
        self.hooks.iter().find(|h| h.0 == hook).map(|h| h.1)
    }
}

/// A verified set of chunks, component layouts, systems and native imports,
/// the input schema its systems read and the tick rate of their fixed step.
///
/// Construction checks every register, jump target, operand-table range,
/// constant, chunk and native reference, state/input slot and event index,
/// and that every system hook is an action of its own component, so the
/// interpreter only meets well-formed code.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    pub(crate) chunks: Box<[Chunk]>,
    pub(crate) components: Box<[Component]>,
    pub(crate) systems: Box<[System]>,
    pub(crate) natives: Box<[NativeImport]>,
    pub(crate) input: Option<Box<InputSchema>>,
    pub(crate) tick_rate: u32,
    pub(crate) determinism: Determinism,
    pub(crate) collision_delivery: CollisionDelivery,
    pub(crate) migrators: Box<[Migrator]>,
    pub(crate) capabilities: Box<[Box<str>]>,
    pub(crate) themes: Box<[(Box<str>, u32)]>,
    pub(crate) catalog: Option<Rc<Catalog>>,
}

/// The tick rate of a module that declares none, 60 Hz.
pub const DEFAULT_TICK_RATE: u32 = 60;

/// The order a tick hands the contacts that began to its
/// `CollisionListener`s (`[game] collision_delivery`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CollisionDelivery {
    /// Each contact in turn to every listener in system order: one event is
    /// seen by all before the next.
    #[default]
    EventMajor,
    /// Each listener in system order sees every contact before the next
    /// listener runs.
    ListenerMajor,
}

impl CollisionDelivery {
    /// Its manifest name.
    pub fn name(self) -> &'static str {
        match self {
            CollisionDelivery::EventMajor => "event_major",
            CollisionDelivery::ListenerMajor => "listener_major",
        }
    }
}

/// Why a module failed verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyError {
    /// The chunk.
    pub chunk: u32,
    /// The instruction, if the fault is in one.
    pub pc: Option<u32>,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.pc {
            Some(pc) => write!(f, "chunk {} at {pc}: {}", self.chunk, self.message),
            None => write!(f, "chunk {}: {}", self.chunk, self.message),
        }
    }
}

impl std::error::Error for VerifyError {}

impl Module {
    /// Verifies and builds a module; `systems` are in the order a scheduler
    /// runs them.
    pub fn new(
        chunks: Vec<Chunk>,
        components: Vec<Component>,
        systems: Vec<System>,
        natives: Vec<NativeImport>,
    ) -> Result<Module, VerifyError> {
        let module = Module {
            chunks: chunks.into(),
            components: components.into(),
            systems: systems.into(),
            natives: natives.into(),
            input: None,
            tick_rate: DEFAULT_TICK_RATE,
            determinism: Determinism::SameBinary,
            collision_delivery: CollisionDelivery::EventMajor,
            migrators: Box::new([]),
            capabilities: Box::new([]),
            themes: Box::new([]),
            catalog: None,
        };
        let mut max_states = 0;
        let mut max_inputs = 0;
        let mut max_events = 0;
        for component in module.components.iter() {
            max_states = max_states.max(component.states.len());
            max_inputs = max_inputs.max(component.inputs.len());
            max_events = max_events.max(component.events.len());
            let refs = component
                .state_inits
                .iter()
                .chain(component.input_defaults.iter())
                .flatten()
                .chain(component.members.iter().map(|m| &m.1))
                .chain(component.handlers.iter());
            for &chunk in refs {
                if chunk as usize >= module.chunks.len() {
                    return Err(VerifyError {
                        chunk,
                        pc: None,
                        message: format!("component `{}` names a missing chunk", component.name),
                    });
                }
            }
            for &chunk in component.handlers.iter() {
                let handler = &module.chunks[chunk as usize];
                let entry = match handler.kind {
                    ChunkKind::Handler => handler.params > 0,
                    ChunkKind::RegionEntry | ChunkKind::StateInit | ChunkKind::Effect => true,
                    _ => false,
                };
                if !entry {
                    return Err(VerifyError {
                        chunk,
                        pc: None,
                        message: format!(
                            "component `{}` names a handler-table chunk that is no event \
                             handler taking a payload, region entry, state initializer or \
                             effect",
                            component.name
                        ),
                    });
                }
            }
            for effect in component.effects.iter() {
                let kind = |entry: u32| {
                    let chunk = *component.handlers.get(entry as usize)?;
                    Some((chunk, module.chunks[chunk as usize].kind))
                };
                let deps = effect.deps.map(kind);
                let body = kind(effect.body);
                let resource = effect.resource.is_none_or(|load| {
                    deps.is_some()
                        && matches!(kind(load.state), Some((_, ChunkKind::RegionEntry)))
                        && matches!(kind(load.write), Some((_, ChunkKind::Handler)))
                });
                let fits = matches!(body, Some((_, ChunkKind::Effect)))
                    && matches!(deps, None | Some(Some((_, ChunkKind::RegionEntry))))
                    && resource;
                if !fits {
                    return Err(VerifyError {
                        chunk: body.map_or(0, |b| b.0),
                        pc: None,
                        message: format!(
                            "an effect of component `{}` names no effect body and region entry \
                             in its handler table",
                            component.name
                        ),
                    });
                }
            }
        }
        for (i, system) in module.systems.iter().enumerate() {
            let fail = |chunk: u32, message: String| VerifyError {
                chunk,
                pc: None,
                message,
            };
            let Some(layout) = module.components.get(system.component as usize) else {
                return Err(fail(0, format!("system {i} names a missing component")));
            };
            if module.systems[..i]
                .iter()
                .any(|s| s.component == system.component)
            {
                return Err(fail(
                    0,
                    format!("component `{}` is listed as two systems", layout.name),
                ));
            }
            for (n, &(hook, chunk)) in system.hooks.iter().enumerate() {
                let action = layout.members.iter().any(|m| m.1 == chunk)
                    && module
                        .chunks
                        .get(chunk as usize)
                        .is_some_and(|c| c.kind == ChunkKind::Action);
                if !action {
                    return Err(fail(
                        chunk,
                        format!(
                            "a hook of system `{}` is not one of its actions",
                            layout.name
                        ),
                    ));
                }
                if system.hooks[..n].iter().any(|h| h.0 == hook) {
                    return Err(fail(
                        chunk,
                        format!("system `{}` binds one hook twice", layout.name),
                    ));
                }
            }
            if module.systems[..i].iter().any(|s| s.id == system.id) {
                return Err(fail(
                    0,
                    format!("system `{}` shares a stable identity", layout.name),
                ));
            }
            for states in [&system.snapshot, &system.locals] {
                for (n, state) in states.iter().enumerate() {
                    if state.slot as usize >= layout.states.len() {
                        return Err(fail(
                            0,
                            format!("system `{}` names a missing state", layout.name),
                        ));
                    }
                    if n > 0 && states[n - 1].id >= state.id {
                        return Err(fail(
                            0,
                            format!(
                                "the states of system `{}` are not in ascending \
                                 identity order",
                                layout.name
                            ),
                        ));
                    }
                }
            }
            for state in system.persist.iter() {
                if state.slot as usize >= layout.states.len() {
                    return Err(fail(
                        0,
                        format!("system `{}` persists a missing state", layout.name),
                    ));
                }
                module.field_defaults(&state.schema)?;
            }
        }
        let fail = |chunk: u32, message: String| VerifyError {
            chunk,
            pc: None,
            message,
        };
        for layout in module.components.iter() {
            for state in layout.persist.iter() {
                if state.slot as usize >= layout.states.len() {
                    return Err(fail(
                        0,
                        format!("component `{}` persists a missing state", layout.name),
                    ));
                }
                module.field_defaults(&state.schema)?;
            }
        }
        let mut keys = std::collections::BTreeSet::new();
        let persisted = module
            .systems
            .iter()
            .flat_map(|s| s.persist.iter())
            .chain(module.components.iter().flat_map(|c| c.persist.iter()));
        for state in persisted {
            if !keys.insert(&*state.key) {
                return Err(fail(
                    0,
                    format!("two states persist under the key `{}`", state.key),
                ));
            }
        }
        let limits = Limits {
            chunks: module
                .chunks
                .iter()
                .map(|c| (c.params, c.captures.len(), c.kind))
                .collect(),
            natives: module.natives.iter().map(|n| n.params).collect(),
            states: max_states,
            inputs: max_inputs,
            events: max_events,
        };
        for (index, chunk) in module.chunks.iter().enumerate() {
            verify_chunk(index as u32, chunk, &limits)?;
        }
        Ok(module)
    }

    /// The module with `schema` as the input its systems read, in place of
    /// the default `InputAction` set.
    ///
    /// # Errors
    ///
    /// A [`VerifyError`] if the schema declares no action, a binding names an
    /// action it does not declare, or its dead zone is outside `[0, 1)`.
    pub fn with_input(mut self, schema: InputSchema) -> Result<Module, VerifyError> {
        let fail = |message: String| VerifyError {
            chunk: 0,
            pc: None,
            message,
        };
        if schema.actions.is_empty() {
            return Err(fail(format!(
                "the input schema `{}` declares no action",
                schema.name
            )));
        }
        if let Some(action) = schema.bindings.max_action()
            && action.0 as usize >= schema.actions.len()
        {
            return Err(fail(format!(
                "a binding of `{}` names action {}, which it does not declare",
                schema.name, action.0
            )));
        }
        if !(0.0..1.0).contains(&schema.bindings.dead_zone) {
            return Err(fail(format!(
                "the dead zone of `{}` is outside [0, 1)",
                schema.name
            )));
        }
        self.input = Some(Box::new(schema));
        Ok(self)
    }

    /// The input schema its systems read, unless they read the default set.
    pub fn input(&self) -> Option<&InputSchema> {
        self.input.as_deref()
    }

    /// The module with its systems stepping `tick_rate` ticks a second.
    ///
    /// # Errors
    ///
    /// A [`VerifyError`] if `tick_rate` is 0.
    pub fn with_tick_rate(mut self, tick_rate: u32) -> Result<Module, VerifyError> {
        if tick_rate == 0 {
            return Err(VerifyError {
                chunk: 0,
                pc: None,
                message: "a tick rate is at least 1 Hz".to_owned(),
            });
        }
        self.tick_rate = tick_rate;
        Ok(self)
    }

    /// The module with its Simulation domain held to `determinism` and its
    /// contacts delivered in `delivery` order: what a physics engine
    /// stepping its games must reproduce, and how listeners see contacts.
    pub fn with_game_profile(
        mut self,
        determinism: Determinism,
        delivery: CollisionDelivery,
    ) -> Module {
        self.determinism = determinism;
        self.collision_delivery = delivery;
        self
    }

    /// The module with `migrators` as the `@migrate` functions a persisted
    /// value of an older type converts by.
    ///
    /// # Errors
    ///
    /// A [`VerifyError`] if a migrator's chunk is no function of one
    /// parameter, or a schema names a chunk that computes no field default.
    pub fn with_migrators(mut self, migrators: Vec<Migrator>) -> Result<Module, VerifyError> {
        for migrator in &migrators {
            let function = self
                .chunks
                .get(migrator.chunk as usize)
                .is_some_and(|c| c.kind == ChunkKind::Fn && c.params == 1);
            if !function {
                return Err(VerifyError {
                    chunk: migrator.chunk,
                    pc: None,
                    message: format!(
                        "the migrator from `{}` is no function of one parameter",
                        migrator.from
                    ),
                });
            }
            self.field_defaults(&migrator.param)?;
            self.field_defaults(&migrator.ret)?;
        }
        self.migrators = migrators.into();
        Ok(self)
    }

    /// The `@migrate` functions a persisted value converts by.
    pub fn migrators(&self) -> &[Migrator] {
        &self.migrators
    }

    /// The module granted `capabilities`, the package's grant a host links
    /// its natives with.
    pub fn with_capabilities<S: Into<Box<str>>>(
        mut self,
        capabilities: impl IntoIterator<Item = S>,
    ) -> Module {
        let mut capabilities: Vec<Box<str>> = capabilities.into_iter().map(Into::into).collect();
        capabilities.sort_unstable();
        capabilities.dedup();
        self.capabilities = capabilities.into();
        self
    }

    /// The capabilities the package is granted, sorted.
    pub fn capabilities(&self) -> &[Box<str>] {
        &self.capabilities
    }

    /// The module with `themes`, each a declared name and the constant chunk
    /// computing its `Theme` value, which a host switches its views to.
    ///
    /// # Errors
    ///
    /// A [`VerifyError`] if a theme's chunk is no constant, or two themes
    /// share a name.
    pub fn with_themes(mut self, mut themes: Vec<(Box<str>, u32)>) -> Result<Module, VerifyError> {
        themes.sort_unstable();
        for (at, (name, chunk)) in themes.iter().enumerate() {
            let constant = self
                .chunks
                .get(*chunk as usize)
                .is_some_and(|c| c.kind == ChunkKind::Const && c.params == 0);
            let message = if !constant {
                format!("the theme `{name}` names a chunk that is no constant")
            } else if at > 0 && themes[at - 1].0 == *name {
                format!("two themes are named `{name}`")
            } else {
                continue;
            };
            return Err(VerifyError {
                chunk: *chunk,
                pc: None,
                message,
            });
        }
        self.themes = themes.into();
        Ok(self)
    }

    /// The constant chunk computing the theme `name`.
    pub fn theme(&self, name: &str) -> Option<u32> {
        self.themes
            .binary_search_by(|(n, _)| (**n).cmp(name))
            .ok()
            .map(|at| self.themes[at].1)
    }

    /// The themes, by name.
    pub fn themes(&self) -> &[(Box<str>, u32)] {
        &self.themes
    }

    /// Adds the package's message catalog, which every `Translate` needs.
    ///
    /// # Errors
    ///
    /// A [`VerifyError`] when a chunk translates and there is no catalog.
    pub fn with_catalog(mut self, catalog: Option<Catalog>) -> Result<Module, VerifyError> {
        if catalog.is_none()
            && let Some((chunk, pc)) = self.chunks.iter().enumerate().find_map(|(i, c)| {
                c.body
                    .as_ref()
                    .ok()?
                    .ops
                    .iter()
                    .position(|op| matches!(op, Op::Translate { .. }))
                    .map(|pc| (i, pc))
            })
        {
            return Err(VerifyError {
                chunk: chunk as u32,
                pc: Some(pc as u32),
                message: "a message is translated but the module has no catalog".into(),
            });
        }
        self.catalog = catalog.map(Rc::new);
        Ok(self)
    }

    /// The package's message catalog.
    pub fn catalog(&self) -> Option<&Catalog> {
        self.catalog.as_deref()
    }

    /// Checks that every chunk `schema` names computes a field default.
    fn field_defaults(&self, schema: &ValueSchema) -> Result<(), VerifyError> {
        match schema.chunks().find(|&chunk| {
            self.chunks
                .get(chunk as usize)
                .is_none_or(|c| c.kind != ChunkKind::FieldDefault || c.params != 0)
        }) {
            Some(chunk) => Err(VerifyError {
                chunk,
                pc: None,
                message: "a value schema names a chunk that computes no field default".to_owned(),
            }),
            None => Ok(()),
        }
    }

    /// The ticks a second its systems step, the compile-time fixed step its
    /// tick timers were converted with.
    pub fn tick_rate(&self) -> u32 {
        self.tick_rate
    }

    /// The determinism tier its Simulation domain was checked against
    /// (`[game] determinism`).
    pub fn determinism(&self) -> Determinism {
        self.determinism
    }

    /// The order its ticks deliver contacts in (`[game] collision_delivery`).
    pub fn collision_delivery(&self) -> CollisionDelivery {
        self.collision_delivery
    }

    /// Every chunk.
    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    /// The chunk `index`.
    pub fn chunk(&self, index: u32) -> &Chunk {
        &self.chunks[index as usize]
    }

    /// Every component layout.
    pub fn components(&self) -> &[Component] {
        &self.components
    }

    /// The component named `name`, by index.
    pub fn component(&self, name: &str) -> Option<u32> {
        self.components
            .iter()
            .position(|c| &*c.name == name)
            .map(|i| i as u32)
    }

    /// The layout of component `index`.
    pub fn layout(&self, index: u32) -> &Component {
        &self.components[index as usize]
    }

    /// Every system, in run order.
    pub fn systems(&self) -> &[System] {
        &self.systems
    }

    /// Every native import, by index.
    pub fn natives(&self) -> &[NativeImport] {
        &self.natives
    }
}

/// What instructions may reference.
struct Limits {
    /// Each chunk's parameter and capture counts and kind.
    chunks: Vec<(u16, usize, ChunkKind)>,
    /// Each native import's parameter count.
    natives: Vec<u16>,
    states: usize,
    inputs: usize,
    events: usize,
}

fn verify_chunk(index: u32, chunk: &Chunk, limits: &Limits) -> Result<(), VerifyError> {
    let fail = |pc: Option<usize>, message: String| VerifyError {
        chunk: index,
        pc: pc.map(|pc| pc as u32),
        message,
    };
    if chunk.params > chunk.regs {
        return Err(fail(None, "more parameters than registers".into()));
    }
    if let Some(r) = chunk.captures.iter().find(|r| **r >= chunk.regs) {
        return Err(fail(
            None,
            format!("capture register r{r} is out of the frame"),
        ));
    }
    let Ok(code) = &chunk.body else {
        return Ok(());
    };
    if code.spans.len() != code.ops.len() {
        return Err(fail(
            None,
            "the span table does not match the instructions".into(),
        ));
    }
    if code
        .consts
        .iter()
        .any(|c| matches!(c, Value::Closure(_) | Value::Handle(_)))
    {
        return Err(fail(None, "a constant that is not plain data".into()));
    }
    let Some(last) = code.ops.last() else {
        return Err(fail(None, "an empty body".into()));
    };
    if !matches!(last, Op::Return { .. } | Op::Jump { .. } | Op::Unreachable) {
        return Err(fail(
            Some(code.ops.len() - 1),
            "the body can run off its end".into(),
        ));
    }
    let v = Verifier {
        code,
        regs: chunk.regs,
        limits,
    };
    for (pc, op) in code.ops.iter().enumerate() {
        v.op(op).map_err(|message| fail(Some(pc), message))?;
    }
    Ok(())
}

struct Verifier<'a> {
    code: &'a Code,
    regs: u16,
    limits: &'a Limits,
}

type Check = Result<(), String>;

impl Verifier<'_> {
    fn reg(&self, r: u16) -> Check {
        if r < self.regs {
            Ok(())
        } else {
            Err(format!("register r{r} is out of the frame"))
        }
    }

    fn regs(&self, rs: &[u16]) -> Check {
        rs.iter().try_for_each(|r| self.reg(*r))
    }

    fn target(&self, target: u32) -> Check {
        if (target as usize) < self.code.ops.len() {
            Ok(())
        } else {
            Err(format!("jump target @{target} is out of the body"))
        }
    }

    fn slot(&self, slot: u32, limit: usize, what: &str) -> Check {
        if (slot as usize) < limit {
            Ok(())
        } else {
            Err(format!("{what} {slot} is out of range"))
        }
    }

    /// Checks that chunk `func` exists and takes `argc` arguments and `captures`
    /// captured values.
    fn chunk(&self, func: u32, argc: u32, captures: usize) -> Check {
        let Some(&(params, caps, _)) = self.limits.chunks.get(func as usize) else {
            return Err(format!("chunk {func} is out of range"));
        };
        if u32::from(params) != argc {
            return Err(format!("chunk {func} takes {params} arguments, not {argc}"));
        }
        if caps != captures {
            return Err(format!(
                "chunk {func} captures {caps} values, not {captures}"
            ));
        }
        Ok(())
    }

    fn constant(&self, index: u32) -> Check {
        self.slot(index, self.code.consts.len(), "constant")
    }

    /// The operand words `ext[at..at + n]`.
    fn words(&self, at: usize, n: usize) -> Result<&[u32], String> {
        at.checked_add(n)
            .and_then(|end| self.code.ext.get(at..end))
            .ok_or_else(|| "the operand table is too short".to_string())
    }

    /// A `head.., n, regs..` operand list: checks the `n` registers after `head`
    /// words and returns the head.
    fn list(&self, ext: u32, head: usize) -> Result<&[u32], String> {
        let at = ext as usize;
        let words = self.words(at, head + 1)?;
        let n = words[head] as usize;
        for &r in self.words(at + head + 1, n)? {
            self.reg16(r)?;
        }
        Ok(&words[..head])
    }

    fn reg16(&self, r: u32) -> Check {
        match u16::try_from(r) {
            Ok(r) => self.reg(r),
            Err(_) => Err(format!("register r{r} is out of the frame")),
        }
    }

    fn op(&self, op: &Op) -> Check {
        match *op {
            Op::Const { dst, index } => {
                self.reg(dst)?;
                self.constant(index)
            }
            Op::Int { dst, .. } | Op::Nil { dst } => self.reg(dst),
            Op::Move { dst, src }
            | Op::Neg { dst, src, .. }
            | Op::Not { dst, src }
            | Op::BitNot { dst, src, .. }
            | Op::Cast { dst, src, .. }
            | Op::Len { dst, src }
            | Op::Tag { dst, src }
            | Op::IsNil { dst, src }
            | Op::Display { dst, src, .. }
            | Op::Field { dst, src, .. }
            | Op::Push {
                list: dst,
                item: src,
            }
            | Op::Truncate {
                list: dst,
                len: src,
            } => self.regs(&[dst, src]),
            Op::Insert { list, index, item } => self.regs(&[list, index, item]),
            Op::Remove { dst, list, index } => self.regs(&[dst, list, index]),
            Op::DisplayDim { dst, src, suffix } => {
                self.regs(&[dst, src])?;
                self.constant(u32::from(suffix))?;
                match &self.code.consts[usize::from(suffix)] {
                    Value::Str(_) => Ok(()),
                    _ => Err("a unit suffix that is not a string".into()),
                }
            }
            Op::LoadState { dst, slot } => {
                self.reg(dst)?;
                self.slot(slot, self.limits.states, "state slot")
            }
            Op::StoreState { src, slot } => {
                self.reg(src)?;
                self.slot(slot, self.limits.states, "state slot")
            }
            Op::LoadInput { dst, slot } => {
                self.reg(dst)?;
                self.slot(slot, self.limits.inputs, "input slot")
            }
            Op::AddI64 { dst, a, b }
            | Op::SubI64 { dst, a, b }
            | Op::MulI64 { dst, a, b }
            | Op::LtI64 { dst, a, b }
            | Op::LeI64 { dst, a, b }
            | Op::AddF64 { dst, a, b }
            | Op::SubF64 { dst, a, b }
            | Op::MulF64 { dst, a, b }
            | Op::DivF64 { dst, a, b }
            | Op::LtF64 { dst, a, b }
            | Op::LeF64 { dst, a, b }
            | Op::Arith { dst, a, b, .. }
            | Op::Eq { dst, a, b }
            | Op::Ne { dst, a, b } => self.regs(&[dst, a, b]),
            Op::Index { dst, list, index } => self.regs(&[dst, list, index]),
            Op::Call { dst, ext } => {
                self.reg(dst)?;
                let head = self.list(ext, 1)?;
                self.chunk(head[0], self.code.ext[ext as usize + 1], 0)
            }
            Op::Native { dst, ext } => {
                self.reg(dst)?;
                let head = self.list(ext, 1)?;
                let argc = self.code.ext[ext as usize + 1];
                match self.limits.natives.get(head[0] as usize) {
                    None => Err(format!("native import {} is out of range", head[0])),
                    Some(&params) if u32::from(params) != argc => Err(format!(
                        "native import {} takes {params} arguments, not {argc}",
                        head[0]
                    )),
                    Some(_) => Ok(()),
                }
            }
            Op::CallValue { dst, ext } => {
                self.reg(dst)?;
                let head = self.list(ext, 1)?;
                self.reg16(head[0])
            }
            Op::Closure { dst, ext } => {
                self.reg(dst)?;
                let head = self.list(ext, 1)?;
                let func = head[0];
                let n = self.code.ext[ext as usize + 1] as usize;
                let params = self.limits.chunks.get(func as usize).map_or(0, |c| c.0);
                self.chunk(func, u32::from(params), n)
            }
            Op::Make { dst, ext } => {
                self.reg(dst)?;
                self.list(ext, 1).map(drop)
            }
            Op::List { dst, ext } | Op::Concat { dst, ext } => {
                self.reg(dst)?;
                self.list(ext, 0).map(drop)
            }
            Op::Translate { dst, ext } => {
                self.reg(dst)?;
                let head = self.list(ext, 2)?;
                self.reg16(head[0])?;
                self.reg16(head[1])
            }
            Op::Emit { ext } => {
                let head = self.list(ext, 1)?;
                self.slot(head[0], self.limits.events, "event")
            }
            Op::Start { ext } => {
                let head = self.list(ext, 1)?;
                let func = head[0];
                let argc = self.code.ext[ext as usize + 1];
                self.chunk(func, argc, 0)?;
                if self.limits.chunks[func as usize].2 != ChunkKind::Task {
                    return Err(format!("chunk {func} started as a task is no task"));
                }
                let tail = self.words(ext as usize + 2 + argc as usize, 5)?;
                tail[..2]
                    .iter()
                    .filter(|&&handler| handler != u32::MAX)
                    .try_for_each(|&handler| self.reg16(handler))
            }
            Op::SetPath { root, ext } => {
                self.reg(root)?;
                let at = ext as usize;
                let words = self.words(at, 2)?;
                self.reg16(words[0])?;
                let n = words[1] as usize;
                let steps = self.words(at + 2, n.checked_mul(2).ok_or("too many steps")?)?;
                for &[kind, arg] in steps.as_chunks::<2>().0 {
                    match kind {
                        0 => {}
                        1 => self.reg16(arg)?,
                        kind => return Err(format!("unknown path step kind {kind}")),
                    }
                }
                Ok(())
            }
            Op::Jump { target } => self.target(target),
            Op::JumpIf { cond, target } | Op::JumpUnless { cond, target } => {
                self.reg(cond)?;
                self.target(target)
            }
            Op::Switch { src, ext } => {
                self.reg(src)?;
                let at = ext as usize;
                let words = self.words(at, 4)?;
                self.target(words[2])?;
                let n = words[3] as usize;
                for &t in self.words(at + 4, n)? {
                    self.target(t)?;
                }
                Ok(())
            }
            Op::Return { src } => self.reg(src),
            Op::Unreachable => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(params: u16, regs: u16, ops: Vec<Op>) -> Chunk {
        let spans = vec![Span::default(); ops.len()].into();
        Chunk {
            name: "f".into(),
            kind: ChunkKind::Fn,
            module: 0,
            params,
            regs,
            captures: Box::new([]),
            body: Ok(Code {
                ops: ops.into(),
                consts: Box::new([]),
                ext: Box::new([]),
                spans,
            }),
        }
    }

    fn verify(chunks: Vec<Chunk>) -> Result<Module, VerifyError> {
        Module::new(chunks, Vec::new(), Vec::new(), Vec::new())
    }

    #[test]
    fn well_formed_code_verifies() {
        let ops = vec![Op::Int { dst: 0, value: 1 }, Op::Return { src: 0 }];
        assert!(verify(vec![chunk(0, 1, ops)]).is_ok());
    }

    #[test]
    fn a_register_outside_the_frame_is_rejected() {
        let ops = vec![Op::Move { dst: 0, src: 2 }, Op::Return { src: 0 }];
        let e = verify(vec![chunk(0, 2, ops)]).unwrap_err();
        assert_eq!(
            (e.pc, e.message.as_str()),
            (Some(0), "register r2 is out of the frame")
        );
    }

    #[test]
    fn a_jump_outside_the_body_is_rejected() {
        let e = verify(vec![chunk(0, 1, vec![Op::Jump { target: 1 }])]).unwrap_err();
        assert_eq!(e.message, "jump target @1 is out of the body");
    }

    #[test]
    fn a_body_that_can_run_off_its_end_is_rejected() {
        let e = verify(vec![chunk(0, 1, vec![Op::Nil { dst: 0 }])]).unwrap_err();
        assert_eq!(e.message, "the body can run off its end");
    }

    #[test]
    fn calls_are_checked_against_the_callee_arity() {
        let mut caller = chunk(
            0,
            1,
            vec![Op::Call { dst: 0, ext: 0 }, Op::Return { src: 0 }],
        );
        // `f(r0)` against a callee without parameters.
        if let Ok(code) = &mut caller.body {
            code.ext = Box::new([1, 1, 0]);
        }
        let callee = chunk(0, 1, vec![Op::Nil { dst: 0 }, Op::Return { src: 0 }]);
        let e = verify(vec![caller, callee]).unwrap_err();
        assert_eq!(e.message, "chunk 1 takes 0 arguments, not 1");
    }

    #[test]
    fn a_state_slot_outside_every_component_is_rejected() {
        let ops = vec![Op::LoadState { dst: 0, slot: 0 }, Op::Return { src: 0 }];
        let e = verify(vec![chunk(0, 1, ops)]).unwrap_err();
        assert_eq!(e.message, "state slot 0 is out of range");
    }

    #[test]
    fn the_handler_table_takes_payload_handlers_region_entries_and_state_inits() {
        let table = |kind, params| {
            let mut entry = chunk(params, 1, vec![Op::Nil { dst: 0 }, Op::Return { src: 0 }]);
            entry.kind = kind;
            let component = Component {
                name: "View".into(),
                handlers: Box::new([0]),
                ..Component::default()
            };
            Module::new(vec![entry], vec![component], Vec::new(), Vec::new())
        };
        assert!(table(ChunkKind::Handler, 1).is_ok());
        assert!(table(ChunkKind::RegionEntry, 0).is_ok());
        assert!(table(ChunkKind::StateInit, 1).is_ok());
        assert!(
            table(ChunkKind::Handler, 0).is_err(),
            "a handler takes a payload"
        );
        assert!(table(ChunkKind::Computed, 0).is_err());
    }
}
