//! `viso-handle` — the shared native-handle vocabulary (DAG leaf).
//!
//! [`RawWindowHandle`], produced by `viso-platform`
//! ([`Window::raw_handle`](../viso_platform/trait.Window.html)) and consumed by
//! `viso-gpu` ([`GpuBackend::create_surface`]) to build a swapchain, and
//! [`ExternalImage`], a picture another framework wrote into memory the GPU
//! samples in place — a video frame from `viso-media` — which a backend
//! adopts as a texture. They live in their own leaf crate so producer and
//! consumer can name them without depending on each other.
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
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

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

/// A picture held in memory a GPU samples in place, as the framework that
/// wrote it hands it over: no texel is copied to show it.
///
/// The handle is valid only while what it came from is (a video frame, until
/// the player's next); a backend that keeps showing it takes its own
/// reference, and keeps the producer from writing into it again until no
/// frame in flight samples it.
#[derive(Debug, Clone, Copy)]
pub enum ExternalImage {
    /// Apple: an `IOSurfaceRef` of one plane of four-byte texels — blue,
    /// green, red, alpha — sRGB-encoded and opaque.
    IoSurface {
        /// `IOSurfaceRef`.
        surface: *mut c_void,
        /// Its width in texels.
        width: u32,
        /// Its height in texels.
        height: u32,
    },
    /// Windows: a shared Direct3D texture of four-byte texels — blue, green,
    /// red, alpha (`DXGI_FORMAT_B8G8R8A8_UNORM`) — sRGB-encoded and opaque,
    /// written by another device on the same adapter. A backend waits on
    /// the GPU for `fence` to reach `value` before it reads the texture, and
    /// holds `holds` while any frame in flight does.
    DxgiShared {
        /// An NT handle to the texture, borrowed.
        texture: *mut c_void,
        /// An NT handle to the shared fence its writer signals, borrowed.
        fence: *mut c_void,
        /// The fence value the picture is written by.
        value: u64,
        /// The holds on the texture: `Arc::as_ptr` of an
        /// `Arc<ExternalHolds>` that lives as long as the texture, which
        /// also names it uniquely while it lives.
        holds: *const ExternalHolds,
        /// Its width in texels.
        width: u32,
        /// Its height in texels.
        height: u32,
    },
    /// Android: an `AHardwareBuffer` a decoder wrote, in a Y′CbCr format of
    /// the device's (or RGBA), for a GPU to sample; the picture is the
    /// `width` × `height` region at (`x`, `y`). Written by the time it is
    /// handed over; a backend holds `holds` while any frame in flight reads
    /// it.
    AndroidHardwareBuffer {
        /// `AHardwareBuffer *`, borrowed.
        buffer: *mut c_void,
        /// The holds on the buffer, as for [`ExternalImage::DxgiShared`].
        holds: *const ExternalHolds,
        /// The picture's left edge in the buffer, in texels.
        x: u32,
        /// The picture's top edge in the buffer, in texels.
        y: u32,
        /// Its width in texels.
        width: u32,
        /// Its height in texels.
        height: u32,
    },
    /// The web: the picture an `HTMLVideoElement` shows now, which the
    /// browser keeps on the GPU. A backend copies it there into a texture of
    /// its own, never through the CPU.
    WebVideo {
        /// The wasm-bindgen heap index of the `JsValue` the element is held
        /// in, borrowed.
        element: u32,
        /// Its width in texels.
        width: u32,
        /// Its height in texels.
        height: u32,
    },
}

impl ExternalImage {
    /// Its size in texels.
    pub fn size(&self) -> (u32, u32) {
        match *self {
            ExternalImage::IoSurface { width, height, .. }
            | ExternalImage::DxgiShared { width, height, .. }
            | ExternalImage::AndroidHardwareBuffer { width, height, .. }
            | ExternalImage::WebVideo { width, height, .. } => (width, height),
        }
    }
}

/// How many users hold an external image's memory: its producer writes into
/// it again only once none does.
#[derive(Debug, Default)]
pub struct ExternalHolds {
    count: AtomicU32,
}

impl ExternalHolds {
    pub fn new() -> Arc<ExternalHolds> {
        Arc::default()
    }

    /// Whether anyone holds it.
    pub fn is_held(&self) -> bool {
        self.count.load(Ordering::Acquire) != 0
    }

    /// The `Arc` the pointer an [`ExternalImage`] carries came from.
    ///
    /// # Safety
    ///
    /// `holds` is `Arc::as_ptr` of an `Arc<ExternalHolds>` alive for the
    /// call, as an image's producer promises while it hands it over.
    pub unsafe fn retain(holds: *const ExternalHolds) -> Arc<ExternalHolds> {
        // SAFETY: the caller's contract: the pointer is a live `Arc`'s, whose
        // strong count is raised for the one made here.
        unsafe {
            Arc::increment_strong_count(holds);
            Arc::from_raw(holds)
        }
    }

    /// One more hold, let go when dropped.
    pub fn hold(self: &Arc<ExternalHolds>) -> ExternalHold {
        self.count.fetch_add(1, Ordering::Relaxed);
        ExternalHold(Arc::clone(self))
    }
}

/// A hold on an external image's memory, let go when dropped.
#[derive(Debug)]
pub struct ExternalHold(Arc<ExternalHolds>);

impl Drop for ExternalHold {
    fn drop(&mut self) {
        // Whatever the holder read is done before the producer writes again.
        self.0.count.fetch_sub(1, Ordering::Release);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_count_their_holders_through_the_pointer_an_image_carries() {
        let holds = ExternalHolds::new();
        assert!(!holds.is_held());
        // SAFETY: the pointer is a live `Arc`'s.
        let retained = unsafe { ExternalHolds::retain(Arc::as_ptr(&holds)) };
        assert_eq!(Arc::strong_count(&holds), 2);
        let (a, b) = (retained.hold(), holds.hold());
        assert!(holds.is_held());
        drop(a);
        assert!(holds.is_held(), "one is still held");
        drop(b);
        assert!(!holds.is_held());
        drop(retained);
        assert_eq!(Arc::strong_count(&holds), 1);
    }
}
