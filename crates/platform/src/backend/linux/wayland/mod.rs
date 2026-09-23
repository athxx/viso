//! The Wayland backend over libwayland-client (loaded at runtime):
//! `xdg-shell` toplevels with server-side decorations where offered, frame
//! callbacks pacing redraws, `fractional-scale-v1` + `viewporter` (integer
//! buffer scale otherwise), the seat's pointer/keyboard/touch with client-side
//! key repeat, `text-input-v3` composition, the `wl_data_device` clipboard, and
//! `cursor-shape-v1` cursors (theme cursors otherwise).

mod clipboard;
mod seat;
mod text_input;

use std::cell::RefCell;
use std::os::fd::{AsRawFd, RawFd};
use std::rc::Rc;

use wayland_client::globals::{GlobalList, GlobalListContents, registry_queue_init};
use wayland_client::protocol::{
    wl_callback::{self, WlCallback},
    wl_compositor::WlCompositor,
    wl_output::{self, WlOutput},
    wl_registry::{self, WlRegistry},
    wl_seat::WlSeat,
    wl_shm::WlShm,
    wl_surface::{self, WlSurface},
};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    zxdg_toplevel_decoration_v1::{self, ZxdgToplevelDecorationV1},
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

use self::clipboard::Clipboard;
use self::seat::Seat;
use super::{
    After, Chore, Display, Pump, WakeReceiver, Waker, poll_readable, translate, wake_channel,
};
use crate::control::{PlatformError, WindowChrome, WindowConfig, WindowId};
use crate::event::{AcceptCell, Appearance, CursorIcon, PointerButtons, RawEvent};
use crate::handler::AppHandler;
use crate::menu::Menu;
use crate::{Instant, LogicalRect, PlatformApp, RawWindowHandle, Window};

/// `wp_fractional_scale_v1` reports scales in 120ths.
const FRACTIONAL_DENOMINATOR: f64 = 120.0;

fn backend_error(e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Backend(e.to_string())
}

pub(crate) struct WaylandApp {
    wl: Rc<RefCell<Wl>>,
    wakes: Rc<WakeReceiver>,
    windows: Vec<WaylandWindow>,
    next_id: u32,
}

impl WaylandApp {
    pub(crate) fn new() -> Result<Self, PlatformError> {
        let (waker, wakes) = wake_channel().map_err(backend_error)?;
        let conn = Connection::connect_to_env().map_err(backend_error)?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn).map_err(backend_error)?;
        let mut state = State::new(conn, &globals, queue.handle(), waker.clone())?;
        // Seat capabilities and output scales arrive in the first round trip.
        queue.roundtrip(&mut state).map_err(backend_error)?;
        state.pump.appearance = super::portal::spawn(waker);
        Ok(Self {
            wl: Rc::new(RefCell::new(Wl { queue, state })),
            wakes: Rc::new(wakes),
            windows: Vec::new(),
            next_id: 1,
        })
    }

    fn with_state(&self, f: impl FnOnce(&mut State)) {
        let mut wl = self.wl.borrow_mut();
        f(&mut wl.state);
        let _ = wl.state.conn.flush();
    }
}

impl PlatformApp for WaylandApp {
    fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError> {
        let id = WindowId(self.next_id);
        let mut wl = self.wl.borrow_mut();
        let Wl { queue, state } = &mut *wl;
        let surface = state.create_window(id, &config);
        // Wait for the first configure so the window has its real size and
        // scale before the app builds a surface for it.
        while state
            .index(id)
            .is_some_and(|i| !state.windows[i].configured)
        {
            queue.blocking_dispatch(state).map_err(backend_error)?;
        }
        self.next_id += 1;
        let handle = RawWindowHandle::Wayland {
            display: state.conn.backend().display_ptr().cast(),
            surface: surface.id().as_ptr().cast(),
        };
        self.windows.retain(|w| state.index(w.id).is_some());
        drop(wl);
        self.windows.push(WaylandWindow {
            id,
            handle,
            wl: self.wl.clone(),
        });
        Ok(id)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        let wl = self.wl.clone();
        let wakes = self.wakes.clone();
        super::run(&wl, &wakes, handler);
    }

