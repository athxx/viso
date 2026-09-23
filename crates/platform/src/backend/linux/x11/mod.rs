//! The X11 backend over XCB (loaded at runtime): ICCCM/EWMH windows,
//! XInput 2.2 pointer, pen and touch input with smooth scrolling, the XKB
//! keymap through libxkbcommon, XIM composition, the CLIPBOARD selection,
//! Xcursor themes, and the scale `Xft.dpi` or the monitor under the window
//! asks for.

mod atoms;
mod ime;
mod input;
mod selection;

use std::cell::RefCell;
use std::os::fd::{AsRawFd, RawFd};
use std::rc::Rc;

use x11rb::connection::{Connection, RequestConnection};
use x11rb::cursor::Handle as CursorTheme;
use x11rb::properties::{WmHints, WmHintsState};
use x11rb::protocol::Event;
use x11rb::protocol::randr;
use x11rb::protocol::xinput::{self, XIEventMask};
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    self, AtomEnum, ChangeWindowAttributesAux, ClientMessageEvent, ConfigureWindowAux,
    ConnectionExt as _, CreateWindowAux, EventMask, Gravity, KeyPressEvent, NotifyDetail, PropMode,
    Timestamp, WindowClass,
};
use x11rb::resource_manager::Database;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;

use self::atoms::Atoms;
use self::ime::{Ime, ImeOut};
use self::input::Devices;
use self::selection::Clipboard;
use super::translate::{self, Edge, RESIZE_BORDER};
use super::xkb::{self as keymap, KeyText, Keyboard};
use super::{After, Chore, Display, Pump, WakeReceiver, poll_readable, wake_channel};
use crate::control::{PlatformError, WindowChrome, WindowConfig, WindowId};
use crate::event::{
    AcceptCell, Appearance, CursorIcon, Modifiers, PointerButtons, PointerKind, PointerPhase,
    RawEvent, RawImePreedit, RawPointer, RawScroll, RawText,
};
use crate::handler::AppHandler;
use crate::menu::Menu;
use crate::{Instant, LogicalRect, PlatformApp, RawWindowHandle, Window};

/// The `_NET_WM_MOVERESIZE` direction that moves the window.
const MOVE: u32 = 8;

fn backend_error(e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Backend(e.to_string())
}

pub(crate) struct X11App {
    x11: Rc<RefCell<X11>>,
    wakes: Rc<WakeReceiver>,
    windows: Vec<X11Window>,
    next_id: u32,
}

impl X11App {
    pub(crate) fn new() -> Result<Self, PlatformError> {
        let (waker, wakes) = wake_channel().map_err(backend_error)?;
        let mut x11 = X11::connect()?;
        let appearance = super::portal::spawn(waker);
        x11.pump.appearance = appearance;
        Ok(Self {
            x11: Rc::new(RefCell::new(x11)),
            wakes: Rc::new(wakes),
            windows: Vec::new(),
            next_id: 1,
        })
    }

    fn with_window(&self, window: WindowId, f: impl FnOnce(&mut X11, usize)) {
        let mut x11 = self.x11.borrow_mut();
        if let Some(i) = x11.index(window) {
            f(&mut x11, i);
            let _ = x11.conn.flush();
        }
    }
}

impl PlatformApp for X11App {
    fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError> {
        let id = WindowId(self.next_id);
        let xid = self.x11.borrow_mut().create_window(id, &config)?;
        self.next_id += 1;
        let live = self.x11.borrow();
        self.windows.retain(|w| live.index(w.id).is_some());
        drop(live);
        self.windows.push(X11Window {
            id,
            xid,
            conn: self.x11.borrow().conn.clone(),
            x11: self.x11.clone(),
        });
        Ok(id)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        let x11 = self.x11.clone();
        let wakes = self.wakes.clone();
        super::run(&x11, &wakes, handler);
    }

    fn window(&self, id: WindowId) -> Option<&dyn Window> {
        let x11 = self.x11.borrow();
        self.windows
            .iter()
            .find(|w| w.id == id && x11.index(id).is_some())
            .map(|w| w as &dyn Window)
    }

    fn request_redraw(&mut self, window: WindowId) {
        self.x11.borrow_mut().pump.push_redraw(window);
    }

    fn set_menu(&mut self, menu: &Menu) {
        self.x11.borrow_mut().pump.accels = translate::AccelTable::build(menu);
    }

    fn set_draggable_regions(&mut self, window: WindowId, regions: &[LogicalRect]) {
        self.with_window(window, |x11, i| {
            x11.windows[i].drag = regions.to_vec();
        });
    }

    fn set_fullscreen(&mut self, window: WindowId, fullscreen: bool) {
        self.with_window(window, |x11, i| {
            let atoms = &x11.atoms;
            let data = [
                u32::from(fullscreen),
                atoms._NET_WM_STATE_FULLSCREEN,
                0,
                1,
                0,
            ];
            x11.send_wm_state(x11.windows[i].xid, data);
        });
    }

    fn close_window(&mut self, window: WindowId) {
        let mut x11 = self.x11.borrow_mut();
        x11.close(window);
        let _ = x11.conn.flush();
    }

    fn set_clipboard_text(&mut self, text: &str) {
        let mut x11 = self.x11.borrow_mut();
        x11.copy(text);
        let _ = x11.conn.flush();
    }

    fn request_paste(&mut self, window: WindowId) {
        let mut x11 = self.x11.borrow_mut();
        x11.chore(Chore::Paste(window));
        let _ = x11.conn.flush();
    }

    fn set_cursor(&mut self, window: WindowId, icon: CursorIcon) {
        self.with_window(window, |x11, i| {
            x11.windows[i].cursor = icon;
            if x11.windows[i].edge.is_none() {
                x11.show_cursor(i, icon);
            }
        });
    }

    fn set_ime_area(&mut self, window: WindowId, caret: Option<LogicalRect>) {
        self.with_window(window, |x11, i| {
            let win = &mut x11.windows[i];
            win.ime_enabled = caret.is_some();
            let scale = win.scale;
            let spot = caret.map(|r| (to_i16(r.x * scale), to_i16((r.y + r.height) * scale)));
            let xid = win.xid;
            if let Some(ime) = &mut x11.ime {
                ime.set_spot(xid, spot);
            }
        });
    }

