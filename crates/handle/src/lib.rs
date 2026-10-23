//! `viso-handle` — the shared native-window-handle vocabulary (DAG leaf).
//!
//! A single type, [`RawWindowHandle`], produced by `viso-platform`
//! ([`Window::raw_handle`](../viso_platform/trait.Window.html)) and consumed by
//! `viso-gpu` ([`GpuBackend::create_surface`]) to build a swapchain. It lives in
//! its own leaf crate so both can name it without `viso-gpu` depending on
//! `viso-platform` directly.
//!
//! The one non-data variant is [`RawWindowHandle::Scanout`]: a display with
//! no compositor is presented to through the platform's page flips, so the
//! handle carries the [`Scanout`] the backend hands its frames to.
//!
//! Deliberately minimal and dependency-free: we do NOT pull in the
//! `raw-window-handle` crate. Viso's RHI is the sole consumer of these handles,
//! so a small owned enum avoids coupling the whole workspace to that crate's
//! version cadence. Each variant carries exactly what the corresponding backend
//! needs to attach a drawable layer.

#![forbid(unsafe_op_in_unsafe_fn)]

use core::ffi::c_void;

/// An OS-native handle to a window's drawable surface.
///
/// The pointer variants are valid only for as long as the originating window
/// lives; the GPU layer must not outlive it.
#[derive(Debug, Clone, Copy)]
pub enum RawWindowHandle {
    /// macOS/AppKit: pointer to the window's content `NSView`.
    ///
    /// The Metal backend sets `wantsLayer = YES` and attaches a `CAMetalLayer`
    /// as this view's backing layer.
    AppKit {
        /// `*mut NSView` — the window's content view.
        ns_view: *mut c_void,
    },

    /// iOS/UIKit: pointer to the root `UIView`.
    ///
    /// The Metal backend adds a `CAMetalLayer` sublayer sized to the view.
    UiKit {
        /// `*mut UIView` — the view that hosts the drawable layer.
        ui_view: *mut c_void,
    },

    /// Windows/Win32: the window `HWND` and its module `HINSTANCE`.
    Win32 {
        /// `HWND` as a raw pointer.
        hwnd: *mut c_void,
        /// `HINSTANCE` of the module that registered the window class.
        hinstance: *mut c_void,
    },

    /// Linux/X11 through XCB: the connection and the window XID
    /// (`VK_KHR_xcb_surface`).
    Xcb {
        /// `*mut xcb_connection_t` — the connection owning the window.
        connection: *mut c_void,
        /// X11 window id.
        window: u32,
    },

    /// Linux/Wayland: the connection and the `wl_surface`
    /// (`VK_KHR_wayland_surface`).
    Wayland {
        /// `*mut wl_display`.
        display: *mut c_void,
        /// `*mut wl_surface` of the toplevel.
        surface: *mut c_void,
    },

    /// Android: the `ANativeWindow` backing the activity's surface
    /// (`VK_KHR_android_surface`). Valid between surface-created and
    /// surface-destroyed.
    AndroidNdk {
        /// `*mut ANativeWindow`.
        a_native_window: *mut c_void,
    },

    /// Web: the `<canvas>` element the platform layer tags with
    /// `data-viso-canvas="{canvas_id}"` and keeps in the document for the
    /// window's lifetime.
    WebCanvas {
        /// The canvas's `data-viso-canvas` attribute value.
        canvas_id: u32,
    },

    /// Linux with no compositor: the display itself, scanned out through
    /// KMS. The platform owns the planes, the buffers and the page
    /// flips; the backend draws a frame and hands it over through the
    /// [`Scanout`].
    Scanout {
        /// The window's scanout, alive as long as the window.
        scanout: *const dyn Scanout,
    },

    /// No native surface — the headless backend renders into a CPU framebuffer.
    Headless,
}

/// The pixel layouts a scanout buffer is in, both little-endian as DRM
/// names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanoutFormat {
    /// `DRM_FORMAT_XRGB8888`: bytes B, G, R, X.
    Xrgb8888,
    /// `DRM_FORMAT_RGB565`: red in the high five bits.
    Rgb565,
}

/// The GBM device and surface a GPU backend renders a [`Scanout`] through
/// (`EGL_PLATFORM_GBM_KHR`).
#[derive(Debug, Clone, Copy)]
pub struct GbmTarget {
    /// `*mut gbm_device`.
    pub device: *mut c_void,
    /// `*mut gbm_surface`, of the scanout's size and format.
    pub surface: *mut c_void,
}

/// A display a backend presents to by handing over finished frames; the
/// platform turns each into a page flip. Every method is called on the
/// thread that runs the platform's loop, between its events.
///
/// Damage rects are `(x, y, w, h)` in pixels from the top-left: what changed
/// since the frame presented before. An empty slice is the whole frame.
pub trait Scanout {
    /// The mode's size in pixels.
    fn size(&self) -> (u32, u32);
    /// The layout of the buffers scanned out.
    fn format(&self) -> ScanoutFormat;
    /// The GBM surface to render into, when the frame comes from a GPU; `None`
    /// when the display takes CPU pixels ([`present_pixels`](Self::present_pixels)).
    fn gbm(&self) -> Option<GbmTarget>;
    /// Quarter turns counter-clockwise a GPU draws each frame with, onto a
    /// GBM surface the panel's shape (`size` with its sides swapped for an
    /// odd count): non-zero only when the panel is turned and the display
    /// cannot turn the image itself. Damage stays in the image's
    /// coordinates. A CPU frame is always the image's shape.
    fn turns(&self) -> u8;
    /// A CPU-drawn frame: `pixels` in [`format`](Self::format), row-major from
    /// the top, `stride` bytes a row. The scanout copies what it needs before
    /// returning.
    fn present_pixels(&self, pixels: &[u8], stride: usize, damage: &[(u32, u32, u32, u32)]);
    /// A GPU-drawn frame was swapped into the GBM surface.
    fn presented(&self, damage: &[(u32, u32, u32, u32)]);
}