    fn window(&self, id: WindowId) -> Option<&dyn Window> {
        let wl = self.wl.borrow();
        self.windows
            .iter()
            .find(|w| w.id == id && wl.state.index(id).is_some())
            .map(|w| w as &dyn Window)
    }

    fn request_redraw(&mut self, window: WindowId) {
        self.wl.borrow_mut().state.request_redraw(window);
    }

    fn set_menu(&mut self, menu: &Menu) {
        self.wl.borrow_mut().state.pump.accels = translate::AccelTable::build(menu);
    }

    fn set_draggable_regions(&mut self, window: WindowId, regions: &[LogicalRect]) {
        self.with_state(|state| {
            if let Some(i) = state.index(window) {
                state.windows[i].drag = regions.to_vec();
            }
        });
    }

    fn set_fullscreen(&mut self, window: WindowId, fullscreen: bool) {
        self.with_state(|state| {
            if let Some(i) = state.index(window) {
                let toplevel = &state.windows[i].toplevel;
                if fullscreen {
                    toplevel.set_fullscreen(None);
                } else {
                    toplevel.unset_fullscreen();
                }
            }
        });
    }

    fn close_window(&mut self, window: WindowId) {
        self.with_state(|state| state.close(window));
    }

    fn set_clipboard_text(&mut self, text: &str) {
        self.with_state(|state| state.copy(text));
    }

    fn request_paste(&mut self, window: WindowId) {
        self.with_state(|state| state.chore(Chore::Paste(window)));
    }

    fn set_cursor(&mut self, window: WindowId, icon: CursorIcon) {
        self.with_state(|state| {
            if let Some(i) = state.index(window) {
                state.windows[i].cursor = icon;
                state.refresh_cursor();
            }
        });
    }

    fn set_ime_area(&mut self, window: WindowId, caret: Option<LogicalRect>) {
        self.with_state(|state| {
            if let Some(i) = state.index(window) {
                state.windows[i].ime_area = caret;
                state.update_text_input(window);
            }
        });
    }

    fn show_soft_keyboard(&mut self, window: WindowId, show: bool) {
        // `text-input-v3` has no separate show request: re-enabling asks the
        // compositor's input method (and its on-screen keyboard) to appear.
        if show {
            self.with_state(|state| state.update_text_input(window));
        }
    }

    fn appearance(&self) -> Appearance {
        self.wl.borrow().state.pump.appearance
    }
}

/// A Wayland toplevel.
pub(crate) struct WaylandWindow {
    id: WindowId,
    handle: RawWindowHandle,
    wl: Rc<RefCell<Wl>>,
}

impl Window for WaylandWindow {
    fn id(&self) -> WindowId {
        self.id
    }

    fn request_redraw(&self) {
        self.wl.borrow_mut().state.request_redraw(self.id);
    }

    fn set_title(&mut self, title: &str) {
        let wl = self.wl.borrow();
        if let Some(i) = wl.state.index(self.id) {
            wl.state.windows[i].toplevel.set_title(title.to_owned());
            let _ = wl.state.conn.flush();
        }
    }

    fn scale_factor(&self) -> f64 {
        let wl = self.wl.borrow();
        wl.state
            .index(self.id)
            .map_or(1.0, |i| wl.state.windows[i].scale)
    }

    fn inner_size(&self) -> (u32, u32) {
        let wl = self.wl.borrow();
        wl.state
            .index(self.id)
            .map_or((0, 0), |i| wl.state.windows[i].physical_size())
    }

    fn raw_handle(&self) -> RawWindowHandle {
        self.handle
    }
}

/// The connection's event queue and everything its events update.
pub(crate) struct Wl {
    queue: EventQueue<State>,
    state: State,
}

struct Globals {
    compositor: WlCompositor,
    wm_base: XdgWmBase,
    shm: WlShm,
    decoration: Option<ZxdgDecorationManagerV1>,
    fractional: Option<WpFractionalScaleManagerV1>,
    viewporter: Option<WpViewporter>,
    cursor_shape: Option<WpCursorShapeManagerV1>,
    text_input: Option<ZwpTextInputManagerV3>,
}

struct Output {
    name: u32,
    output: WlOutput,
    scale: i32,
}

