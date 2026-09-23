//! Linux and the BSDs: a Wayland backend and an X11 backend, picked at
//! startup from the session's environment (Wayland first when a compositor is
//! advertised), with system appearance from the XDG desktop portal.
//!
//! Both backends share the event loop below. The OS side of each lives
//! behind [`Display`]: read what the connection has ready, sleep on its file
//! descriptor, finish the handshakes the handler answered. The handler runs
//! with no borrow of backend state held, so the calls it makes back into the
//! app during `run` see consistent state.

mod portal;
mod wayland;
mod x11;
mod xkb;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::rc::Rc;
use std::sync::{Arc, mpsc};

use super::linux_translate as translate;
use crate::control::{ControlFlow, PlatformError, WindowId};
use crate::event::{
    AcceptCell, Appearance, ClipboardReply, ClipboardShortcut, KeyCode, Modifiers, RawEvent,
    RawKey, clipboard_shortcut,
};
use crate::handler::AppHandler;
use crate::menu::SystemAction;
use crate::{Instant, PlatformApp};
use translate::{AccelTable, MenuAction, Session};

/// Connect to the session's windowing system.
pub(crate) fn create() -> Result<Box<dyn PlatformApp>, PlatformError> {
    let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
    let display = std::env::var("DISPLAY").ok();
    let mut last = PlatformError::NoBackend;
    for session in translate::session_order(wayland_display.as_deref(), display.as_deref()) {
        let app = match session {
            Session::Wayland => wayland::WaylandApp::new().map(|a| Box::new(a) as Box<_>),
            Session::X11 => x11::X11App::new().map(|a| Box::new(a) as Box<_>),
        };
        match app {
            Ok(app) => return Ok(app),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Work a background thread hands to the event loop.
#[derive(Debug)]
pub(crate) enum Wake {
    /// The portal reported a new system appearance.
    Appearance(Appearance),
    /// A clipboard read finished.
    Paste { window: WindowId, text: String },
}

/// The sending half of the loop's wake channel: a message queue plus a
/// self-pipe byte that interrupts the loop's `poll`.
#[derive(Clone)]
pub(crate) struct Waker {
    tx: mpsc::Sender<Wake>,
    pipe: Arc<OwnedFd>,
}

impl Waker {
    /// Queue `wake` and interrupt the loop; `false` once the loop is gone.
    pub(crate) fn send(&self, wake: Wake) -> bool {
        if self.tx.send(wake).is_err() {
            return false;
        }
        let byte = 1u8;
        // SAFETY: writing one byte from a live local to the pipe's write end,
        // owned by `self.pipe`. The end is non-blocking: a full pipe already
        // holds a pending wake, so a short write loses nothing.
        let _ = unsafe { libc::write(self.pipe.as_raw_fd(), (&raw const byte).cast(), 1) };
        true
    }
}

/// The receiving half of the wake channel, owned by the event loop.
pub(crate) struct WakeReceiver {
    rx: mpsc::Receiver<Wake>,
    pipe: OwnedFd,
}

impl WakeReceiver {
    pub(crate) fn fd(&self) -> RawFd {
        self.pipe.as_raw_fd()
    }

    /// Empty the pipe and move every queued wake into `pump`.
    fn drain(&self, pump: &mut Pump) {
        let mut buf = [0u8; 64];
        loop {
            // SAFETY: reading into a live local buffer of the given length
            // from the pipe's non-blocking read end.
            let n = unsafe { libc::read(self.fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
        }
        while let Ok(wake) = self.rx.try_recv() {
            pump.wake(wake);
        }
    }
}

pub(crate) fn wake_channel() -> io::Result<(Waker, WakeReceiver)> {
    let mut fds = [0 as RawFd; 2];
    // SAFETY: `fds` is a writable pair of descriptors, filled on success.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `pipe2` returned two fresh descriptors nothing else owns.
    let (read, write) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    let (tx, rx) = mpsc::channel();
    Ok((
        Waker {
            tx,
            pipe: Arc::new(write),
        },
        WakeReceiver { rx, pipe: read },
    ))
}

/// Which of `fds` turned readable before `deadline` (`None` waits
/// indefinitely). An interrupted wait returns with none readable.
pub(crate) fn poll_readable<const N: usize>(
    fds: [RawFd; N],
    deadline: Option<Instant>,
) -> [bool; N] {
    let mut polls = fds.map(|fd| libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    });
    let timeout = deadline.map_or(-1, |d| {
        let ms = d
            .saturating_duration_since(Instant::now())
            .as_micros()
            .div_ceil(1000);
        i32::try_from(ms).unwrap_or(i32::MAX)
    });
    // SAFETY: `polls` is a live array of `N` initialized `pollfd`s.
    let n = unsafe { libc::poll(polls.as_mut_ptr(), N as libc::nfds_t, timeout) };
    if n <= 0 {
        return [false; N];
    }
    polls.map(|p| p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0)
}

/// Loop-side work a key press or menu action asks the backend for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Chore {
    Minimize(WindowId),
    /// Read the clipboard and deliver it as a `Paste` to the window.
    Paste(WindowId),
}

/// How the loop routed a key press.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct KeyRoute {
    /// A menu accelerator took the press: it produces no key or text event.
    pub(crate) consumed: bool,
    pub(crate) chore: Option<Chore>,
}

/// The events both backends queue for the handler, plus the app-wide state
/// the queueing needs.
#[derive(Default)]
pub(crate) struct Pump {
    events: VecDeque<RawEvent>,
    redraws: VecDeque<WindowId>,
    pub(crate) should_exit: bool,
    pub(crate) appearance: Appearance,
    pub(crate) accels: AccelTable,
}

impl Pump {
    pub(crate) fn push(&mut self, event: RawEvent) {
        self.events.push_back(event);
    }

    pub(crate) fn push_redraw(&mut self, window: WindowId) {
        if !self.redraws.contains(&window) {
            self.redraws.push_back(window);
        }
    }

    pub(crate) fn has_work(&self) -> bool {
        !self.events.is_empty() || !self.redraws.is_empty()
    }

    fn next(&mut self) -> Option<RawEvent> {
        if let Some(event) = self.events.pop_front() {
            return Some(event);
        }
        self.redraws
            .pop_front()
            .map(|window| RawEvent::RedrawRequested { window })
    }

    pub(crate) fn forget_window(&mut self, window: WindowId) {
        self.redraws.retain(|w| *w != window);
    }

    fn wake(&mut self, wake: Wake) {
        match wake {
            Wake::Appearance(appearance) => self.set_appearance(appearance),
            Wake::Paste { window, text } => self.push(RawEvent::Paste { window, text }),
        }
    }

    pub(crate) fn set_appearance(&mut self, appearance: Appearance) {
        if self.appearance != appearance {
            self.appearance = appearance;
            self.push(RawEvent::AppearanceChanged(appearance));
        }
    }

    /// Route a key transition: accelerators first, then the key event, then
    /// the clipboard gesture it spells. `base` is the unshifted character
    /// the key prints, for character accelerators.
    pub(crate) fn key(
        &mut self,
        window: WindowId,
        code: KeyCode,
        base: Option<char>,
        modifiers: Modifiers,
        pressed: bool,
        repeat: bool,
    ) -> KeyRoute {
        if pressed && let Some(action) = self.accels.lookup(code, base, modifiers) {
            return KeyRoute {
                consumed: true,
                chore: self.menu_action(window, action),
            };
        }
        self.push(RawEvent::Key(RawKey {
            window,
            code,
            pressed,
            repeat,
            modifiers,
        }));
        let chore = match clipboard_shortcut(code, modifiers).filter(|_| pressed) {
            Some(ClipboardShortcut::Copy) => self.copy(window, false),
            Some(ClipboardShortcut::Cut) => self.copy(window, true),
            Some(ClipboardShortcut::Paste) => Some(Chore::Paste(window)),
            None => None,
        };
        KeyRoute {
            consumed: false,
            chore,
        }
    }

    fn copy(&mut self, window: WindowId, cut: bool) -> Option<Chore> {
        self.push(RawEvent::CopyRequested {
            window,
            cut,
            reply: ClipboardReply::new(),
        });
        None
    }

    fn menu_action(&mut self, window: WindowId, action: MenuAction) -> Option<Chore> {
        match action {
            MenuAction::Command(id) => {
                self.push(RawEvent::MenuCommand { id });
                None
            }
            MenuAction::System(action) => match action {
                SystemAction::Quit => {
                    self.should_exit = true;
                    None
                }
                SystemAction::CloseWindow => {
                    self.push(RawEvent::CloseRequested {
                        window,
                        accept: AcceptCell::new(),
                    });
                    None
                }
                SystemAction::Hide | SystemAction::Minimize => Some(Chore::Minimize(window)),
                SystemAction::Copy => self.copy(window, false),
                SystemAction::Cut => self.copy(window, true),
                SystemAction::Paste => Some(Chore::Paste(window)),
            },
        }
    }
}

/// The handshake a delivered event leaves for the backend to finish.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum After {
    /// Put the handler's answer to a `CopyRequested` on the clipboard.
    Copy(String),
    /// The handler accepted a `CloseRequested`: destroy the window.
    Close(WindowId),
    /// The handler drew (or declined to draw) a requested frame.
    Redrawn(WindowId),
}

/// One windowing-system connection, as the shared loop drives it.
pub(crate) trait Display {
    fn pump(&mut self) -> &mut Pump;
    fn pump_ref(&self) -> &Pump;
    /// Move whatever the connection has ready into the pump without
    /// blocking; `false` once the connection is lost.
    fn dispatch(&mut self) -> bool;
    /// Sleep until the connection or `wake_fd` has input or `deadline`
    /// passes, then dispatch; `false` once the connection is lost.
    fn wait(&mut self, wake_fd: RawFd, deadline: Option<Instant>) -> bool;
    /// A redraw is about to be delivered.
    fn before_redraw(&mut self, window: WindowId);
    fn finish(&mut self, after: After);
}

/// Run the shared loop over `display` until the handler or the user exits.
pub(crate) fn run<D: Display>(
    display: &Rc<RefCell<D>>,
    wakes: &WakeReceiver,
    handler: &mut dyn AppHandler,
) {
    if handler.handle(RawEvent::AppLaunched) == ControlFlow::Exit {
        return;
    }
    let mut flow = ControlFlow::Wait;
    loop {
        let next = {
            let mut d = display.borrow_mut();
            if !d.dispatch() {
                break;
            }
            wakes.drain(d.pump());
            if d.pump().should_exit {
                break;
            }
            let next = d.pump().next();
            if let Some(RawEvent::RedrawRequested { window }) = &next {
                d.before_redraw(*window);
            }
            next
        };
        if let Some(event) = next {
            flow = deliver(display, handler, event);
            if flow == ControlFlow::Exit {
                break;
            }
            continue;
        }
        match flow {
            ControlFlow::Exit => break,
            ControlFlow::Poll => flow = ControlFlow::Wait,
            ControlFlow::Wait => {
                if !display.borrow_mut().wait(wakes.fd(), None) {
                    break;
                }
            }
            ControlFlow::WaitUntil(deadline) => {
                let alive = Instant::now() >= deadline
                    || display.borrow_mut().wait(wakes.fd(), Some(deadline));
                if !alive {
                    break;
                }
                let idle = !display.borrow().pump_ref().has_work();
                if idle && Instant::now() >= deadline {
                    flow = deliver(display, handler, RawEvent::Wakeup);
                    if flow == ControlFlow::Exit {
                        break;
                    }
                }
            }
        }
    }
}

/// Hand one event to the handler, with no backend borrow held, then finish
/// its handshake.
fn deliver<D: Display>(
    display: &Rc<RefCell<D>>,
    handler: &mut dyn AppHandler,
    event: RawEvent,
) -> ControlFlow {
    enum Pending {
        Copy(ClipboardReply),
        Close(AcceptCell, WindowId),
        Redrawn(WindowId),
    }
    let pending = match &event {
        RawEvent::CopyRequested { reply, .. } => Some(Pending::Copy(reply.clone())),
        RawEvent::CloseRequested { accept, window } => {
            Some(Pending::Close(accept.clone(), *window))
        }
        RawEvent::RedrawRequested { window } => Some(Pending::Redrawn(*window)),
        _ => None,
    };
    let flow = handler.handle(event);
    let after = match pending {
        Some(Pending::Copy(reply)) => reply.take().map(After::Copy),
        Some(Pending::Close(accept, window)) => {
            accept.is_accepted().then_some(After::Close(window))
        }
        Some(Pending::Redrawn(window)) => Some(After::Redrawn(window)),
        None => None,
    };
    if let Some(after) = after {
        display.borrow_mut().finish(after);
    }
    flow
}
