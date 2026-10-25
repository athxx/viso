//! The display's output: one connector, the CRTC driving it and its primary
//! plane, scanning out the window's frames. It is the window's
//! [`Scanout`]: a CPU-drawn frame is copied — only what changed — into the
//! back of two dumb buffers, a GPU-drawn one is the GBM surface's front
//! buffer, and either goes to the screen in one atomic commit carrying the
//! frame's `FB_DAMAGE_CLIPS`, completed by the page-flip event that paces the
//! next frame.
//!
//! The panel turns through the plane's `rotation` where the hardware takes
//! it; otherwise a CPU-drawn frame is turned as it is copied and a GPU draws
//! turned ([`Scanout::turns`]) into a surface the panel's shape.

use std::cell::{Cell, OnceCell, RefCell};
use std::ffi::c_void;
use std::io;
use std::os::fd::AsRawFd;

use viso_handle::{GbmTarget, Scanout, ScanoutFormat};

use super::drm::{self, Card, ModeInfo, Request};
use super::gbm::Gbm;
use crate::cursor::CursorImage;
use crate::event::CursorIcon;

/// `(x, y, w, h)` in pixels from the top-left.
pub(super) type Rect = (u32, u32, u32, u32);

/// Damage rects kept per buffer before they merge into their bounds.
const MAX_RECTS: usize = 16;

/// The hardware cursor's buffer edge, the size every driver accepts.
const CURSOR: u32 = 64;

const PLANE_TYPE_PRIMARY: u64 = 1;

/// A turn of the panel, as the plane's `rotation` names it: the image is
/// turned counter-clockwise by this much on its way to the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Rotation {
    #[default]
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    pub fn parse(s: &str) -> Option<Rotation> {
        crate::panel::parse_turns(s).map(|turns| {
            [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270][usize::from(turns)]
        })
    }

    fn degrees(self) -> u32 {
        match self {
            Rotation::R0 => 0,
            Rotation::R90 => 90,
            Rotation::R180 => 180,
            Rotation::R270 => 270,
        }
    }

    /// The image's sides swap.
    pub fn quarter(self) -> bool {
        matches!(self, Rotation::R90 | Rotation::R270)
    }

    /// The image size for a panel of `native` pixels.
    pub fn logical(self, (w, h): (u32, u32)) -> (u32, u32) {
        if self.quarter() { (h, w) } else { (w, h) }
    }

    /// Where a point of the image, as fractions `0..=1` of its size, lands on
    /// the panel.
    pub fn to_native(self, u: f64, v: f64) -> (f64, f64) {
        match self {
            Rotation::R0 => (u, v),
            Rotation::R90 => (v, 1.0 - u),
            Rotation::R180 => (1.0 - u, 1.0 - v),
            Rotation::R270 => (1.0 - v, u),
        }
    }

    /// The image point behind a panel point: the inverse of
    /// [`to_native`](Self::to_native).
    pub fn to_logical(self, u: f64, v: f64) -> (f64, f64) {
        match self {
            Rotation::R0 => (u, v),
            Rotation::R90 => (1.0 - v, u),
            Rotation::R180 => (1.0 - u, 1.0 - v),
            Rotation::R270 => (v, 1.0 - u),
        }
    }

    /// The panel pixel of image pixel `(x, y)` in an image of `w`×`h`.
    fn pixel(self, x: u32, y: u32, (w, h): (u32, u32)) -> (u32, u32) {
        match self {
            Rotation::R0 => (x, y),
            Rotation::R90 => (y, w - 1 - x),
            Rotation::R180 => (w - 1 - x, h - 1 - y),
            Rotation::R270 => (h - 1 - y, x),
        }
    }

    /// An image rect's panel rect.
    fn rect(self, (x, y, rw, rh): Rect, (w, h): (u32, u32)) -> Rect {
        match self {
            Rotation::R0 => (x, y, rw, rh),
            Rotation::R90 => (y, w - x - rw, rh, rw),
            Rotation::R180 => (w - x - rw, h - y - rh, rw, rh),
            Rotation::R270 => (h - y - rh, x, rh, rw),
        }
    }
}

/// The property ids an atomic commit sets.
#[derive(Debug, Default, Clone, Copy)]
struct Props {
    connector_crtc: u32,
    mode_id: u32,
    active: u32,
    fb_id: u32,
    crtc_id: u32,
    src: [u32; 4],
    dst: [u32; 4],
    damage: Option<u32>,
    rotation: Option<u32>,
}

/// A mapped dumb buffer and its framebuffer.
struct Dumb {
    handle: u32,
    fb: u32,
    pitch: usize,
    map: *mut u8,
    len: usize,
}

impl Dumb {
    fn new(card: &Card, (w, h): (u32, u32), format: ScanoutFormat) -> io::Result<Dumb> {
        let (bpp, fourcc) = match format {
            ScanoutFormat::Xrgb8888 => (32, drm::FORMAT_XRGB8888),
            ScanoutFormat::Rgb565 => (16, drm::FORMAT_RGB565),
        };
        Dumb::with(card, (w, h), bpp, fourcc)
    }

