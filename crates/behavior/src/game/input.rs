//! Typed game input: the schema enums of keys, pad buttons, sticks and touch
//! buttons, the `InputMap` a package declares, the [`InputLatch`] that turns
//! device events into action edges, and the frozen `InputSnapshot` each tick
//! reads.
//!
//! An action's `pressed`/`released` edge belongs to the first tick that runs
//! after it and is seen by that tick only: a frame that runs no tick keeps it
//! for the next, a frame that runs several delivers it once. `held` and the
//! move axes hold one value for every tick of a frame. A snapshot keeps only
//! action and axis values, never raw keys, so a recorded input tape does not
//! depend on the bindings.

use std::cell::Cell;

use crate::native::{NativeError, NativeFunction, NativeObject, NativeValue, Obj, SchemaTy};
use crate::value::Value;

/// The derive that makes a unit-only enum an action set an `InputMap` maps.
pub const INPUT_ACTION_DERIVE: &str = "InputAction";

/// The default radius below which a stick reads as centred.
pub const DEFAULT_DEAD_ZONE: f64 = 0.2;

macro_rules! schema_enum {
    (@name $variant:ident) => { stringify!($variant) };
    (@name $variant:ident $schema:literal) => { $schema };
    (
        $(#[$meta:meta])*
        $name:ident = $path:literal {
            $($(#[$vmeta:meta])* $variant:ident $(= $schema:literal)?),* $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        #[repr(u8)]
        pub enum $name {
            $($(#[$vmeta])* $variant),*
        }

        impl $name {
            /// Its schema path.
            pub const PATH: &'static str = $path;
            /// Its schema variant names, in index order.
            pub const VARIANTS: &'static [&'static str] =
                &[$(schema_enum!(@name $variant $($schema)?)),*];
            const ALL: &'static [$name] = &[$($name::$variant),*];

            /// The variant at `index`.
            pub fn from_index(index: i64) -> Option<$name> {
                usize::try_from(index).ok().and_then(|i| Self::ALL.get(i)).copied()
            }
        }

        impl NativeValue for $name {
            const TY: SchemaTy = SchemaTy::Enum($path);

            fn from_value(value: &Value) -> Option<$name> {
                value.as_int().and_then($name::from_index)
            }

            fn into_value(self) -> Value {
                Value::Int(self as i64)
            }
        }
    };
}

schema_enum! {
    /// A keyboard key, by its position on a US layout.
    Key = "viso::game::Key" {
        A, B, C, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
        Digit0, Digit1, Digit2, Digit3, Digit4, Digit5, Digit6, Digit7, Digit8, Digit9,
        Space, Enter, Escape, Tab, Backspace,
        ArrowUp, ArrowDown, ArrowLeft, ArrowRight,
        ShiftLeft, ShiftRight, ControlLeft, ControlRight, AltLeft, AltRight,
    }
}

schema_enum! {
    /// A gamepad button, by its position: `South` is the bottom face button.
    PadButton = "viso::game::PadButton" {
        South, East, West, North,
        LeftShoulder, RightShoulder, LeftTrigger, RightTrigger,
        Select, Start, Home, LeftThumb, RightThumb,
        DpadUp, DpadDown, DpadLeft, DpadRight,
    }
}

schema_enum! {
    /// A gamepad stick.
    PadStick = "viso::game::PadStick" { Left, Right }
}

schema_enum! {
    /// An on-screen touch button.
    TouchButton = "viso::game::TouchButton" { Primary, Secondary, Tertiary, Quaternary }
}

schema_enum! {
    /// The default action set, used when a package declares no `InputMap`.
    InputAction = "viso::game::InputAction" {
        Jump = "jump", Fire = "fire", Interact = "interact", Pause = "pause",
    }
}

schema_enum! {
    /// An axis of the move vector: `move_x` is right, `move_z` forward.
    InputAxis = "viso::game::InputAxis" { MoveX = "move_x", MoveZ = "move_z" }
}

const _: () = assert!(Key::VARIANTS.len() <= 64);
const _: () = assert!(PadButton::VARIANTS.len() <= 32);
const _: () = assert!(TouchButton::VARIANTS.len() <= 8);

/// An input action: a variant index of the package's action enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Action(pub u32);

impl NativeValue for Action {
    const TY: SchemaTy = SchemaTy::Action;

    fn from_value(value: &Value) -> Option<Action> {
        value
            .as_int()
            .and_then(|i| u32::try_from(i).ok())
            .map(Action)
    }

    fn into_value(self) -> Value {
        Value::Int(i64::from(self.0))
    }
}

/// Four keys read as a move vector, behind a `viso::game::KeySet` handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeySet {
    /// Forward.
    pub up: Key,
    /// Left.
    pub left: Key,
    /// Back.
    pub down: Key,
    /// Right.
    pub right: Key,
}

impl KeySet {
    /// `W`, `A`, `S`, `D`.
    pub const WASD: KeySet = KeySet {
        up: Key::W,
        left: Key::A,
        down: Key::S,
        right: Key::D,
    };

    /// The arrow keys.
    pub const ARROWS: KeySet = KeySet {
        up: Key::ArrowUp,
        left: Key::ArrowLeft,
        down: Key::ArrowDown,
        right: Key::ArrowRight,
    };
}

impl NativeObject for KeySet {
    const PATH: &'static str = "viso::game::KeySet";
}

/// Where the move vector comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MoveSource {
    /// The keys.
    pub keys: KeySet,
    /// The stick.
    pub stick: PadStick,
}

/// What an `InputMap` maps: each binding names an action by variant index.
#[derive(Debug, Clone, PartialEq)]
pub struct InputBindings {
    /// Key bindings.
    pub keys: Vec<(Key, Action)>,
    /// Gamepad button bindings.
    pub pads: Vec<(PadButton, Action)>,
    /// Touch button bindings.
    pub touches: Vec<(TouchButton, Action)>,
    /// The move vector's source, if any.
    pub move_axes: Option<MoveSource>,
    /// The stick radius that reads as centred, in `[0, 1)`.
    pub dead_zone: f64,
}

impl InputBindings {
    /// No bindings and the default dead zone.
    pub fn new() -> InputBindings {
        InputBindings {
            keys: Vec::new(),
            pads: Vec::new(),
            touches: Vec::new(),
            move_axes: None,
            dead_zone: DEFAULT_DEAD_ZONE,
        }
    }

    /// The bindings of the default [`InputAction`] set.
    pub fn standard() -> InputBindings {
        let a = |action: InputAction| Action(action as u32);
        InputBindings {
            keys: vec![
                (Key::Space, a(InputAction::Jump)),
                (Key::J, a(InputAction::Fire)),
                (Key::E, a(InputAction::Interact)),
                (Key::Escape, a(InputAction::Pause)),
            ],
            pads: vec![
                (PadButton::South, a(InputAction::Jump)),
                (PadButton::West, a(InputAction::Fire)),
                (PadButton::North, a(InputAction::Interact)),
                (PadButton::Start, a(InputAction::Pause)),
            ],
            touches: vec![
                (TouchButton::Primary, a(InputAction::Jump)),
                (TouchButton::Secondary, a(InputAction::Fire)),
                (TouchButton::Tertiary, a(InputAction::Interact)),
                (TouchButton::Quaternary, a(InputAction::Pause)),
            ],
            move_axes: Some(MoveSource {
                keys: KeySet::WASD,
                stick: PadStick::Left,
            }),
            dead_zone: DEFAULT_DEAD_ZONE,
        }
    }

    /// Whether some key, pad button or touch button maps to `action`.
    pub fn is_bound(&self, action: Action) -> bool {
        self.keys.iter().any(|b| b.1 == action)
            || self.pads.iter().any(|b| b.1 == action)
            || self.touches.iter().any(|b| b.1 == action)
    }

    /// The largest action index any binding names.
    pub fn max_action(&self) -> Option<Action> {
        let keys = self.keys.iter().map(|b| b.1);
        let pads = self.pads.iter().map(|b| b.1);
        let touches = self.touches.iter().map(|b| b.1);
        keys.chain(pads).chain(touches).max()
    }
}

impl Default for InputBindings {
    fn default() -> InputBindings {
        InputBindings::new()
    }
}

/// The input schema of a module: its action names and their bindings.
#[derive(Debug, Clone, PartialEq)]
pub struct InputSchema {
    /// The action enum's name.
    pub name: Box<str>,
    /// Its variant names, the actions in index order.
    pub actions: Box<[Box<str>]>,
    /// The bindings.
    pub bindings: InputBindings,
}

impl InputSchema {
    /// The default [`InputAction`] set and its bindings.
    pub fn standard() -> InputSchema {
        InputSchema {
            name: "InputAction".into(),
            actions: InputAction::VARIANTS.iter().map(|&n| n.into()).collect(),
            bindings: InputBindings::standard(),
        }
    }
}

/// The bindings a `const` builds, behind a `viso::game::InputMap` handle.
#[derive(Debug, Clone, PartialEq)]
pub struct InputMap {
    /// What it maps.
    pub bindings: InputBindings,
}

impl NativeObject for InputMap {
    const PATH: &'static str = "viso::game::InputMap";
}

/// `map` with `edit` applied, as a new map.
fn edited(map: &InputMap, edit: impl FnOnce(&mut InputBindings)) -> Obj<InputMap> {
    let mut bindings = map.bindings.clone();
    edit(&mut bindings);
    Obj::new(InputMap { bindings })
}

/// A set of action bits.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Bits(Box<[u64]>);

impl Bits {
    fn new(len: usize) -> Bits {
        Bits(vec![0; len.div_ceil(64)].into())
    }

    fn set(&mut self, i: u32) {
        let i = i as usize;
        if let Some(w) = self.0.get_mut(i / 64) {
            *w |= 1 << (i % 64);
        }
    }

    fn clear(&mut self) {
        self.0.fill(0);
    }
}

/// The device state the host reports and the action edges it implies since
/// the last tick.
#[derive(Debug)]
pub(crate) struct InputLatch {
    bindings: InputBindings,
    keys: u64,
    pads: u32,
    touches: u8,
    sticks: [(f64, f64); 2],
    held: Bits,
    scratch: Bits,
    pressed: Bits,
    released: Bits,
}

impl InputLatch {
    /// A latch over `schema`, nothing held.
    pub(crate) fn new(schema: &InputSchema) -> InputLatch {
        let len = schema.actions.len();
        InputLatch {
            bindings: schema.bindings.clone(),
            keys: 0,
            pads: 0,
            touches: 0,
            sticks: [(0.0, 0.0); 2],
            held: Bits::new(len),
            scratch: Bits::new(len),
            pressed: Bits::new(len),
            released: Bits::new(len),
        }
    }

    pub(crate) fn key(&mut self, key: Key, down: bool) {
        set_bit(&mut self.keys, key as u32, down);
        self.update();
    }

    pub(crate) fn pad(&mut self, button: PadButton, down: bool) {
        set_bit(&mut self.pads, button as u32, down);
        self.update();
    }

    pub(crate) fn touch(&mut self, button: TouchButton, down: bool) {
        set_bit(&mut self.touches, button as u32, down);
        self.update();
    }

    pub(crate) fn stick(&mut self, stick: PadStick, x: f64, y: f64) {
        let finite = |v: f64| {
            if v.is_finite() {
                v.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        };
        self.sticks[stick as usize] = (finite(x), finite(y));
    }

    /// Releases every key, button and stick, as when the game loses focus.
    pub(crate) fn release_all(&mut self) {
        self.keys = 0;
        self.pads = 0;
        self.touches = 0;
        self.sticks = [(0.0, 0.0); 2];
        self.update();
    }

    /// Recomputes which actions are held, latching each change as an edge.
    fn update(&mut self) {
        let next = &mut self.scratch;
        next.clear();
        let b = &self.bindings;
        let keys = b.keys.iter().filter(|k| self.keys >> k.0 as u32 & 1 == 1);
        let pads = b.pads.iter().filter(|p| self.pads >> p.0 as u32 & 1 == 1);
        let touches = b
            .touches
            .iter()
            .filter(|t| self.touches >> t.0 as u32 & 1 == 1);
        keys.map(|k| k.1)
            .chain(pads.map(|p| p.1))
            .chain(touches.map(|t| t.1))
            .for_each(|a| next.set(a.0));
        for (i, (new, old)) in next.0.iter().zip(self.held.0.iter_mut()).enumerate() {
            self.pressed.0[i] |= new & !*old;
            self.released.0[i] |= *old & !new;
            *old = *new;
        }
    }

    /// The move vector: the keys plus the stick past its dead zone, no longer
    /// than 1.
    fn move_axes(&self) -> (f64, f64) {
        let Some(source) = self.bindings.move_axes else {
            return (0.0, 0.0);
        };
        let key = |k: Key| f64::from(u8::from(self.keys >> k as u32 & 1 == 1));
        let keys = source.keys;
        let (kx, ky) = (
            key(keys.right) - key(keys.left),
            key(keys.up) - key(keys.down),
        );
        let (sx, sy) = self.sticks[source.stick as usize];
        let dead = self.bindings.dead_zone;
        let length = sx.hypot(sy);
        let scale = if length <= dead {
            0.0
        } else {
            ((length - dead) / (1.0 - dead)).min(1.0) / length
        };
        let (x, y) = (kx + sx * scale, ky + sy * scale);
        let length = x.hypot(y);
        if length > 1.0 {
            (x / length, y / length)
        } else {
            (x, y)
        }
    }

    /// Freezes the state into `snapshot` for one tick, handing it the edges
    /// latched since the last.
    pub(crate) fn deliver(&mut self, snapshot: &InputSnapshot) {
        let words = snapshot
            .held
            .iter()
            .zip(&snapshot.pressed)
            .zip(&snapshot.released);
        for (i, ((held, pressed), released)) in words.enumerate() {
            held.set(self.held.0[i]);
            pressed.set(self.pressed.0[i]);
            released.set(self.released.0[i]);
        }
        let (x, y) = self.move_axes();
        snapshot.axes.x.set(x);
        snapshot.axes.y.set(y);
        self.pressed.clear();
        self.released.clear();
    }
}

fn set_bit<T>(bits: &mut T, i: u32, on: bool)
where
    T: Copy + From<u8> + std::ops::Shl<u32, Output = T> + std::ops::BitOrAssign,
    T: std::ops::BitAndAssign + std::ops::Not<Output = T>,
{
    let mask = T::from(1) << i;
    if on {
        *bits |= mask;
    } else {
        *bits &= !mask;
    }
}

/// The input one tick sees, behind a borrowed `viso::game::InputSnapshot`
/// handle.
#[derive(Debug)]
pub struct InputSnapshot {
    held: Box<[Cell<u64>]>,
    pressed: Box<[Cell<u64>]>,
    released: Box<[Cell<u64>]>,
    axes: Obj<MoveAxes>,
}

impl InputSnapshot {
    /// A snapshot of `actions` actions, nothing held.
    pub(crate) fn new(actions: usize) -> InputSnapshot {
        let words = || (0..actions.div_ceil(64)).map(|_| Cell::new(0)).collect();
        InputSnapshot {
            held: words(),
            pressed: words(),
            released: words(),
            axes: Obj::new(MoveAxes::default()),
        }
    }

    fn bit(words: &[Cell<u64>], action: Action) -> bool {
        let i = action.0 as usize;
        words
            .get(i / 64)
            .is_some_and(|w| w.get() >> (i % 64) & 1 == 1)
    }

    /// Whether `action` is held.
    pub fn held(&self, action: Action) -> bool {
        Self::bit(&self.held, action)
    }

    /// Whether `action` went down since the previous tick.
    pub fn pressed(&self, action: Action) -> bool {
        Self::bit(&self.pressed, action)
    }

    /// Whether `action` went up since the previous tick.
    pub fn released(&self, action: Action) -> bool {
        Self::bit(&self.released, action)
    }

    /// The move vector.
    pub fn move_axes(&self) -> (f64, f64) {
        (self.axes.x.get(), self.axes.y.get())
    }
}

impl NativeObject for InputSnapshot {
    const PATH: &'static str = "viso::game::InputSnapshot";
}

/// A move vector, behind a borrowed `viso::game::MoveAxes` handle.
#[derive(Debug, Default)]
pub struct MoveAxes {
    x: Cell<f64>,
    y: Cell<f64>,
}

impl NativeObject for MoveAxes {
    const PATH: &'static str = "viso::game::MoveAxes";
}

pub(super) static KEY_SET_METHODS: [NativeFunction; 3] = [
    crate::native!(fn "wasd" |_cx| -> Obj<KeySet> { Ok(Obj::new(KeySet::WASD)) }).constant(),
    crate::native!(fn "arrows" |_cx| -> Obj<KeySet> { Ok(Obj::new(KeySet::ARROWS)) }).constant(),
    crate::native!(fn "of" |_cx, up: Key, left: Key, down: Key, right: Key| -> Obj<KeySet> {
        Ok(Obj::new(KeySet { up, left, down, right }))
    })
    .constant(),
];

pub(super) static INPUT_MAP_METHODS: [NativeFunction; 6] = [
    crate::native!(fn "new" |_cx| -> Obj<InputMap> {
        Ok(Obj::new(InputMap { bindings: InputBindings::new() }))
    })
    .constant(),
    crate::native!(fn "key" |_cx, this: Obj<InputMap>, key: Key, action: Action| -> Obj<InputMap> {
        Ok(edited(&this, |b| b.keys.push((key, action))))
    })
    .constant(),
    crate::native!(fn "pad" |_cx, this: Obj<InputMap>, button: PadButton, action: Action| -> Obj<InputMap> {
        Ok(edited(&this, |b| b.pads.push((button, action))))
    })
    .constant(),
    crate::native!(fn "touch" |_cx, this: Obj<InputMap>, button: TouchButton, action: Action| -> Obj<InputMap> {
        Ok(edited(&this, |b| b.touches.push((button, action))))
    })
    .constant(),
    crate::native!(fn "move_axes" |_cx, this: Obj<InputMap>, keys: Obj<KeySet>, stick: PadStick| -> Obj<InputMap> {
        let keys = *keys;
        Ok(edited(&this, |b| b.move_axes = Some(MoveSource { keys, stick })))
    })
    .constant(),
    crate::native!(fn "dead_zone" |_cx, this: Obj<InputMap>, radius: f64| -> Obj<InputMap> {
        if !(0.0..1.0).contains(&radius) {
            return Err(NativeError::new(format!(
                "a dead zone is a radius in [0, 1), not {radius}"
            )));
        }
        Ok(edited(&this, |b| b.dead_zone = radius))
    })
    .constant(),
];

pub(super) static INPUT_SNAPSHOT_METHODS: [NativeFunction; 5] = [
    crate::native!(fn "pressed" |_cx, this: Obj<InputSnapshot>, action: Action| -> bool {
        Ok(this.pressed(action))
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "released" |_cx, this: Obj<InputSnapshot>, action: Action| -> bool {
        Ok(this.released(action))
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "held" |_cx, this: Obj<InputSnapshot>, action: Action| -> bool {
        Ok(this.held(action))
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "axis" |_cx, this: Obj<InputSnapshot>, axis: InputAxis| -> f64 {
        let (x, z) = this.move_axes();
        Ok(match axis {
            InputAxis::MoveX => x,
            InputAxis::MoveZ => z,
        })
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "move_axes" |_cx, this: Obj<InputSnapshot>| -> Obj<MoveAxes> {
        Ok(this.axes.clone())
    })
    .deterministic()
    .realtime_safe(),
];

pub(super) static MOVE_AXES_METHODS: [NativeFunction; 4] = [
    crate::native!(fn "x" |_cx, this: Obj<MoveAxes>| -> f64 { Ok(this.x.get()) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(fn "y" |_cx, this: Obj<MoveAxes>| -> f64 { Ok(this.y.get()) })
        .deterministic()
        .realtime_safe()
        .property(),
    crate::native!(fn "length" |_cx, this: Obj<MoveAxes>| -> f64 {
        Ok(this.x.get().hypot(this.y.get()))
    })
    .deterministic()
    .realtime_safe(),
    crate::native!(fn "relative_to" |_cx, this: Obj<MoveAxes>, yaw: f64| -> Obj<MoveAxes> {
        let (sin, cos) = yaw.sin_cos();
        let (x, y) = (this.x.get(), this.y.get());
        Ok(Obj::new(MoveAxes {
            x: Cell::new(x * cos - y * sin),
            y: Cell::new(x * sin + y * cos),
        }))
    }),
];