    fn show_soft_keyboard(&mut self, _window: WindowId, _show: bool) {
        // Desktop X11 has no on-screen keyboard an app raises.
    }

    fn appearance(&self) -> Appearance {
        self.x11.borrow().pump.appearance
    }
}

/// An X11 toplevel.
pub(crate) struct X11Window {
    id: WindowId,
    xid: xproto::Window,
    conn: Rc<XCBConnection>,
    x11: Rc<RefCell<X11>>,
}

impl Window for X11Window {
    fn id(&self) -> WindowId {
        self.id
    }

    fn request_redraw(&self) {
        self.x11.borrow_mut().pump.push_redraw(self.id);
    }

    fn set_title(&mut self, title: &str) {
        let x11 = self.x11.borrow();
        x11.set_title(self.xid, title);
        let _ = x11.conn.flush();
    }

    fn scale_factor(&self) -> f64 {
        let x11 = self.x11.borrow();
        x11.index(self.id).map_or(1.0, |i| x11.windows[i].scale)
    }

    fn inner_size(&self) -> (u32, u32) {
        let x11 = self.x11.borrow();
        x11.index(self.id).map_or((0, 0), |i| x11.windows[i].size)
    }

    fn raw_handle(&self) -> RawWindowHandle {
        RawWindowHandle::Xcb {
            connection: self.conn.get_raw_xcb_connection(),
            window: self.xid,
        }
    }
}

/// Key codes as a 256-bit set.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct KeySet([u64; 4]);

impl KeySet {
    fn contains(&self, key: u8) -> bool {
        self.0[usize::from(key >> 6)] & (1 << (key & 63)) != 0
    }

    fn insert(&mut self, key: u8) {
        self.0[usize::from(key >> 6)] |= 1 << (key & 63);
    }

    /// `true` when `key` was in the set.
    fn remove(&mut self, key: u8) -> bool {
        let had = self.contains(key);
        self.0[usize::from(key >> 6)] &= !(1 << (key & 63));
        had
    }

    fn take_all(&mut self) -> impl Iterator<Item = u8> + use<> {
        let set = std::mem::take(self);
        (0..=u8::MAX).filter(move |k| set.contains(*k))
    }
}

struct WinState {
    id: WindowId,
    xid: xproto::Window,
    chrome: WindowChrome,
    scale: f64,
    /// Content size in physical pixels.
    size: (u32, u32),
    /// The content origin on the root window, for picking the monitor.
    origin: (i32, i32),
    fullscreen: bool,
    maximized: bool,
    drag: Vec<LogicalRect>,
    /// The cursor the app asked for.
    cursor: CursorIcon,
    /// The cursor currently shown.
    shown: Option<CursorIcon>,
    /// The resize edge under the pointer of a self-drawn window.
    edge: Option<Edge>,
    buttons: PointerButtons,
    pen_pressure: f32,
    last_pointer: (f64, f64),
    /// The last primary press on a caption, for double-click maximize.
    last_caption_press: Option<(Timestamp, f64, f64)>,
    ime_enabled: bool,
    /// Keys whose press reached the app, so their release does too (and a
    /// focus loss can release them).
    delivered: KeySet,
}

struct Monitor {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    scale: f64,
}

pub(crate) struct X11 {
    conn: Rc<XCBConnection>,
    screen: usize,
    root: xproto::Window,
    atoms: Atoms,
    pump: Pump,
    windows: Vec<WinState>,
    keyboard: Option<Keyboard>,
    /// The XKB core keyboard device, when libxkbcommon-x11 set it up.
    xkb_device: Option<i32>,
    devices: Devices,
    ime: Option<Ime>,
    clipboard: Clipboard,
    cursor_theme: Option<CursorTheme>,
    cursors: Vec<(CursorIcon, xproto::Cursor)>,
    /// The scale `Xft.dpi` sets for every window, if any.
    xft_scale: Option<f64>,
    monitors: Vec<Monitor>,
    /// The latest server time seen, for selection ownership.
    last_time: Timestamp,
}

impl X11 {
    fn connect() -> Result<Self, PlatformError> {
        let (conn, screen) = XCBConnection::connect(None).map_err(backend_error)?;
        let conn = Rc::new(conn);
        let root = conn.setup().roots[screen].root;
        let atoms = Atoms::new(&*conn)
            .map_err(backend_error)?
            .reply()
            .map_err(backend_error)?;

        if conn
            .extension_information(xinput::X11_EXTENSION_NAME)
            .map_err(backend_error)?
            .is_none()
        {
            return Err(PlatformError::Backend("the X server lacks XInput".into()));
        }
        let version = xinput::xi_query_version(&*conn, 2, 2)
            .map_err(backend_error)?
            .reply()
            .map_err(backend_error)?;
        if (version.major_version, version.minor_version) < (2, 2) {
            return Err(PlatformError::Backend("XInput 2.2 is required".into()));
        }
        let mut devices = Devices::default();
        devices
            .refresh(&*conn, atoms.ABS_PRESSURE)
            .map_err(backend_error)?;
        xinput::xi_select_events(
            &*conn,
            root,
            &[xinput::EventMask {
                deviceid: xinput::Device::ALL.into(),
                mask: vec![XIEventMask::HIERARCHY | XIEventMask::DEVICE_CHANGED],
            }],
        )
        .map_err(backend_error)?;

        let (keyboard, xkb_device) = Self::setup_keyboard(&conn);

        let has_randr = conn
            .extension_information(randr::X11_EXTENSION_NAME)
            .map_err(backend_error)?
            .is_some()
            && randr::query_version(&*conn, 1, 5)
                .ok()
                .and_then(|c| c.reply().ok())
                .is_some();
        if has_randr {
            let _ = randr::select_input(
                &*conn,
                root,
                randr::NotifyMask::SCREEN_CHANGE
                    | randr::NotifyMask::CRTC_CHANGE
                    | randr::NotifyMask::OUTPUT_CHANGE,
            );
        }
        // Settings daemons publish `Xft.dpi` and the cursor theme here.
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )
        .map_err(backend_error)?;