pub(crate) struct Win {
    id: WindowId,
    surface: WlSurface,
    xdg_surface: XdgSurface,
    toplevel: XdgToplevel,
    decoration: Option<ZxdgToplevelDecorationV1>,
    fractional: Option<WpFractionalScaleV1>,
    viewport: Option<WpViewport>,
    chrome: WindowChrome,
    /// The first configure arrived and was acknowledged.
    configured: bool,
    /// Content size in logical points, as the compositor configures it.
    logical: (i32, i32),
    scale: f64,
    /// The integer buffer scale, when fractional scaling is unavailable.
    buffer_scale: i32,
    /// The latest `preferred_buffer_scale` (surface v6), which overrides the
    /// scale of the outputs the surface is on.
    preferred_buffer_scale: Option<i32>,
    outputs: Vec<WlOutput>,
    pending: PendingConfigure,
    fullscreen: bool,
    maximized: bool,
    /// A frame callback is outstanding: redraws wait for it.
    frame_pending: bool,
    wants_redraw: bool,
    drag: Vec<LogicalRect>,
    cursor: CursorIcon,
    /// The resize edge under the pointer of a self-drawn window.
    edge: Option<translate::Edge>,
    buttons: PointerButtons,
    last_caption_press: Option<(u32, f64, f64)>,
    ime_area: Option<LogicalRect>,
}

#[derive(Default)]
struct PendingConfigure {
    size: Option<(i32, i32)>,
    fullscreen: bool,
    maximized: bool,
}

impl Win {
    fn physical_size(&self) -> (u32, u32) {
        (
            physical(self.logical.0, self.scale),
            physical(self.logical.1, self.scale),
        )
    }

    fn logical_size(&self) -> (f64, f64) {
        (f64::from(self.logical.0), f64::from(self.logical.1))
    }
}

pub(crate) struct State {
    conn: Connection,
    qh: QueueHandle<State>,
    globals: Globals,
    pump: Pump,
    waker: Waker,
    windows: Vec<Win>,
    outputs: Vec<Output>,
    seat: Option<Seat>,
    clipboard: Clipboard,
}

impl State {
    fn new(
        conn: Connection,
        globals: &GlobalList,
        qh: QueueHandle<State>,
        waker: Waker,
    ) -> Result<Self, PlatformError> {
        let required = |what: &str| PlatformError::Backend(format!("the compositor lacks {what}"));
        let compositor = globals
            .bind::<WlCompositor, _, _>(&qh, 4..=6, ())
            .map_err(|_| required("wl_compositor v4"))?;
        let wm_base = globals
            .bind::<XdgWmBase, _, _>(&qh, 2..=6, ())
            .map_err(|_| required("xdg_wm_base"))?;
        let shm = globals
            .bind::<WlShm, _, _>(&qh, 1..=1, ())
            .map_err(|_| required("wl_shm"))?;
        let data_device = globals.bind(&qh, 3..=3, ()).ok();
        let globals_ = Globals {
            compositor,
            wm_base,
            shm,
            decoration: globals.bind(&qh, 1..=1, ()).ok(),
            fractional: globals.bind(&qh, 1..=1, ()).ok(),
            viewporter: globals.bind(&qh, 1..=1, ()).ok(),
            cursor_shape: globals.bind(&qh, 1..=1, ()).ok(),
            text_input: globals.bind(&qh, 1..=1, ()).ok(),
        };
        let mut state = Self {
            conn,
            qh: qh.clone(),
            globals: globals_,
            pump: Pump::default(),
            waker,
            windows: Vec::new(),
            outputs: Vec::new(),
            seat: None,
            clipboard: Clipboard::new(data_device),
        };
        globals.contents().with_list(|list| {
            for global in list {
                state.global_added(
                    globals.registry(),
                    global.name,
                    &global.interface,
                    global.version,
                );
            }
        });
        Ok(state)
    }

