//! Native Windows backend (Win32).
//!
//! One `CreateWindowExW` window per [`WindowId`] and a
//! `PeekMessage`/`GetMessage` pump whose blocking follows the runtime's
//! [`ControlFlow`]. The window procedure (`proc`) turns messages into
//! [`RawEvent`]s on the shared queue and the pump drains that queue into the
//! handler between OS messages. Win32's modal loops (a title-bar drag, a border
//! resize) park the pump inside `DispatchMessageW`; for their duration the
//! window procedure drives the handler itself so content tracks the drag.
//!
//! Compile-checked from the development host; runtime behavior needs a
//! Windows machine.

mod menu;
mod proc;
mod system;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ptr::NonNull;
use std::rc::Rc;

use ::windows::Win32::Foundation::{HINSTANCE, HWND, POINT, RECT};
use ::windows::Win32::System::LibraryLoader::GetModuleHandleW;
use ::windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use ::windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DestroyWindow, DispatchMessageW, GWL_EXSTYLE, GWL_STYLE,
    GetClientRect, GetMenu, GetMessageW, GetWindowLongPtrW, MSG, MWMO_INPUTAVAILABLE,
    MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, QS_ALLINPUT, RegisterClassExW, SW_SHOW,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER, SetWindowPos, SetWindowTextW, ShowWindow,
    TranslateAcceleratorW, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_QUIT, WNDCLASSEXW,
    WS_OVERLAPPEDWINDOW,
};
use ::windows::core::{PCWSTR, w};

use super::win32_translate as translate;
use crate::control::{ControlFlow, PlatformError, WindowConfig, WindowId};
use crate::event::{Appearance, CursorIcon, PointerButtons, PointerKind, RawEvent};
use crate::handler::AppHandler;
use crate::menu::Menu;
use crate::{Instant, LogicalRect, PlatformApp, RawWindowHandle, Window};

const CLASS_NAME: PCWSTR = w!("VisoWindowClass");

/// Events the window procedure produces, drained by the pump between OS
/// messages, plus the little app-wide state the procedure needs.
#[derive(Default)]
struct PumpQueue {
    events: VecDeque<RawEvent>,
    /// Windows asked to redraw; converted to `RedrawRequested` beats.
    redraws: VecDeque<WindowId>,
    /// Set by the menu's Quit action.
    should_exit: bool,
    /// The pump's handler, exposed to the window procedure for the lifetime of
    /// `run()` so a modal size/move loop can drive frames. `run()` installs it
    /// before its loop and [`DriveGuard`] clears it on every exit path.
    drive: Option<NonNull<dyn AppHandler>>,
    /// True while the handler runs. A message the handler itself causes (a
    /// `SetWindowPos`, a `DestroyWindow`) is dispatched synchronously inside
    /// that call; the procedure must then only queue, never drive.
    handling: bool,
    /// The last flow the handler asked for, so a modal loop can fire a due
    /// timer the pump cannot reach.
    flow: Option<ControlFlow>,
    /// Every live window, for the procedure's app-wide work (appearance
    /// broadcast, menu rebuilds, close completion).
    windows: Vec<Rc<WindowState>>,
    /// The installed menu bar, attached to every window.
    menu: Option<Rc<menu::NativeMenu>>,
    /// The appearance last reported, so each of the several broadcast
    /// messages that signal a change emits one `AppearanceChanged`.
    appearance: Appearance,
    /// Wheel lines (vertical) and characters (horizontal) per notch.
    wheel: (u32, u32),
}

type Shared = Rc<RefCell<PumpQueue>>;

impl PumpQueue {
    fn hwnd_of(&self, window: WindowId) -> Option<HWND> {
        self.windows
            .iter()
            .find(|w| w.id == window)
            .map(|w| w.hwnd.get())
    }

    fn push_redraw(&mut self, window: WindowId) {
        if !self.redraws.contains(&window) {
            self.redraws.push_back(window);
        }
    }

    fn next_synthetic(&mut self) -> Option<RawEvent> {
        if let Some(window) = self.redraws.pop_front() {
            return Some(RawEvent::RedrawRequested { window });
        }
        self.events.pop_front()
    }
}

/// A window's saved frame while it is fullscreen.
struct Restore {
    style: isize,
    placement: ::windows::Win32::UI::WindowsAndMessaging::WINDOWPLACEMENT,
}

