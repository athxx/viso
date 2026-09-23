//! The seat: pointer (buttons, frames of wheel/touchpad axes, self-drawn
//! chrome moves and resizes, cursors), keyboard (the compositor's keymap,
//! client-side repeat) and touch.

use std::os::fd::{AsRawFd, OwnedFd};
use std::time::Duration;

use wayland_client::protocol::{
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
    wl_seat::{self, WlSeat},
    wl_surface::WlSurface,
    wl_touch::{self, WlTouch},
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, delegate_noop};
use wayland_cursor::CursorTheme;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    self, WpCursorShapeDeviceV1,
};

use super::text_input::TextInput;
use super::{Globals, State, known, resize_edge};
use crate::Instant;
use crate::backend::linux::translate::{self, Edge, RESIZE_BORDER};
use crate::backend::linux::xkb::{KeyText, Keyboard};
use crate::control::{WindowChrome, WindowId};
use crate::event::{
    Modifiers, PointerButtons, PointerId, PointerKind, PointerPhase, RawEvent, RawPointer,
    RawScroll, RawText,
};

/// Linux input-event button codes.
const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;
const BTN_MIDDLE: u32 = 0x112;

/// Touch points get ids of their own, apart from the mouse.
const TOUCH_BIT: u64 = 1 << 32;

/// The theme cursor size when `XCURSOR_SIZE` does not say.
const DEFAULT_CURSOR_SIZE: u32 = 24;

/// Axis frames arrive from `wl_pointer` v5.
const FRAME_VERSION: u32 = 5;

pub(super) struct Seat {
    /// The registry name, to notice the seat's removal.
    pub(super) name: u32,
    seat: WlSeat,
    pointer: Option<Pointer>,
    keyboard: Option<WlKeyboard>,
    touch: Option<WlTouch>,
    text_input: Option<TextInput>,
    /// The latest input serial, for requests that need a user action.
    pub(super) last_serial: u32,
    xkb: Option<Keyboard>,
    key_focus: Option<WindowId>,
    /// Keycodes whose press reached the app, so each release does too.
    delivered: Vec<u32>,
    repeat: Repeat,
    touches: Vec<Touch>,
}

struct Pointer {
    pointer: WlPointer,
    shape: Option<WpCursorShapeDeviceV1>,
    focus: Option<WindowId>,
    /// The serial of the latest enter, which cursor requests quote.
    enter_serial: u32,
    x: f64,
    y: f64,
    axes: [Axis; 2],
    surface: WlSurface,
    theme: Option<Theme>,
}

/// One axis's scrolling within a pointer frame.
#[derive(Default, Clone, Copy)]
struct Axis {
    value120: Option<i32>,
    discrete: Option<i32>,
    continuous: f64,
}

impl Axis {
    /// The frame's scroll in logical points; high-resolution wheel steps
    /// win over legacy notches, which win over the continuous value.
    fn delta(self) -> f64 {
        match (self.value120, self.discrete) {
            (Some(v), _) => translate::wheel_notches(f64::from(v) / 120.0),
            (None, Some(n)) => translate::wheel_notches(f64::from(n)),
            (None, None) => self.continuous,
        }
    }
}

struct Theme {
    theme: CursorTheme,
    scale: i32,
}

struct Repeat {
    /// Keys per second; 0 disables repeat.
    rate: i32,
    /// Milliseconds before the first repeat.
    delay: i32,
    key: Option<(WindowId, u32)>,
    next: Option<Instant>,
}

struct Touch {
    id: i32,
    window: WindowId,
    x: f64,
    y: f64,
}

impl Seat {
    pub(super) fn new(seat: WlSeat, name: u32, globals: &Globals, qh: &QueueHandle<State>) -> Self {
        let text_input = globals
            .text_input
            .as_ref()
            .map(|m| TextInput::new(m.get_text_input(&seat, qh, ())));
        Self {
            name,
            seat,
            pointer: None,
            keyboard: None,
            touch: None,
            text_input,
            last_serial: 0,
            xkb: Keyboard::new(),
            key_focus: None,
            delivered: Vec::new(),
            repeat: Repeat {
                rate: 25,
                delay: 600,
                key: None,
                next: None,
            },
            touches: Vec::new(),
        }
    }