    fn with(card: &Card, (w, h): (u32, u32), bpp: u32, fourcc: u32) -> io::Result<Dumb> {
        let (handle, pitch, size) = card.create_dumb(w, h, bpp)?;
        let fb = match card.add_fb(w, h, fourcc, handle, pitch) {
            Ok(fb) => fb,
            Err(e) => {
                card.destroy_dumb(handle);
                return Err(e);
            }
        };
        let map = card.map_dumb(handle).and_then(|offset| {
            // SAFETY: mapping the dumb buffer's `size` bytes at the offset the
            // card gave for it, shared with the card; unmapped in `free`.
            let p = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    size as usize,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED,
                    card.as_raw_fd(),
                    offset as libc::off_t,
                )
            };
            if p == libc::MAP_FAILED {
                Err(io::Error::last_os_error())
            } else {
                Ok(p.cast::<u8>())
            }
        });
        let map = match map {
            Ok(map) => map,
            Err(e) => {
                card.remove_fb(fb);
                card.destroy_dumb(handle);
                return Err(e);
            }
        };
        // SAFETY: the mapping is `size` writable bytes; a new dumb buffer
        // starts black.
        unsafe { std::ptr::write_bytes(map, 0, size as usize) };
        Ok(Dumb {
            handle,
            fb,
            pitch: pitch as usize,
            map,
            len: size as usize,
        })
    }

    fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: the live mapping of `len` bytes, borrowed through `self`.
        unsafe { std::slice::from_raw_parts_mut(self.map, self.len) }
    }

    fn free(self, card: &Card) {
        // SAFETY: the mapping `new` made, unmapped once.
        unsafe { libc::munmap(self.map.cast(), self.len) };
        card.remove_fb(self.fb);
        card.destroy_dumb(self.handle);
    }
}

/// The GBM surface a GPU renders the frames into, and the framebuffers made
/// for its buffers.
struct GbmSurface {
    surface: *mut c_void,
    size: (u32, u32),
    fbs: Vec<(*mut c_void, u32)>,
    /// The buffer on the screen.
    front: Option<*mut c_void>,
    /// The buffer a flip in flight puts on the screen.
    next: Option<*mut c_void>,
}

struct Cursor {
    dumb: Dumb,
    icon: Option<CursorIcon>,
    /// The hotspot inside the buffer, in panel orientation.
    hotspot: (i32, i32),
    shown: bool,
}

struct State {
    mode: ModeInfo,
    mode_blob: u32,
    mm_width: u32,
    /// The plane turns the image: its `rotation` value.
    hw_rotation: Option<u64>,
    /// The next commit sets the mode.
    modeset: bool,
    /// The session is in the foreground.
    active: bool,
    connected: bool,
    /// A commit's flip is in flight.
    pending: bool,
    dumb: Vec<Dumb>,
    front: usize,
    /// What each dumb buffer misses of the newest frame.
    stale: [Vec<Rect>; 2],
    gbm: Option<GbmSurface>,
    gbm_device: *mut c_void,
    cursor: Option<Cursor>,
}

/// What a connector change did to the output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Change {
    /// The panel went away; frames are kept, not shown.
    Disconnected,
    /// The panel is back in the same mode.
    Reconnected,
    /// The panel is back in a mode of another size.
    Resized,
}

pub(super) struct Output {
    card: Card,
    connector: u32,
    crtc: u32,
    plane: u32,
    props: Props,
    rotation: Rotation,
    format: ScanoutFormat,
    s: RefCell<State>,
    /// A flip completed since the loop last asked.
    flipped: Cell<bool>,
    gbm: OnceCell<Option<Gbm>>,
}

/// Each named property's id, from an object's property list.
fn prop(props: &[(drm::Property, u64)], name: &str) -> Option<u32> {
    drm::find(props, name).map(|(p, _)| p.id)
}

impl Output {
    /// Sets up the first connected connector of `card`; `None` when none is.
    pub fn open(card: Card) -> io::Result<Option<Output>> {
        let (crtcs, connectors) = card.resources()?;
        let mut found = None;
        for id in connectors {
            let c = card.connector(id)?;
            if c.connected && !c.modes.is_empty() {
                found = Some(c);
                break;
            }
        }
        let Some(connector) = found else {
            return Ok(None);
        };
        let want = super::setting("VISO_KMS_MODE", option_env!("VISO_KMS_MODE"));
        let mode =
            drm::pick_mode(&connector.modes, want.as_deref()).expect("a connector with modes");
        // The CRTC the connector's encoder drives now, else the first one an
        // encoder of it can drive.
        let mut crtc = None;
        if connector.encoder != 0
            && let Ok((current, _)) = card.encoder(connector.encoder)
            && current != 0
        {
            crtc = Some(current);
        }
        if crtc.is_none() {
            'search: for &e in &connector.encoders {
                let Ok((_, possible)) = card.encoder(e) else {
                    continue;
                };
                for (i, &c) in crtcs.iter().enumerate() {
                    if possible & (1 << i) != 0 {
                        crtc = Some(c);
                        break 'search;
                    }
                }
            }
        }
        let Some(crtc) = crtc else {
            return Ok(None);
        };
        let crtc_index = crtcs.iter().position(|&c| c == crtc).unwrap_or(0);
        let want_format = super::setting("VISO_KMS_FORMAT", option_env!("VISO_KMS_FORMAT"));
        let rotation = super::setting("VISO_KMS_ROTATION", option_env!("VISO_KMS_ROTATION"))
            .and_then(|r| Rotation::parse(&r))
            .unwrap_or_default();

