//! Native Linux/X11 backend (via `x11rb`).
//!
//! Compile-checked only from the macOS dev host; behavior on Linux is
//! unverified in Phase 1 (needs a Linux host with an X server). Wayland is
//! deferred to a later phase. The shape mirrors the other backends: connect to
//! the X server, create and map a window, then a manual event loop over
//! `ConfigureNotify` (resize), the `WM_DELETE_WINDOW` `ClientMessage` (close),
//! and `Expose` (redraw), whose blocking is chosen by the runtime's
//! [`ControlFlow`].

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use x11rb::connection::Connection;
use x11rb::protocol::Event as XEvent;
use x11rb::protocol::xproto::{ConnectionExt, CreateWindowAux, EventMask, WindowClass};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

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

/// The native X11 application.
pub struct X11App {
    conn: RustConnection,
    screen_num: usize,
    shared: Shared,
    next_window_id: u32,
    windows: Vec<X11Window>,
    wm_delete_window: u32,
    launched: bool,
}

impl X11App {
    pub fn new() -> Result<Self, PlatformError> {
        let (conn, screen_num) =
            RustConnection::connect(None).map_err(|e| PlatformError::Backend(e.to_string()))?;
        // Intern WM_DELETE_WINDOW so we can honor the WM close protocol.
        let wm_delete = conn
            .intern_atom(false, b"WM_DELETE_WINDOW")
            .map_err(|e| PlatformError::Backend(e.to_string()))?
            .reply()
            .map_err(|e| PlatformError::Backend(e.to_string()))?
            .atom;
        Ok(Self {
            conn,
            screen_num,
            shared: Rc::new(RefCell::new(PumpQueue::default())),
            next_window_id: 1,
            windows: Vec::new(),
            wm_delete_window: wm_delete,
            launched: false,
        })
    }
}

impl PlatformApp for X11App {
    fn create_window(&mut self, config: WindowConfig) -> Result<WindowId, PlatformError> {
        let id = WindowId(self.next_window_id);
        self.next_window_id += 1;

        let screen = &self.conn.setup().roots[self.screen_num];
        let xid = self
            .conn
            .generate_id()
            .map_err(|e| PlatformError::Backend(e.to_string()))?;
        let (w, h) = config.logical_size;

        let aux =
            CreateWindowAux::new().event_mask(EventMask::EXPOSURE | EventMask::STRUCTURE_NOTIFY);
        self.conn
            .create_window(
                x11rb::COPY_DEPTH_FROM_PARENT,
                xid,
                screen.root,
                0,
                0,
                w as u16,
                h as u16,
                0,
                WindowClass::INPUT_OUTPUT,
                screen.root_visual,
                &aux,
            )
            .map_err(|e| PlatformError::WindowCreation(e.to_string()))?;

        // Register for the delete-window protocol.
        let wm_protocols = self
            .conn
            .intern_atom(false, b"WM_PROTOCOLS")
            .map_err(|e| PlatformError::Backend(e.to_string()))?
            .reply()
            .map_err(|e| PlatformError::Backend(e.to_string()))?
            .atom;
        self.conn
            .change_property32(
                x11rb::protocol::xproto::PropMode::REPLACE,
                xid,
                wm_protocols,
                x11rb::protocol::xproto::AtomEnum::ATOM,
                &[self.wm_delete_window],
            )
            .map_err(|e| PlatformError::Backend(e.to_string()))?;

        self.conn
            .map_window(xid)
            .map_err(|e| PlatformError::Backend(e.to_string()))?;
        self.conn
            .flush()
            .map_err(|e| PlatformError::Backend(e.to_string()))?;

        self.windows.push(X11Window {
            id,
            xid,
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
            let xevent = if block {
                self.conn.wait_for_event().ok()
            } else {
                match self.conn.poll_for_event() {
                    Ok(Some(e)) => Some(e),
                    Ok(None) => {
                        flow = ControlFlow::Wait;
                        None
                    }
                    Err(_) => None,
                }
            };
            let Some(xevent) = xevent else { continue };
            self.translate(xevent);
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

impl X11App {
    /// Map an X event into our raw event vocabulary.
    fn translate(&mut self, event: XEvent) {
        match event {
            XEvent::ConfigureNotify(ev) => {
                if let Some(win) = self.windows.iter().find(|w| w.xid == ev.window) {
                    let mut q = self.shared.borrow_mut();
                    q.events.push_back(RawEvent::Resized {
                        window: win.id,
                        width: ev.width as u32,
                        height: ev.height as u32,
                    });
                    q.redraws.push_back(win.id);
                }
            }
            XEvent::Expose(ev) => {
                if let Some(win) = self.windows.iter().find(|w| w.xid == ev.window) {
                    self.shared.borrow_mut().redraws.push_back(win.id);
                }
            }
            XEvent::ClientMessage(ev) => {
                let data = ev.data.as_data32();
                if data[0] == self.wm_delete_window
                    && let Some(win) = self.windows.iter().find(|w| w.xid == ev.window)
                {
                    let id = win.id;
                    let mut q = self.shared.borrow_mut();
                    q.events.push_back(RawEvent::WindowClosed { window: id });
                    q.should_exit = true;
                }
            }
            _ => {}
        }
    }
}

/// A native X11 window.
pub struct X11Window {
    id: WindowId,
    xid: u32,
    scale: f64,
}

impl Window for X11Window {
    fn id(&self) -> WindowId {
        self.id
    }

    fn request_redraw(&self) {
        let _ = self.xid;
    }

    fn set_title(&mut self, _title: &str) {
        // change_property on _NET_WM_NAME would go here.
    }

    fn scale_factor(&self) -> f64 {
        self.scale
    }

    fn inner_size(&self) -> (u32, u32) {
        (0, 0)
    }

    fn raw_handle(&self) -> RawWindowHandle {
        RawWindowHandle::Xlib { window: self.xid }
    }
}