    fn global_added(&mut self, registry: &WlRegistry, name: u32, interface: &str, version: u32) {
        match interface {
            "wl_output" => {
                let output = registry.bind::<WlOutput, _, _>(name, version.min(4), &self.qh, name);
                self.outputs.push(Output {
                    name,
                    output,
                    scale: 1,
                });
            }
            // One seat drives input; a second seat (rare outside kiosks) is
            // ignored rather than interleaved.
            "wl_seat" if self.seat.is_none() => {
                let seat = registry.bind::<WlSeat, _, _>(name, version.min(9), &self.qh, ());
                self.clipboard.attach_seat(&seat, &self.qh);
                self.seat = Some(Seat::new(seat, name, &self.globals, &self.qh));
            }
            _ => {}
        }
    }

    fn global_removed(&mut self, name: u32) {
        if let Some(i) = self.outputs.iter().position(|o| o.name == name) {
            let output = self.outputs.remove(i);
            for win in &mut self.windows {
                win.outputs.retain(|o| *o != output.output);
            }
            if output.output.version() >= 3 {
                output.output.release();
            }
            self.rescale_all();
        }
        if self.seat.as_ref().is_some_and(|s| s.name == name)
            && let Some(seat) = self.seat.take()
        {
            seat.release(self);
        }
    }

    pub(crate) fn index(&self, id: WindowId) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    fn create_window(&mut self, id: WindowId, config: &WindowConfig) -> WlSurface {
        let qh = &self.qh;
        let surface = self.globals.compositor.create_surface(qh, id);
        let xdg_surface = self.globals.wm_base.get_xdg_surface(&surface, qh, id);
        let toplevel = xdg_surface.get_toplevel(qh, id);
        toplevel.set_title(config.title.clone());
        toplevel.set_app_id(app_id());
        let decoration = self.globals.decoration.as_ref().map(|m| {
            let d = m.get_toplevel_decoration(&toplevel, qh, id);
            d.set_mode(match config.chrome {
                WindowChrome::Native => zxdg_toplevel_decoration_v1::Mode::ServerSide,
                WindowChrome::SelfDrawn => zxdg_toplevel_decoration_v1::Mode::ClientSide,
            });
            d
        });
        let viewport = self
            .globals
            .viewporter
            .as_ref()
            .map(|v| v.get_viewport(&surface, qh, ()));
        let fractional = viewport
            .as_ref()
            .and(self.globals.fractional.as_ref())
            .map(|f| f.get_fractional_scale(&surface, qh, id));
        // Until the compositor says otherwise, assume the largest output
        // scale so the first frame is not blurry on a HiDPI screen.
        let scale = self
            .outputs
            .iter()
            .map(|o| o.scale)
            .max()
            .unwrap_or(1)
            .max(1);
        let logical = (
            logical_extent(config.logical_size.0),
            logical_extent(config.logical_size.1),
        );
        if let Some(v) = &viewport {
            v.set_destination(logical.0, logical.1);
        }
        // The initial commit, with no buffer, asks for the first configure.
        surface.commit();
        self.windows.push(Win {
            id,
            surface: surface.clone(),
            xdg_surface,
            toplevel,
            decoration,
            fractional,
            viewport,
            chrome: config.chrome,
            configured: false,
            logical,
            scale: f64::from(scale),
            buffer_scale: 1,
            preferred_buffer_scale: None,
            outputs: Vec::new(),
            pending: PendingConfigure::default(),
            fullscreen: false,
            maximized: false,
            frame_pending: false,
            wants_redraw: false,
            drag: Vec::new(),
            cursor: CursorIcon::Default,
            edge: None,
            buttons: PointerButtons::NONE,
            last_caption_press: None,
            ime_area: None,
        });
        let i = self.windows.len() - 1;
        self.apply_scale(i, f64::from(scale), false);
        surface
    }

    fn close(&mut self, id: WindowId) {
        let Some(i) = self.index(id) else { return };
        let win = self.windows.remove(i);
        if let Some(seat) = &mut self.seat {
            seat.forget_window(id);
        }
        if let Some(f) = win.fractional {
            f.destroy();
        }
        if let Some(v) = win.viewport {
            v.destroy();
        }
        if let Some(d) = win.decoration {
            d.destroy();
        }
        win.toplevel.destroy();
        win.xdg_surface.destroy();
        win.surface.destroy();
        self.pump.forget_window(id);
        self.pump.push(RawEvent::WindowClosed { window: id });
    }