        let mut plane = 0;
        let mut props = Props::default();
        let mut formats = vec![drm::FORMAT_XRGB8888, drm::FORMAT_RGB565];
        let mut hw_rotation = None;
        if card.atomic {
            let mut primary = None;
            for p in card.planes()? {
                if p.possible_crtcs & (1 << crtc_index) == 0 {
                    continue;
                }
                let pp = card.plane_properties(p.id)?;
                if drm::find(&pp, "type").is_some_and(|(_, v)| *v == PLANE_TYPE_PRIMARY) {
                    primary = Some((p, pp));
                    break;
                }
            }
            let Some((p, pp)) = primary else {
                return Ok(None);
            };
            let cp = card.connector_properties(connector.id)?;
            let rp = card.crtc_properties(crtc)?;
            let need = |props: &[(drm::Property, u64)], name: &str| {
                prop(props, name).ok_or_else(|| io::Error::other(format!("no `{name}` property")))
            };
            props = Props {
                connector_crtc: need(&cp, "CRTC_ID")?,
                mode_id: need(&rp, "MODE_ID")?,
                active: need(&rp, "ACTIVE")?,
                fb_id: need(&pp, "FB_ID")?,
                crtc_id: need(&pp, "CRTC_ID")?,
                src: [
                    need(&pp, "SRC_X")?,
                    need(&pp, "SRC_Y")?,
                    need(&pp, "SRC_W")?,
                    need(&pp, "SRC_H")?,
                ],
                dst: [
                    need(&pp, "CRTC_X")?,
                    need(&pp, "CRTC_Y")?,
                    need(&pp, "CRTC_W")?,
                    need(&pp, "CRTC_H")?,
                ],
                damage: prop(&pp, "FB_DAMAGE_CLIPS"),
                rotation: prop(&pp, "rotation"),
            };
            if let Some((r, _)) = drm::find(&pp, "rotation") {
                let name = format!("rotate-{}", rotation.degrees());
                hw_rotation = r
                    .enums
                    .iter()
                    .find(|(_, n)| *n == name)
                    .map(|(bit, _)| 1u64 << bit);
            }
            plane = p.id;
            formats = p.formats;
        }
        let format = match want_format.as_deref().and_then(crate::panel::parse_format) {
            Some(ScanoutFormat::Rgb565) if formats.contains(&drm::FORMAT_RGB565) => {
                ScanoutFormat::Rgb565
            }
            _ if formats.contains(&drm::FORMAT_XRGB8888) => ScanoutFormat::Xrgb8888,
            _ if formats.contains(&drm::FORMAT_RGB565) => ScanoutFormat::Rgb565,
            _ => ScanoutFormat::Xrgb8888,
        };
        let mode_blob = if card.atomic {
            card.create_blob(&[mode])?
        } else {
            0
        };
        let output = Output {
            connector: connector.id,
            crtc,
            plane,
            props,
            rotation,
            format,
            s: RefCell::new(State {
                mode,
                mode_blob,
                mm_width: connector.mm_width,
                hw_rotation,
                modeset: true,
                active: true,
                connected: true,
                pending: false,
                dumb: Vec::new(),
                front: 0,
                stale: [Vec::new(), Vec::new()],
                gbm: None,
                gbm_device: std::ptr::null_mut(),
                cursor: None,
            }),
            flipped: Cell::new(false),
            gbm: OnceCell::new(),
            card,
        };
        output.probe_rotation();
        Ok(Some(output))
    }

    pub fn card(&self) -> &Card {
        &self.card
    }

    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    /// The panel's size in pixels.
    pub fn native_size(&self) -> (u32, u32) {
        let s = self.s.borrow();
        (u32::from(s.mode.hdisplay), u32::from(s.mode.vdisplay))
    }

    /// The image's size in pixels: the panel's, turned.
    pub fn logical_size(&self) -> (u32, u32) {
        self.rotation.logical(self.native_size())
    }

    /// The scale: `VISO_SCALE`, else the panel's density.
    pub fn scale(&self) -> f64 {
        if let Some(scale) = crate::panel::scale() {
            return scale;
        }
        let s = self.s.borrow();
        super::translate::monitor_scale(u32::from(s.mode.hdisplay), s.mm_width)
    }

    /// The size frames are drawn into: the image, or the panel when a CPU
    /// frame is turned as it is copied.
    fn fb_size(&self, s: &State) -> (u32, u32) {
        let native = (u32::from(s.mode.hdisplay), u32::from(s.mode.vdisplay));
        if self.software_rotation(s) {
            native
        } else {
            self.rotation.logical(native)
        }
    }

    fn software_rotation(&self, s: &State) -> bool {
        self.rotation != Rotation::R0 && s.hw_rotation.is_none()
    }

    /// Whether a flip is in flight: the next frame waits for it.
    pub fn flip_pending(&self) -> bool {
        self.s.borrow().pending
    }

    /// Whether a flip completed since the last call.
    pub fn take_flipped(&self) -> bool {
        self.flipped.replace(false)
    }

    /// Reads the card's completed flips.
    pub fn read_flips(&self) {
        let mut s = self.s.borrow_mut();
        self.read_events(&mut s);
    }

    fn read_events(&self, s: &mut State) {
        let mut done = false;
        self.card
            .read_events(|flip| done |= flip.crtc == self.crtc || flip.crtc == 0);
        if done {
            self.complete_flip(s);
        }
    }

    fn complete_flip(&self, s: &mut State) {
        s.pending = false;
        self.flipped.set(true);
        if let (Some(g), Some(gbm)) = (&mut s.gbm, self.gbm.get().and_then(Option::as_ref))
            && let Some(next) = g.next.take()
            && let Some(old) = g.front.replace(next)
            && old != next
        {
            // SAFETY: `old` was locked from this surface and is off the screen
            // now that `next` is on it.
            unsafe { gbm.release(g.surface, old) };
        }
    }

    /// Waits out a flip in flight, so the buffer it replaces is free.
    fn wait_flip(&self, s: &mut State) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(100);
        while s.pending {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                // A flip that never completes (a panel gone mid-flip) must not
                // stall the loop.
                self.complete_flip(s);
                return;
            }
            let mut fd = libc::pollfd {
                fd: self.card.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            // SAFETY: polling one live, initialized `pollfd`.
            unsafe { libc::poll(&mut fd, 1, left.as_millis().max(1) as libc::c_int) };
            self.read_events(s);
        }
    }

    /// The atomic request that shows `fb`: with the mode, connector and
    /// plane set up when the next commit sets the mode.
    fn request(&self, s: &State, fb: u32) -> Request {
        let (fw, fh) = self.fb_size(s);
        let (w, h) = (u32::from(s.mode.hdisplay), u32::from(s.mode.vdisplay));
        let p = self.props;
        let mut req = Request::default();
        if s.modeset {
            req.set(self.connector, p.connector_crtc, u64::from(self.crtc));
            req.set(self.crtc, p.mode_id, u64::from(s.mode_blob));
            req.set(self.crtc, p.active, 1);
            req.set(self.plane, p.crtc_id, u64::from(self.crtc));
            let src = [0, 0, u64::from(fw) << 16, u64::from(fh) << 16];
            let dst = [0, 0, u64::from(w), u64::from(h)];
            for (id, v) in p.src.iter().zip(src).chain(p.dst.iter().zip(dst)) {
                req.set(self.plane, *id, v);
            }
            if let Some(r) = p.rotation {
                req.set(self.plane, r, s.hw_rotation.unwrap_or(1));
            }
        }
        req.set(self.plane, p.fb_id, u64::from(fb));
        req
    }

    /// Puts framebuffer `fb` on the screen with `damage` (its coordinates);
    /// `Ok(true)` when a flip event will complete it.
    fn commit(&self, s: &mut State, fb: u32, damage: &[Rect]) -> io::Result<bool> {
        if !self.card.atomic {
            if s.modeset {
                self.card.set_crtc(self.crtc, fb, self.connector, s.mode)?;
                s.modeset = false;
                return Ok(false);
            }
            self.card.page_flip(self.crtc, fb)?;
            return Ok(true);
        }
        let mut req = self.request(s, fb);
        let blob = match self.props.damage {
            Some(id) if !s.modeset && !damage.is_empty() => {
                let blob = self.card.damage_blob(damage).ok();
                if let Some(b) = blob {
                    req.set(self.plane, id, u64::from(b));
                }
                blob
            }
            _ => None,
        };
        let flags = drm::PAGE_FLIP_EVENT
            | if s.modeset {
                drm::ATOMIC_ALLOW_MODESET
            } else {
                drm::ATOMIC_NONBLOCK
            };
        let r = self.card.commit(&req, flags);
        if let Some(b) = blob {
            self.card.destroy_blob(b);
        }
        r?;
        s.modeset = false;
        Ok(true)
    }

    /// Keeps the plane's rotation only if the card accepts it for buffers
    /// of the scanout's format: some planes turn only tiled buffers. The
    /// image is otherwise turned as it is drawn or copied.
    fn probe_rotation(&self) {
        let mut s = self.s.borrow_mut();
        if self.rotation == Rotation::R0 || s.hw_rotation.is_none() {
            return;
        }
        let size = self.fb_size(&s);
        let accepted = Dumb::new(&self.card, size, self.format).is_ok_and(|d| {
            let req = self.request(&s, d.fb);
            let ok = self
                .card
                .commit(&req, drm::ATOMIC_TEST_ONLY | drm::ATOMIC_ALLOW_MODESET)
                .is_ok();
            d.free(&self.card);
            ok
        });
        if !accepted {
            s.hw_rotation = None;
        }
    }

    /// After a commit attempt: a flip to wait for, or a failure that leaves
    /// the next frame to set the mode again.
    fn committed(&self, s: &mut State, r: io::Result<bool>) -> bool {
        match r {
            Ok(true) => {
                s.pending = true;
                true
            }
            Ok(false) => {
                self.complete_flip(s);
                true
            }
            Err(e) => {
                if matches!(e.raw_os_error(), Some(libc::EACCES | libc::EPERM)) {
                    s.active = false;
                }
                s.modeset = true;
                false
            }
        }
    }

    /// The session paused: nothing touches the card until [`resume`](Self::resume).
    pub fn pause(&self) {
        let mut s = self.s.borrow_mut();
        s.active = false;
        s.pending = false;
        s.modeset = true;
    }

    /// The session is back: the mode is set again with the newest frame.
    pub fn resume(&self) {
        let mut s = self.s.borrow_mut();
        s.active = true;
        s.modeset = true;
        self.relight(&mut s);
        if let Some(c) = &mut s.cursor {
            c.shown = false;
        }
    }

    /// Shows the newest frame again, after the panel or the session left.
    fn relight(&self, s: &mut State) {
        if !(s.active && s.connected) {
            return;
        }
        let fb = if let Some(d) = s.dumb.get(s.front) {
            Some(d.fb)
        } else if let Some(front) = s.gbm.as_ref().and_then(|g| g.front) {
            self.gbm_fb(s, front).ok()
        } else {
            None
        };
        if let Some(fb) = fb {
            let r = self.commit(s, fb, &[]);
            self.committed(s, r);
        }
    }

    /// Reads the connector again after a hotplug.
    pub fn reprobe(&self) -> Option<Change> {
        let c = self.card.connector(self.connector).ok()?;
        let mut s = self.s.borrow_mut();
        let connected = c.connected && !c.modes.is_empty();
        if !connected {
            if s.connected {
                s.connected = false;
                s.pending = false;
                s.modeset = true;
                return Some(Change::Disconnected);
            }
            return None;
        }
        let current = s.mode;
        let same = |m: &ModeInfo| {
            (m.hdisplay, m.vdisplay, m.vrefresh, m.clock)
                == (
                    current.hdisplay,
                    current.vdisplay,
                    current.vrefresh,
                    current.clock,
                )
        };
        let was = s.connected;
        s.connected = true;
        s.mm_width = c.mm_width;
        if c.modes.iter().any(same) {
            if was {
                return None;
            }
            s.modeset = true;
            self.relight(&mut s);
            return Some(Change::Reconnected);
        }
        let want = super::setting("VISO_KMS_MODE", option_env!("VISO_KMS_MODE"));
        let mode = drm::pick_mode(&c.modes, want.as_deref())?;
        let resized = (mode.hdisplay, mode.vdisplay) != (s.mode.hdisplay, s.mode.vdisplay);
        if self.card.atomic {
            let Ok(blob) = self.card.create_blob(&[mode]) else {
                return None;
            };
            self.card
                .destroy_blob(std::mem::replace(&mut s.mode_blob, blob));
        }
        s.mode = mode;
        s.modeset = true;
        if !resized {
            self.relight(&mut s);
            return Some(Change::Reconnected);
        }
        // The buffers are the old size: a CPU frame makes new ones, a GPU
        // surface is made anew once the window's surface is.
        for d in std::mem::take(&mut s.dumb) {
            d.free(&self.card);
        }
        s.stale = [Vec::new(), Vec::new()];
        Some(Change::Resized)
    }

    fn gbm_lib(&self) -> Option<&Gbm> {
        self.gbm.get_or_init(Gbm::load).as_ref()
    }

    /// The framebuffer for GBM buffer `bo`, made on first use.
    fn gbm_fb(&self, s: &mut State, bo: *mut c_void) -> io::Result<u32> {
        let gbm = self
            .gbm_lib()
            .ok_or_else(|| io::Error::other("no libgbm"))?;
        let fourcc = match self.format {
            ScanoutFormat::Xrgb8888 => drm::FORMAT_XRGB8888,
            ScanoutFormat::Rgb565 => drm::FORMAT_RGB565,
        };
        let g = s
            .gbm
            .as_mut()
            .ok_or_else(|| io::Error::other("no GBM surface"))?;
        if let Some(&(_, fb)) = g.fbs.iter().find(|(b, _)| *b == bo) {
            return Ok(fb);
        }
        // SAFETY: `bo` is a buffer of the live surface, locked by the caller.
        let (handle, stride) = unsafe { gbm.handle_and_stride(bo) };
        let fb = self
            .card
            .add_fb(g.size.0, g.size.1, fourcc, handle, stride)?;
        g.fbs.push((bo, fb));
        Ok(fb)
    }

    fn free_gbm_surface(&self, s: &mut State) {
        let (Some(g), Some(gbm)) = (s.gbm.take(), self.gbm.get().and_then(Option::as_ref)) else {
            return;
        };
        for (_, fb) in &g.fbs {
            self.card.remove_fb(*fb);
        }
        // SAFETY: the buffers were locked from this surface; the window's GPU
        // surface on it is gone (the window's surface was destroyed before
        // the platform asked for a new one), so it can go too.
        unsafe {
            for bo in g.front.into_iter().chain(g.next) {
                gbm.release(g.surface, bo);
            }
            gbm.destroy_surface(g.surface);
        }
    }

    /// Copies what the back buffer misses of `pixels` into it.
    fn copy_back(&self, s: &mut State, pixels: &[u8], stride: usize) {
        let bpp = match self.format {
            ScanoutFormat::Xrgb8888 => 4,
            ScanoutFormat::Rgb565 => 2,
        };
        let logical = self
            .rotation
            .logical((u32::from(s.mode.hdisplay), u32::from(s.mode.vdisplay)));
        let turn = self.software_rotation(s).then_some(self.rotation);
        let back = 1 - s.front;
        let rects = std::mem::take(&mut s.stale[back]);
        let d = &mut s.dumb[back];
        let pitch = d.pitch;
        let dst = d.bytes();
        for &r in &rects {
            let (x, y, w, h) = clip(r, logical);
            if w == 0 || h == 0 {
                continue;
            }
            for row in y..y + h {
                let src_at = row as usize * stride + x as usize * bpp;
                let Some(src) = pixels.get(src_at..src_at + w as usize * bpp) else {
                    continue;
                };
                match turn {
                    None => {
                        let at = row as usize * pitch + x as usize * bpp;
                        dst[at..at + src.len()].copy_from_slice(src);
                    }
                    Some(turn) => {
                        for (i, px) in src.chunks_exact(bpp).enumerate() {
                            let (nx, ny) = turn.pixel(x + i as u32, row, logical);
                            let at = ny as usize * pitch + nx as usize * bpp;
                            dst[at..at + bpp].copy_from_slice(px);
                        }
                    }
                }
            }
        }
    }

    /// Damage in the image's coordinates, in the framebuffer's.
    fn fb_damage(&self, s: &State, damage: &[Rect]) -> Vec<Rect> {
        let logical = self
            .rotation
            .logical((u32::from(s.mode.hdisplay), u32::from(s.mode.vdisplay)));
        let turn = self.software_rotation(s);
        damage
            .iter()
            .map(|&r| clip(r, logical))
            .filter(|r| r.2 > 0 && r.3 > 0)
            .map(|r| {
                if turn {
                    self.rotation.rect(r, logical)
                } else {
                    r
                }
            })
            .collect()
    }

    /// Shows `icon` as the hardware cursor (`None` hides it).
    pub fn set_cursor(&self, icon: Option<CursorIcon>, scale: f64) {
        let mut s = self.s.borrow_mut();
        if !s.active {
            return;
        }
        let Some(icon) = icon else {
            if let Some(c) = &mut s.cursor
                && c.shown
            {
                let _ = self.card.set_cursor(self.crtc, 0, 0, 0);
                c.shown = false;
            }
            return;
        };
        if s.cursor.is_none() {
            match Dumb::with(&self.card, (CURSOR, CURSOR), 32, drm::FORMAT_ARGB8888) {
                Ok(dumb) => {
                    s.cursor = Some(Cursor {
                        dumb,
                        icon: None,
                        hotspot: (0, 0),
                        shown: false,
                    });
                }
                Err(_) => return,
            }
        }
        let rotation = self.rotation;
        let c = s.cursor.as_mut().expect("made above");
        if c.icon != Some(icon) {
            c.icon = Some(icon);
            let pitch = c.dumb.pitch;
            c.hotspot = draw_cursor(c.dumb.bytes(), pitch, icon, scale, rotation);
        }
        if !c.shown {
            c.shown = self
                .card
                .set_cursor(self.crtc, c.dumb.handle, CURSOR, CURSOR)
                .is_ok();
        }
    }

    /// Moves the hardware cursor's hotspot to panel pixel `(x, y)`.
    pub fn move_cursor(&self, x: f64, y: f64) {
        let s = self.s.borrow();
        if let Some(c) = &s.cursor
            && c.shown
            && s.active
        {
            self.card
                .move_cursor(self.crtc, x as i32 - c.hotspot.0, y as i32 - c.hotspot.1);
        }
    }
}

