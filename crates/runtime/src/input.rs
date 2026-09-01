//! Runtime-tier normalized input: the sample the scheduler hands the driver.
//!
//! Raw platform events arrive in logical points and are not yet resolved to a
//! coordinate space the UI tree can hit-test. The scheduler owns the window, so
//! it is the one place that can read the window scale factor; it resolves the
//! scale at the event seam, converts pointer coordinates logical → physical
//! pixels (the same space as node bounds), and hands the driver an
//! `InputSample` that already carries physical-space data.
//!
//! These are runtime-tier value types, deliberately small and `Copy` on the
//! pointer path so threading a sample through the driver allocates nothing.
//! They mirror the platform event meaning without re-exporting the platform
//! types, keeping the driver contract independent of the raw event shape.

use viso_platform::WindowId;

/// The lifecycle position of a pointer sample. Mirrors the platform phase, with
/// `Leave` naming the pointer-exited-the-window case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerPhase {
    Down,
    Move,
    Up,
    /// The pointer left the window bounds.
    Leave,
}

/// A pointer sample already resolved to physical pixels (window-top-left
/// origin) — the space node bounds and hit testing use, so no further
/// conversion happens above this point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerSample {
    /// The window the sample belongs to.
    pub window: WindowId,
    /// Position in physical pixels, origin at the window's top-left.
    pub x: f32,
    pub y: f32,
    /// Buttons currently held, as a raw bitmask (matches the UI-tier mask).
    pub buttons: u8,
    /// Keyboard modifier state at the time of the sample.
    pub modifiers: Modifiers,
    pub phase: PointerPhase,
}

/// Keyboard modifier state accompanying an input sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    /// The Command/Windows/Super key.
    pub logo: bool,
}

/// A normalized input sample handed to the driver.
///
/// Only the pointer channel is routed this slice; scroll, key, and text arrive
/// as their own variants when the keyboard/focus/IME subsystem lands. Each new
/// variant carries data already normalized for its channel, so the driver never
/// touches raw platform types or resolves a scale factor itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputSample {
    /// A pointer (mouse/touch/pen) sample in physical pixels.
    Pointer(PointerSample),
}