/// The per-window state the window procedure reads and writes. Owned by an
/// `Rc`: one strong count sits in the window's `GWLP_USERDATA` from
/// `WM_NCCREATE` to `WM_NCDESTROY`, one in the app's window list, and the
/// procedure holds another for the length of each call.
struct WindowState {
    id: WindowId,
    shared: Shared,
    hwnd: Cell<HWND>,
    dpi: Cell<u32>,
    /// The client size in physical pixels last reported.
    size: Cell<(u32, u32)>,
    /// Set once the window is shown; geometry before that is not reported.
    ready: Cell<bool>,
    alive: Cell<bool>,
    /// True while `WM_DPICHANGED` resizes the window, so the resize is
    /// reported once, as the scale change.
    dpi_changing: Cell<bool>,
    in_size_move: Cell<bool>,
    buttons: Cell<PointerButtons>,
    tracking_leave: Cell<bool>,
    /// The touch and pen contacts currently down, for capture-loss cancels.
    contacts: RefCell<Vec<(u32, PointerKind)>>,
    cursor: Cell<CursorIcon>,
    /// The last mouse position in logical points, for leave and cancel.
    last_mouse: Cell<(f64, f64)>,
    surrogates: RefCell<translate::Utf16Assembler>,
    /// The caret rect the IME places its windows at; `None` disables the IME.
    ime_caret: Cell<Option<LogicalRect>>,
    ime_enabled: Cell<bool>,
    /// The composition currently shown as preedit is non-empty.
    preedit_shown: Cell<bool>,
    fullscreen: RefCell<Option<Restore>>,
}

impl WindowState {
    fn new(id: WindowId, shared: Shared) -> Self {
        Self {
            id,
            shared,
            hwnd: Cell::new(HWND::default()),
            dpi: Cell::new(96),
            size: Cell::new((0, 0)),
            ready: Cell::new(false),
            alive: Cell::new(true),
            dpi_changing: Cell::new(false),
            in_size_move: Cell::new(false),
            buttons: Cell::new(PointerButtons::NONE),
            tracking_leave: Cell::new(false),
            contacts: RefCell::new(Vec::new()),
            cursor: Cell::new(CursorIcon::Default),
            last_mouse: Cell::new((0.0, 0.0)),
            surrogates: RefCell::new(translate::Utf16Assembler::default()),
            ime_caret: Cell::new(None),
            ime_enabled: Cell::new(true),
            preedit_shown: Cell::new(false),
            fullscreen: RefCell::new(None),
        }
    }

    fn push(&self, event: RawEvent) {
        self.shared.borrow_mut().events.push_back(event);
    }

    fn scale(&self) -> f64 {
        translate::scale_of(self.dpi.get())
    }

    fn client_size(&self) -> (u32, u32) {
        let mut rect = RECT::default();
        // SAFETY: `hwnd` is this state's live window (cleared never; a
        // destroyed handle only makes the call fail, leaving `rect` zeroed).
        let _ = unsafe { GetClientRect(self.hwnd.get(), &mut rect) };
        (
            (rect.right - rect.left).max(0) as u32,
            (rect.bottom - rect.top).max(0) as u32,
        )
    }

    fn logical_point(&self, x: i32, y: i32) -> (f64, f64) {
        let dpi = self.dpi.get();
        (translate::to_logical(x, dpi), translate::to_logical(y, dpi))
    }

    fn physical_point(&self, x: f64, y: f64) -> POINT {
        let dpi = self.dpi.get();
        POINT {
            x: translate::to_physical(x, dpi),
            y: translate::to_physical(y, dpi),
        }
    }
}

/// Clears `PumpQueue::drive` when `run`'s loop ends, on every path including
/// unwind, so the erased handler pointer never outlives the borrow it came
/// from.
struct DriveGuard(Shared);

impl Drop for DriveGuard {
    fn drop(&mut self) {
        self.0.borrow_mut().drive = None;
    }
}

/// Deliver every queued event now, from inside a modal loop.
fn drive(shared: &Shared) {
    let handler = {
        let q = shared.borrow();
        if q.handling { None } else { q.drive }
    };
    if let Some(handler) = handler {
        // SAFETY: the pointer is the one `run` installed and `DriveGuard`
        // clears, so it is live; `handling` is false, so no `handle` call is
        // on the stack and the reborrow is unaliased.
        unsafe { drain_and_drive(handler, shared) };
    }
}

/// Drain the queue through the handler. Each event is popped under a short
/// borrow released before `handle` runs, so the handler may queue more.
///
/// # Safety
///
/// `handler` must be live and not borrowed anywhere else for the call.
unsafe fn drain_and_drive(mut handler: NonNull<dyn AppHandler>, shared: &Shared) {
    loop {
        let event = shared.borrow_mut().next_synthetic();
        let Some(event) = event else { break };
        // SAFETY: per the function contract.
        deliver(unsafe { handler.as_mut() }, shared, event);
    }
}