    /// The seat went away: drop its devices.
    pub(super) fn release(mut self, state: &mut State) {
        if let Some(window) = self.key_focus.take() {
            self.leave_keyboard(state, window);
        }
        self.set_pointer(false, state);
        self.set_keyboard(false);
        self.set_touch(false);
        if let Some(ti) = self.text_input.take() {
            ti.destroy();
        }
        state.clipboard.detach_seat();
        if self.seat.version() >= 5 {
            self.seat.release();
        }
    }

    pub(super) fn forget_window(&mut self, window: WindowId) {
        if self.key_focus == Some(window) {
            self.key_focus = None;
            self.delivered.clear();
            self.repeat.key = None;
            self.repeat.next = None;
        }
        if let Some(p) = &mut self.pointer
            && p.focus == Some(window)
        {
            p.focus = None;
        }
        self.touches.retain(|t| t.window != window);
        if let Some(ti) = &mut self.text_input {
            ti.forget_window(window);
        }
    }

    fn modifiers(&self) -> Modifiers {
        self.xkb
            .as_ref()
            .map(Keyboard::modifiers)
            .unwrap_or_default()
    }

    // ---- capabilities --------------------------------------------------

    fn set_pointer(&mut self, present: bool, state: &State) {
        match (present, &self.pointer) {
            (true, None) => {
                let pointer = self.seat.get_pointer(&state.qh, ());
                let shape = state
                    .globals
                    .cursor_shape
                    .as_ref()
                    .map(|m| m.get_pointer(&pointer, &state.qh, ()));
                let surface = state.globals.compositor.create_surface(&state.qh, ());
                self.pointer = Some(Pointer {
                    pointer,
                    shape,
                    focus: None,
                    enter_serial: 0,
                    x: 0.0,
                    y: 0.0,
                    axes: [Axis::default(); 2],
                    surface,
                    theme: None,
                });
            }
            (false, Some(_)) => {
                if let Some(p) = self.pointer.take() {
                    if let Some(shape) = p.shape {
                        shape.destroy();
                    }
                    p.surface.destroy();
                    if p.pointer.version() >= 3 {
                        p.pointer.release();
                    }
                }
            }
            _ => {}
        }
    }

    fn set_keyboard(&mut self, present: bool) {
        if !present && let Some(kb) = self.keyboard.take() {
            if kb.version() >= 3 {
                kb.release();
            }
            self.repeat.key = None;
            self.repeat.next = None;
        }
    }

    fn set_touch(&mut self, present: bool) {
        if !present && let Some(touch) = self.touch.take() {
            if touch.version() >= 3 {
                touch.release();
            }
            self.touches.clear();
        }
    }

    // ---- cursor --------------------------------------------------------

    /// Show the cursor the pointer's window asks for (its resize edge's,
    /// when over one).
    pub(super) fn apply_cursor(&mut self, state: &State) {
        let Some(p) = &mut self.pointer else { return };
        let Some(i) = p.focus.and_then(|w| state.index(w)) else {
            return;
        };
        let win = &state.windows[i];
        let icon = win.edge.map_or(win.cursor, Edge::cursor);
        let serial = p.enter_serial;
        if let Some(device) = &p.shape {
            match translate::cursor_shape(icon)
                .and_then(|v| wp_cursor_shape_device_v1::Shape::try_from(v).ok())
            {
                Some(shape) => device.set_shape(serial, shape),
                None => p.pointer.set_cursor(serial, None, 0, 0),
            }
            return;
        }
        let names = translate::cursor_names(icon);
        if names.is_empty() {
            p.pointer.set_cursor(serial, None, 0, 0);
            return;
        }
        let scale = (win.scale.ceil() as i32).max(1);
        if p.theme.as_ref().is_none_or(|t| t.scale != scale) {
            let size = cursor_size() * scale as u32;
            p.theme = CursorTheme::load(&state.conn, state.globals.shm.clone(), size)
                .ok()
                .map(|theme| Theme { theme, scale });
        }
        let Some(theme) = &mut p.theme else { return };
        let Some(name) = names.iter().find(|n| theme.theme.get_cursor(n).is_some()) else {
            return;
        };
        let Some(cursor) = theme.theme.get_cursor(name) else {
            return;
        };
        let image = &cursor[0];
        let (width, height) = image.dimensions();
        let (hot_x, hot_y) = image.hotspot();
        p.surface.attach(Some(&**image), 0, 0);
        p.surface.set_buffer_scale(scale);
        p.surface.damage_buffer(0, 0, width as i32, height as i32);
        p.surface.commit();
        p.pointer.set_cursor(
            serial,
            Some(&p.surface),
            hot_x as i32 / scale,
            hot_y as i32 / scale,
        );
    }

