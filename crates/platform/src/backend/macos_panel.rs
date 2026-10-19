//! A window that shows frames drawn on the CPU: the software raster's pixels
//! handed to the content view's layer as an image, one copy a frame. A
//! CPU-drawn build shows in a Mac window this way, and with `emulate` the
//! window stands in for an embedded panel: its size, pixel format (RGB565
//! drawn as RGB565 and widened only for display, so the banding shows) and
//! scale are the target display's, each panel pixel a square of screen
//! pixels.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use objc2::Message;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_app_kit::NSView;
use objc2_foundation::NSString;
use viso_handle::{GbmTarget, Scanout, ScanoutFormat};

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    static kCGColorSpaceSRGB: *const c_void;
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut c_void;
    fn CGColorSpaceRelease(space: *mut c_void);
    fn CGDataProviderCreateWithCFData(data: *const c_void) -> *mut c_void;
    fn CGDataProviderRelease(provider: *mut c_void);
    #[allow(clippy::too_many_arguments, reason = "the C signature")]
    fn CGImageCreate(
        width: usize,
        height: usize,
        bits_per_component: usize,
        bits_per_pixel: usize,
        bytes_per_row: usize,
        space: *mut c_void,
        bitmap_info: u32,
        provider: *mut c_void,
        decode: *const f64,
        interpolate: bool,
        intent: i32,
    ) -> *mut c_void;
    fn CGImageRelease(image: *mut c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFDataCreate(allocator: *const c_void, bytes: *const u8, length: isize) -> *const c_void;
    fn CFRelease(cf: *const c_void);
}

/// `kCGBitmapByteOrder32Little | kCGImageAlphaNoneSkipFirst`: B, G, R, X in
/// memory, the raster's `Bgra8Unorm` with its alpha ignored.
const BGRX: u32 = (2 << 12) | 6;
/// `kCGRenderingIntentDefault`.
const INTENT_DEFAULT: i32 = 0;

/// The frames of one window's content view.
pub(crate) struct LayerScanout {
    view: Retained<NSView>,
    format: ScanoutFormat,
    /// Screen pixels a frame pixel covers along each side: one, or the
    /// panel's zoom when emulating.
    zoom: f64,
    /// Window points to frame pixels: the screen's backing scale over the
    /// zoom.
    scale: Cell<f64>,
    /// The scale the window reports to the app when it is not the screen's:
    /// the panel's, when emulating.
    app_scale: Option<f64>,
    /// The last frame, B, G, R, X, kept so a frame that redraws part of the
    /// window changes only that part.
    frame: RefCell<Frame>,
}

#[derive(Default)]
struct Frame {
    width: usize,
    height: usize,
    bgrx: Vec<u8>,
}

impl LayerScanout {
    /// The frames of `view`, on a screen of `backing` scale, each frame pixel
    /// `zoom` screen pixels a side, reporting `app_scale` in place of the
    /// screen's when it is set.
    pub(crate) fn new(
        view: &NSView,
        format: ScanoutFormat,
        backing: f64,
        zoom: f64,
        app_scale: Option<f64>,
    ) -> LayerScanout {
        view.setWantsLayer(true);
        // SAFETY: a layer-backed view's `layer` is its live `CALayer`; setting
        // the magnification filter takes an `NSString` filter name.
        unsafe {
            let layer: *mut AnyObject = msg_send![view, layer];
            if !layer.is_null() {
                let nearest = NSString::from_str("nearest");
                let _: () = msg_send![layer, setMagnificationFilter: &*nearest];
            }
        }
        LayerScanout {
            view: view.retain(),
            format,
            zoom,
            scale: Cell::new(backing / zoom),
            app_scale,
            frame: RefCell::new(Frame::default()),
        }
    }

    /// The window moved to a screen of `backing` scale.
    pub(crate) fn rescale(&self, backing: f64) {
        self.scale.set(backing / self.zoom);
    }

    /// Screen pixels a frame pixel covers along each side.
    pub(crate) fn zoom(&self) -> f64 {
        self.zoom
    }

    /// What the window reports: the app's scale and the frame's size in
    /// pixels.
    pub(crate) fn metrics(&self) -> (f64, u32, u32) {
        let (w, h) = self.size();
        (self.app_scale.unwrap_or(self.scale.get()), w, h)
    }

    /// The app's logical units in one window point.
    pub(crate) fn logical_per_point(&self) -> f64 {
        self.app_scale.map_or(1.0, |app| self.scale.get() / app)
    }

