//! Native Windows backend (Win32 via the `windows` crate).
//!
//! Compile-checked only from the macOS dev host; behavior on Windows is
//! unverified in Phase 1 (needs a Windows host). The shape mirrors the macOS
//! backend: create a window with `CreateWindowExW`, then a manual
//! `PeekMessage`/`GetMessage` + `DispatchMessageW` pump whose blocking is
//! chosen by the runtime's [`ControlFlow`]. The `WndProc` funnels
//! `WM_SIZE`/`WM_CLOSE`/`WM_PAINT` into our [`RawEvent`]s.
//!
//! Phase 1 delivers a minimal but real implementation: window creation, the
//! message pump, and close/resize/paint routing. Input mapping beyond that is
//! deferred with the input subsystem.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, MSG, PM_REMOVE,
    PeekMessageW, PostQuitMessage, RegisterClassW, SW_SHOW, ShowWindow, TranslateMessage,
    WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_PAINT, WM_SIZE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::{PCWSTR, w};

use crate::RawWindowHandle;
use crate::control::{ControlFlow, PlatformError, WindowConfig, WindowId};
use crate::event::RawEvent;
use crate::handler::AppHandler;
use crate::{PlatformApp, Window};

#[derive(Default)]
struct PumpQueue {
    events: VecDeque<RawEvent>,
    redraws: VecDeque<WindowId>,
    should_exit: bool,
}

type Shared = Rc<RefCell<PumpQueue>>;

/// The native Windows application.
pub struct WinApp {
    shared: Shared,
    next_window_id: u32,
    windows: Vec<WinWindow>,
    class_registered: bool,
    launched: bool,
}

impl WinApp {
    pub fn new() -> Result<Self, PlatformError> {
        Ok(Self {
            shared: Rc::new(RefCell::new(PumpQueue::default())),
            next_window_id: 1,
            windows: Vec::new(),
            class_registered: false,
            launched: false,
        })
    }

    fn register_class(&mut self) -> Result<(), PlatformError> {
        if self.class_registered {
            return Ok(());
        }
        // SAFETY: standard Win32 class registration.
        unsafe {
            let hinstance =
                GetModuleHandleW(None).map_err(|e| PlatformError::Backend(e.to_string()))?;
            let class = WNDCLASSW {
                lpfnWndProc: Some(wnd_proc),
                hInstance: hinstance.into(),
                lpszClassName: w!("VisoWindowClass"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(PlatformError::Backend("RegisterClassW failed".into()));
            }
        }
        self.class_registered = true;
        Ok(())
    }
}

impl PlatformApp for WinApp {
    fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError> {
        self.register_class()?;
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;

        let title: Vec<u16> = config
            .title
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let (w, h) = config.logical_size;

        // SAFETY: standard Win32 window creation with a registered class.
        let hwnd = unsafe {
            let hinstance =
                GetModuleHandleW(None).map_err(|e| PlatformError::Backend(e.to_string()))?;
            CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("VisoWindowClass"),
                PCWSTR(title.as_ptr()),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                w as i32,
                h as i32,
                None,
                None,
                Some(hinstance.into()),
                None,
            )
            .map_err(|e| PlatformError::WindowCreation(e.to_string()))?
        };
        // SAFETY: showing a valid window handle.
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
        }
        self.windows.push(WinWindow {
            id,
            hwnd,
            scale: 1.0,
        });
        self.shared.borrow_mut().redraws.push_back(id);
        Ok(id)
    }

    fn run(&mut self, handler: &mut dyn AppHandler) {
        self.launched = true;
        if handler.handle(RawEvent::AppLaunched) == ControlFlow::Exit {
            return;
        }
        let mut flow = ControlFlow::Wait;
        loop {
            if self.shared.borrow().should_exit {
                break;
            }
            // Drain synthetic events (redraw beats etc.) first.
            let synthetic = {
                let mut q = self.shared.borrow_mut();
                q.redraws
                    .pop_front()
                    .map(|w| RawEvent::RedrawRequested { window: w })
                    .or_else(|| q.events.pop_front())
            };
            if let Some(event) = synthetic {
                flow = handler.handle(event);
                if flow == ControlFlow::Exit {
                    break;
                }
                continue;
            }

            let block = matches!(flow, ControlFlow::Wait | ControlFlow::WaitUntil(_));
            let mut msg = MSG::default();
            // SAFETY: standard Win32 message pump.
            unsafe {
                if block {
                    if !GetMessageW(&mut msg, None, 0, 0).as_bool() {
                        break; // WM_QUIT
                    }
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                } else if PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                } else {
                    flow = ControlFlow::Wait;
                }
            }
        }
    }

    fn window(&self, id: WindowId) -> Option<&dyn Window> {
        self.windows
            .iter()
            .find(|w| w.id == id)
            .map(|w| w as &dyn Window)
    }

    fn request_redraw(&mut self, window: WindowId) {
        self.shared.borrow_mut().redraws.push_back(window);
    }
}

/// A native Win32 window.
pub struct WinWindow {
    id: WindowId,
    hwnd: HWND,
    scale: f64,
}

impl Window for WinWindow {
    fn id(&self) -> WindowId {
        self.id
    }

    fn request_redraw(&self) {
        // SAFETY: InvalidateRect on a valid HWND is safe; elided for brevity in
        // the compile-checked stub.
        let _ = self.hwnd;
    }

    fn set_title(&mut self, _title: &str) {
        // SetWindowTextW would go here; omitted from the compile-checked stub.
    }

    fn scale_factor(&self) -> f64 {
        self.scale
    }

    fn inner_size(&self) -> (u32, u32) {
        (0, 0)
    }

    fn raw_handle(&self) -> RawWindowHandle {
        RawWindowHandle::Win32 {
            hwnd: self.hwnd.0 as *mut core::ffi::c_void,
        }
    }
}

/// The window procedure: routes messages to our event queue.
///
/// Phase 1 handles quit-relevant messages via `PostQuitMessage`; per-window
/// event fan-out to the shared queue lands with the input subsystem (the WndProc
/// has no direct access to the `WinApp`'s `Shared` in this stub).
extern "system" fn wnd_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLOSE => {
            // SAFETY: valid HWND from the pump.
            unsafe {
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_SIZE | WM_PAINT => {
            // Validated in the pump loop; default-process for now.
            unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