    // ---- pointer -------------------------------------------------------

    fn on_pointer(&mut self, state: &mut State, event: wl_pointer::Event) {
        let modifiers = self.modifiers();
        let Some(p) = &mut self.pointer else { return };
        match event {
            wl_pointer::Event::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                self.last_serial = serial;
                p.enter_serial = serial;
                p.focus = surface.data::<WindowId>().copied();
                p.x = surface_x;
                p.y = surface_y;
                self.hover(state);
                self.apply_cursor(state);
                self.push_pointer(state, PointerPhase::Moved, modifiers);
            }
            wl_pointer::Event::Leave { .. } => {
                if let Some(window) = p.focus
                    && let Some(i) = state.index(window)
                {
                    state.windows[i].edge = None;
                }
                self.push_pointer(state, PointerPhase::Left, modifiers);
                if let Some(p) = &mut self.pointer {
                    p.focus = None;
                }
            }
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                p.x = surface_x;
                p.y = surface_y;
                self.hover(state);
                self.push_pointer(state, PointerPhase::Moved, modifiers);
            }
            wl_pointer::Event::Button {
                serial,
                time,
                button,
                state: button_state,
            } => {
                self.last_serial = serial;
                let button = match button {
                    BTN_LEFT => PointerButtons::PRIMARY,
                    BTN_RIGHT => PointerButtons::SECONDARY,
                    BTN_MIDDLE => PointerButtons::MIDDLE,
                    _ => return,
                };
                let pressed = known(button_state) == Some(wl_pointer::ButtonState::Pressed);
                self.on_button(state, button, pressed, serial, time, modifiers);
            }
            wl_pointer::Event::Axis { axis, value, .. } => {
                if let Some(a) = axis_index(known(axis)) {
                    p.axes[a].continuous += value;
                }
                if p.pointer.version() < FRAME_VERSION {
                    self.flush_axes(state, modifiers);
                }
            }
            wl_pointer::Event::AxisDiscrete { axis, discrete } => {
                if let Some(a) = axis_index(known(axis)) {
                    *p.axes[a].discrete.get_or_insert(0) += discrete;
                }
            }
            wl_pointer::Event::AxisValue120 { axis, value120 } => {
                if let Some(a) = axis_index(known(axis)) {
                    *p.axes[a].value120.get_or_insert(0) += value120;
                }
            }
            wl_pointer::Event::Frame => self.flush_axes(state, modifiers),
            _ => {}
        }
    }

    /// Track the resize edge under the pointer of a self-drawn window and
    /// show its cursor.
    fn hover(&mut self, state: &mut State) {
        let Some(p) = &self.pointer else { return };
        let Some(i) = p.focus.and_then(|w| state.index(w)) else {
            return;
        };
        let (x, y) = (p.x, p.y);
        let win = &mut state.windows[i];
        if win.chrome != WindowChrome::SelfDrawn || !win.buttons.is_empty() {
            return;
        }
        let edge = if win.maximized || win.fullscreen {
            None
        } else {
            translate::resize_edge(x, y, win.logical_size(), RESIZE_BORDER)
        };
        if edge != win.edge {
            win.edge = edge;
            self.apply_cursor(state);
        }
    }

    fn on_button(
        &mut self,
        state: &mut State,
        button: PointerButtons,
        pressed: bool,
        serial: u32,
        time: u32,
        modifiers: Modifiers,
    ) {
        let Some(p) = &self.pointer else { return };
        let Some(i) = p.focus.and_then(|w| state.index(w)) else {
            return;
        };
        let (x, y) = (p.x, p.y);
        if pressed
            && button == PointerButtons::PRIMARY
            && self.chrome_press(state, i, serial, time, x, y)
        {
            return;
        }
        let win = &mut state.windows[i];
        win.buttons = if pressed {
            PointerButtons(win.buttons.0 | button.0)
        } else {
            PointerButtons(win.buttons.0 & !button.0)
        };
        let phase = if pressed {
            PointerPhase::Down
        } else {
            PointerPhase::Up
        };
        self.push_pointer(state, phase, modifiers);
    }

    /// A primary press on a self-drawn window's edge or caption: hand it to
    /// the compositor as an interactive resize or move. `true` when it did.
    fn chrome_press(
        &self,
        state: &mut State,
        i: usize,
        serial: u32,
        time: u32,
        x: f64,
        y: f64,
    ) -> bool {
        let win = &mut state.windows[i];
        if win.chrome != WindowChrome::SelfDrawn || win.fullscreen {
            return false;
        }
        let edge = if win.maximized {
            None
        } else {
            translate::resize_edge(x, y, win.logical_size(), RESIZE_BORDER)
        };
        if let Some(edge) = edge {
            win.toplevel.resize(&self.seat, serial, resize_edge(edge));
            return true;
        }
        if !win.drag.iter().any(|r| translate::rect_contains(r, x, y)) {
            return false;
        }
        if translate::is_double_click(win.last_caption_press, time, x, y) {
            win.last_caption_press = None;
            if win.maximized {
                win.toplevel.unset_maximized();
            } else {
                win.toplevel.set_maximized();
            }
        } else {
            win.last_caption_press = Some((time, x, y));
            win.toplevel._move(&self.seat, serial);
        }
        true
    }

    fn push_pointer(&self, state: &mut State, phase: PointerPhase, modifiers: Modifiers) {
        let Some(p) = &self.pointer else { return };
        let Some(window) = p.focus else { return };
        let Some(i) = state.index(window) else { return };
        let buttons = state.windows[i].buttons;
        state.pump.push(RawEvent::Pointer(RawPointer {
            window,
            pointer: PointerId::MOUSE,
            kind: PointerKind::Mouse,
            x: p.x,
            y: p.y,
            pressure: if buttons.is_empty() { 0.0 } else { 0.5 },
            buttons,
            modifiers,
            phase,
        }));
    }

    fn flush_axes(&mut self, state: &mut State, modifiers: Modifiers) {
        let Some(p) = &mut self.pointer else { return };
        let [h, v] = std::mem::take(&mut p.axes);
        let (delta_x, delta_y) = (h.delta(), v.delta());
        if (delta_x, delta_y) == (0.0, 0.0) {
            return;
        }
        let Some(window) = p.focus else { return };
        state.pump.push(RawEvent::Scroll(RawScroll {
            window,
            x: p.x,
            y: p.y,
            delta_x,
            delta_y,
            modifiers,
        }));
    }

    // ---- keyboard ------------------------------------------------------

    fn on_keyboard(&mut self, state: &mut State, event: wl_keyboard::Event) {
        match event {
            wl_keyboard::Event::Keymap { format, fd, size } => {
                if known(format) == Some(wl_keyboard::KeymapFormat::XkbV1)
                    && let Some(text) = read_keymap(&fd, size as usize)
                    && let Some(xkb) = &mut self.xkb
                {
                    xkb.set_keymap_text(&text);
                }
            }
            wl_keyboard::Event::Enter {
                serial, surface, ..
            } => {
                self.last_serial = serial;
                let Some(&window) = surface.data::<WindowId>() else {
                    return;
                };
                self.key_focus = Some(window);
                state.pump.push(RawEvent::WindowFocused {
                    window,
                    focused: true,
                });
            }
            wl_keyboard::Event::Leave { serial, .. } => {
                self.last_serial = serial;
                if let Some(window) = self.key_focus.take() {
                    self.leave_keyboard(state, window);
                }
            }
            wl_keyboard::Event::Key {
                serial,
                key,
                state: key_state,
                ..
            } => {
                self.last_serial = serial;
                let Some(window) = self.key_focus else { return };
                let code = key + translate::XKB_EVDEV_OFFSET;
                let pressed = known(key_state) == Some(wl_keyboard::KeyState::Pressed);
                if pressed {
                    let repeats = self.repeat.rate > 0
                        && self.xkb.as_ref().is_some_and(|kb| kb.repeats(code));
                    if repeats {
                        self.repeat.key = Some((window, code));
                        self.repeat.next = Some(
                            Instant::now() + Duration::from_millis(self.repeat.delay.max(0) as u64),
                        );
                    }
                } else if self.repeat.key == Some((window, code)) {
                    self.repeat.key = None;
                    self.repeat.next = None;
                }
                self.deliver_key(state, window, code, pressed, false);
            }
            wl_keyboard::Event::Modifiers {
                serial,
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
            } => {
                self.last_serial = serial;
                if let Some(xkb) = &mut self.xkb {
                    xkb.update_mask(mods_depressed, mods_latched, mods_locked, 0, 0, group);
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => {
                self.repeat.rate = rate;
                self.repeat.delay = delay;
                if rate <= 0 {
                    self.repeat.key = None;
                    self.repeat.next = None;
                }
            }
            _ => {}
        }
    }

    /// Keyboard focus left `window`: release what it saw pressed.
    fn leave_keyboard(&mut self, state: &mut State, window: WindowId) {
        let modifiers = self.modifiers();
        for code in std::mem::take(&mut self.delivered) {
            state.pump.key(
                window,
                translate::key_code(code),
                None,
                modifiers,
                false,
                false,
            );
        }
        self.repeat.key = None;
        self.repeat.next = None;
        if let Some(xkb) = &self.xkb {
            xkb.reset_compose();
        }
        state.pump.push(RawEvent::WindowFocused {
            window,
            focused: false,
        });
    }

    fn deliver_key(
        &mut self,
        state: &mut State,
        window: WindowId,
        code: u32,
        pressed: bool,
        repeat: bool,
    ) {
        if pressed {
            if !self.delivered.contains(&code) {
                self.delivered.push(code);
            }
        } else if let Some(at) = self.delivered.iter().position(|c| *c == code) {
            self.delivered.swap_remove(at);
        } else {
            return;
        }
        let modifiers = self.modifiers();
        let base = self.xkb.as_ref().and_then(|kb| kb.base_char(code));
        let route = state.pump.key(
            window,
            translate::key_code(code),
            base,
            modifiers,
            pressed,
            repeat,
        );
        if route.consumed {
            self.delivered.retain(|c| *c != code);
            if self.repeat.key == Some((window, code)) {
                self.repeat.key = None;
                self.repeat.next = None;
            }
        }
        if let Some(chore) = route.chore {
            state.chore(chore);
        }
        if !pressed || route.consumed || modifiers.control || modifiers.alt || modifiers.logo {
            return;
        }
        if let Some(xkb) = &mut self.xkb
            && let KeyText::Text(text) = xkb.press_text(code)
        {
            state.pump.push(RawEvent::Text(RawText { window, text }));
        }
    }

    fn tick_repeat(&mut self, state: &mut State, now: Instant) {
        let (Some((window, code)), Some(next)) = (self.repeat.key, self.repeat.next) else {
            return;
        };
        if now < next {
            return;
        }
        let interval = Duration::from_micros(1_000_000 / self.repeat.rate.max(1) as u64);
        // One repeat per tick: a loop that fell behind catches up at the
        // rate rather than in a burst.
        let mut following = next + interval;
        if following <= now {
            following = now + interval;
        }
        self.repeat.next = Some(following);
        self.deliver_key(state, window, code, true, true);
    }

    // ---- touch ---------------------------------------------------------

    fn on_touch(&mut self, state: &mut State, event: wl_touch::Event) {
        let modifiers = self.modifiers();
        match event {
            wl_touch::Event::Down {
                serial,
                surface,
                id,
                x,
                y,
                ..
            } => {
                self.last_serial = serial;
                let Some(&window) = surface.data::<WindowId>() else {
                    return;
                };
                self.touches.retain(|t| t.id != id);
                self.touches.push(Touch { id, window, x, y });
                push_touch(state, window, id, x, y, PointerPhase::Down, modifiers);
            }
            wl_touch::Event::Motion { id, x, y, .. } => {
                if let Some(t) = self.touches.iter_mut().find(|t| t.id == id) {
                    t.x = x;
                    t.y = y;
                    push_touch(state, t.window, id, x, y, PointerPhase::Moved, modifiers);
                }
            }
            wl_touch::Event::Up { serial, id, .. } => {
                self.last_serial = serial;
                if let Some(at) = self.touches.iter().position(|t| t.id == id) {
                    let t = self.touches.swap_remove(at);
                    push_touch(state, t.window, id, t.x, t.y, PointerPhase::Up, modifiers);
                }
            }
            wl_touch::Event::Cancel => {
                for t in std::mem::take(&mut self.touches) {
                    push_touch(
                        state,
                        t.window,
                        t.id,
                        t.x,
                        t.y,
                        PointerPhase::Cancel,
                        modifiers,
                    );
                }
            }
            _ => {}
        }
    }

    pub(super) fn update_text_input(&mut self, state: &mut State, window: WindowId) {
        if let Some(ti) = &mut self.text_input {
            ti.update(state, window);
        }
    }

    pub(super) fn text_input(&mut self) -> Option<&mut TextInput> {
        self.text_input.as_mut()
    }
}

fn push_touch(
    state: &mut State,
    window: WindowId,
    id: i32,
    x: f64,
    y: f64,
    phase: PointerPhase,
    modifiers: Modifiers,
) {
    let down = matches!(phase, PointerPhase::Down | PointerPhase::Moved);
    state.pump.push(RawEvent::Pointer(RawPointer {
        window,
        pointer: PointerId(TOUCH_BIT | u64::from(id as u32)),
        kind: PointerKind::Touch,
        x,
        y,
        pressure: if down { 0.5 } else { 0.0 },
        buttons: if down {
            PointerButtons::PRIMARY
        } else {
            PointerButtons::NONE
        },
        modifiers,
        phase,
    }));
}

fn axis_index(axis: Option<wl_pointer::Axis>) -> Option<usize> {
    match axis? {
        wl_pointer::Axis::HorizontalScroll => Some(0),
        wl_pointer::Axis::VerticalScroll => Some(1),
        _ => None,
    }
}

fn cursor_size() -> u32 {
    std::env::var("XCURSOR_SIZE")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n: &u32| (8..=256).contains(&n))
        .unwrap_or(DEFAULT_CURSOR_SIZE)
}

