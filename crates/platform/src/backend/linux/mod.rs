//! Linux and the BSDs: a Wayland backend (`wayland`) and an X11 backend
//! (`x11`), picked at startup from the session's environment (Wayland first
//! when a compositor is advertised), with system appearance from the XDG
//! desktop portal and AT-SPI accessibility; and, with the `kms` feature, a
//! backend that drives the card directly where there is no display server at
//! all (embedded Linux). A build with none of them reports no backend.
//!
//! Every backend shares the event loop below. The OS side of each lives
//! behind [`Display`]: read what the connection has ready, sleep on its file
//! descriptor, finish the handshakes the handler answered. The handler runs
//! with no borrow of backend state held, so the calls it makes back into the
//! app during `run` see consistent state.

// A build with no backend keeps the shared loop and reports `NoBackend`.
#![cfg_attr(
    not(any(feature = "x11", feature = "wayland", feature = "kms")),
    allow(dead_code, unused_imports)
)]

#[cfg(any(feature = "x11", feature = "wayland"))]
mod access;
#[cfg(feature = "kms")]
mod kms;
mod links;
#[cfg(any(feature = "x11", feature = "wayland"))]
mod portal;
#[cfg(feature = "wayland")]
mod wayland;
#[cfg(feature = "x11")]
mod x11;
mod xkb;

use std::cell::RefCell;
use std::collections::VecDeque;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::rc::Rc;
use std::sync::{Arc, mpsc};

use super::linux_translate as translate;
#[cfg(any(feature = "x11", feature = "wayland"))]
use crate::accessibility::AccessRequest;
use crate::clipboard::{ClipboardReply, PasteReply};
use crate::control::{ControlFlow, PlatformError, WindowId};
use crate::event::{
    AcceptCell, Appearance, ClipboardShortcut, KeyCode, Modifiers, RawEvent, RawKey,
    clipboard_shortcut,
};
use crate::handler::AppHandler;
use crate::menu::SystemAction;
use crate::{Instant, PlatformApp};
use translate::{AccelTable, MenuAction, Session};

