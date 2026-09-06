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
use std::os::fd::AsRawFd;
use std::rc::Rc;
use std::time::Instant;

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

            let xevent = match flow {
                // A live one-shot timer: sleep on the X connection's fd only
                // until the deadline, then wake to fire it. If an X event is
                // already buffered we take it; otherwise we `poll(2)` the fd
                // with a `deadline - now` timeout. On timeout with nothing
                // readable we synthesize a `Wakeup` beat so the runtime runs one
                // frame and fires the due timer — zero frames until the deadline.
                ControlFlow::WaitUntil(deadline) => match self.conn.poll_for_event() {
                    Ok(Some(e)) => Some(e),
                    Ok(None) => {
                        let ms = deadline
                            .saturating_duration_since(Instant::now())
                            .as_millis()
                            .min(i32::MAX as u128) as i32;
                        if !wait_readable(self.conn.stream().as_raw_fd(), ms) {
                            flow = handler.handle(RawEvent::Wakeup);
                            if flow == ControlFlow::Exit {
                                break;
                            }
                        }
                        None
                    }
                    Err(_) => None,
                },
                ControlFlow::Wait => self.conn.wait_for_event().ok(),
                _ => match self.conn.poll_for_event() {
                    Ok(Some(e)) => Some(e),
                    Ok(None) => {
                        flow = ControlFlow::Wait;
                        None
                    }
                    Err(_) => None,
                },
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

    fn close_window(&mut self, window: WindowId) {
        // Same close path as a user-driven close (the WM_DELETE_WINDOW handshake
        // in `translate`), initiated by the app: destroy the X window and enqueue
        // `WindowClosed` so the scheduler decrements its open-window count and the
        // driver tears the window down through the one close path. Enqueue
        // directly for deterministic delivery identical to the other backends.
        // Unknown id is a no-op. We do NOT set `should_exit`: closing one window
        // in a multi-window session must not end the pump; the scheduler's
        // open-window gate owns exit.
        let Some(pos) = self.windows.iter().position(|w| w.id == window) else {
            return;
        };
        let closed = self.windows.remove(pos);
        // Best-effort: ignore protocol errors from a server-side race (the window
        // may already be gone). The queued `WindowClosed` is what the runtime acts
        // on, regardless.
        let _ = self.conn.destroy_window(closed.xid);
        let _ = self.conn.flush();
        self.shared
            .borrow_mut()
            .events
            .push_back(RawEvent::WindowClosed { window });
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

/// Block until `fd` is readable or `timeout_ms` elapses, whichever comes first.
///
/// Returns `true` when the fd became readable (an X event is waiting), `false`
/// on timeout (the caller's timer deadline arrived). A negative `timeout_ms`
/// would mean "block forever" to `poll(2)`; the caller only passes a
/// non-negative `deadline - now`, and a zero timeout returns immediately.
fn wait_readable(fd: std::os::fd::RawFd, timeout_ms: i32) -> bool {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: `pfd` is a single valid, initialized `pollfd` and we pass a count
    // of 1, matching the pointer. `poll` only reads `fd`/`events` and writes
    // `revents`; no memory outside `pfd` is touched.
    let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    n > 0 && (pfd.revents & libc::POLLIN) != 0
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