/// Hand one event to the handler and complete its handshake: a filled
/// `CopyRequested` reply goes to the clipboard, and an accepted
/// `CloseRequested` destroys the window (whose `WM_DESTROY` then reports
/// `WindowClosed`).
fn deliver(handler: &mut dyn AppHandler, shared: &Shared, event: RawEvent) -> ControlFlow {
    enum After {
        Copy(crate::ClipboardReply, WindowId),
        Close(crate::AcceptCell, WindowId),
    }
    let after = match &event {
        RawEvent::CopyRequested { reply, window, .. } => Some(After::Copy(reply.clone(), *window)),
        RawEvent::CloseRequested { accept, window } => Some(After::Close(accept.clone(), *window)),
        _ => None,
    };
    let outer = std::mem::replace(&mut shared.borrow_mut().handling, true);
    let flow = handler.handle(event);
    {
        let mut q = shared.borrow_mut();
        q.handling = outer;
        q.flow = Some(flow);
    }
    match after {
        Some(After::Copy(reply, window)) => {
            if let Some(text) = reply.take() {
                let owner = shared.borrow().hwnd_of(window);
                system::write_clipboard(owner, &text);
            }
        }
        Some(After::Close(accept, window)) if accept.is_accepted() => {
            let hwnd = shared.borrow().hwnd_of(window);
            if let Some(hwnd) = hwnd {
                // SAFETY: a live window of ours; its `WM_DESTROY` re-borrows
                // the queue, which no borrow here is holding.
                let _ = unsafe { DestroyWindow(hwnd) };
            }
        }
        _ => {}
    }
    flow
}

/// Resize `hwnd`'s frame so its client area is `width`×`height` pixels at its
/// current DPI, keeping its position.
fn fit_client(hwnd: HWND, width: i32, height: i32, dpi: u32) {
    // SAFETY: plain queries and a resize of a live window we own.
    unsafe {
        let style = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32);
        let ex_style = WINDOW_EX_STYLE(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32);
        let has_menu = !GetMenu(hwnd).is_invalid();
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        };
        if AdjustWindowRectExForDpi(&mut rect, style, has_menu, ex_style, dpi).is_ok() {
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

/// The native Windows application.
pub struct WinApp {
    shared: Shared,
    hinstance: HINSTANCE,
    next_window_id: u32,
    windows: Vec<WinWindow>,
    class_registered: bool,
}

impl WinApp {
    pub fn new() -> Result<Self, PlatformError> {
        // SAFETY: process-wide setup calls with no pointer arguments. The DPI
        // call fails harmlessly when a manifest already chose the awareness.
        let hinstance = unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            GetModuleHandleW(None).map_err(|e| PlatformError::Backend(e.to_string()))?
        };
        let shared = Rc::new(RefCell::new(PumpQueue {
            appearance: system::read_appearance(),
            wheel: system::read_wheel_settings(),
            ..PumpQueue::default()
        }));
        Ok(Self {
            shared,
            hinstance: hinstance.into(),
            next_window_id: 1,
            windows: Vec::new(),
            class_registered: false,
        })
    }

    fn register_class(&mut self) -> Result<(), PlatformError> {
        if self.class_registered {
            return Ok(());
        }
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(proc::wnd_proc),
            hInstance: self.hinstance,
            lpszClassName: CLASS_NAME,
            ..Default::default()
        };
        // SAFETY: `class` is fully initialized and its strings are static.
        if unsafe { RegisterClassExW(&class) } == 0 {
            return Err(PlatformError::Backend("RegisterClassExW failed".into()));
        }
        self.class_registered = true;
        Ok(())
    }

    fn state(&self, window: WindowId) -> Option<&Rc<WindowState>> {
        self.windows
            .iter()
            .map(|w| &w.state)
            .find(|s| s.id == window && s.alive.get())
    }

    fn dispatch(&self, msg: &MSG) {
        let accel = self.shared.borrow().menu.as_ref().and_then(|m| m.accel());
        // SAFETY: `msg` came from this thread's queue; the accelerator table
        // is owned by the installed menu, alive while it is installed.
        unsafe {
            if let Some(accel) = accel
                && !msg.hwnd.is_invalid()
                && TranslateAcceleratorW(msg.hwnd, accel, msg) != 0
            {
                return;
            }
            let _ = TranslateMessage(msg);
            DispatchMessageW(msg);
        }
    }

    fn prune_closed(&mut self) {
        self.windows.retain(|w| w.state.alive.get());
    }
}

