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

mod access;
mod badge;
mod clipboard;
mod drop;
mod links;
mod lock;
mod menu;
mod proc;
mod system;
mod tray;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ::windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT, RECT, WPARAM};
use ::windows::Win32::System::LibraryLoader::GetModuleHandleW;
use ::windows::Win32::System::Threading::GetCurrentThreadId;
use ::windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use ::windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DestroyWindow, DispatchMessageW, GWL_EXSTYLE, GWL_STYLE,
    GetClientRect, GetMenu, GetMessageW, GetWindowLongPtrW, GetWindowRect, IsIconic, IsZoomed, MSG,
    MWMO_INPUTAVAILABLE, MsgWaitForMultipleObjectsEx, PM_REMOVE, PeekMessageW, PostThreadMessageW,
    QS_ALLINPUT, RegisterClassExW, SW_SHOW, SW_SHOWMAXIMIZED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOZORDER, SetWindowPos, SetWindowTextW, ShowWindow, TranslateAcceleratorW,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_QUIT, WNDCLASSEXW, WS_OVERLAPPEDWINDOW,
};
use ::windows::core::{PCWSTR, w};

use super::drive::Drive;
use super::win32_translate as translate;
use crate::control::{ControlFlow, PlatformError, WindowConfig, WindowId};
use crate::event::{Appearance, CursorIcon, PointerButtons, PointerKind, RawEvent};
use crate::handler::AppHandler;
use crate::menu::Menu;
use crate::placement::{DeskRect, fit_on_screens};
use crate::{
    EditLabels, EditMenu, Instant, LogicalRect, LoopWaker, PlatformApp, RawWindowHandle, TextEntry,
    Window,
};

const CLASS_NAME: PCWSTR = w!("VisoWindowClass");

/// The thread message a [`LoopWaker`] kick posts to end the pump's wait. It
/// carries nothing; the pump drains `PumpQueue::woken`.
const WM_WAKE: u32 = ::windows::Win32::UI::WindowsAndMessaging::WM_APP + 2;
/// Pops the context menu asked for up, from the window's own message loop.
const WM_CONTEXT_MENU: u32 = ::windows::Win32::UI::WindowsAndMessaging::WM_APP + 3;

/// Events the window procedure produces, drained by the pump between OS
/// messages, plus the little app-wide state the procedure needs.
#[derive(Default)]
struct PumpQueue {
    events: VecDeque<RawEvent>,
    /// Windows asked to redraw; converted to `RedrawRequested` beats.
    redraws: VecDeque<WindowId>,
    /// Set by the menu's Quit action.
    should_exit: bool,
    /// The pump's app and handler, exposed to the window procedure while a
    /// pump turn has handed the thread to the OS, so a modal size/move loop
    /// can drive frames. `run()` installs it around each turn, holding no
    /// borrow of either across it, and [`DriveGuard`] clears it on unwind.
    drive: Option<Drive>,
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
    /// The locale last reported.
    locale: String,
    /// Wheel lines (vertical) and characters (horizontal) per notch.
    wheel: (u32, u32),
    /// Set by a [`LoopWaker`] kick from any thread; drained as one
    /// `RawEvent::Wakeup`, by the pump or by a modal loop's drive.
    woken: Arc<AtomicBool>,
    /// A drag the app carries out of a window, run by OLE once the handler
    /// that started it returns.
    drag_out: Option<(WindowId, crate::DragOut)>,
    /// The badge on the windows' taskbar buttons.
    badge: Rc<badge::Badge>,
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