/// Copy the keymap the compositor shares through `fd`.
fn read_keymap(fd: &OwnedFd, size: usize) -> Option<Vec<u8>> {
    if size == 0 {
        return None;
    }
    // SAFETY: mapping `size` bytes of a descriptor we own, read-only and
    // private, so the compositor's later writes (it must make none) cannot
    // race the copy below.
    let map = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            fd.as_raw_fd(),
            0,
        )
    };
    if map == libc::MAP_FAILED {
        return None;
    }
    // SAFETY: the mapping above is `size` readable bytes until unmapped.
    let text = unsafe { std::slice::from_raw_parts(map.cast::<u8>(), size) }.to_vec();
    // SAFETY: unmapping exactly the region mapped above, after its last use.
    unsafe { libc::munmap(map, size) };
    Some(text)
}

impl State {
    pub(super) fn tick_repeat(&mut self, now: Instant) {
        if let Some(mut seat) = self.seat.take() {
            seat.tick_repeat(self, now);
            self.seat = Some(seat);
        }
    }

    pub(super) fn next_repeat(&self) -> Option<Instant> {
        self.seat.as_ref().and_then(|s| s.repeat.next)
    }

    /// Run `f` on the seat with the rest of the state borrowable.
    pub(super) fn with_seat(&mut self, f: impl FnOnce(&mut Seat, &mut State)) {
        if let Some(mut seat) = self.seat.take() {
            f(&mut seat, self);
            // A removal during `f` cannot happen: registry events are not
            // dispatched from inside a seat event.
            self.seat = Some(seat);
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_seat::Event::Capabilities { capabilities } = event else {
            return;
        };
        let caps = match capabilities {
            wayland_client::WEnum::Value(c) => c,
            wayland_client::WEnum::Unknown(bits) => wl_seat::Capability::from_bits_truncate(bits),
        };
        state.with_seat(|seat, state| {
            seat.set_pointer(caps.contains(wl_seat::Capability::Pointer), state);
            let keyboard = caps.contains(wl_seat::Capability::Keyboard);
            if keyboard && seat.keyboard.is_none() {
                seat.keyboard = Some(seat.seat.get_keyboard(qh, ()));
            }
            seat.set_keyboard(keyboard);
            let touch = caps.contains(wl_seat::Capability::Touch);
            if touch && seat.touch.is_none() {
                seat.touch = Some(seat.seat.get_touch(qh, ()));
            }
            seat.set_touch(touch);
        });
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.with_seat(|seat, state| seat.on_pointer(state, event));
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.with_seat(|seat, state| seat.on_keyboard(state, event));
    }
}

impl Dispatch<WlTouch, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlTouch,
        event: wl_touch::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.with_seat(|seat, state| seat.on_touch(state, event));
    }
}