impl Scanout for Output {
    fn size(&self) -> (u32, u32) {
        self.logical_size()
    }

    fn format(&self) -> ScanoutFormat {
        self.format
    }

    fn turns(&self) -> u8 {
        if !self.software_rotation(&self.s.borrow()) {
            return 0;
        }
        match self.rotation {
            Rotation::R0 => 0,
            Rotation::R90 => 1,
            Rotation::R180 => 2,
            Rotation::R270 => 3,
        }
    }

    fn gbm(&self) -> Option<GbmTarget> {
        let gbm = self.gbm_lib()?;
        let mut s = self.s.borrow_mut();
        if s.gbm_device.is_null() {
            s.gbm_device = gbm.create_device(self.card.as_raw_fd());
            if s.gbm_device.is_null() {
                return None;
            }
        }
        let size = self.fb_size(&s);
        if s.gbm.as_ref().is_some_and(|g| g.size != size) {
            self.wait_flip(&mut s);
            self.free_gbm_surface(&mut s);
        }
        if s.gbm.is_none() {
            let fourcc = match self.format {
                ScanoutFormat::Xrgb8888 => drm::FORMAT_XRGB8888,
                ScanoutFormat::Rgb565 => drm::FORMAT_RGB565,
            };
            // SAFETY: the device made above, alive with the output.
            let surface = unsafe { gbm.create_surface(s.gbm_device, size.0, size.1, fourcc) };
            if surface.is_null() {
                return None;
            }
            s.gbm = Some(GbmSurface {
                surface,
                size,
                fbs: Vec::new(),
                front: None,
                next: None,
            });
        }
        let surface = s.gbm.as_ref().map(|g| g.surface)?;
        Some(GbmTarget {
            device: s.gbm_device,
            surface,
        })
    }

