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
    /// as this view's backing layer (mirrors makepad's `setLayer:` path).
    AppKit {
        /// `*mut NSView` — the window's content view.
        ns_view: *mut c_void,
    },

    /// Windows/Win32: the window `HWND`.
    ///
    /// Compile-checked stub in Phase 2; the D3D12 backend consumes it later.
    Win32 {
        /// `HWND` as a raw pointer.
        hwnd: *mut c_void,
    },

    /// Linux/X11: the window XID.
    ///
    /// Compile-checked stub in Phase 2; the Vulkan backend consumes it later.
    Xlib {
        /// X11 window id.
        window: u32,
    },

    /// No native surface — the headless backend renders into a CPU framebuffer.
    Headless,
}