// The cursor surface: its enter/leave events carry nothing we use.
delegate_noop!(State: ignore WlSurface);
delegate_noop!(State: WpCursorShapeDeviceV1);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn high_resolution_steps_win_over_notches_and_pixels() {
        let wheel = Axis {
            value120: Some(60),
            discrete: Some(1),
            continuous: 10.0,
        };
        assert_eq!(wheel.delta(), translate::wheel_notches(0.5));
        let legacy = Axis {
            value120: None,
            discrete: Some(-2),
            continuous: -20.0,
        };
        assert_eq!(legacy.delta(), translate::wheel_notches(-2.0));
        let touchpad = Axis {
            continuous: 3.5,
            ..Axis::default()
        };
        assert_eq!(touchpad.delta(), 3.5);
    }

    #[test]
    fn touch_ids_stay_apart_from_the_mouse() {
        let id = PointerId(TOUCH_BIT | u64::from(0i32 as u32));
        assert_ne!(id, PointerId::MOUSE);
        assert_ne!(
            PointerId(TOUCH_BIT | u64::from(7i32 as u32)),
            PointerId(TOUCH_BIT | u64::from(8i32 as u32))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn keymaps_are_copied_out_of_their_descriptor() {
        use std::io::Write;
        let mut file = tempfile_in_memory();
        file.write_all(b"xkb_keymap {};\0").unwrap();
        let fd = OwnedFd::from(file);
        assert_eq!(
            read_keymap(&fd, 15).as_deref(),
            Some(&b"xkb_keymap {};\0"[..])
        );
        assert_eq!(read_keymap(&fd, 0), None);
    }

    #[cfg(target_os = "linux")]
    fn tempfile_in_memory() -> std::fs::File {
        use std::os::fd::FromRawFd;
        // SAFETY: a NUL-terminated name; the descriptor is fresh and owned
        // by the returned file.
        unsafe {
            let fd = libc::memfd_create(c"viso-keymap-test".as_ptr(), libc::MFD_CLOEXEC);
            assert!(fd >= 0);
            std::fs::File::from_raw_fd(fd)
        }
    }
}