        let clipboard = Clipboard::new(&conn, root).map_err(backend_error)?;
        let ime = Ime::connect(conn.clone(), screen);
        let mut x11 = Self {
            conn,
            screen,
            root,
            atoms,
            pump: Pump::default(),
            windows: Vec::new(),
            keyboard,
            xkb_device,
            devices,
            ime,
            clipboard,
            cursor_theme: None,
            cursors: Vec::new(),
            xft_scale: None,
            monitors: Vec::new(),
            last_time: x11rb::CURRENT_TIME,
        };
        x11.load_resources();
        x11.refresh_monitors();
        x11.conn.flush().map_err(backend_error)?;
        Ok(x11)
    }

    /// The keymap and modifier state of the core keyboard, following
    /// layout switches and keymap reloads.
    fn setup_keyboard(conn: &XCBConnection) -> (Option<Keyboard>, Option<i32>) {
        let Some(mut keyboard) = Keyboard::new() else {
            return (None, None);
        };
        let raw = conn.get_raw_xcb_connection();
        // SAFETY: `raw` is the live connection `conn` owns.
        let Some(device) = (unsafe { keymap::setup_x11(raw) }) else {
            return (Some(keyboard), None);
        };
        // SAFETY: `raw` is live and its XKB extension was set up above.
        if !unsafe { keyboard.set_keymap_x11(raw, device) } {
            return (Some(keyboard), None);
        }
        let Ok(spec) = u16::try_from(device) else {
            return (Some(keyboard), None);
        };
        // x11rb parses XKB events once it knows the extension is in use.
        let _ = conn.xkb_use_extension(1, 0).map(|c| c.reply());
        let parts = xkb::MapPart::KEY_TYPES
            | xkb::MapPart::KEY_SYMS
            | xkb::MapPart::MODIFIER_MAP
            | xkb::MapPart::EXPLICIT_COMPONENTS
            | xkb::MapPart::KEY_ACTIONS
            | xkb::MapPart::KEY_BEHAVIORS
            | xkb::MapPart::VIRTUAL_MODS
            | xkb::MapPart::VIRTUAL_MOD_MAP;
        let _ = conn.xkb_select_events(
            spec,
            xkb::EventType::from(0u16),
            xkb::EventType::NEW_KEYBOARD_NOTIFY
                | xkb::EventType::MAP_NOTIFY
                | xkb::EventType::STATE_NOTIFY,
            parts,
            parts,
            &xkb::SelectEventsAux::new(),
        );
        // Held keys repeat as presses alone, without synthetic releases.
        let _ = conn
            .xkb_per_client_flags(
                spec,
                xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT,
                xkb::PerClientFlag::DETECTABLE_AUTO_REPEAT,
                xkb::BoolCtrl::from(0u32),
                xkb::BoolCtrl::from(0u32),
                xkb::BoolCtrl::from(0u32),
            )
            .map(|c| c.reply());
        (Some(keyboard), Some(device))
    }

    /// Read `RESOURCE_MANAGER`: the `Xft.dpi` scale and the cursor theme.
    fn load_resources(&mut self) {
        let db = x11rb::resource_manager::new_from_default(&*self.conn)
            .unwrap_or_else(|_| Database::new_from_data(&[]));
        self.xft_scale = db
            .get_value::<f64>("Xft.dpi", "")
            .ok()
            .flatten()
            .and_then(translate::xft_scale);
        self.cursor_theme = CursorTheme::new(&*self.conn, self.screen, &db)
            .ok()
            .and_then(|c| c.reply().ok());
        for (_, cursor) in self.cursors.drain(..) {
            let _ = self.conn.free_cursor(cursor);
        }
        for win in &mut self.windows {
            win.shown = None;
        }
    }

    fn refresh_monitors(&mut self) {
        let monitors = randr::get_monitors(&*self.conn, self.root, true)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.monitors)
            .unwrap_or_default();
        self.monitors = monitors
            .iter()
            .map(|m| Monitor {
                x: i32::from(m.x),
                y: i32::from(m.y),
                width: i32::from(m.width),
                height: i32::from(m.height),
                scale: translate::monitor_scale(u32::from(m.width), m.width_in_millimeters),
            })
            .collect();
        if self.monitors.is_empty() {
            let s = &self.conn.setup().roots[self.screen];
            self.monitors.push(Monitor {
                x: 0,
                y: 0,
                width: i32::from(s.width_in_pixels),
                height: i32::from(s.height_in_pixels),
                scale: translate::monitor_scale(
                    u32::from(s.width_in_pixels),
                    u32::from(s.width_in_millimeters),
                ),
            });
        }
    }

    /// The scale for content whose centre is at `centre` on the root window.
    fn scale_at(&self, centre: (i32, i32)) -> f64 {
        if let Some(scale) = self.xft_scale {
            return scale;
        }
        self.monitors
            .iter()
            .find(|m| {
                (m.x..m.x + m.width).contains(&centre.0)
                    && (m.y..m.y + m.height).contains(&centre.1)
            })
            .or(self.monitors.first())
            .map_or(1.0, |m| m.scale)
    }

    fn index(&self, id: WindowId) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    fn index_xid(&self, xid: xproto::Window) -> Option<usize> {
        self.windows.iter().position(|w| w.xid == xid)
    }

    fn create_window(
        &mut self,
        id: WindowId,
        config: &WindowConfig,
    ) -> Result<xproto::Window, PlatformError> {
        let conn = self.conn.clone();
        let screen = &conn.setup().roots[self.screen];
        let scale = self.scale_at((
            i32::from(screen.width_in_pixels) / 2,
            i32::from(screen.height_in_pixels) / 2,
        ));
        let width = physical(config.logical_size.0, scale);
        let height = physical(config.logical_size.1, scale);
        let create = || -> Result<xproto::Window, Box<dyn std::error::Error>> {
            let xid = conn.generate_id()?;
            conn.create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                xid,
                self.root,
                0,
                0,
                width,
                height,
                0,
                WindowClass::INPUT_OUTPUT,
                screen.root_visual,
                &CreateWindowAux::new()
                    .background_pixmap(x11rb::NONE)
                    .bit_gravity(Gravity::NORTH_WEST)
                    .event_mask(
                        EventMask::EXPOSURE
                            | EventMask::STRUCTURE_NOTIFY
                            | EventMask::PROPERTY_CHANGE
                            | EventMask::FOCUS_CHANGE
                            | EventMask::KEY_PRESS
                            | EventMask::KEY_RELEASE,
                    ),
            )?;
            let atoms = &self.atoms;
            conn.change_property32(
                PropMode::REPLACE,
                xid,
                atoms.WM_PROTOCOLS,
                AtomEnum::ATOM,
                &[atoms.WM_DELETE_WINDOW, atoms._NET_WM_PING],
            )?;
            let (instance, class) = wm_class();
            let mut wm_class = Vec::with_capacity(instance.len() + class.len() + 2);
            wm_class.extend_from_slice(instance.as_bytes());
            wm_class.push(0);
            wm_class.extend_from_slice(class.as_bytes());
            wm_class.push(0);
            conn.change_property8(
                PropMode::REPLACE,
                xid,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                &wm_class,
            )?;
            conn.change_property32(
                PropMode::REPLACE,
                xid,
                atoms._NET_WM_PID,
                AtomEnum::CARDINAL,
                &[std::process::id()],
            )?;
            conn.change_property8(
                PropMode::REPLACE,
                xid,
                AtomEnum::WM_CLIENT_MACHINE,
                AtomEnum::STRING,
                hostname().as_bytes(),
            )?;
            conn.change_property32(
                PropMode::REPLACE,
                xid,
                atoms._NET_WM_WINDOW_TYPE,
                AtomEnum::ATOM,
                &[atoms._NET_WM_WINDOW_TYPE_NORMAL],
            )?;
            let mut hints = WmHints::new();
            hints.input = Some(true);
            hints.initial_state = Some(WmHintsState::Normal);
            hints.set(&*conn, xid)?;
            if config.chrome == WindowChrome::SelfDrawn {
                // Motif hints: the decorations field is set, and says none.
                conn.change_property32(
                    PropMode::REPLACE,
                    xid,
                    atoms._MOTIF_WM_HINTS,
                    atoms._MOTIF_WM_HINTS,
                    &[2, 0, 0, 0, 0],
                )?;
            }
            xinput::xi_select_events(
                &*conn,
                xid,
                &[xinput::EventMask {
                    deviceid: xinput::Device::ALL_MASTER.into(),
                    mask: vec![
                        XIEventMask::BUTTON_PRESS
                            | XIEventMask::BUTTON_RELEASE
                            | XIEventMask::MOTION
                            | XIEventMask::ENTER
                            | XIEventMask::LEAVE
                            | XIEventMask::DEVICE_CHANGED
                            | XIEventMask::TOUCH_BEGIN
                            | XIEventMask::TOUCH_UPDATE
                            | XIEventMask::TOUCH_END,
                    ],
                }],
            )?;
            Ok(xid)
        };
        let xid = create().map_err(|e| PlatformError::WindowCreation(e.to_string()))?;
        self.set_title(xid, &config.title);
        let _ = conn.map_window(xid);
        self.windows.push(WinState {
            id,
            xid,
            chrome: config.chrome,
            scale,
            size: (u32::from(width), u32::from(height)),
            origin: (0, 0),
            fullscreen: false,
            maximized: false,
            drag: Vec::new(),
            cursor: CursorIcon::Default,
            shown: None,
            edge: None,
            buttons: PointerButtons::NONE,
            pen_pressure: 0.0,
            last_pointer: (f64::NAN, f64::NAN),
            last_caption_press: None,
            ime_enabled: false,
            delivered: KeySet::default(),
        });
        if let Some(ime) = &mut self.ime {
            ime.add_window(xid);
        }
        self.pump.push_redraw(id);
        conn.flush().map_err(backend_error)?;
        Ok(xid)
    }

    fn set_title(&self, xid: xproto::Window, title: &str) {
        let _ = self.conn.change_property8(
            PropMode::REPLACE,
            xid,
            self.atoms._NET_WM_NAME,
            self.atoms.UTF8_STRING,
            title.as_bytes(),
        );
        let _ = self.conn.change_property8(
            PropMode::REPLACE,
            xid,
            AtomEnum::WM_NAME,
            AtomEnum::STRING,
            title.as_bytes(),
        );
    }

    /// Ask the window manager to change `_NET_WM_STATE`: `data` is
    /// `[action, first, second, source, 0]`.
    fn send_wm_state(&self, xid: xproto::Window, data: [u32; 5]) {
        self.send_to_root(ClientMessageEvent::new(
            32,
            xid,
            self.atoms._NET_WM_STATE,
            data,
        ));
    }

    fn send_to_root(&self, event: ClientMessageEvent) {
        let _ = self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        );
    }

    fn close(&mut self, id: WindowId) {
        let Some(i) = self.index(id) else { return };
        let win = self.windows.remove(i);
        if let Some(ime) = &mut self.ime {
            ime.remove_window(win.xid);
        }
        let _ = self.conn.destroy_window(win.xid);
        self.pump.forget_window(id);
        self.pump.push(RawEvent::WindowClosed { window: id });
    }

    fn copy(&mut self, text: &str) {
        let _ = self
            .clipboard
            .set(&self.conn, &self.atoms, text, self.last_time);
    }

    fn chore(&mut self, chore: Chore) {
        match chore {
            Chore::Paste(window) => {
                if let Ok(Some(text)) =
                    self.clipboard
                        .request(&self.conn, &self.atoms, window, self.last_time)
                {
                    self.pump.push(RawEvent::Paste { window, text });
                }
            }
            Chore::Minimize(window) => {
                if let Some(i) = self.index(window) {
                    const ICONIC_STATE: u32 = 3;
                    self.send_to_root(ClientMessageEvent::new(
                        32,
                        self.windows[i].xid,
                        self.atoms.WM_CHANGE_STATE,
                        [ICONIC_STATE, 0, 0, 0, 0],
                    ));
                }
            }
        }
    }

    fn show_cursor(&mut self, i: usize, icon: CursorIcon) {
        if self.windows[i].shown == Some(icon) {
            return;
        }
        let Some(cursor) = self.cursor(icon) else {
            return;
        };
        self.windows[i].shown = Some(icon);
        let _ = self.conn.change_window_attributes(
            self.windows[i].xid,
            &ChangeWindowAttributesAux::new().cursor(cursor),
        );
    }

    fn cursor(&mut self, icon: CursorIcon) -> Option<xproto::Cursor> {
        if let Some((_, c)) = self.cursors.iter().find(|(i, _)| *i == icon) {
            return Some(*c);
        }
        let cursor = if icon == CursorIcon::Hidden {
            self.invisible_cursor()?
        } else {
            let theme = self.cursor_theme.as_ref()?;
            translate::cursor_names(icon)
                .iter()
                .filter_map(|name| theme.load_cursor(&*self.conn, name).ok())
                .find(|c| *c != x11rb::NONE)?
        };
        self.cursors.push((icon, cursor));
        Some(cursor)
    }

    fn invisible_cursor(&self) -> Option<xproto::Cursor> {
        let pixmap = self.conn.generate_id().ok()?;
        let cursor = self.conn.generate_id().ok()?;
        self.conn.create_pixmap(1, pixmap, self.root, 1, 1).ok()?;
        let made = self
            .conn
            .create_cursor(cursor, pixmap, pixmap, 0, 0, 0, 0, 0, 0, 0, 0);
        let _ = self.conn.free_pixmap(pixmap);
        made.ok().map(|_| cursor)
    }

    fn modifiers(&self, fallback: u32) -> Modifiers {
        match &self.keyboard {
            Some(kb) if kb.has_keymap() => kb.modifiers(),
            _ => input::modifiers(fallback),
        }
    }

    // ---- events ----------------------------------------------------------

    fn handle(&mut self, event: Event) {
        if let Some(ime) = &mut self.ime {
            let consumed = ime.filter(&event);
            let out = ime.take();
            if ime.is_lost() {
                self.ime = None;
            }
            for out in out {
                self.ime_out(out);
            }
            if consumed {
                return;
            }
        }
        match event {
            Event::Expose(e) if e.count == 0 => {
                if let Some(i) = self.index_xid(e.window) {
                    self.pump.push_redraw(self.windows[i].id);
                }
            }
            Event::ConfigureNotify(e) => self.on_configure(e.window, e.width, e.height),
            Event::ClientMessage(e) => self.on_client_message(&e),
            Event::FocusIn(e) => self.on_focus(e.event, e.detail, true),
            Event::FocusOut(e) => self.on_focus(e.event, e.detail, false),
            Event::KeyPress(e) => self.on_key(&e, true),
            Event::KeyRelease(e) => self.on_key(&e, false),
            Event::PropertyNotify(e) => self.on_property(&e),
            Event::SelectionNotify(e) => {
                if let Ok(Some((window, text))) =
                    self.clipboard.on_notify(&self.conn, &self.atoms, &e)
                {
                    self.pump.push(RawEvent::Paste { window, text });
                }
            }
            Event::SelectionRequest(e) => {
                let _ = self.clipboard.on_request(&self.conn, &self.atoms, &e);
            }
            Event::SelectionClear(e) => self.clipboard.on_clear(&self.atoms, &e),
            Event::XkbStateNotify(e) => {
                self.last_time = e.time;
                if let Some(kb) = &mut self.keyboard {
                    kb.update_mask(
                        u32::from(e.base_mods),
                        u32::from(e.latched_mods),
                        u32::from(e.locked_mods),
                        group(e.base_group),
                        group(e.latched_group),
                        u32::from(u8::from(e.locked_group)),
                    );
                }
            }
            Event::XkbNewKeyboardNotify(_) | Event::XkbMapNotify(_) => self.reload_keymap(),
            Event::RandrScreenChangeNotify(_) | Event::RandrNotify(_) => {
                self.refresh_monitors();
                self.rescale_all();
            }
            Event::XinputHierarchy(_) => {
                let _ = self.devices.refresh(&*self.conn, self.atoms.ABS_PRESSURE);
            }
            Event::XinputDeviceChanged(e) => {
                if e.reason == xinput::ChangeReason::SLAVE_SWITCH {
                    self.devices.reset_scroll();
                } else {
                    self.devices
                        .changed(e.deviceid, &e.classes, self.atoms.ABS_PRESSURE);
                }
            }
            Event::XinputEnter(e) => {
                self.last_time = e.time;
                self.devices.reset_scroll();
                if let Some(i) = self.index_xid(e.event) {
                    let icon = self.windows[i].cursor;
                    self.windows[i].shown = None;
                    self.show_cursor(i, icon);
                }
            }
            Event::XinputLeave(e) => {
                self.last_time = e.time;
                if let Some(i) = self.index_xid(e.event) {
                    let win = &mut self.windows[i];
                    win.edge = None;
                    win.last_pointer = (f64::NAN, f64::NAN);
                    let (x, y) = (
                        input::fp1616(e.event_x) / win.scale,
                        input::fp1616(e.event_y) / win.scale,
                    );
                    let pointer = RawPointer::mouse(
                        win.id,
                        x,
                        y,
                        win.buttons,
                        self.modifiers(e.mods.effective),
                        PointerPhase::Left,
                    );
                    self.pump.push(RawEvent::Pointer(pointer));
                }
            }
            Event::XinputButtonPress(e) => self.on_button(&e, true),
            Event::XinputButtonRelease(e) => self.on_button(&e, false),
            Event::XinputMotion(e) => self.on_motion(&e),
            Event::XinputTouchBegin(e) => self.on_touch(&e, PointerPhase::Down),
            Event::XinputTouchUpdate(e) => self.on_touch(&e, PointerPhase::Moved),
            Event::XinputTouchEnd(e) => self.on_touch(&e, PointerPhase::Up),
            _ => {}
        }
    }

    fn reload_keymap(&mut self) {
        let (Some(kb), Some(device)) = (&mut self.keyboard, self.xkb_device) else {
            return;
        };
        // SAFETY: the connection is live and its XKB extension was set up
        // when `xkb_device` was found.
        unsafe { kb.set_keymap_x11(self.conn.get_raw_xcb_connection(), device) };
    }

    fn on_configure(&mut self, xid: xproto::Window, width: u16, height: u16) {
        let Some(i) = self.index_xid(xid) else { return };
        let size = (u32::from(width), u32::from(height));
        if self.windows[i].size != size {
            self.windows[i].size = size;
            let id = self.windows[i].id;
            self.pump.push(RawEvent::Resized {
                window: id,
                width: size.0,
                height: size.1,
            });
            self.pump.push_redraw(id);
        }
        if self.xft_scale.is_none() && self.monitors.len() > 1 {
            if let Ok(Ok(t)) = self
                .conn
                .translate_coordinates(xid, self.root, 0, 0)
                .map(|c| c.reply())
            {
                self.windows[i].origin = (i32::from(t.dst_x), i32::from(t.dst_y));
            }
            self.rescale(i);
        }
    }

    fn rescale_all(&mut self) {
        for i in 0..self.windows.len() {
            self.rescale(i);
        }
    }

    /// Follow the scale the window's monitor (or `Xft.dpi`) asks for,
    /// keeping its logical size.
    fn rescale(&mut self, i: usize) {
        let win = &self.windows[i];
        let centre = (
            win.origin.0 + i32::try_from(win.size.0 / 2).unwrap_or(0),
            win.origin.1 + i32::try_from(win.size.1 / 2).unwrap_or(0),
        );
        let scale = self.scale_at(centre);
        let win = &mut self.windows[i];
        if (scale - win.scale).abs() < f64::EPSILON {
            return;
        }
        let ratio = scale / win.scale;
        win.scale = scale;
        let width = physical(f64::from(win.size.0) * ratio, 1.0);
        let height = physical(f64::from(win.size.1) * ratio, 1.0);
        win.size = (u32::from(width), u32::from(height));
        let (id, xid) = (win.id, win.xid);
        let _ = self.conn.configure_window(
            xid,
            &ConfigureWindowAux::new()
                .width(u32::from(width))
                .height(u32::from(height)),
        );
        self.pump.push(RawEvent::ScaleFactorChanged {
            window: id,
            scale,
            width: u32::from(width),
            height: u32::from(height),
        });
        self.pump.push_redraw(id);
    }

    fn on_client_message(&mut self, e: &ClientMessageEvent) {
        if e.type_ != self.atoms.WM_PROTOCOLS || e.format != 32 {
            return;
        }
        let data = e.data.as_data32();
        if data[0] == self.atoms.WM_DELETE_WINDOW {
            if let Some(i) = self.index_xid(e.window) {
                self.pump.push(RawEvent::CloseRequested {
                    window: self.windows[i].id,
                    accept: AcceptCell::new(),
                });
            }
        } else if data[0] == self.atoms._NET_WM_PING {
            let mut reply = *e;
            reply.window = self.root;
            self.send_to_root(reply);
        }
    }

    fn on_focus(&mut self, xid: xproto::Window, detail: NotifyDetail, focused: bool) {
        if detail == NotifyDetail::POINTER {
            return;
        }
        let Some(i) = self.index_xid(xid) else { return };
        let id = self.windows[i].id;
        if !focused {
            let modifiers = self.modifiers(0);
            for key in self.windows[i].delivered.take_all() {
                self.pump.key(
                    id,
                    translate::key_code(u32::from(key)),
                    None,
                    modifiers,
                    false,
                    false,
                );
            }
        }
        if let Some(kb) = &self.keyboard {
            kb.reset_compose();
        }
        if let Some(ime) = &mut self.ime {
            ime.focus(xid, focused);
        }
        self.pump.push(RawEvent::WindowFocused {
            window: id,
            focused,
        });
    }

    fn on_property(&mut self, e: &xproto::PropertyNotifyEvent) {
        self.last_time = e.time;
        if e.window == self.root {
            if e.atom == u32::from(AtomEnum::RESOURCE_MANAGER) {
                self.load_resources();
                self.rescale_all();
                for i in 0..self.windows.len() {
                    let icon = self.windows[i].cursor;
                    self.show_cursor(i, icon);
                }
            }
            return;
        }
        if let Some(i) = self.index_xid(e.window) {
            if e.atom == self.atoms._NET_WM_STATE {
                self.read_wm_state(i);
            }
            return;
        }
        if let Ok(Some((window, text))) = self.clipboard.on_property(&self.conn, &self.atoms, e) {
            self.pump.push(RawEvent::Paste { window, text });
        }
    }

    fn read_wm_state(&mut self, i: usize) {
        let xid = self.windows[i].xid;
        let Ok(Ok(reply)) = self
            .conn
            .get_property(false, xid, self.atoms._NET_WM_STATE, AtomEnum::ATOM, 0, 64)
            .map(|c| c.reply())
        else {
            return;
        };
        let states: Vec<u32> = reply.value32().map(Iterator::collect).unwrap_or_default();
        let atoms = &self.atoms;
        let fullscreen = states.contains(&atoms._NET_WM_STATE_FULLSCREEN);
        let maximized = states.contains(&atoms._NET_WM_STATE_MAXIMIZED_VERT)
            && states.contains(&atoms._NET_WM_STATE_MAXIMIZED_HORZ);
        let win = &mut self.windows[i];
        win.maximized = maximized;
        if win.fullscreen != fullscreen {
            win.fullscreen = fullscreen;
            self.pump.push(RawEvent::FullscreenChanged {
                window: win.id,
                fullscreen,
            });
        }
    }

    // ---- keys ------------------------------------------------------------

    fn on_key(&mut self, e: &KeyPressEvent, pressed: bool) {
        self.last_time = e.time;
        let Some(i) = self.index_xid(e.event) else {
            return;
        };
        let to_ime = self.windows[i].ime_enabled
            && self.ime.as_ref().is_some_and(|ime| ime.active_for(e.event));
        if to_ime {
            let delivered = self.windows[i].delivered.contains(e.detail);
            let forwarded = self.ime.as_mut().is_some_and(|ime| ime.forward(e.event, e));
            if forwarded {
                // A press comes back as `ImeOut::Forward` unless the server
                // keeps it; a release goes straight to the app when its press
                // did.
                if !pressed && delivered {
                    self.deliver_key(i, e, false);
                }
                return;
            }
        }
        self.deliver_key(i, e, pressed);
    }

    fn deliver_key(&mut self, i: usize, e: &KeyPressEvent, pressed: bool) {
        let keycode = e.detail;
        let win = &mut self.windows[i];
        let id = win.id;
        let repeat = pressed && win.delivered.contains(keycode);
        if pressed {
            win.delivered.insert(keycode);
        } else if !win.delivered.remove(keycode) {
            return;
        }
        let code = u32::from(keycode);
        if repeat && self.keyboard.as_ref().is_some_and(|kb| !kb.repeats(code)) {
            return;
        }
        let modifiers = self.modifiers(u32::from(e.state));
        let base = self.keyboard.as_ref().and_then(|kb| kb.base_char(code));
        let route = self.pump.key(
            id,
            translate::key_code(code),
            base,
            modifiers,
            pressed,
            repeat,
        );
        if route.consumed {
            self.windows[i].delivered.remove(keycode);
        }
        if let Some(chore) = route.chore {
            self.chore(chore);
        }
        if !pressed || route.consumed || modifiers.control || modifiers.alt || modifiers.logo {
            return;
        }
        if let Some(kb) = &mut self.keyboard
            && let KeyText::Text(text) = kb.press_text(code)
        {
            self.pump.push(RawEvent::Text(RawText { window: id, text }));
        }
    }

    fn ime_out(&mut self, out: ImeOut) {
        match out {
            ImeOut::Commit { window, text } => {
                if let Some(i) = self.index_xid(window) {
                    let id = self.windows[i].id;
                    self.pump.push(RawEvent::Text(RawText { window: id, text }));
                }
            }
            ImeOut::Forward { window, event } => {
                // Releases were delivered directly; only presses return.
                if event.response_type & 0x7f == xproto::KEY_PRESS_EVENT
                    && let Some(i) = self.index_xid(window)
                {
                    self.deliver_key(i, &event, true);
                }
            }
            ImeOut::Preedit {
                window,
                text,
                caret,
            } => {
                if let Some(i) = self.index_xid(window) {
                    let caret = translate::char_to_byte(&text, caret);
                    self.pump.push(RawEvent::ImePreedit(RawImePreedit {
                        window: self.windows[i].id,
                        text,
                        caret,
                    }));
                }
            }
        }
    }

    // ---- pointer ---------------------------------------------------------

    fn on_button(&mut self, e: &xinput::ButtonPressEvent, pressed: bool) {
        self.last_time = e.time;
        if e.flags
            .contains(xinput::PointerEventFlags::POINTER_EMULATED)
        {
            return;
        }
        let Some(i) = self.index_xid(e.event) else {
            return;
        };
        let scale = self.windows[i].scale;
        let (x, y) = (
            input::fp1616(e.event_x) / scale,
            input::fp1616(e.event_y) / scale,
        );
        let modifiers = self.modifiers(e.mods.effective);
        let id = self.windows[i].id;
        if let Some((dx, dy)) = input::wheel_button(e.detail) {
            if pressed && !self.devices.has_scroll(e.sourceid) {
                self.pump.push(RawEvent::Scroll(RawScroll {
                    window: id,
                    x,
                    y,
                    delta_x: dx,
                    delta_y: dy,
                    modifiers,
                }));
            }
            return;
        }
        let button = input::button(e.detail);
        if button.is_empty() {
            return;
        }
        if pressed && button == PointerButtons::PRIMARY && self.chrome_press(i, e, x, y) {
            return;
        }
        let win = &mut self.windows[i];
        win.buttons = if pressed {
            PointerButtons(win.buttons.0 | button.0)
        } else {
            PointerButtons(win.buttons.0 & !button.0)
        };
        let kind = self.devices.kind(e.sourceid);
        let pressure = if kind == PointerKind::Pen {
            if pressed {
                win.pen_pressure.max(0.5)
            } else {
                0.0
            }
        } else if win.buttons.is_empty() {
            0.0
        } else {
            0.5
        };
        let phase = if pressed {
            PointerPhase::Down
        } else {
            PointerPhase::Up
        };
        win.last_pointer = (x, y);
        let pointer = RawPointer {
            window: id,
            pointer: input::pointer_id(kind, e.sourceid, 0),
            kind,
            x,
            y,
            pressure,
            buttons: win.buttons,
            modifiers,
            phase,
        };
        self.pump.push(RawEvent::Pointer(pointer));
    }

    /// A primary press on a self-drawn window's edge or caption: hand it to
    /// the window manager as a resize or move. `true` when it did.
    fn chrome_press(&mut self, i: usize, e: &xinput::ButtonPressEvent, x: f64, y: f64) -> bool {
        let win = &mut self.windows[i];
        if win.chrome != WindowChrome::SelfDrawn || win.fullscreen {
            return false;
        }
        let logical = (
            f64::from(win.size.0) / win.scale,
            f64::from(win.size.1) / win.scale,
        );
        let edge = if win.maximized {
            None
        } else {
            translate::resize_edge(x, y, logical, RESIZE_BORDER)
        };
        let direction = match edge {
            Some(edge) => moveresize_direction(edge),
            None if win.drag.iter().any(|r| translate::rect_contains(r, x, y)) => {
                if translate::is_double_click(win.last_caption_press, e.time, x, y) {
                    win.last_caption_press = None;
                    let xid = win.xid;
                    let atoms = &self.atoms;
                    let data = [
                        2,
                        atoms._NET_WM_STATE_MAXIMIZED_VERT,
                        atoms._NET_WM_STATE_MAXIMIZED_HORZ,
                        1,
                        0,
                    ];
                    self.send_wm_state(xid, data);
                    return true;
                }
                win.last_caption_press = Some((e.time, x, y));
                MOVE
            }
            None => return false,
        };
        let xid = win.xid;
        // The window manager can only take the pointer once the implicit
        // grab this press started is released.
        let _ = xinput::xi_ungrab_device(&*self.conn, e.time, e.deviceid);
        let _ = self.conn.ungrab_pointer(e.time);
        let root_x = input::fp1616(e.root_x).round() as u32;
        let root_y = input::fp1616(e.root_y).round() as u32;
        self.send_to_root(ClientMessageEvent::new(
            32,
            xid,
            self.atoms._NET_WM_MOVERESIZE,
            [root_x, root_y, direction, 1, 1],
        ));
        true
    }

    fn on_motion(&mut self, e: &xinput::MotionEvent) {
        self.last_time = e.time;
        if e.flags
            .contains(xinput::PointerEventFlags::POINTER_EMULATED)
        {
            return;
        }
        let Some(i) = self.index_xid(e.event) else {
            return;
        };
        let modifiers = self.modifiers(e.mods.effective);
        let motion = self
            .devices
            .motion(e.sourceid, &e.valuator_mask, &e.axisvalues);
        let kind = self.devices.kind(e.sourceid);
        let win = &mut self.windows[i];
        let (x, y) = (
            input::fp1616(e.event_x) / win.scale,
            input::fp1616(e.event_y) / win.scale,
        );
        let id = win.id;
        if motion.scroll != (0.0, 0.0) {
            self.pump.push(RawEvent::Scroll(RawScroll {
                window: id,
                x,
                y,
                delta_x: motion.scroll.0,
                delta_y: motion.scroll.1,
                modifiers,
            }));
        }
        if let Some(p) = motion.pressure {
            win.pen_pressure = p;
        }
        if win.last_pointer == (x, y) && motion.pressure.is_none() {
            return;
        }
        win.last_pointer = (x, y);
        if win.chrome == WindowChrome::SelfDrawn && win.buttons.is_empty() {
            let logical = (
                f64::from(win.size.0) / win.scale,
                f64::from(win.size.1) / win.scale,
            );
            let edge = if win.maximized || win.fullscreen {
                None
            } else {
                translate::resize_edge(x, y, logical, RESIZE_BORDER)
            };
            if edge != win.edge {
                win.edge = edge;
                let icon = edge.map_or(win.cursor, Edge::cursor);
                self.show_cursor(i, icon);
            }
        }
        let win = &self.windows[i];
        let pressure = match kind {
            PointerKind::Pen if !win.buttons.is_empty() => win.pen_pressure,
            PointerKind::Pen => 0.0,
            _ if win.buttons.is_empty() => 0.0,
            _ => 0.5,
        };
        let pointer = RawPointer {
            window: id,
            pointer: input::pointer_id(kind, e.sourceid, 0),
            kind,
            x,
            y,
            pressure,
            buttons: win.buttons,
            modifiers,
            phase: PointerPhase::Moved,
        };
        self.pump.push(RawEvent::Pointer(pointer));
    }

    fn on_touch(&mut self, e: &xinput::TouchBeginEvent, phase: PointerPhase) {
        self.last_time = e.time;
        let Some(i) = self.index_xid(e.event) else {
            return;
        };
        let win = &self.windows[i];
        let down = phase != PointerPhase::Up;
        let pointer = RawPointer {
            window: win.id,
            pointer: input::pointer_id(PointerKind::Touch, e.sourceid, e.detail),
            kind: PointerKind::Touch,
            x: input::fp1616(e.event_x) / win.scale,
            y: input::fp1616(e.event_y) / win.scale,
            pressure: if down { 0.5 } else { 0.0 },
            buttons: if down {
                PointerButtons::PRIMARY
            } else {
                PointerButtons::NONE
            },
            modifiers: self.modifiers(e.mods.effective),
            phase,
        };
        self.pump.push(RawEvent::Pointer(pointer));
    }
}

