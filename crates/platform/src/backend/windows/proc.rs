//! The window procedure: Win32 messages to [`RawEvent`]s.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::rc::Rc;

use ::windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use ::windows::Win32::Graphics::Gdi::{ScreenToClient, ValidateRect};
use ::windows::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_EXCLUDE, CFS_POINT, COMPOSITIONFORM, CPS_CANCEL, GCS_COMPSTR, GCS_CURSORPOS,
    GCS_RESULTSTR, HIMC, IACE_DEFAULT, IME_COMPOSITION_STRING, ISC_SHOWUICOMPOSITIONWINDOW,
    ImmAssociateContextEx, ImmGetCompositionStringW, ImmGetContext, ImmNotifyIME,
    ImmReleaseContext, ImmSetCandidateWindow, ImmSetCompositionWindow, NI_COMPOSITIONSTR,
};
use ::windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, ReleaseCapture, SetCapture, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent,
    VK_CONTROL, VK_LWIN, VK_MENU, VK_RWIN, VK_SHIFT,
};
use ::windows::Win32::UI::Input::Pointer::{
    GetPointerInfo, GetPointerPenInfo, GetPointerTouchInfo, POINTER_INFO, POINTER_PEN_INFO,
    POINTER_TOUCH_INFO,
};
use ::windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, DefWindowProcW, GWLP_USERDATA, GetWindowLongPtrW, HTCLIENT, KillTimer, PT_PEN,
    PT_TOUCH, PostMessageW, SW_MINIMIZE, SWP_NOACTIVATE, SWP_NOZORDER, SetTimer, SetWindowLongPtrW,
    SetWindowPos, ShowWindow, WM_CAPTURECHANGED, WM_CHAR, WM_CLOSE, WM_COMMAND, WM_DESTROY,
    WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_ERASEBKGND, WM_EXITSIZEMOVE, WM_IME_COMPOSITION,
    WM_IME_ENDCOMPOSITION, WM_IME_SETCONTEXT, WM_IME_STARTCOMPOSITION, WM_KEYDOWN, WM_KEYUP,
    WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP, WM_MOUSEHWHEEL,
    WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_POINTERCAPTURECHANGED,
    WM_POINTERDOWN, WM_POINTERLEAVE, WM_POINTERUP, WM_POINTERUPDATE, WM_RBUTTONDOWN, WM_RBUTTONUP,
    WM_SETCURSOR, WM_SETFOCUS, WM_SETTINGCHANGE, WM_SIZE, WM_SYSCOLORCHANGE, WM_SYSKEYDOWN,
    WM_SYSKEYUP, WM_THEMECHANGED, WM_TIMER, WM_UNICHAR, WM_XBUTTONDOWN, WM_XBUTTONUP,
};

use super::translate::{self, MenuAction};
use super::{WindowState, drive, system};
use crate::control::ControlFlow;
use crate::event::{
    AcceptCell, ClipboardReply, ClipboardShortcut, KeyCode, Modifiers, PointerButtons, PointerId,
    PointerKind, PointerPhase, RawEvent, RawImePreedit, RawKey, RawPointer, RawScroll, RawText,
    clipboard_shortcut,
};
use crate::menu::SystemAction;
use crate::{Instant, LogicalRect};

const WM_MOUSELEAVE: u32 = 0x02A3;
const SIZE_MINIMIZED: usize = 1;
const UNICODE_NOCHAR: usize = 0xFFFF;
const VK_PROCESSKEY: u16 = 0xE5;
const XBUTTON1: usize = 1;
/// The timer that keeps frames flowing while a modal size/move loop owns
/// the thread.
const MODAL_TIMER: usize = 1;
const MODAL_TICK_MS: u32 = 16;

const POINTER_FLAG_INCONTACT: u32 = 0x4;
const POINTER_FLAG_SECONDBUTTON: u32 = 0x20;
const POINTER_FLAG_CANCELED: u32 = 0x8000;
const PEN_MASK_PRESSURE: u32 = 0x1;
const TOUCH_MASK_PRESSURE: u32 = 0x4;

fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}

fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}

