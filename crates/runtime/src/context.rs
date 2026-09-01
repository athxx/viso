//! The runtime-scope context handed to a [`crate::FrameDriver`] (§11.3).
//!
//! `RuntimeCx` is the *UI-agnostic* capability handle: it lets a driver create
//! windows and request redraws through the live platform app, without the
//! runtime ever naming a UI type. The facade wraps this together with a
//! `viso-ui` `AppCx` to give the user a full application context; the runtime
//! itself sees only this.

use viso_platform::{PlatformApp, PlatformError, RawWindowHandle, WindowConfig, WindowId};

/// Capabilities available to a frame driver for the duration of one callback.
///
/// Borrows the platform app mutably, so window creation and redraw requests go
/// straight to the OS backend. Deliberately narrow: no input, no GPU, no state
/// — those belong to per-phase contexts above the runtime.
pub struct RuntimeCx<'a> {
    app: &'a mut dyn PlatformApp,
    /// How many windows the driver created during this callback. The scheduler
    /// reads it back to keep its open-window count accurate.
    windows_created: u32,
    /// The first window created during this callback, if any. The scheduler
    /// reads it to target the launch redraw at the initial window.
    first_window: Option<WindowId>,
}

impl<'a> RuntimeCx<'a> {
    /// Wrap a live platform app. Called by the scheduler per callback.
    pub(crate) fn new(app: &'a mut dyn PlatformApp) -> Self {
        Self {
            app,
            windows_created: 0,
            first_window: None,
        }
    }

    /// How many windows were created through this context so far.
    pub(crate) fn windows_created(&self) -> u32 {
        self.windows_created
    }

    /// The first window created through this context, if any.
    pub(crate) fn first_window(&self) -> Option<WindowId> {
        self.first_window
    }

    /// Create a native window; returns its stable id.
    pub fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError> {
        let id = self.app.create_window(config)?;
        self.windows_created += 1;
        if self.first_window.is_none() {
            self.first_window = Some(id);
        }
        Ok(id)
    }

    /// Ask the platform to schedule a redraw beat for `window`.
    pub fn request_redraw(&mut self, window: WindowId) {
        self.app.request_redraw(window);
    }

    /// The scale factor currently reported for `window`, if it exists.
    pub fn scale_factor(&self, window: WindowId) -> Option<f64> {
        self.app.window(window).map(|w| w.scale_factor())
    }

    /// The physical inner size currently reported for `window`, if it exists.
    pub fn inner_size(&self, window: WindowId) -> Option<(u32, u32)> {
        self.app.window(window).map(|w| w.inner_size())
    }

    /// The OS-native handle for `window` (for GPU surface creation), if it
    /// exists. The facade passes this to [`viso_gpu::GpuBackend::create_surface`]
    /// on launch. The handle borrows the window; use it immediately.
    pub fn raw_handle(&self, window: WindowId) -> Option<RawWindowHandle> {
        self.app.window(window).map(|w| w.raw_handle())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_platform::backend::headless::HeadlessApp;

    #[test]
    fn first_window_records_the_first_created_window() {
        let mut app = HeadlessApp::scripted(vec![]);
        let mut cx = RuntimeCx::new(&mut app);
        assert_eq!(cx.first_window(), None, "no window yet");

        let a = cx.create_window(WindowConfig::default()).unwrap();
        let b = cx.create_window(WindowConfig::default()).unwrap();
        assert_ne!(a, b, "each window gets a distinct id");
        assert_eq!(
            cx.first_window(),
            Some(a),
            "first_window stays the first created, not the latest"
        );
        assert_eq!(cx.windows_created(), 2);
    }
}