impl Display for X11 {
    fn pump(&mut self) -> &mut Pump {
        &mut self.pump
    }

    fn pump_ref(&self) -> &Pump {
        &self.pump
    }

    fn dispatch(&mut self) -> bool {
        loop {
            match self.conn.poll_for_event() {
                Ok(Some(event)) => self.handle(event),
                Ok(None) => break,
                Err(_) => return false,
            }
        }
        self.conn.flush().is_ok()
    }

    fn wait(&mut self, wake_fd: RawFd, deadline: Option<Instant>) -> bool {
        // Replies read since the last dispatch may have queued events the
        // socket no longer signals.
        if !self.dispatch() {
            return false;
        }
        if self.pump.has_work() {
            return true;
        }
        poll_readable([self.conn.as_raw_fd(), wake_fd], deadline);
        self.dispatch()
    }

    fn before_redraw(&mut self, _window: WindowId) {}

    fn finish(&mut self, after: After) {
        match after {
            After::Copy(text) => self.copy(&text),
            After::Close(window) => self.close(window),
            After::Redrawn(_) => {}
        }
        let _ = self.conn.flush();
    }
}

/// A logical length in physical pixels, as an X window dimension.
fn physical(logical: f64, scale: f64) -> u16 {
    (logical * scale).round().clamp(1.0, f64::from(i16::MAX)) as u16
}