    /// Ask for a redraw, paced by the window's frame callback.
    fn request_redraw(&mut self, id: WindowId) {
        let Some(i) = self.index(id) else { return };
        let win = &mut self.windows[i];
        if !win.configured || win.frame_pending {
            win.wants_redraw = true;
        } else {
            self.pump.push_redraw(id);
        }
    }

    fn copy(&mut self, text: &str) {
        let serial = self.seat.as_ref().map_or(0, |s| s.last_serial);
        self.clipboard.set(&self.qh, text, serial);
    }

    fn chore(&mut self, chore: Chore) {
        match chore {
            Chore::Paste(window) => {
                if let Some(text) = self.clipboard.paste(window, &self.waker) {
                    self.pump.push(RawEvent::Paste { window, text });
                }
            }
            Chore::Minimize(window) => {
                if let Some(i) = self.index(window) {
                    self.windows[i].toplevel.set_minimized();
                }
            }
        }
    }

    fn on_configure(&mut self, i: usize, serial: u32) {
        let win = &mut self.windows[i];
        win.xdg_surface.ack_configure(serial);
        let pending = std::mem::take(&mut win.pending);
        let first = !win.configured;
        win.configured = true;
        win.maximized = pending.maximized;
        let id = win.id;
        if win.fullscreen != pending.fullscreen {
            win.fullscreen = pending.fullscreen;
            self.pump.push(RawEvent::FullscreenChanged {
                window: id,
                fullscreen: pending.fullscreen,
            });
        }
        let win = &mut self.windows[i];
        if let Some(size) = pending.size.filter(|&(w, h)| w > 0 && h > 0)
            && size != win.logical
        {
            win.logical = size;
            if let Some(v) = &win.viewport {
                v.set_destination(size.0, size.1);
            }
            let (width, height) = win.physical_size();
            if !first {
                self.pump.push(RawEvent::Resized {
                    window: id,
                    width,
                    height,
                });
            }
        }
        // The compositor waits for a buffer matching the configure: draw now,
        // outside frame pacing.
        let win = &mut self.windows[i];
        win.wants_redraw = false;
        self.pump.push_redraw(id);
    }

    /// Recompute the integer buffer scale of windows without fractional
    /// scaling, from the preferred scale or the outputs they are on.
    fn rescale_all(&mut self) {
        for i in 0..self.windows.len() {
            self.rescale_integer(i);
        }
    }

    fn rescale_integer(&mut self, i: usize) {
        let win = &self.windows[i];
        if win.fractional.is_some() {
            return;
        }
        let from_outputs = win
            .outputs
            .iter()
            .filter_map(|o| self.outputs.iter().find(|out| out.output == *o))
            .map(|o| o.scale)
            .max();
        let scale = win
            .preferred_buffer_scale
            .or(from_outputs)
            .unwrap_or(win.buffer_scale)
            .max(1);
        self.apply_scale(i, f64::from(scale), true);
    }

    /// Adopt `scale` for window `i`; `notify` reports the change to the app.
    fn apply_scale(&mut self, i: usize, scale: f64, notify: bool) {
        let win = &mut self.windows[i];
        if (win.scale - scale).abs() < f64::EPSILON
            && (win.viewport.is_some() || win.buffer_scale == scale as i32)
        {
            return;
        }
        win.scale = scale;
        if let Some(v) = &win.viewport {
            // The viewport maps whatever buffer size to the logical size.
            v.set_destination(win.logical.0, win.logical.1);
        } else {
            win.buffer_scale = scale as i32;
            win.surface.set_buffer_scale(win.buffer_scale);
        }
        if notify {
            let (width, height) = win.physical_size();
            let id = win.id;
            self.pump.push(RawEvent::ScaleFactorChanged {
                window: id,
                scale,
                width,
                height,
            });
            self.pump.push_redraw(id);
            self.refresh_cursor();
        }
    }

    fn refresh_cursor(&mut self) {
        if let Some(mut seat) = self.seat.take() {
            seat.apply_cursor(self);
            self.seat = Some(seat);
        }
    }

