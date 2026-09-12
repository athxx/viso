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

/// A rectangle in logical (pre-scale) points, top-left origin.
///
/// The platform layer holds no general geometry vocabulary (§23 keeps it
/// narrow) — events carry bare `f64` fields. This is the one shape that crosses
/// the boundary in both directions for self-drawn chrome: the app pushes
/// draggable caption regions down as a slice of these
/// ([`PlatformApp::set_draggable_regions`](crate::PlatformApp::set_draggable_regions)),
/// and the backend reports the native traffic-light bounding box back up in the
/// same coordinate space (via a chrome-geometry event). Four `f64` — compact
/// data, not a shared object (§29), `Copy` so hit-testing a cached slice in
/// `mouseDown` allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalRect {
    /// Left edge, logical points.
    pub x: f64,
    /// Top edge, logical points (top-left origin).
    pub y: f64,
    /// Width, logical points.
    pub width: f64,
    /// Height, logical points.
    pub height: f64,
}

impl LogicalRect {
    /// A rectangle from its top-left corner and size, in logical points.
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Whether `(px, py)` (logical points, top-left origin) lies inside the
    /// rectangle. Left/top edges are inclusive, right/bottom exclusive — the
    /// half-open convention that keeps adjacent regions from double-hitting.
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }
}

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
    /// Who draws the window chrome (title bar / caption). See [`WindowChrome`].
    pub chrome: WindowChrome,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Viso".to_string(),
            logical_size: (800.0, 600.0),
            chrome: WindowChrome::Native,
        }
    }
}

/// Who owns the window's title bar / caption region.
///
/// [`Native`](Self::Native) keeps the OS-drawn title bar (default, unchanged
/// behavior). [`SelfDrawn`](Self::SelfDrawn) asks the backend for a full-size
/// content area with the native title bar de-decorated (transparent, no title
/// text), so the app paints its own caption content while the OS keeps native
/// affordances that cannot be reproduced (on macOS: the traffic-light buttons).
/// The backend measures those native affordances and reports their geometry so
/// the app can align its caption bar around them; the app in turn declares which
/// regions of its caption are draggable (see
/// [`PlatformApp::set_draggable_regions`](crate::PlatformApp::set_draggable_regions)).
///
/// A backend with no self-drawn-chrome support treats this as `Native`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowChrome {
    /// OS-drawn title bar (default).
    #[default]
    Native,
    /// App-drawn caption over a full-size content area.
    SelfDrawn,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_rect_contains_is_half_open() {
        let r = LogicalRect::new(10.0, 20.0, 100.0, 30.0);
        // Interior.
        assert!(r.contains(10.0, 20.0), "top-left corner is inclusive");
        assert!(r.contains(50.0, 35.0), "an interior point is inside");
        // Right/bottom edges are exclusive.
        assert!(!r.contains(110.0, 35.0), "right edge is exclusive");
        assert!(!r.contains(50.0, 50.0), "bottom edge is exclusive");
        assert!(
            !r.contains(110.0, 50.0),
            "bottom-right corner is exclusive on both axes"
        );
        // Left/top edges just outside.
        assert!(!r.contains(9.999, 35.0), "left of the left edge is outside");
        assert!(!r.contains(50.0, 19.999), "above the top edge is outside");
    }

    #[test]
    fn window_chrome_defaults_to_native() {
        assert_eq!(WindowChrome::default(), WindowChrome::Native);
        assert_eq!(
            WindowConfig::default().chrome,
            WindowChrome::Native,
            "a default window keeps the OS-drawn title bar"
        );
    }
}