/// Non-blocking read of the next message; `false` when the queue is empty.
fn peek(msg: &mut MSG) -> bool {
    // SAFETY: `msg` is a valid out-pointer.
    unsafe { PeekMessageW(msg, None, 0, 0, PM_REMOVE).as_bool() }
}

impl PlatformApp for WinApp {
    fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError> {
        self.register_class()?;
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;

        let state = Rc::new(WindowState::new(id, self.shared.clone()));
        let title = translate::wide(&config.title);
        // The procedure adopts this count at `WM_NCCREATE` and releases it at
        // `WM_NCDESTROY`.
        let param = Rc::into_raw(state.clone());
        // SAFETY: the class is registered; `title` outlives the call; `param`
        // is a leaked `Rc<WindowState>` the procedure takes ownership of.
        let created = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS_NAME,
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                None,
                None,
                Some(self.hinstance),
                Some(param.cast()),
            )
        };
        let hwnd = match created {
            Ok(hwnd) => hwnd,
            Err(e) => {
                if state.hwnd.get().is_invalid() {
                    // SAFETY: `WM_NCCREATE` never ran, so the procedure never
                    // adopted the count leaked above; reclaim it here.
                    drop(unsafe { Rc::from_raw(param) });
                }
                return Err(PlatformError::WindowCreation(e.to_string()));
            }
        };

        // SAFETY: `hwnd` was just created and is live.
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        state.dpi.set(dpi);
        let menu = self.shared.borrow().menu.clone();
        if let Some(menu) = menu {
            menu.attach(hwnd);
        }
        let (w, h) = config.logical_size;
        fit_client(
            hwnd,
            translate::to_physical(w, dpi),
            translate::to_physical(h, dpi),
            dpi,
        );
        let dark = self.shared.borrow().appearance.color_scheme == crate::ColorScheme::Dark;
        system::set_dark_title(hwnd, dark);
        state.size.set(state.client_size());
        // No text field has focus yet: keys reach the window unconverted
        // until one asks for the IME.
        proc::set_ime_area(&state, None);
        self.shared.borrow_mut().windows.push(state.clone());
        // SAFETY: showing the live window.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        state.ready.set(true);
        self.shared.borrow_mut().push_redraw(id);
        self.windows.push(WinWindow { state });
        Ok(id)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        if handler.handle(RawEvent::AppLaunched) == ControlFlow::Exit {
            return;
        }
        // SAFETY: `handler` outlives `_drive_guard`, whose `Drop` clears the
        // stored pointer before `run` returns; the `transmute` only erases the
        // borrow's lifetime, which the guard upholds dynamically.
        let drive: NonNull<dyn AppHandler> = unsafe {
            let ptr: *mut dyn AppHandler = handler;
            NonNull::new_unchecked(std::mem::transmute::<
                *mut dyn AppHandler,
                *mut (dyn AppHandler + 'static),
            >(ptr))
        };
        self.shared.borrow_mut().drive = Some(drive);
        let _drive_guard = DriveGuard(self.shared.clone());

        let mut flow = ControlFlow::Wait;
        loop {
            if self.shared.borrow().should_exit {
                break;
            }
            let synthetic = self.shared.borrow_mut().next_synthetic();
            if let Some(event) = synthetic {
                let closed = matches!(event, RawEvent::WindowClosed { .. });
                flow = deliver(handler, &self.shared, event);
                if closed {
                    self.prune_closed();
                }
                if flow == ControlFlow::Exit {
                    break;
                }
                continue;
            }

            let mut msg = MSG::default();
            match flow {
                // Sleep until input arrives or the timer deadline passes; a
                // timeout with nothing queued means the deadline arrived, so a
                // `Wakeup` beat lets the runtime fire it.
                ControlFlow::WaitUntil(deadline) => {
                    if !peek(&mut msg) {
                        let ms = deadline
                            .saturating_duration_since(Instant::now())
                            .as_millis()
                            .min(u128::from(u32::MAX - 1)) as u32;
                        // SAFETY: no handles; waits on this thread's queue.
                        unsafe {
                            MsgWaitForMultipleObjectsEx(None, ms, QS_ALLINPUT, MWMO_INPUTAVAILABLE);
                        }
                        if !peek(&mut msg) {
                            if Instant::now() >= deadline {
                                flow = deliver(handler, &self.shared, RawEvent::Wakeup);
                                if flow == ControlFlow::Exit {
                                    break;
                                }
                            }
                            continue;
                        }
                    }
                }
                ControlFlow::Wait => {
                    // SAFETY: `msg` is a valid out-pointer.
                    let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                    if got.0 <= 0 {
                        break;
                    }
                }
                _ => {
                    if !peek(&mut msg) {
                        flow = ControlFlow::Wait;
                        continue;
                    }
                }
            }
            if msg.message == WM_QUIT {
                break;
            }
            self.dispatch(&msg);
        }
    }

    fn window(&self, id: WindowId) -> Option<&dyn Window> {
        self.windows
            .iter()
            .find(|w| w.state.id == id && w.state.alive.get())
            .map(|w| w as &dyn Window)
    }

    fn request_redraw(&mut self, window: WindowId) {
        self.shared.borrow_mut().push_redraw(window);
    }

    fn set_menu(&mut self, menu: &Menu) {
        let native = menu::NativeMenu::build(menu).map(Rc::new);
        let windows = self.shared.borrow().windows.clone();
        for state in &windows {
            let hwnd = state.hwnd.get();
            let (w, h) = state.client_size();
            match &native {
                Some(native) => native.attach(hwnd),
                None => menu::detach(hwnd),
            }
            fit_client(hwnd, w as i32, h as i32, state.dpi.get());
        }
        // The previous menu's accelerator table goes with it, after every
        // window has moved to the new bar.
        self.shared.borrow_mut().menu = native;
    }

    fn close_window(&mut self, window: WindowId) {
        // The same path as an accepted user close: `WM_DESTROY` queues the
        // `WindowClosed` the scheduler counts against its open windows.
        let Some(hwnd) = self.state(window).map(|s| s.hwnd.get()) else {
            return;
        };
        // SAFETY: a live window of ours.
        let _ = unsafe { DestroyWindow(hwnd) };
        self.prune_closed();
    }

    fn set_fullscreen(&mut self, window: WindowId, fullscreen: bool) {
        if let Some(state) = self.state(window).cloned() {
            system::set_fullscreen(&state, fullscreen);
        }
    }

    fn set_clipboard_text(&mut self, text: &str) {
        let owner = self.windows.first().map(|w| w.state.hwnd.get());
        system::write_clipboard(owner, text);
    }

    fn request_paste(&mut self, window: WindowId) {
        let Some(state) = self.state(window) else {
            return;
        };
        if let Some(text) = system::read_clipboard(Some(state.hwnd.get())) {
            state.push(RawEvent::Paste { window, text });
        }
    }

    fn set_cursor(&mut self, window: WindowId, icon: CursorIcon) {
        if let Some(state) = self.state(window) {
            system::set_cursor(state, icon);
        }
    }

    fn set_ime_area(&mut self, window: WindowId, caret: Option<LogicalRect>) {
        if let Some(state) = self.state(window) {
            proc::set_ime_area(state, caret);
        }
    }

    fn show_soft_keyboard(&mut self, _window: WindowId, _show: bool) {
        // Desktop Windows raises its touch keyboard from focus in a text
        // field reported through UI Automation, not from an app call.
    }

    fn appearance(&self) -> Appearance {
        self.shared.borrow().appearance
    }
}

/// A native Win32 window.
pub struct WinWindow {
    state: Rc<WindowState>,
}

impl Window for WinWindow {
    fn id(&self) -> WindowId {
        self.state.id
    }

    fn request_redraw(&self) {
        self.state.shared.borrow_mut().push_redraw(self.state.id);
    }

    fn set_title(&mut self, title: &str) {
        let title = translate::wide(title);
        // SAFETY: `title` is NUL-terminated and outlives the call.
        let _ = unsafe { SetWindowTextW(self.state.hwnd.get(), PCWSTR(title.as_ptr())) };
    }

    fn scale_factor(&self) -> f64 {
        self.state.scale()
    }

    fn inner_size(&self) -> (u32, u32) {
        self.state.client_size()
    }

    fn raw_handle(&self) -> RawWindowHandle {
        let hwnd = self.state.hwnd.get();
        // SAFETY: reads the module handle the window was created with.
        let hinstance = unsafe {
            ::windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
                hwnd,
                ::windows::Win32::UI::WindowsAndMessaging::GWLP_HINSTANCE,
            )
        };
        RawWindowHandle::Win32 {
            hwnd: hwnd.0,
            hinstance: hinstance as *mut std::ffi::c_void,
        }
    }
}