/// Connect to the session's windowing system.
pub(crate) fn create() -> Result<Box<dyn PlatformApp>, PlatformError> {
    links::claim();
    let wayland_display = std::env::var("WAYLAND_DISPLAY").ok();
    let display = std::env::var("DISPLAY").ok();
    let forced = std::env::var("VISO_SESSION").ok();
    let mut last = PlatformError::NoBackend;
    for session in translate::session_order(
        wayland_display.as_deref(),
        display.as_deref(),
        forced.as_deref(),
    ) {
        let app = match session {
            #[cfg(feature = "wayland")]
            Session::Wayland => wayland::WaylandApp::new().map(|a| Box::new(a) as Box<_>),
            #[cfg(not(feature = "wayland"))]
            Session::Wayland => Err(PlatformError::NoBackend),
            #[cfg(feature = "x11")]
            Session::X11 => x11::X11App::new().map(|a| Box::new(a) as Box<_>),
            #[cfg(not(feature = "x11"))]
            Session::X11 => Err(PlatformError::NoBackend),
            #[cfg(feature = "kms")]
            Session::Kms => kms::KmsApp::new().map(|a| Box::new(a) as Box<_>),
            #[cfg(not(feature = "kms"))]
            Session::Kms => Err(PlatformError::NoBackend),
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
    #[cfg(any(feature = "x11", feature = "wayland"))]
    Appearance(Appearance),
    /// A clipboard read finished.
    #[cfg_attr(not(feature = "wayland"), allow(dead_code))]
    Paste {
        window: WindowId,
        data: crate::ClipboardData,
        token: Option<u32>,
    },
    /// A drop's data was read: the `text`, a `text/uri-list` when `list`.
    #[cfg(feature = "wayland")]
    Dropped { text: String, list: bool },
    /// An assistive technology asked something of a window.
    #[cfg(any(feature = "x11", feature = "wayland"))]
    Access {
        window: WindowId,
        request: AccessRequest,
    },
    /// A later start of the app handed it a URL to open.
    OpenUrl(String),
    /// A [`LoopWaker`](crate::LoopWaker) kick.
    Tasks,
}

/// The sending half of the loop's wake channel: a message queue plus a
/// self-pipe byte that interrupts the loop's `poll`.
#[derive(Clone)]
pub(crate) struct Waker {
    tx: mpsc::Sender<Wake>,
    pipe: Arc<OwnedFd>,
}

impl Waker {
    /// A [`LoopWaker`](crate::LoopWaker) whose kick sends [`Wake::Tasks`].
    pub(crate) fn loop_waker(&self) -> crate::LoopWaker {
        let waker = self.clone();
        crate::LoopWaker::new(move || {
            waker.send(Wake::Tasks);
        })
    }

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
#[cfg_attr(not(any(feature = "x11", feature = "wayland")), allow(dead_code))]
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
    /// Read what the clipboard holds of `kinds` and deliver it as a `Paste`
    /// to the window, carrying `token`: always for a request, only when it
    /// holds some for a gesture (no token).
    Paste {
        window: WindowId,
        kinds: crate::ClipboardKinds,
        token: Option<u32>,
    },
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
    /// Each window's accessibility adapter.
    #[cfg(any(feature = "x11", feature = "wayland"))]
    access: Vec<(WindowId, access::LinuxAccess)>,
    /// A drop waiting for its data, read on a background thread.
    #[cfg(feature = "wayland")]
    dropping: Option<crate::RawDrop>,
}

impl Pump {
    pub(crate) fn push(&mut self, event: RawEvent) {
        #[cfg(any(feature = "x11", feature = "wayland"))]
        if let RawEvent::WindowFocused { window, focused } = event
            && let Some((_, access)) = self.access.iter_mut().find(|(w, _)| *w == window)
        {
            access.set_focused(focused);
        }
        self.events.push_back(event);
    }

    /// Give `window` an accessibility adapter whose requests arrive through
    /// `waker`.
    #[cfg(any(feature = "x11", feature = "wayland"))]
    pub(crate) fn attach_access(&mut self, window: WindowId, waker: &Waker) {
        self.access
            .push((window, access::LinuxAccess::attach(window, waker)));
    }

    #[cfg(any(feature = "x11", feature = "wayland"))]
    pub(crate) fn update_access(&mut self, window: WindowId, delta: crate::SemanticsDelta) {
        if let Some((_, access)) = self.access.iter_mut().find(|(w, _)| *w == window) {
            access.update(delta);
        }
    }

    /// Holds drop `step` until its data arrives as a [`Wake::Dropped`].
    #[cfg(feature = "wayland")]
    pub(crate) fn hold_drop(&mut self, step: crate::RawDrop) {
        self.dropping = Some(step);
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
        #[cfg(any(feature = "x11", feature = "wayland"))]
        self.access.retain(|(w, _)| *w != window);
    }

    fn wake(&mut self, wake: Wake) {
        match wake {
            #[cfg(any(feature = "x11", feature = "wayland"))]
            Wake::Appearance(appearance) => self.set_appearance(appearance),
            Wake::Paste {
                window,
                data,
                token,
            } => self.push(RawEvent::Paste {
                window,
                data,
                token,
            }),
            #[cfg(any(feature = "x11", feature = "wayland"))]
            Wake::Access { window, request } => {
                self.push(RawEvent::Accessibility { window, request });
            }
            #[cfg(feature = "wayland")]
            Wake::Dropped { text, list } => {
                if let Some(mut step) = self.dropping.take() {
                    let offer = &mut step.offer;
                    if list {
                        let (files, uri) = crate::drop::uri_list(&text);
                        offer.text = uri.clone();
                        offer.uri = uri;
                        offer.files = files;
                    } else {
                        offer.text = Some(text);
                    }
                    offer.kinds = crate::DropKinds {
                        text: offer.text.is_some(),
                        uri: offer.uri.is_some(),
                        files: !offer.files.is_empty(),
                    };
                    self.push(RawEvent::Drop(step));
                }
            }
            Wake::OpenUrl(url) => self.push(RawEvent::OpenUrl { url }),
            Wake::Tasks => self.push(RawEvent::Wakeup),
        }
    }

    #[cfg_attr(not(any(feature = "x11", feature = "wayland")), allow(dead_code))]
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
            Some(ClipboardShortcut::Paste) => self.paste(window),
            None => None,
        };
        KeyRoute {
            consumed: false,
            chore,
        }
    }

    /// A paste gesture: the handler says what it takes, read once it
    /// returns.
    fn paste(&mut self, window: WindowId) -> Option<Chore> {
        self.push(RawEvent::PasteRequested {
            window,
            reply: PasteReply::new(),
        });
        None
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
                SystemAction::Paste => self.paste(window),
            },
        }
    }
}