fn to_i16(v: f64) -> i16 {
    v.round().clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

/// An XKB group number from a signed delta field.
fn group(g: i16) -> u32 {
    u32::from(g.unsigned_abs()) & 0x3
}

/// The `_NET_WM_MOVERESIZE` direction that resizes from `edge`.
fn moveresize_direction(edge: Edge) -> u32 {
    match edge {
        Edge::TopLeft => 0,
        Edge::Top => 1,
        Edge::TopRight => 2,
        Edge::Right => 3,
        Edge::BottomRight => 4,
        Edge::Bottom => 5,
        Edge::BottomLeft => 6,
        Edge::Left => 7,
    }
}

/// `WM_CLASS` from the executable name: instance as is, class capitalized.
fn wm_class() -> (String, String) {
    let instance = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "viso".into());
    let mut chars = instance.chars();
    let class = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    (instance, class)
}

fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is writable for its length; on success it holds a
    // NUL-terminated name (truncation leaves it unterminated, handled below).
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } != 0 {
        return String::new();
    }
    let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..len]).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_set_tracks_every_keycode() {
        let mut set = KeySet::default();
        for k in [0u8, 8, 63, 64, 127, 128, 255] {
            set.insert(k);
            assert!(set.contains(k));
        }
        assert!(!set.contains(9));
        assert!(set.remove(64));
        assert!(!set.remove(64));
        let rest: Vec<u8> = set.take_all().collect();
        assert_eq!(rest, [0, 8, 63, 127, 128, 255]);
        assert_eq!(set, KeySet::default());
    }

    #[test]
    fn edges_map_to_moveresize_directions() {
        assert_eq!(moveresize_direction(Edge::TopLeft), 0);
        assert_eq!(moveresize_direction(Edge::Right), 3);
        assert_eq!(moveresize_direction(Edge::Left), 7);
        assert_ne!(moveresize_direction(Edge::Left), MOVE);
    }

    #[test]
    fn sizes_round_and_stay_in_protocol_range() {
        assert_eq!(physical(100.0, 1.5), 150);
        assert_eq!(physical(0.0, 2.0), 1);
        assert_eq!(physical(1e9, 1.0), 32767);
        assert_eq!(to_i16(-1e9), i16::MIN);
        assert_eq!(group(-1), 1);
        assert_eq!(group(2), 2);
    }

    #[test]
    fn wm_class_capitalizes_the_class() {
        let (instance, class) = wm_class();
        assert!(!instance.is_empty());
        assert_eq!(instance.chars().count(), class.chars().count());
        assert!(class.chars().next().is_some_and(|c| !c.is_lowercase()));
    }
}