    fn update_text_input(&mut self, window: WindowId) {
        if let Some(mut seat) = self.seat.take() {
            seat.update_text_input(self, window);
            self.seat = Some(seat);
        }
    }
}

impl Display for Wl {
    fn pump(&mut self) -> &mut Pump {
        &mut self.state.pump
    }

    fn pump_ref(&self) -> &Pump {
        &self.state.pump
    }

    fn dispatch(&mut self) -> bool {
        if self.queue.dispatch_pending(&mut self.state).is_err() {
            return false;
        }
        if let Some(guard) = self.queue.prepare_read()
            && let Err(e) = guard.read()
            && !would_block(&e)
        {
            return false;
        }
        if self.queue.dispatch_pending(&mut self.state).is_err() {
            return false;
        }
        self.state.tick_repeat(Instant::now());
        flush(&self.state.conn)
    }

    fn wait(&mut self, wake_fd: RawFd, deadline: Option<Instant>) -> bool {
        if !self.dispatch() {
            return false;
        }
        if self.state.pump.has_work() {
            return true;
        }
        let deadline = match (deadline, self.state.next_repeat()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        // `None`: events are already queued, so there is nothing to sleep on.
        if let Some(guard) = self.queue.prepare_read() {
            let fd = guard.connection_fd().as_raw_fd();
            let [ready, _] = poll_readable([fd, wake_fd], deadline);
            if ready
                && let Err(e) = guard.read()
                && !would_block(&e)
            {
                return false;
            }
        }
        self.dispatch()
    }

    fn before_redraw(&mut self, window: WindowId) {
        let state = &mut self.state;
        if let Some(i) = state.index(window) {
            let win = &mut state.windows[i];
            if !win.frame_pending {
                // Requested before the app presents, so the present's commit
                // carries it.
                win.surface.frame(&state.qh, window);
                win.frame_pending = true;
            }
            win.wants_redraw = false;
        }
    }

    fn finish(&mut self, after: After) {
        let state = &mut self.state;
        match after {
            After::Copy(text) => state.copy(&text),
            After::Close(window) => state.close(window),
            After::Redrawn(window) => {
                // A frame the app declined to draw still has to carry the
                // frame callback, or pacing would stall.
                if let Some(i) = state.index(window) {
                    state.windows[i].surface.commit();
                }
            }
        }
        let _ = flush(&state.conn);
    }
}

fn would_block(e: &wayland_client::backend::WaylandError) -> bool {
    matches!(e, wayland_client::backend::WaylandError::Io(io) if io.kind() == std::io::ErrorKind::WouldBlock)
}

/// Flush queued requests; a full socket is not a lost connection.
fn flush(conn: &Connection) -> bool {
    match conn.flush() {
        Ok(()) => true,
        Err(e) => would_block(&e),
    }
}

/// A logical length in physical pixels.
fn physical(logical: i32, scale: f64) -> u32 {
    (f64::from(logical.max(1)) * scale).round().max(1.0) as u32
}

/// A requested logical length as a Wayland surface extent.
fn logical_extent(v: f64) -> i32 {
    v.round().clamp(1.0, f64::from(i16::MAX)) as i32
}

/// The app id compositors match desktop files against: the executable name.
fn app_id() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "viso".into())
}

// ---- window-level dispatch ------------------------------------------------

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => state.global_added(registry, name, &interface, version),
            wl_registry::Event::GlobalRemove { name } => state.global_removed(name),
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<WlSurface, WindowId> for State {
    fn event(
        state: &mut Self,
        _: &WlSurface,
        event: wl_surface::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(i) = state.index(*id) else { return };
        let win = &mut state.windows[i];
        match event {
            wl_surface::Event::Enter { output } => win.outputs.push(output),
            wl_surface::Event::Leave { output } => win.outputs.retain(|o| *o != output),
            wl_surface::Event::PreferredBufferScale { factor } => {
                win.preferred_buffer_scale = Some(factor);
            }
            _ => return,
        }
        state.rescale_integer(i);
    }
}

impl Dispatch<XdgSurface, WindowId> for State {
    fn event(
        state: &mut Self,
        _: &XdgSurface,
        event: xdg_surface::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event
            && let Some(i) = state.index(*id)
        {
            state.on_configure(i, serial);
        }
    }
}