    fn present_pixels(&self, pixels: &[u8], stride: usize, damage: &[Rect]) {
        let mut s = self.s.borrow_mut();
        let s = &mut *s;
        self.wait_flip(s);
        if s.dumb.len() != 2 {
            let size = self.fb_size(s);
            let (Ok(a), Ok(b)) = (
                Dumb::new(&self.card, size, self.format),
                Dumb::new(&self.card, size, self.format),
            ) else {
                return;
            };
            s.dumb = vec![a, b];
            s.front = 0;
            s.stale = [Vec::new(), Vec::new()];
            add_damage(&mut s.stale[0], &[]);
            add_damage(&mut s.stale[1], &[]);
        }
        add_damage(&mut s.stale[0], damage);
        add_damage(&mut s.stale[1], damage);
        self.copy_back(s, pixels, stride);
        let back = 1 - s.front;
        if !(s.active && s.connected) {
            // Nothing is shown: the newest frame waits as the front.
            s.front = back;
            s.modeset = true;
            return;
        }
        let clips = self.fb_damage(s, damage);
        let r = self.commit(s, s.dumb[back].fb, &clips);
        if self.committed(s, r) {
            s.front = back;
        }
    }

    fn presented(&self, damage: &[Rect]) {
        let Some(gbm) = self.gbm.get().and_then(Option::as_ref) else {
            return;
        };
        let mut s = self.s.borrow_mut();
        let s = &mut *s;
        self.wait_flip(s);
        let Some(surface) = s.gbm.as_ref().map(|g| g.surface) else {
            return;
        };
        // SAFETY: the live surface the GPU just swapped.
        let bo = unsafe { gbm.lock_front(surface) };
        if bo.is_null() {
            return;
        }
        let release = |g: &GbmSurface, bo| {
            // SAFETY: `bo` was locked from this surface and is not on the
            // screen.
            unsafe { gbm.release(g.surface, bo) };
        };
        if !(s.active && s.connected) {
            // Nothing is shown: the newest frame waits as the front.
            let g = s.gbm.as_mut().expect("checked above");
            if let Some(old) = g.front.replace(bo) {
                release(g, old);
            }
            s.modeset = true;
            return;
        }
        let fb = match self.gbm_fb(s, bo) {
            Ok(fb) => fb,
            Err(_) => {
                release(s.gbm.as_ref().expect("checked above"), bo);
                return;
            }
        };
        let clips = self.fb_damage(s, damage);
        let r = self.commit(s, fb, &clips);
        if r.is_ok() {
            s.gbm.as_mut().expect("checked above").next = Some(bo);
        }
        if !self.committed(s, r) {
            release(s.gbm.as_ref().expect("checked above"), bo);
        }
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        let mut s = self.s.borrow_mut();
        let s = &mut *s;
        self.wait_flip(s);
        for d in std::mem::take(&mut s.dumb) {
            d.free(&self.card);
        }
        if let Some(c) = s.cursor.take() {
            let _ = self.card.set_cursor(self.crtc, 0, 0, 0);
            c.dumb.free(&self.card);
        }
        self.free_gbm_surface(s);
        // The GBM device stays: a GPU backend's EGL display is made on it and
        // may outlive the window, until the process ends.
        if s.mode_blob != 0 {
            self.card.destroy_blob(s.mode_blob);
        }
    }
}