/// Signed client or screen coordinates packed in an `LPARAM`.
fn point_of(lparam: LPARAM) -> (i32, i32) {
    let v = lparam.0 as usize;
    (i32::from(loword(v) as i16), i32::from(hiword(v) as i16))
}

pub(super) unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_NCCREATE {
        // SAFETY: for `WM_NCCREATE`, `lparam` points at the `CREATESTRUCTW`
        // whose `lpCreateParams` is the leaked `Rc<WindowState>` that
        // `create_window` passed; the window adopts that count here.
        unsafe {
            let create = &*(lparam.0 as *const CREATESTRUCTW);
            let state = create.lpCreateParams as *const WindowState;
            if !state.is_null() {
                (*state).hwnd.set(hwnd);
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
    }
    // SAFETY: `GWLP_USERDATA` is zero or the pointer adopted above, which
    // stays valid until `WM_NCDESTROY` below clears the slot and releases it.
    unsafe {
        let raw = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const WindowState;
        if raw.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        if msg == WM_NCDESTROY {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            drop(Rc::from_raw(raw));
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        // Hold a count for the call, so a nested `WM_NCDESTROY` cannot free
        // the state under this frame.
        Rc::increment_strong_count(raw);
        let state = Rc::from_raw(raw);
        let handled = catch_unwind(AssertUnwindSafe(|| {
            handle(&state, hwnd, msg, wparam, lparam)
        }));
        match handled {
            Ok(Some(result)) => result,
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

/// `None` hands the message to `DefWindowProcW`.
fn handle(
    state: &Rc<WindowState>,
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> Option<LRESULT> {
    let id = state.id;
    match msg {
        WM_ERASEBKGND => Some(LRESULT(1)),
        WM_PAINT => {
            // SAFETY: validating our own live window's update region.
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            state.shared.borrow_mut().push_redraw(id);
            if state.in_size_move.get() {
                drive(&state.shared);
            }
            Some(LRESULT(0))
        }
        WM_SIZE => {
            if wparam.0 == SIZE_MINIMIZED {
                return Some(LRESULT(0));
            }
            let v = lparam.0 as usize;
            let size = (u32::from(loword(v)), u32::from(hiword(v)));
            if !state.ready.get() || state.dpi_changing.get() || size == state.size.get() {
                state.size.set(size);
                return Some(LRESULT(0));
            }
            state.size.set(size);
            state.push(RawEvent::Resized {
                window: id,
                width: size.0,
                height: size.1,
            });
            state.shared.borrow_mut().push_redraw(id);
            if state.in_size_move.get() {
                drive(&state.shared);
            }
            Some(LRESULT(0))
        }
        WM_DPICHANGED => {
            let dpi = u32::from(loword(wparam.0)).max(96);
            // SAFETY: for `WM_DPICHANGED`, `lparam` points at the suggested
            // window rect for the new DPI.
            let rect = unsafe { *(lparam.0 as *const RECT) };
            state.dpi.set(dpi);
            state.dpi_changing.set(true);
            // SAFETY: resizing our own live window to the rect Windows chose.
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
            state.dpi_changing.set(false);
            let (width, height) = state.client_size();
            state.size.set((width, height));
            if state.ready.get() {
                state.push(RawEvent::ScaleFactorChanged {
                    window: id,
                    scale: state.scale(),
                    width,
                    height,
                });
                state.shared.borrow_mut().push_redraw(id);
            }
            place_ime(state);
            Some(LRESULT(0))
        }
        WM_ENTERSIZEMOVE => {
            state.in_size_move.set(true);
            // SAFETY: a timer on our own live window, killed on exit.
            unsafe { SetTimer(Some(hwnd), MODAL_TIMER, MODAL_TICK_MS, None) };
            None
        }
        WM_EXITSIZEMOVE => {
            state.in_size_move.set(false);
            // SAFETY: the timer set on entry.
            let _ = unsafe { KillTimer(Some(hwnd), MODAL_TIMER) };
            None
        }
        WM_TIMER if wparam.0 == MODAL_TIMER => {
            modal_tick(state);
            Some(LRESULT(0))
        }
        WM_CLOSE => {
            state.push(RawEvent::CloseRequested {
                window: id,
                accept: AcceptCell::new(),
            });
            Some(LRESULT(0))
        }
        WM_DESTROY => {
            state.alive.set(false);
            {
                let mut q = state.shared.borrow_mut();
                q.windows.retain(|w| !Rc::ptr_eq(w, state));
                q.redraws.retain(|w| *w != id);
                q.events.push_back(RawEvent::WindowClosed { window: id });
            }
            Some(LRESULT(0))
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            state.push(RawEvent::WindowFocused {
                window: id,
                focused: msg == WM_SETFOCUS,
            });
            None
        }
        WM_SETCURSOR if u32::from(loword(lparam.0 as usize)) == HTCLIENT => {
            system::apply_cursor(state.cursor.get());
            Some(LRESULT(1))
        }
        WM_MOUSEMOVE => {
            track_leave(state, hwnd);
            let (x, y) = point_of(lparam);
            mouse(state, x, y, state.buttons.get(), PointerPhase::Moved);
            Some(LRESULT(0))
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN => {
            let (x, y) = point_of(lparam);
            button(state, hwnd, x, y, button_of(msg), true);
            Some(LRESULT(0))
        }
        WM_LBUTTONUP | WM_RBUTTONUP | WM_MBUTTONUP => {
            let (x, y) = point_of(lparam);
            button(state, hwnd, x, y, button_of(msg), false);
            Some(LRESULT(0))
        }
        WM_XBUTTONDOWN | WM_XBUTTONUP => {
            if usize::from(hiword(wparam.0)) == XBUTTON1 {
                state.push(RawEvent::Key(RawKey {
                    window: id,
                    code: KeyCode::Back,
                    pressed: msg == WM_XBUTTONDOWN,
                    repeat: false,
                    modifiers: modifiers(),
                }));
            }
            Some(LRESULT(1))
        }
        WM_CAPTURECHANGED => {
            if HWND(lparam.0 as *mut _) != hwnd && !state.buttons.get().is_empty() {
                state.buttons.set(PointerButtons::NONE);
                let (x, y) = state.last_mouse.get();
                state.push(RawEvent::Pointer(RawPointer::mouse(
                    id,
                    x,
                    y,
                    PointerButtons::NONE,
                    modifiers(),
                    PointerPhase::Cancel,
                )));
            }
            Some(LRESULT(0))
        }
        WM_MOUSELEAVE => {
            state.tracking_leave.set(false);
            let (x, y) = state.last_mouse.get();
            state.push(RawEvent::Pointer(RawPointer::mouse(
                id,
                x,
                y,
                state.buttons.get(),
                modifiers(),
                PointerPhase::Left,
            )));
            Some(LRESULT(0))
        }
        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            let (sx, sy) = point_of(lparam);
            let mut pt = POINT { x: sx, y: sy };
            // SAFETY: converting a screen point for our own live window.
            let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
            let (x, y) = state.logical_point(pt.x, pt.y);
            let raw = hiword(wparam.0) as i16;
            let (lines, chars) = state.shared.borrow().wheel;
            let (delta_x, delta_y) = if msg == WM_MOUSEWHEEL {
                let page = translate::to_logical(state.size.get().1 as i32, state.dpi.get());
                (0.0, translate::wheel_delta(raw, lines, page))
            } else {
                (translate::hwheel_delta(raw, chars), 0.0)
            };
            state.push(RawEvent::Scroll(RawScroll {
                window: id,
                x,
                y,
                delta_x,
                delta_y,
                modifiers: modifiers(),
            }));
            Some(LRESULT(0))
        }
        WM_POINTERDOWN
        | WM_POINTERUP
        | WM_POINTERUPDATE
        | WM_POINTERLEAVE
        | WM_POINTERCAPTURECHANGED => pointer(state, hwnd, msg, wparam),
        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let vk = wparam.0 as u16;
            if vk == VK_PROCESSKEY {
                return None;
            }
            let bits = lparam.0 as usize;
            let pressed = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            let scan = ((bits >> 16) & 0xFF) as u32;
            let extended = bits & (1 << 24) != 0;
            if let Some(code) = translate::key_code(scan, extended, vk) {
                let mods = modifiers();
                state.push(RawEvent::Key(RawKey {
                    window: id,
                    code,
                    pressed,
                    repeat: pressed && bits & (1 << 30) != 0,
                    modifiers: mods,
                }));
                if pressed && let Some(shortcut) = clipboard_shortcut(code, mods) {
                    clipboard(state, hwnd, shortcut);
                }
            }
            // System keys keep their default meaning: Alt and F10 open the
            // menu bar, Alt+F4 closes, Alt+Space opens the system menu.
            if msg == WM_SYSKEYDOWN || msg == WM_SYSKEYUP {
                None
            } else {
                Some(LRESULT(0))
            }
        }
        WM_CHAR => {
            let c = state.surrogates.borrow_mut().push(wparam.0 as u16);
            if let Some(c) = c.filter(|c| translate::is_text(*c)) {
                state.push(RawEvent::Text(RawText {
                    window: id,
                    text: c.to_string(),
                }));
            }
            Some(LRESULT(0))
        }
        WM_UNICHAR => {
            if wparam.0 == UNICODE_NOCHAR {
                return Some(LRESULT(1));
            }
            if let Some(c) = char::from_u32(wparam.0 as u32).filter(|c| translate::is_text(*c)) {
                state.push(RawEvent::Text(RawText {
                    window: id,
                    text: c.to_string(),
                }));
            }
            Some(LRESULT(0))
        }
        WM_IME_SETCONTEXT => {
            // The composition is drawn inline as preedit, not in the IME's
            // own window.
            let lparam = LPARAM(lparam.0 & !(ISC_SHOWUICOMPOSITIONWINDOW as isize));
            // SAFETY: forwarding the message with the adjusted flags.
            Some(unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) })
        }
        WM_IME_STARTCOMPOSITION => {
            place_ime(state);
            Some(LRESULT(0))
        }
        WM_IME_COMPOSITION => {
            composition(state, hwnd, lparam.0 as u32);
            Some(LRESULT(0))
        }
        WM_IME_ENDCOMPOSITION => {
            clear_preedit(state);
            Some(LRESULT(0))
        }
        WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
            system::refresh_settings(&state.shared);
            None
        }
        WM_COMMAND if lparam.0 == 0 && hiword(wparam.0) <= 1 => {
            let action = {
                let q = state.shared.borrow();
                q.menu.as_ref().and_then(|m| m.action(loword(wparam.0)))
            };
            let action = action?;
            menu_action(state, hwnd, action);
            Some(LRESULT(0))
        }
        _ => None,
    }
}

/// Queue a `Wakeup` when the runtime's timer comes due during a modal loop,
/// then drive whatever is queued.
fn modal_tick(state: &WindowState) {
    let due = match state.shared.borrow().flow {
        Some(ControlFlow::WaitUntil(deadline)) => Instant::now() >= deadline,
        Some(ControlFlow::Poll) => true,
        _ => false,
    };
    if due {
        state.push(RawEvent::Wakeup);
    }
    drive(&state.shared);
}

fn modifiers() -> Modifiers {
    // SAFETY: `GetKeyState` reads this thread's keyboard state; no pointers.
    let down = |vk: u16| unsafe { GetKeyState(i32::from(vk)) } < 0;
    Modifiers {
        shift: down(VK_SHIFT.0),
        control: down(VK_CONTROL.0),
        alt: down(VK_MENU.0),
        logo: down(VK_LWIN.0) || down(VK_RWIN.0),
    }
}

fn button_of(msg: u32) -> PointerButtons {
    match msg {
        WM_LBUTTONDOWN | WM_LBUTTONUP => PointerButtons::PRIMARY,
        WM_RBUTTONDOWN | WM_RBUTTONUP => PointerButtons::SECONDARY,
        _ => PointerButtons::MIDDLE,
    }
}

fn track_leave(state: &WindowState, hwnd: HWND) {
    if state.tracking_leave.get() {
        return;
    }
    let mut track = TRACKMOUSEEVENT {
        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    // SAFETY: `track` is fully initialized for our own live window.
    if unsafe { TrackMouseEvent(&mut track) }.is_ok() {
        state.tracking_leave.set(true);
    }
}

fn mouse(state: &WindowState, x: i32, y: i32, buttons: PointerButtons, phase: PointerPhase) {
    let (x, y) = state.logical_point(x, y);
    state.last_mouse.set((x, y));
    state.push(RawEvent::Pointer(RawPointer::mouse(
        state.id,
        x,
        y,
        buttons,
        modifiers(),
        phase,
    )));
}

/// A press captures the mouse so the release arrives even outside the
/// window; the last release lets it go.
fn button(state: &WindowState, hwnd: HWND, x: i32, y: i32, b: PointerButtons, down: bool) {
    let before = state.buttons.get();
    if down {
        let after = PointerButtons(before.0 | b.0);
        state.buttons.set(after);
        if before.is_empty() {
            // SAFETY: capturing to our own live window.
            unsafe { SetCapture(hwnd) };
        }
        mouse(state, x, y, after, PointerPhase::Down);
    } else {
        if !before.contains(b) {
            return;
        }
        let after = PointerButtons(before.0 & !b.0);
        state.buttons.set(after);
        mouse(state, x, y, after, PointerPhase::Up);
        if after.is_empty() {
            // SAFETY: releasing the capture taken on the first press; the
            // resulting `WM_CAPTURECHANGED` sees no held buttons.
            let _ = unsafe { ReleaseCapture() };
        }
    }
}

/// Touch and pen. Mouse input stays on the `WM_*BUTTON*` path.
fn pointer(state: &WindowState, hwnd: HWND, msg: u32, wparam: WPARAM) -> Option<LRESULT> {
    let win32_id = u32::from(loword(wparam.0));
    let pointer = translate::pointer_id(win32_id);
    if msg == WM_POINTERCAPTURECHANGED {
        cancel_contact(state, win32_id, pointer);
        return Some(LRESULT(0));
    }
    let mut info = POINTER_INFO::default();
    // SAFETY: `info` is a valid out-pointer for the pointer this message names.
    unsafe { GetPointerInfo(win32_id, &mut info) }.ok()?;
    let (kind, pressure) = if info.pointerType == PT_TOUCH {
        let mut touch = POINTER_TOUCH_INFO::default();
        // SAFETY: as above, for a touch pointer.
        let has = unsafe { GetPointerTouchInfo(win32_id, &mut touch) }.is_ok()
            && touch.touchMask & TOUCH_MASK_PRESSURE != 0;
        (PointerKind::Touch, (has, touch.pressure))
    } else if info.pointerType == PT_PEN {
        let mut pen = POINTER_PEN_INFO::default();
        // SAFETY: as above, for a pen pointer.
        let has = unsafe { GetPointerPenInfo(win32_id, &mut pen) }.is_ok()
            && pen.penMask & PEN_MASK_PRESSURE != 0;
        (PointerKind::Pen, (has, pen.pressure))
    } else {
        // Mouse input (`PT_MOUSE`) and touchpads stay on the legacy path.
        return None;
    };
    let flags = info.pointerFlags.0;
    let in_contact = flags & POINTER_FLAG_INCONTACT != 0;
    let phase = if flags & POINTER_FLAG_CANCELED != 0 {
        PointerPhase::Cancel
    } else {
        match msg {
            WM_POINTERDOWN => PointerPhase::Down,
            WM_POINTERUP => PointerPhase::Up,
            WM_POINTERLEAVE => PointerPhase::Left,
            _ => PointerPhase::Moved,
        }
    };
    {
        let mut contacts = state.contacts.borrow_mut();
        match phase {
            PointerPhase::Down => contacts.push((win32_id, kind)),
            PointerPhase::Up | PointerPhase::Cancel => contacts.retain(|c| c.0 != win32_id),
            _ => {}
        }
    }
    let mut buttons = PointerButtons::NONE;
    if in_contact {
        buttons = PointerButtons(buttons.0 | PointerButtons::PRIMARY.0);
    }
    if flags & POINTER_FLAG_SECONDBUTTON != 0 {
        buttons = PointerButtons(buttons.0 | PointerButtons::SECONDARY.0);
    }
    let mut pt = info.ptPixelLocation;
    // SAFETY: converting a screen point for our own live window.
    let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
    let (x, y) = state.logical_point(pt.x, pt.y);
    state.push(RawEvent::Pointer(RawPointer {
        window: state.id,
        pointer,
        kind,
        x,
        y,
        pressure: translate::pressure(pressure.0, pressure.1, in_contact),
        buttons,
        modifiers: modifiers(),
        phase,
    }));
    Some(LRESULT(0))
}

fn cancel_contact(state: &WindowState, win32_id: u32, pointer: PointerId) {
    let kind = {
        let mut contacts = state.contacts.borrow_mut();
        let Some(i) = contacts.iter().position(|c| c.0 == win32_id) else {
            return;
        };
        contacts.remove(i).1
    };
    let (x, y) = state.last_mouse.get();
    state.push(RawEvent::Pointer(RawPointer {
        window: state.id,
        pointer,
        kind,
        x,
        y,
        pressure: 0.0,
        buttons: PointerButtons::NONE,
        modifiers: modifiers(),
        phase: PointerPhase::Cancel,
    }));
}

fn clipboard(state: &WindowState, hwnd: HWND, shortcut: ClipboardShortcut) {
    let window = state.id;
    match shortcut {
        ClipboardShortcut::Copy | ClipboardShortcut::Cut => {
            state.push(RawEvent::CopyRequested {
                window,
                cut: shortcut == ClipboardShortcut::Cut,
                reply: ClipboardReply::new(),
            });
        }
        ClipboardShortcut::Paste => {
            if let Some(text) = system::read_clipboard(Some(hwnd)) {
                state.push(RawEvent::Paste { window, text });
            }
        }
    }
}

fn menu_action(state: &WindowState, hwnd: HWND, action: MenuAction) {
    match action {
        MenuAction::Command(id) => state.push(RawEvent::MenuCommand { id }),
        MenuAction::System(SystemAction::Quit) => state.shared.borrow_mut().should_exit = true,
        MenuAction::System(SystemAction::CloseWindow) => {
            // SAFETY: posting to our own live window; the close then runs
            // the ordinary veto handshake.
            let _ = unsafe { PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)) };
        }
        MenuAction::System(SystemAction::Hide | SystemAction::Minimize) => {
            // SAFETY: minimizing our own live window.
            let _ = unsafe { ShowWindow(hwnd, SW_MINIMIZE) };
        }
        MenuAction::System(SystemAction::Copy) => clipboard(state, hwnd, ClipboardShortcut::Copy),
        MenuAction::System(SystemAction::Cut) => clipboard(state, hwnd, ClipboardShortcut::Cut),
        MenuAction::System(SystemAction::Paste) => {
            clipboard(state, hwnd, ClipboardShortcut::Paste);
        }
    }
}

/// The window's input context, released on drop.
struct ImeContext {
    hwnd: HWND,
    himc: HIMC,
}

impl ImeContext {
    fn open(hwnd: HWND) -> Option<Self> {
        // SAFETY: querying our own live window's input context.
        let himc = unsafe { ImmGetContext(hwnd) };
        (!himc.is_invalid()).then_some(Self { hwnd, himc })
    }

    fn string(&self, kind: IME_COMPOSITION_STRING) -> String {
        // SAFETY: a size query (no buffer) on a live context.
        let bytes = unsafe { ImmGetCompositionStringW(self.himc, kind, None, 0) };
        let Ok(len) = usize::try_from(bytes) else {
            return String::new();
        };
        let mut units = vec![0u16; len / 2];
        // SAFETY: `units` holds `len` bytes, the size just reported.
        let got = unsafe {
            ImmGetCompositionStringW(
                self.himc,
                kind,
                Some(units.as_mut_ptr().cast()),
                (units.len() * 2) as u32,
            )
        };
        units.truncate(usize::try_from(got).unwrap_or(0) / 2);
        String::from_utf16_lossy(&units)
    }

    /// The caret within the composition string, in UTF-16 units.
    fn cursor(&self) -> usize {
        // SAFETY: `GCS_CURSORPOS` returns the position directly, no buffer.
        let pos = unsafe { ImmGetCompositionStringW(self.himc, GCS_CURSORPOS, None, 0) };
        usize::try_from(pos).unwrap_or(0)
    }
}

impl Drop for ImeContext {
    fn drop(&mut self) {
        // SAFETY: releasing the context `open` obtained for this window.
        let _ = unsafe { ImmReleaseContext(self.hwnd, self.himc) };
    }
}

fn composition(state: &WindowState, hwnd: HWND, flags: u32) {
    let Some(ime) = ImeContext::open(hwnd) else {
        return;
    };
    if flags & GCS_RESULTSTR.0 != 0 {
        let text = ime.string(GCS_RESULTSTR);
        // A commit supersedes the preedit it replaces.
        state.preedit_shown.set(false);
        if !text.is_empty() {
            state.push(RawEvent::Text(RawText {
                window: state.id,
                text,
            }));
        }
    }
    if flags & GCS_COMPSTR.0 != 0 {
        let text = ime.string(GCS_COMPSTR);
        let caret = if flags & GCS_CURSORPOS.0 != 0 {
            translate::utf16_to_byte(&text, ime.cursor())
        } else {
            text.len()
        };
        if text.is_empty() && !state.preedit_shown.get() {
            return;
        }
        state.preedit_shown.set(!text.is_empty());
        state.push(RawEvent::ImePreedit(RawImePreedit {
            window: state.id,
            text,
            caret,
        }));
    }
    drop(ime);
    place_ime(state);
}

fn clear_preedit(state: &WindowState) {
    if state.preedit_shown.replace(false) {
        state.push(RawEvent::ImePreedit(RawImePreedit {
            window: state.id,
            text: String::new(),
            caret: 0,
        }));
    }
}

/// Put the composition window at the caret and keep the candidate list off
/// the caret line.
fn place_ime(state: &WindowState) {
    let Some(caret) = state.ime_caret.get() else {
        return;
    };
    let Some(ime) = ImeContext::open(state.hwnd.get()) else {
        return;
    };
    let top_left = state.physical_point(caret.x, caret.y);
    let bottom_right = state.physical_point(caret.x + caret.width, caret.y + caret.height);
    let area = RECT {
        left: top_left.x,
        top: top_left.y,
        right: bottom_right.x.max(top_left.x + 1),
        bottom: bottom_right.y,
    };
    let composition = COMPOSITIONFORM {
        dwStyle: CFS_POINT,
        ptCurrentPos: top_left,
        rcArea: area,
    };
    let candidate = CANDIDATEFORM {
        dwIndex: 0,
        dwStyle: CFS_EXCLUDE,
        ptCurrentPos: top_left,
        rcArea: area,
    };
    // SAFETY: both forms are fully initialized; the context is live.
    unsafe {
        let _ = ImmSetCompositionWindow(ime.himc, &composition);
        let _ = ImmSetCandidateWindow(ime.himc, &candidate);
    }
}

/// `None` detaches the input context so keys reach the window unconverted;
/// `Some` reattaches it and places its windows at the caret.
pub(super) fn set_ime_area(state: &WindowState, caret: Option<LogicalRect>) {
    let hwnd = state.hwnd.get();
    state.ime_caret.set(caret);
    match caret {
        None => {
            if !state.ime_enabled.replace(false) {
                return;
            }
            if let Some(ime) = ImeContext::open(hwnd) {
                // SAFETY: cancelling any open composition on a live context.
                let _ = unsafe { ImmNotifyIME(ime.himc, NI_COMPOSITIONSTR, CPS_CANCEL, 0) };
            }
            clear_preedit(state);
            // SAFETY: associating no context disables the IME for the window.
            let _ = unsafe { ImmAssociateContextEx(hwnd, HIMC::default(), 0) };
        }
        Some(_) => {
            if !state.ime_enabled.replace(true) {
                // SAFETY: restoring the window's default input context.
                let _ = unsafe { ImmAssociateContextEx(hwnd, HIMC::default(), IACE_DEFAULT) };
            }
            place_ime(state);
        }
    }
}