    /// The next synthetic event: a pending wake first, so a window that
    /// draws every beat cannot hold off what it delivers, then redraw beats,
    /// then the queue.
    fn next_synthetic(&mut self) -> Option<RawEvent> {
        if self.woken.swap(false, Ordering::AcqRel) {
            return Some(RawEvent::Wakeup);
        }
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
    /// The mouse's pointer is locked to the window.
    locked: Cell<bool>,
    /// The last mouse position in logical points, for leave and cancel.
    last_mouse: Cell<(f64, f64)>,
    surrogates: RefCell<translate::Utf16Assembler>,
    /// The caret rect the IME places its windows at; `None` disables the IME.
    ime_caret: Cell<Option<LogicalRect>>,
    ime_enabled: Cell<bool>,
    /// The input scope last set, which picks the touch keyboard's layout.
    input_scope: Cell<i32>,
    /// The text the input method edits: the focused field's around its
    /// caret, and the composition under way.
    ime: RefCell<super::ime_text::ImeText>,
    fullscreen: RefCell<Option<Restore>>,
    /// The context menu asked for, popped up from the message loop.
    context_menu: Cell<Option<(EditMenu, EditLabels)>>,
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
            locked: Cell::new(false),
            last_mouse: Cell::new((0.0, 0.0)),
            surrogates: RefCell::new(translate::Utf16Assembler::default()),
            ime_caret: Cell::new(None),
            ime_enabled: Cell::new(true),
            input_scope: Cell::new(0),
            ime: RefCell::new(super::ime_text::ImeText::default()),
            fullscreen: RefCell::new(None),
            context_menu: Cell::new(None),
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
    let drive = {
        let q = shared.borrow();
        if q.handling { None } else { q.drive }
    };
    if let Some(drive) = drive {
        // SAFETY: the drive is the one `run` installed for the OS turn this
        // runs inside, while it holds no borrow of the app or handler;
        // `handling` is false, so no `handle` call is on the stack either.
        unsafe { drain_and_drive(drive, shared) };
    }
}

/// Drain the queue through the handler. Each event is popped under a short
/// borrow released before `handle` runs, so the handler may queue more.
///
/// # Safety
///
/// The app and handler behind `drive` must be live and not borrowed anywhere
/// else for the call.
unsafe fn drain_and_drive(drive: Drive, shared: &Shared) {
    loop {
        let event = shared.borrow_mut().next_synthetic();
        let Some(event) = event else { break };
        // SAFETY: per the function contract.
        let (app, handler) = unsafe { drive.get() };
        deliver(app, handler, shared, event);
    }
}

/// Hand one event to the handler and complete its handshake: a filled
/// `CopyRequested` reply goes to the clipboard, and an accepted
/// `CloseRequested` destroys the window (whose `WM_DESTROY` then reports
/// `WindowClosed`).
fn deliver(
    app: &mut dyn PlatformApp,
    handler: &mut dyn AppHandler,
    shared: &Shared,
    event: RawEvent,
) -> ControlFlow {
    enum After {
        Copy(crate::ClipboardReply, WindowId),
        Paste(crate::PasteReply, WindowId),
        Close(crate::AcceptCell, WindowId),
    }
    let after = match &event {
        RawEvent::CopyRequested { reply, window, .. } => Some(After::Copy(reply.clone(), *window)),
        RawEvent::PasteRequested { reply, window } => Some(After::Paste(reply.clone(), *window)),
        RawEvent::CloseRequested { accept, window } => Some(After::Close(accept.clone(), *window)),
        _ => None,
    };
    let outer = std::mem::replace(&mut shared.borrow_mut().handling, true);
    let flow = handler.handle(app, event);
    let drag_out = {
        let mut q = shared.borrow_mut();
        q.handling = outer;
        q.flow = Some(flow);
        if outer { None } else { q.drag_out.take() }
    };
    if let Some((window, drag)) = drag_out {
        // OLE runs the drag in its own loop, whose messages the procedure
        // drives as usual: no handler is on the stack now.
        let state = shared
            .borrow()
            .windows
            .iter()
            .find(|w| w.id == window)
            .cloned();
        if let Some(state) = state {
            let action = drop::drag_out(&state, &drag);
            state.push(RawEvent::DragEnded { window, action });
        }
    }
    match after {
        Some(After::Copy(reply, window)) => {
            if let Some(data) = reply.take() {
                let owner = shared.borrow().hwnd_of(window);
                clipboard::write(owner, data);
            }
        }
        Some(After::Paste(reply, window)) => {
            let kinds = reply.get();
            if kinds.any() {
                let owner = shared.borrow().hwnd_of(window);
                let data = clipboard::read(owner, kinds);
                if !data.is_empty() {
                    shared.borrow_mut().events.push_back(RawEvent::Paste {
                        window,
                        data,
                        token: None,
                    });
                }
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
/// `hwnd`'s outer top-left corner in physical desktop pixels.
fn outer_position(hwnd: HWND) -> Option<(f64, f64)> {
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid out-parameter and the window is live.
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    Some((f64::from(rect.left), f64::from(rect.top)))
}

/// Moves, and shrinks if it must, `hwnd`'s frame onto the monitors' work
/// areas when it is not wholly on them.
fn keep_on_monitors(hwnd: HWND) {
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid out-parameter and the window is live.
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return;
    }
    let frame = DeskRect::new(
        f64::from(rect.left),
        f64::from(rect.top),
        f64::from(rect.right - rect.left),
        f64::from(rect.bottom - rect.top),
    );
    let fitted = fit_on_screens(frame, &system::work_areas());
    if fitted != frame {
        // SAFETY: a move and resize of a live window we own.
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                None,
                fitted.x.round() as i32,
                fitted.y.round() as i32,
                fitted.width.round() as i32,
                fitted.height.round() as i32,
                SWP_NOZORDER | SWP_NOACTIVATE,
            )
        };
    }
}

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
    /// The thread that created the app and runs its pump.
    ui_thread: u32,
    /// The URLs the app was started with, opened once it launches.
    launch_urls: Vec<String>,
    /// The game controllers, read from the first poll.
    gamepads: crate::gamepad::Gamepads,
    /// The tray icon, while the app shows one.
    tray: Option<tray::TrayIcon>,
}

impl WinApp {
    pub fn new() -> Result<Self, PlatformError> {
        // SAFETY: process-wide setup calls with no pointer arguments. The DPI
        // call fails harmlessly when a manifest already chose the awareness.
        let hinstance = unsafe {
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            // OLE, for the windows' drop targets; a thread that already
            // entered another apartment keeps it and takes no drops.
            let _ = ::windows::Win32::System::Ole::OleInitialize(None);
            GetModuleHandleW(None).map_err(|e| PlatformError::Backend(e.to_string()))?
        };
        let shared = Rc::new(RefCell::new(PumpQueue {
            appearance: system::read_appearance(),
            locale: system::read_locale(),
            wheel: system::read_wheel_settings(),
            ..PumpQueue::default()
        }));
        let launch_urls = links::claim(&shared, hinstance.into());
        Ok(Self {
            shared,
            launch_urls,
            hinstance: hinstance.into(),
            next_window_id: 1,
            windows: Vec::new(),
            class_registered: false,
            // SAFETY: reads the calling thread's id; no arguments.
            ui_thread: unsafe { GetCurrentThreadId() },
            gamepads: crate::gamepad::Gamepads::default(),
            tray: None,
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

    fn dispatch(shared: &Shared, msg: &MSG) {
        let accel = shared.borrow().menu.as_ref().and_then(|m| m.accel());
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

/// How one OS turn of the pump ended.
enum Turn {
    /// A message was dispatched, or nothing is due yet.
    Done,
    /// `WM_QUIT`, or the queue failed.
    Quit,
    /// A poll found nothing: block on the next turn.
    Idle,
    /// The timer deadline passed with nothing queued.
    Due,
}

/// Take and dispatch one OS message, blocking or polling per `flow`. After a
/// beat (`yielding`) only what is already queued is taken.
fn os_turn(shared: &Shared, yielding: bool, flow: ControlFlow) -> Turn {
    let mut msg = MSG::default();
    if yielding {
        if !peek(&mut msg) {
            return Turn::Done;
        }
    } else {
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
                        return if Instant::now() >= deadline {
                            Turn::Due
                        } else {
                            Turn::Done
                        };
                    }
                }
            }
            ControlFlow::Wait => {
                // SAFETY: `msg` is a valid out-pointer.
                let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
                if got.0 <= 0 {
                    return Turn::Quit;
                }
            }
            _ => {
                if !peek(&mut msg) {
                    return Turn::Idle;
                }
            }
        }
    }
    if msg.message == WM_QUIT {
        return Turn::Quit;
    }
    WinApp::dispatch(shared, &msg);
    Turn::Done
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
        // A position puts the outer top-left there; the size is fitted to the
        // client area once the window knows its DPI, and the frame is then
        // kept on the monitors.
        let (x, y) = config
            .position
            .map_or((CW_USEDEFAULT, CW_USEDEFAULT), |(x, y)| {
                (x.round() as i32, y.round() as i32)
            });
        // SAFETY: the class is registered; `title` outlives the call; `param`
        // is a leaked `Rc<WindowState>` the procedure takes ownership of.
        let created = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                CLASS_NAME,
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW,
                x,
                y,
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
        if config.position.is_some() {
            keep_on_monitors(hwnd);
        }
        let dark = self.shared.borrow().appearance.color_scheme == crate::ColorScheme::Dark;
        system::set_dark_title(hwnd, dark);
        state.size.set(state.client_size());
        let access = access::attach(hwnd);
        drop::register(&state);
        badge::allow(hwnd);
        // No text field has focus yet: keys reach the window unconverted
        // until one asks for the IME.
        proc::set_text_entry(&state, None);
        self.shared.borrow_mut().windows.push(state.clone());
        // SAFETY: showing the live window.
        unsafe {
            let _ = ShowWindow(
                hwnd,
                if config.maximized {
                    SW_SHOWMAXIMIZED
                } else {
                    SW_SHOW
                },
            );
        }
        state.ready.set(true);
        self.shared.borrow_mut().push_redraw(id);
        self.windows.push(WinWindow { state, access });
        Ok(id)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        if handler.handle(self, RawEvent::AppLaunched) == ControlFlow::Exit {
            return;
        }
        for url in std::mem::take(&mut self.launch_urls) {
            self.shared
                .borrow_mut()
                .events
                .push_back(RawEvent::OpenUrl { url });
        }
        // Clears the drive on every exit path, including unwind out of an OS
        // turn.
        let queue = self.shared.clone();
        let _drive_guard = DriveGuard(queue.clone());

        let mut flow = ControlFlow::Wait;
        // Whether a redraw beat was just delivered: the pump then takes what
        // the OS has queued before the next synthetic event, so a window that
        // draws every beat does not starve input.
        let mut drew = false;
        loop {
            if queue.borrow().should_exit {
                break;
            }
            let synthetic = if drew {
                None
            } else {
                queue.borrow_mut().next_synthetic()
            };
            if let Some(event) = synthetic {
                let closed = matches!(event, RawEvent::WindowClosed { .. });
                drew = matches!(event, RawEvent::RedrawRequested { .. });
                flow = deliver(self, handler, &queue, event);
                if closed {
                    self.prune_closed();
                }
                if flow == ControlFlow::Exit {
                    break;
                }
                continue;
            }

            // The window procedure may drive frames during the turn (a modal
            // size/move loop), reaching the app and handler through the drive
            // alone: the turn touches neither.
            // A rumble playing wakes the pump when it is due to stop.
            let wait = match (flow, self.gamepads.deadline()) {
                (ControlFlow::Wait, Some(due)) => ControlFlow::WaitUntil(due),
                (ControlFlow::WaitUntil(at), Some(due)) => ControlFlow::WaitUntil(at.min(due)),
                (flow, _) => flow,
            };
            queue.borrow_mut().drive = Some(Drive::new(self, handler));
            let turn = os_turn(&queue, std::mem::take(&mut drew), wait);
            queue.borrow_mut().drive = None;
            match turn {
                Turn::Done => {}
                Turn::Quit => break,
                Turn::Idle => flow = ControlFlow::Wait,
                Turn::Due => {
                    flow = deliver(self, handler, &queue, RawEvent::Wakeup);
                    if flow == ControlFlow::Exit {
                        break;
                    }
                }
            }
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

    fn poll_gamepads(&mut self, events: &mut Vec<crate::GamepadEvent>) {
        if !self.gamepads.started() {
            let waker = self.loop_waker();
            self.gamepads.start(waker);
        }
        self.gamepads.poll(events);
    }

    fn rumble_gamepad(&mut self, id: crate::GamepadId, rumble: crate::Rumble) -> bool {
        self.gamepads.rumble(id, rumble)
    }

    fn loop_waker(&self) -> LoopWaker {
        let woken = self.shared.borrow().woken.clone();
        let thread = self.ui_thread;
        LoopWaker::new(move || {
            woken.store(true, Ordering::Release);
            // SAFETY: posts a payload-free message to the pump's thread
            // queue; any thread may. A failure (the thread has exited)
            // leaves nothing to wake.
            let _ = unsafe { PostThreadMessageW(thread, WM_WAKE, WPARAM(0), LPARAM(0)) };
        })
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

    fn set_tray(&mut self, tray: Option<&crate::Tray>) {
        match (tray, &self.tray) {
            (None, _) => self.tray = None,
            (Some(tray), Some(shown)) => shown.update(tray),
            (Some(tray), None) => {
                self.tray = tray::TrayIcon::new(self.hinstance, &self.shared, tray);
            }
        }
    }

    fn set_badge(&mut self, badge: Option<&crate::AppBadge>) {
        let (shown, windows) = {
            let q = self.shared.borrow();
            let windows: Vec<HWND> = q.windows.iter().map(|w| w.hwnd.get()).collect();
            (q.badge.clone(), windows)
        };
        shown.set(badge.and_then(|b| b.text(true)).as_deref(), &windows);
    }

    fn update_accessibility(&mut self, window: WindowId, delta: crate::SemanticsDelta) {
        if let Some(win) = self.windows.iter_mut().find(|w| w.state.id == window) {
            access::update(&mut win.access, delta);
        }
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

    fn set_clipboard(&mut self, data: crate::ClipboardData) {
        let owner = self.windows.first().map(|w| w.state.hwnd.get());
        clipboard::write(owner, data);
    }

    fn request_paste(&mut self, window: WindowId, kinds: crate::ClipboardKinds, token: u32) {
        let Some(state) = self.state(window) else {
            return;
        };
        let data = clipboard::read(Some(state.hwnd.get()), kinds);
        state.push(RawEvent::Paste {
            window,
            data,
            token: Some(token),
        });
    }

    fn start_drag(&mut self, window: WindowId, drag: crate::DragOut) -> bool {
        let held = self
            .state(window)
            .is_some_and(|s| !s.buttons.get().is_empty());
        let mut q = self.shared.borrow_mut();
        if !held || q.drag_out.is_some() {
            return false;
        }
        q.drag_out = Some((window, drag));
        true
    }

    fn set_cursor(&mut self, window: WindowId, icon: CursorIcon) {
        if let Some(state) = self.state(window) {
            system::set_cursor(state, icon);
        }
    }

    fn set_pointer_lock(&mut self, window: WindowId, lock: bool) {
        if let Some(state) = self.state(window) {
            lock::set(state, lock);
        }
    }

    fn set_text_entry(&mut self, window: WindowId, entry: Option<TextEntry>) {
        if let Some(state) = self.state(window) {
            proc::set_text_entry(state, entry);
        }
    }

    fn set_surrounding_text(&mut self, window: WindowId, text: Option<&crate::SurroundingText>) {
        if let Some(state) = self.state(window) {
            state.ime.borrow_mut().show(text);
        }
    }

    fn show_context_menu(&mut self, window: WindowId, menu: EditMenu, labels: EditLabels) {
        let Some(state) = self.state(window) else {
            return;
        };
        state.context_menu.set(Some((menu, labels)));
        // SAFETY: posting to our own live window; its procedure takes the
        // menu from the state when the loop gets to it.
        let _ = unsafe {
            ::windows::Win32::UI::WindowsAndMessaging::PostMessageW(
                Some(state.hwnd.get()),
                WM_CONTEXT_MENU,
                WPARAM(0),
                LPARAM(0),
            )
        };
    }

    fn show_soft_keyboard(&mut self, _window: WindowId, _show: bool) {
        // Desktop Windows raises its touch keyboard from focus in a text
        // field reported through UI Automation, not from an app call.
    }

    fn appearance(&self) -> Appearance {
        self.shared.borrow().appearance
    }

    fn locale(&self) -> String {
        self.shared.borrow().locale.clone()
    }
}

/// A native Win32 window.
pub struct WinWindow {
    state: Rc<WindowState>,
    /// The window's accessibility adapter.
    access: accesskit_windows::SubclassingAdapter,
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

    fn outer_position(&self) -> Option<(f64, f64)> {
        outer_position(self.state.hwnd.get())
    }

    fn mode(&self) -> crate::WindowMode {
        let hwnd = self.state.hwnd.get();
        // SAFETY: plain state queries of a live window.
        let (iconic, zoomed) = unsafe { (IsIconic(hwnd).as_bool(), IsZoomed(hwnd).as_bool()) };
        if iconic {
            crate::WindowMode::Minimized
        } else if self.state.fullscreen.borrow().is_some() {
            crate::WindowMode::Fullscreen
        } else if zoomed {
            crate::WindowMode::Maximized
        } else {
            crate::WindowMode::Normal
        }
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