/// `r` cut to a `w`×`h` frame.
fn clip((x, y, rw, rh): Rect, (w, h): (u32, u32)) -> Rect {
    let x0 = x.min(w);
    let y0 = y.min(h);
    let x1 = x.saturating_add(rw).min(w);
    let y1 = y.saturating_add(rh).min(h);
    (x0, y0, x1 - x0, y1 - y0)
}

/// Adds `damage` (empty: the whole frame) to a buffer's stale list, merging
/// into the bounds once the list is long.
pub(super) fn add_damage(list: &mut Vec<Rect>, damage: &[Rect]) {
    if damage.is_empty() {
        list.clear();
        list.push((0, 0, u32::MAX, u32::MAX));
        return;
    }
    if list.first() == Some(&(0, 0, u32::MAX, u32::MAX)) {
        return;
    }
    list.extend_from_slice(damage);
    if list.len() > MAX_RECTS {
        let x0 = list.iter().map(|r| r.0).min().unwrap_or(0);
        let y0 = list.iter().map(|r| r.1).min().unwrap_or(0);
        let x1 = list
            .iter()
            .map(|r| r.0.saturating_add(r.2))
            .max()
            .unwrap_or(0);
        let y1 = list
            .iter()
            .map(|r| r.1.saturating_add(r.3))
            .max()
            .unwrap_or(0);
        list.clear();
        list.push((x0, y0, x1 - x0, y1 - y0));
    }
}

