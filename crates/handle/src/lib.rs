//! `viso-handle` — the shared native-window-handle vocabulary (DAG leaf).
//!
//! A single type, [`RawWindowHandle`], produced by `viso-platform`
//! ([`Window::raw_handle`](../viso_platform/trait.Window.html)) and consumed by
//! `viso-gpu` ([`GpuBackend::create_surface`]) to build a swapchain. It lives in
//! its own leaf crate so both can name it without `viso-gpu` depending on
//! `viso-platform` directly.
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

    /// Linux/X11 through Xlib: the connection and the window XID
    /// (`VK_KHR_xlib_surface`).
    Xlib {
        /// `*mut Display` — the Xlib connection owning the window.
        display: *mut c_void,
        /// X11 window id.
        window: u64,
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

    /// No native surface — the headless backend renders into a CPU framebuffer.
    Headless,
}