/// The handshake a delivered event leaves for the backend to finish.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum After {
    /// Put the handler's answer to a `CopyRequested` on the clipboard.
    Copy(crate::ClipboardData),
    /// Read the kinds the handler took for a paste gesture into the window.
    Paste(WindowId, crate::ClipboardKinds),
    /// The handler accepted a `CloseRequested`: destroy the window.
    Close(WindowId),
    /// The handler drew (or declined to draw) a requested frame.
    Redrawn(WindowId),
    /// The handler heard a step of a drag from outside and answered.
    Dropped(WindowId, crate::DropPhase, crate::DropReply),
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

/// Run the shared loop over `display` until the handler or the user exits,
/// lending `app` (the backend whose loop this is) to every delivery.
pub(crate) fn run<D: Display>(
    app: &mut dyn PlatformApp,
    display: &Rc<RefCell<D>>,
    waker: Waker,
    wakes: &WakeReceiver,
    handler: &mut dyn AppHandler,
) {
    let (urls, _socket) = links::serve(move |url| {
        waker.send(Wake::OpenUrl(url));
    });
    if handler.handle(app, RawEvent::AppLaunched) == ControlFlow::Exit {
        return;
    }
    for url in urls {
        display.borrow_mut().pump().push(RawEvent::OpenUrl { url });
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
            flow = deliver(app, display, handler, event);
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
                    flow = deliver(app, display, handler, RawEvent::Wakeup);
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
    app: &mut dyn PlatformApp,
    display: &Rc<RefCell<D>>,
    handler: &mut dyn AppHandler,
    event: RawEvent,
) -> ControlFlow {
    enum Pending {
        Copy(ClipboardReply),
        Paste(WindowId, PasteReply),
        Close(AcceptCell, WindowId),
        Redrawn(WindowId),
        Dropped(WindowId, crate::DropPhase, crate::DropReply),
    }
    let pending = match &event {
        RawEvent::Drop(d) => Some(Pending::Dropped(d.window, d.phase, d.reply.clone())),
        RawEvent::CopyRequested { reply, .. } => Some(Pending::Copy(reply.clone())),
        RawEvent::PasteRequested { window, reply } => Some(Pending::Paste(*window, reply.clone())),
        RawEvent::CloseRequested { accept, window } => {
            Some(Pending::Close(accept.clone(), *window))
        }
        RawEvent::RedrawRequested { window } => Some(Pending::Redrawn(*window)),
        _ => None,
    };
    let flow = handler.handle(app, event);
    let after = match pending {
        Some(Pending::Copy(reply)) => reply.take().map(After::Copy),
        Some(Pending::Paste(window, reply)) => Some(reply.get())
            .filter(|k| k.any())
            .map(|k| After::Paste(window, k)),
        Some(Pending::Close(accept, window)) => {
            accept.is_accepted().then_some(After::Close(window))
        }
        Some(Pending::Redrawn(window)) => Some(After::Redrawn(window)),
        Some(Pending::Dropped(window, phase, reply)) => Some(After::Dropped(window, phase, reply)),
        None => None,
    };
    if let Some(after) = after {
        display.borrow_mut().finish(after);
    }
    flow
}