    /// Widens the damaged rows of `pixels` into the kept frame.
    fn take(&self, pixels: &[u8], stride: usize, damage: &[(u32, u32, u32, u32)]) {
        let bpp = match self.format {
            ScanoutFormat::Xrgb8888 => 4,
            ScanoutFormat::Rgb565 => 2,
        };
        if stride == 0 {
            return;
        }
        let (width, height) = (stride / bpp, pixels.len() / stride);
        let mut f = self.frame.borrow_mut();
        let whole = (0, 0, width as u32, height as u32);
        let resized = (f.width, f.height) != (width, height);
        if resized {
            *f = Frame {
                width,
                height,
                bgrx: vec![0; width * height * 4],
            };
        }
        let rects: &[(u32, u32, u32, u32)] = if resized || damage.is_empty() {
            std::slice::from_ref(&whole)
        } else {
            damage
        };
        for &(x, y, w, h) in rects {
            let x0 = (x as usize).min(width);
            let x1 = (x as usize + w as usize).min(width);
            let y1 = (y as usize + h as usize).min(height);
            for row in (y as usize).min(height)..y1 {
                let src = &pixels[row * stride..][x0 * bpp..x1 * bpp];
                let dst = &mut f.bgrx[(row * width + x0) * 4..(row * width + x1) * 4];
                match self.format {
                    ScanoutFormat::Xrgb8888 => dst.copy_from_slice(src),
                    ScanoutFormat::Rgb565 => widen_565(src, dst),
                }
            }
        }
    }

    /// Hands the kept frame to the layer as its contents.
    fn show(&self) {
        let f = self.frame.borrow();
        if f.width == 0 || f.height == 0 {
            return;
        }
        // SAFETY: CoreGraphics and CoreFoundation calls on objects created
        // here and released once below; `CFDataCreate` copies the frame, so
        // the image owns its pixels. The layer retains the image it is given.
        unsafe {
            let data = CFDataCreate(std::ptr::null(), f.bgrx.as_ptr(), f.bgrx.len() as isize);
            if data.is_null() {
                return;
            }
            let provider = CGDataProviderCreateWithCFData(data);
            let space = CGColorSpaceCreateWithName(kCGColorSpaceSRGB);
            let image = CGImageCreate(
                f.width,
                f.height,
                8,
                32,
                f.width * 4,
                space,
                BGRX,
                provider,
                std::ptr::null(),
                false,
                INTENT_DEFAULT,
            );
            if !image.is_null() {
                let layer: *mut AnyObject = msg_send![&*self.view, layer];
                if !layer.is_null() {
                    let _: () = msg_send![layer, setContents: image.cast::<AnyObject>()];
                }
                CGImageRelease(image);
            }
            CGColorSpaceRelease(space);
            CGDataProviderRelease(provider);
            CFRelease(data);
        }
    }
}

impl Scanout for LayerScanout {
    fn size(&self) -> (u32, u32) {
        let size = self.view.frame().size;
        let scale = self.scale.get();
        (
            (size.width * scale).round().max(1.0) as u32,
            (size.height * scale).round().max(1.0) as u32,
        )
    }

    fn format(&self) -> ScanoutFormat {
        self.format
    }

    fn gbm(&self) -> Option<GbmTarget> {
        None
    }

    fn turns(&self) -> u8 {
        0
    }

    fn present_pixels(&self, pixels: &[u8], stride: usize, damage: &[(u32, u32, u32, u32)]) {
        self.take(pixels, stride, damage);
        self.show();
    }

    fn presented(&self, _damage: &[(u32, u32, u32, u32)]) {}
}

/// How a CPU-drawn window stands in for an embedded panel: the frame's size,
/// pixel format and scale, and the screen pixels each frame pixel covers.
pub(crate) struct Emulation {
    pub(crate) size: (u32, u32),
    pub(crate) format: ScanoutFormat,
    pub(crate) scale: f64,
    pub(crate) zoom: f64,
    /// The panel, for the window's title.
    pub(crate) note: String,
}

/// The emulation a window of `cpu_frames` gets on a main screen of `backing`
/// scale with `room` points free: the configured panel's, with the `emulate`
/// feature.
pub(crate) fn emulation(cpu_frames: bool, backing: f64, room: (f64, f64)) -> Option<Emulation> {
    #[cfg(feature = "emulate")]
    if cpu_frames {
        let panel = Panel::configured();
        return Some(Emulation {
            size: panel.size,
            format: panel.format,
            scale: panel.scale,
            zoom: panel.zoom((room.0 * backing, room.1 * backing)),
            note: panel.describe(),
        });
    }
    let _ = (cpu_frames, backing, room);
    None
}

