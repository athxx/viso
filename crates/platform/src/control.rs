//! Loop control, window identity, and configuration types (§9, §11.1).
//!
//! These are the values that cross the platform↔runtime boundary *besides*
//! events: the identity of a window, how to create one, what the runtime tells
//! the pump to do next, and how creation can fail. None of them reference a
//! runtime type — the platform layer stays the bottom of the DAG.

use core::time::Duration;

/// Opaque, process-stable identity of a native window.
///
/// Assigned by the platform layer at window creation and echoed back on every
/// event that targets that window. Distinct from [`crate::SurfaceId`], which is
/// the GPU-surface identity; in Phase 1 they map 1:1, but the split lets a
/// window later host several surfaces (§17) without churning event routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u32);

/// What the pump should do after delivering the current event batch.
///
/// The runtime returns this from [`crate::AppHandler::handle`]; the backend
/// blocks, spins, sleeps, or exits accordingly. Mirrors makepad's
/// `EventFlow { Poll, Wait, Exit }`, extended with a deadline variant so timers
/// need no busy-poll.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlFlow {
    /// A frame is pending — do not block; process the next event immediately.
    Poll,
    /// Nothing is pending — block until the OS delivers the next event.
    Wait,
    /// Block until the OS delivers an event or this deadline elapses.
    WaitUntil(std::time::Instant),
    /// Tear down the pump and return from `run`.
    Exit,
}

/// How to create the initial (or an additional) window.
///
/// Sizes are in logical points; the backend applies the display scale factor.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// Title bar text.
    pub title: String,
    /// Inner (content) size in logical points.
    pub logical_size: (f64, f64),
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Viso".to_string(),
            logical_size: (800.0, 600.0),
        }
    }
}

/// Why the platform layer could not satisfy a request.
///
/// Kept deliberately coarse: the runtime reacts to the *category*, not to
/// OS-specific error codes, which stay in the backend as the `detail` string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformError {
    /// No native backend is compiled/available for this target.
    NoBackend,
    /// The OS refused to create a window or surface.
    WindowCreation(String),
    /// A backend call failed for a reason the runtime cannot act on.
    Backend(String),
}

impl core::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PlatformError::NoBackend => write!(f, "no native platform backend for this target"),
            PlatformError::WindowCreation(d) => write!(f, "window creation failed: {d}"),
            PlatformError::Backend(d) => write!(f, "platform backend error: {d}"),
        }
    }
}

impl std::error::Error for PlatformError {}

/// Convenience: the animation-frame budget the backend targets when polling.
///
/// Backends without a real display link (headless) use this to pace synthetic
/// redraw beats so tests and idle-cost benches stay deterministic.
pub const DEFAULT_FRAME_BUDGET: Duration = Duration::from_micros(16_666);