impl Dispatch<XdgToplevel, WindowId> for State {
    fn event(
        state: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(i) = state.index(*id) else { return };
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                let has = |s: xdg_toplevel::State| {
                    states
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .any(|c| u32::from_ne_bytes(*c) == s as u32)
                };
                let pending = &mut state.windows[i].pending;
                pending.size = Some((width, height));
                pending.fullscreen = has(xdg_toplevel::State::Fullscreen);
                pending.maximized = has(xdg_toplevel::State::Maximized);
            }
            xdg_toplevel::Event::Close => state.pump.push(RawEvent::CloseRequested {
                window: *id,
                accept: AcceptCell::new(),
            }),
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, WindowId> for State {
    fn event(
        state: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event
            && let Some(i) = state.index(*id)
        {
            let win = &mut state.windows[i];
            win.frame_pending = false;
            if std::mem::take(&mut win.wants_redraw) {
                state.pump.push_redraw(*id);
            }
        }
    }
}

impl Dispatch<WpFractionalScaleV1, WindowId> for State {
    fn event(
        state: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        id: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(i) = state.index(*id)
        {
            let notify = state.windows[i].configured;
            state.apply_scale(i, f64::from(scale) / FRACTIONAL_DENOMINATOR, notify);
        }
    }
}

impl Dispatch<ZxdgToplevelDecorationV1, WindowId> for State {
    fn event(
        _: &mut Self,
        _: &ZxdgToplevelDecorationV1,
        _: zxdg_toplevel_decoration_v1::Event,
        _: &WindowId,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The mode the compositor settled on needs no action: a native
        // window it will not decorate stays undecorated (as on GNOME).
    }
}

impl Dispatch<WlOutput, u32> for State {
    fn event(
        state: &mut Self,
        output: &WlOutput,
        event: wl_output::Event,
        _: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_output::Event::Scale { factor } => {
                if let Some(o) = state.outputs.iter_mut().find(|o| o.output == *output) {
                    o.scale = factor;
                }
            }
            wl_output::Event::Done => state.rescale_all(),
            _ => {}
        }
    }
}

delegate_noop!(State: WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: WpViewporter);
delegate_noop!(State: WpViewport);
delegate_noop!(State: WpFractionalScaleManagerV1);
delegate_noop!(State: ZxdgDecorationManagerV1);
delegate_noop!(State: WpCursorShapeManagerV1);
delegate_noop!(State: ZwpTextInputManagerV3);

/// The xdg-shell resize edge for `edge`.
fn resize_edge(edge: translate::Edge) -> xdg_toplevel::ResizeEdge {
    use translate::Edge as E;
    use xdg_toplevel::ResizeEdge as R;
    match edge {
        E::TopLeft => R::TopLeft,
        E::Top => R::Top,
        E::TopRight => R::TopRight,
        E::Right => R::Right,
        E::BottomRight => R::BottomRight,
        E::Bottom => R::Bottom,
        E::BottomLeft => R::BottomLeft,
        E::Left => R::Left,
    }
}

/// The value of a protocol enum this client knows.
fn known<T>(value: WEnum<T>) -> Option<T> {
    match value {
        WEnum::Value(v) => Some(v),
        WEnum::Unknown(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_sizes_round_and_never_vanish() {
        assert_eq!(physical(100, 1.25), 125);
        assert_eq!(physical(101, 1.5), 152);
        assert_eq!(physical(0, 2.0), 2);
        assert_eq!(logical_extent(0.2), 1);
        assert_eq!(logical_extent(1e9), 32767);
    }

    #[test]
    fn every_edge_has_a_distinct_xdg_edge() {
        use translate::Edge as E;
        let edges = [
            E::TopLeft,
            E::Top,
            E::TopRight,
            E::Right,
            E::BottomRight,
            E::Bottom,
            E::BottomLeft,
            E::Left,
        ];
        let mut values: Vec<u32> = edges.iter().map(|e| resize_edge(*e) as u32).collect();
        values.sort_unstable();
        values.dedup();
        assert_eq!(values.len(), edges.len());
        assert!(!values.contains(&(xdg_toplevel::ResizeEdge::None as u32)));
    }
}