/// The panel a CPU-drawn window stands in for when emulating
/// ([`crate::panel`]): `VISO_KMS_MODE` else 800×480, turned by
/// `VISO_KMS_ROTATION` as it is mounted, in `VISO_KMS_FORMAT` else XRGB8888,
/// at `VISO_SCALE` else 1.
#[cfg(feature = "emulate")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Panel {
    /// The image's size in pixels: the mode's, sides swapped for a quarter
    /// turn.
    pub(crate) size: (u32, u32),
    pub(crate) format: ScanoutFormat,
    pub(crate) scale: f64,
    /// Quarter turns the panel is mounted at.
    pub(crate) turns: u8,
}

#[cfg(feature = "emulate")]
impl Panel {
    /// The panel the build and the environment configure.
    pub(crate) fn configured() -> Panel {
        use crate::panel::{parse_format, parse_turns, setting};
        Panel::from_settings(
            setting("VISO_KMS_MODE", option_env!("VISO_KMS_MODE")).as_deref(),
            setting("VISO_KMS_FORMAT", option_env!("VISO_KMS_FORMAT"))
                .as_deref()
                .and_then(parse_format),
            setting("VISO_KMS_ROTATION", option_env!("VISO_KMS_ROTATION"))
                .as_deref()
                .and_then(parse_turns),
            crate::panel::scale(),
        )
    }

    fn from_settings(
        mode: Option<&str>,
        format: Option<ScanoutFormat>,
        turns: Option<u8>,
        scale: Option<f64>,
    ) -> Panel {
        let (w, h) = mode
            .and_then(crate::panel::parse_mode)
            .map_or((800, 480), |((w, h), _)| (u32::from(w), u32::from(h)));
        let turns = turns.unwrap_or(0);
        Panel {
            size: if turns % 2 == 1 { (h, w) } else { (w, h) },
            format: format.unwrap_or(ScanoutFormat::Xrgb8888),
            scale: scale.unwrap_or(1.0),
            turns,
        }
    }

    /// Screen pixels a panel pixel covers along each side: the most whole
    /// pixels that fit the panel in `room` screen pixels, from 1 to 8.
    pub(crate) fn zoom(&self, room: (f64, f64)) -> f64 {
        let fit = (room.0 / f64::from(self.size.0)).min(room.1 / f64::from(self.size.1));
        fit.floor().clamp(1.0, 8.0)
    }

    /// The window title's note of the panel: `800×480 rgb565 90° @1`.
    pub(crate) fn describe(&self) -> String {
        let format = match self.format {
            ScanoutFormat::Xrgb8888 => "xrgb8888",
            ScanoutFormat::Rgb565 => "rgb565",
        };
        format!(
            "{}×{} {format} {}° @{}",
            self.size.0,
            self.size.1,
            u32::from(self.turns) * 90,
            self.scale
        )
    }
}

/// RGB565 widened to B, G, R, X, each channel's high bits repeated into its
/// low ones so white stays white.
fn widen_565(src: &[u8], dst: &mut [u8]) {
    for (p, out) in src
        .as_chunks::<2>()
        .0
        .iter()
        .zip(dst.as_chunks_mut::<4>().0)
    {
        let v = u16::from_le_bytes(*p);
        let (r, g, b) = ((v >> 11) & 31, (v >> 5) & 63, v & 31);
        out[0] = ((b << 3) | (b >> 2)) as u8;
        out[1] = ((g << 2) | (g >> 4)) as u8;
        out[2] = ((r << 3) | (r >> 2)) as u8;
        out[3] = 0xff;
    }
}

#[cfg(test)]
mod tests {
    use super::widen_565;

    #[cfg(feature = "emulate")]
    #[test]
    fn a_panel_takes_its_mode_turned_as_mounted() {
        use super::Panel;
        use viso_handle::ScanoutFormat;
        let panel = Panel::from_settings(
            Some("1024x600@60"),
            Some(ScanoutFormat::Rgb565),
            Some(1),
            Some(1.5),
        );
        assert_eq!(panel.size, (600, 1024));
        assert_eq!(panel.describe(), "600×1024 rgb565 90° @1.5");
        assert_eq!(
            Panel::from_settings(None, None, None, None).size,
            (800, 480)
        );
        assert_eq!(panel.zoom((3000.0, 1800.0)), 1.0);
        assert_eq!(panel.zoom((1300.0, 2100.0)), 2.0);
        assert_eq!(panel.zoom((100.0, 100.0)), 1.0);
    }

    #[test]
    fn rgb565_widens_to_full_range() {
        let mut out = [0u8; 12];
        let src = [0xffff_u16, 0xf800, 0x07e0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>();
        widen_565(&src, &mut out);
        assert_eq!(&out[0..4], &[255, 255, 255, 255], "white");
        assert_eq!(&out[4..8], &[0, 0, 255, 255], "red");
        assert_eq!(&out[8..12], &[0, 255, 0, 255], "green");
    }
}