/// The cursor images: `X` black, `o` white; transparent pixels touching
/// black get a white outline.
const ARROW: [&str; 17] = [
    "X",
    "XX",
    "XoX",
    "XooX",
    "XoooX",
    "XooooX",
    "XoooooX",
    "XooooooX",
    "XoooooooX",
    "XooooooooX",
    "XoooooXXXXX",
    "XooXooX",
    "XoX XooX",
    "XX  XooX",
    "X    XooX",
    "     XooX",
    "      XX",
];
const IBEAM: [&str; 16] = [
    "XXX XXX", "   X", "   X", "   X", "   X", "   X", "   X", "   X", "   X", "   X", "   X",
    "   X", "   X", "   X", "   X", "XXX XXX",
];
const CROSS: [&str; 15] = [
    "       X",
    "       X",
    "       X",
    "       X",
    "       X",
    "       X",
    "",
    "XXXXXX   XXXXXX",
    "",
    "       X",
    "       X",
    "       X",
    "       X",
    "       X",
    "       X",
];

/// Draws `icon` at `scale` into a cursor buffer, turned like the image;
/// returns the hotspot in the buffer.
fn draw_cursor(
    dst: &mut [u8],
    pitch: usize,
    icon: CursorIcon,
    scale: f64,
    rotation: Rotation,
) -> (i32, i32) {
    if let CursorIcon::Custom(custom) = icon {
        return draw_image(dst, pitch, &custom.image(), scale, rotation);
    }
    let (rows, hotspot): (&[&str], (u32, u32)) = match icon {
        CursorIcon::Text | CursorIcon::VerticalText => (&IBEAM, (3, 8)),
        CursorIcon::Crosshair => (&CROSS, (7, 7)),
        _ => (&ARROW, (0, 0)),
    };
    let k = (scale.round() as u32).clamp(1, CURSOR / 20);
    let at = |x: i32, y: i32| -> u8 {
        let row = usize::try_from(y).ok().and_then(|y| rows.get(y));
        row.and_then(|r| r.as_bytes().get(usize::try_from(x).ok()?))
            .copied()
            .unwrap_or(b' ')
    };
    for b in dst.iter_mut() {
        *b = 0;
    }
    let n = CURSOR;
    for y in 0..n / k {
        for x in 0..n / k {
            let (xi, yi) = (x as i32, y as i32);
            let argb: u32 = match at(xi, yi) {
                b'X' => 0xff00_0000,
                b'o' => 0xffff_ffff,
                _ if (-1..=1).any(|dy| (-1..=1).any(|dx| at(xi + dx, yi + dy) == b'X')) => {
                    0xffff_ffff
                }
                _ => continue,
            };
            for sy in 0..k {
                for sx in 0..k {
                    let (px, py) = rotation.pixel(x * k + sx, y * k + sy, (n, n));
                    let o = py as usize * pitch + px as usize * 4;
                    dst[o..o + 4].copy_from_slice(&argb.to_le_bytes());
                }
            }
        }
    }
    let (hx, hy) = rotation.pixel(hotspot.0 * k, hotspot.1 * k, (n, n));
    (hx as i32, hy as i32)
}

/// Draws an image cursor at `scale` into a cursor buffer, premultiplied and
/// turned like the image, cut to the buffer; returns the hotspot in it.
fn draw_image(
    dst: &mut [u8],
    pitch: usize,
    image: &CursorImage,
    scale: f64,
    rotation: Rotation,
) -> (i32, i32) {
    dst.fill(0);
    let shown = image.at_scale(scale);
    let n = CURSOR;
    let w = shown.width as usize;
    for (y, row) in shown.rgba.chunks_exact(w * 4).enumerate().take(n as usize) {
        for (x, p) in row.as_chunks::<4>().0.iter().enumerate().take(n as usize) {
            let a = u32::from(p[3]);
            if a == 0 {
                continue;
            }
            let k = |c: u8| (u32::from(c) * a + 127) / 255;
            let argb = a << 24 | k(p[0]) << 16 | k(p[1]) << 8 | k(p[2]);
            let (px, py) = rotation.pixel(x as u32, y as u32, (n, n));
            let o = py as usize * pitch + px as usize * 4;
            dst[o..o + 4].copy_from_slice(&argb.to_le_bytes());
        }
    }
    let [hx, hy] = shown.hotspot.map(|v| v.min(n - 1));
    let (hx, hy) = rotation.pixel(hx, hy, (n, n));
    (hx as i32, hy as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_image_cursor_draws_premultiplied_with_its_hotspot() {
        let pixels = [[255, 0, 0, 255], [0, 0, 255, 128]].concat().repeat(2);
        let image = CursorImage::new(2, 2, pixels, [1, 0]).unwrap();
        let mut buf = vec![0u8; (CURSOR * CURSOR * 4) as usize];
        let pitch = CURSOR as usize * 4;
        let hot = draw_image(&mut buf, pitch, &image, 1.0, Rotation::R0);
        assert_eq!(hot, (1, 0));
        assert_eq!(
            u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            0xffff_0000
        );
        assert_eq!(
            u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            0x8000_0080
        );
        assert_eq!(&buf[8..12], &[0; 4]);
        let hot = draw_image(&mut buf, pitch, &image, 2.0, Rotation::R0);
        assert_eq!(hot, (2, 0), "twice the size at twice the scale");
    }

    #[test]
    fn rotation_maps_points_and_pixels_the_same_way() {
        for r in [Rotation::R0, Rotation::R90, Rotation::R180, Rotation::R270] {
            let (u, v) = (0.25, 0.75);
            let (nu, nv) = r.to_native(u, v);
            let (bu, bv) = r.to_logical(nu, nv);
            assert!(
                (bu - u).abs() < 1e-9 && (bv - v).abs() < 1e-9,
                "{r:?} round-trips"
            );
            // An image 40 × 20: its pixel (10, 15) lands where its centre
            // fraction says.
            let size = (40, 20);
            let (px, py) = r.pixel(10, 15, size);
            let native = if r.quarter() {
                (20.0, 40.0)
            } else {
                (40.0, 20.0)
            };
            let (fu, fv) = r.to_native(10.5 / 40.0, 15.5 / 20.0);
            assert_eq!(
                (px, py),
                ((fu * native.0) as u32, (fv * native.1) as u32),
                "{r:?}"
            );
            // A rect's panel rect covers exactly its pixels' panel pixels.
            let rect = r.rect((10, 15, 3, 2), size);
            for (x, y) in [(10, 15), (12, 16)] {
                let (px, py) = r.pixel(x, y, size);
                assert!(
                    px >= rect.0 && px < rect.0 + rect.2 && py >= rect.1 && py < rect.1 + rect.3,
                    "{r:?}"
                );
            }
        }
    }

    #[test]
    fn stale_damage_merges_and_saturates() {
        let mut list = Vec::new();
        add_damage(&mut list, &[(0, 0, 4, 4)]);
        add_damage(&mut list, &[(10, 10, 2, 2)]);
        assert_eq!(list.len(), 2);
        for i in 0..MAX_RECTS as u32 {
            add_damage(&mut list, &[(i, i, 1, 1)]);
        }
        assert_eq!(list, [(0, 0, 16, 16)]);
        add_damage(&mut list, &[]);
        add_damage(&mut list, &[(1, 1, 1, 1)]);
        assert_eq!(list, [(0, 0, u32::MAX, u32::MAX)]);
        assert_eq!(clip(list[0], (800, 480)), (0, 0, 800, 480));
    }

    #[test]
    fn cursor_images_draw_with_their_hotspot() {
        let mut buf = vec![0u8; (CURSOR * CURSOR * 4) as usize];
        let hot = draw_cursor(
            &mut buf,
            CURSOR as usize * 4,
            CursorIcon::Default,
            1.0,
            Rotation::R0,
        );
        assert_eq!(hot, (0, 0));
        assert_eq!(
            &buf[0..4],
            &0xff00_0000u32.to_le_bytes(),
            "the arrow's tip is black"
        );
        let hot = draw_cursor(
            &mut buf,
            CURSOR as usize * 4,
            CursorIcon::Text,
            2.0,
            Rotation::R180,
        );
        assert_eq!(hot, (CURSOR as i32 - 1 - 6, CURSOR as i32 - 1 - 16));
    }
}
