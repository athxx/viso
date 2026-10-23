//! The clipboard and drops over `wl_data_device`: copies offer a data source
//! the compositor asks to write out, pastes read the current selection's
//! offer through a pipe on a background thread. A drag from another client
//! over a window reaches the app at each move, its offer accepted or not as
//! the app answers, and dropped, its data is read the same way before the app
//! takes it. A drag the app carries out of a window is a data source the
//! compositor runs from the button's grab, written out to whoever takes it.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::sync::Mutex;

use wayland_client::protocol::{
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{DndAction, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
    wl_seat::WlSeat,
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop, event_created_child,
};

use super::State;
use crate::backend::linux::{Wake, Waker};
use crate::control::WindowId;
use crate::drop::{DropAction, DropActions, DropKinds, DropOffer, DropPhase, DropReply, RawDrop};
use crate::event::RawEvent;

/// Text types in the order a paste prefers them; a copy offers them all.
const TEXT_MIMES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "TEXT",
    "STRING",
];

/// What an offer advertised, filled by its events.
struct Offered {
    mimes: Vec<String>,
    /// For a drag, the actions its source allows.
    actions: DndAction,
}

impl Default for Offered {
    fn default() -> Self {
        Self {
            mimes: Vec::new(),
            actions: DndAction::empty(),
        }
    }
}

type OfferMimes = Mutex<Offered>;

/// A drag from another client over one of the windows.
struct Drag {
    offer: WlDataOffer,
    serial: u32,
    window: WindowId,
    /// Where it is, in the window's logical points.
    at: (f64, f64),
    kinds: DropKinds,
    reply: DropReply,
}

pub(super) struct Clipboard {
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    /// The current selection, when another client owns it.
    selection: Option<WlDataOffer>,
    /// The drag hovering one of our surfaces.
    drag: Option<Drag>,
    /// A drag let go, its data being read, then taken by the app.
    dropped: Option<WlDataOffer>,
    /// Our own selection and its text, while the compositor holds it.
    own: Option<(WlDataSource, String)>,
    /// The drag the app carries out, while the compositor runs it.
    out: Option<Carried>,
}

/// A drag the app carries out of `window`.
struct Carried {
    source: WlDataSource,
    window: WindowId,
    text: String,
    /// A `text/uri-list`; empty for none.
    uris: String,
    /// The action the compositor last settled on.
    action: DndAction,
}

/// The user data of the source of a drag the app carries out.
pub(super) struct DragSource;

impl Clipboard {
    pub(super) fn new(manager: Option<WlDataDeviceManager>) -> Self {
        Self {
            manager,
            device: None,
            selection: None,
            drag: None,
            dropped: None,
            own: None,
            out: None,
        }
    }

    pub(super) fn attach_seat(&mut self, seat: &WlSeat, qh: &QueueHandle<State>) {
        if let Some(manager) = &self.manager {
            self.device = Some(manager.get_data_device(seat, qh, ()));
        }
    }

    pub(super) fn detach_seat(&mut self) {
        if let Some(offer) = self.selection.take() {
            offer.destroy();
        }
        if let Some(drag) = self.drag.take() {
            drag.offer.destroy();
        }
        if let Some(offer) = self.dropped.take() {
            offer.destroy();
        }
        if let Some(device) = self.device.take()
            && device.version() >= 2
        {
            device.release();
        }
    }

    /// Carries `drag` out of `window`, whose `surface` the button press
    /// with `serial` grabbed. Returns whether it started.
    pub(super) fn start_drag(
        &mut self,
        qh: &QueueHandle<State>,
        window: WindowId,
        surface: &wayland_client::protocol::wl_surface::WlSurface,
        drag: &crate::DragOut,
        serial: u32,
    ) -> bool {
        let (Some(manager), Some(device)) = (&self.manager, &self.device) else {
            return false;
        };
        if self.out.is_some() {
            return false;
        }
        let (text, uris) = (drag.plain_text(), drag.uri_list());
        let source = manager.create_data_source(qh, DragSource);
        if !uris.is_empty() {
            source.offer("text/uri-list".to_owned());
        }
        if !text.is_empty() {
            for mime in TEXT_MIMES {
                source.offer(mime.to_owned());
            }
        }
        // Wayland has no link; a drop that would link copies.
        let mut actions = DndAction::empty();
        if drag.actions.copies || drag.actions.links {
            actions |= DndAction::Copy;
        }
        if drag.actions.moves {
            actions |= DndAction::Move;
        }
        source.set_actions(actions);
        device.start_drag(Some(&source), surface, None, serial);
        self.out = Some(Carried {
            source,
            window,
            text,
            uris,
            action: DndAction::empty(),
        });
        true
    }

    /// Put `text` on the clipboard, quoting the input `serial` that asked.
    pub(super) fn set(&mut self, qh: &QueueHandle<State>, text: &str, serial: u32) {
        let (Some(manager), Some(device)) = (&self.manager, &self.device) else {
            return;
        };
        let source = manager.create_data_source(qh, ());
        for mime in TEXT_MIMES {
            source.offer(mime.to_owned());
        }
        device.set_selection(Some(&source), serial);
        if let Some((old, _)) = self.own.replace((source, text.to_owned())) {
            old.destroy();
        }
    }

    /// Start reading the clipboard for `window`. Our own selection is
    /// answered at once; another client's arrives later as a
    /// [`Wake::Paste`].
    pub(super) fn paste(&mut self, window: WindowId, waker: &Waker) -> Option<String> {
        if let Some((_, text)) = &self.own {
            return Some(text.clone());
        }
        let offer = self.selection.as_ref()?;
        let mime = offer
            .data::<OfferMimes>()
            .and_then(|m| pick_mime(&m.lock().ok()?.mimes))?;
        let (read, write) = pipe().ok()?;
        offer.receive(mime.to_owned(), write.as_fd());
        // Our copy of the write end closes now, so the read below ends
        // when the owner closes theirs.
        drop(write);
        let waker = waker.clone();
        let spawned = std::thread::Builder::new()
            .name("viso-paste".into())
            .spawn(move || {
                let mut bytes = Vec::new();
                if File::from(read).read_to_end(&mut bytes).is_ok() && !bytes.is_empty() {
                    let text = String::from_utf8_lossy(&bytes).into_owned();
                    waker.send(Wake::Paste { window, text });
                }
            });
        drop(spawned);
        None
    }
}

impl Clipboard {
    /// The app heard step `phase` of the drag over `window` and said
    /// `reply`: over it, the offer is accepted for the action it said or
    /// declined; dropped, the drag is finished.
    pub(super) fn answer_drop(&mut self, window: WindowId, phase: DropPhase, reply: &DropReply) {
        match phase {
            DropPhase::Over => {
                let Some(drag) = self.drag.as_ref().filter(|d| d.window == window) else {
                    return;
                };
                let (mimes, mut allowed) = drag
                    .offer
                    .data::<OfferMimes>()
                    .and_then(|m| m.lock().ok().map(|m| (m.mimes.clone(), m.actions)))
                    .unwrap_or((Vec::new(), DndAction::empty()));
                // A source of an older protocol allows a copy, and says nothing.
                if allowed.is_empty() {
                    allowed = DndAction::Copy;
                }
                let actions = DropActions {
                    moves: allowed.contains(DndAction::Move),
                    copies: allowed.contains(DndAction::Copy),
                    links: false,
                };
                match actions.settle(reply.get()).zip(mimes.first()) {
                    Some((action, mime)) => {
                        let action = match action {
                            DropAction::Move => DndAction::Move,
                            _ => DndAction::Copy,
                        };
                        drag.offer.accept(drag.serial, Some(mime.clone()));
                        drag.offer.set_actions(action, action);
                    }
                    None => {
                        drag.offer.accept(drag.serial, None);
                        drag.offer
                            .set_actions(DndAction::empty(), DndAction::empty());
                    }
                }
            }
            DropPhase::Drop => {
                if let Some(offer) = self.dropped.take() {
                    if reply.get().is_some() {
                        offer.finish();
                    }
                    offer.destroy();
                }
            }
            DropPhase::Leave => {}
        }
    }

    /// The drag was let go: its data is read through a pipe, and arrives as
    /// a [`Wake::Dropped`] the app then takes, the step waiting for it; with
    /// nothing readable the step goes at once, `false`.
    fn read_drop(&mut self, waker: &Waker) -> Option<(RawDrop, bool)> {
        let drag = self.drag.take()?;
        let step = RawDrop {
            window: drag.window,
            phase: DropPhase::Drop,
            x: drag.at.0,
            y: drag.at.1,
            offer: DropOffer::default(),
            reply: drag.reply,
        };
        let mimes = drag
            .offer
            .data::<OfferMimes>()
            .and_then(|m| m.lock().ok().map(|m| m.mimes.clone()))
            .unwrap_or_default();
        let list = mimes.iter().any(|m| m == "text/uri-list");
        let mime = if list {
            Some("text/uri-list")
        } else {
            pick_mime(&mimes)
        };
        let read = mime.and_then(|mime| {
            let (read, write) = pipe().ok()?;
            drag.offer.receive(mime.to_owned(), write.as_fd());
            drop(write);
            Some(read)
        });
        if let Some(old) = self.dropped.replace(drag.offer) {
            old.destroy();
        }
        let Some(read) = read else {
            return Some((step, false));
        };
        let waker = waker.clone();
        let spawned = std::thread::Builder::new()
            .name("viso-drop".into())
            .spawn(move || {
                let mut bytes = Vec::new();
                let _ = File::from(read).read_to_end(&mut bytes);
                let text = String::from_utf8_lossy(&bytes).into_owned();
                waker.send(Wake::Dropped { text, list });
            });
        if spawned.is_err() {
            return Some((step, false));
        }
        Some((step, true))
    }
}

/// What a drag offering `mimes` offers: a `text/uri-list` may hold files,
/// links, or both.
fn kinds_of(mimes: &[String]) -> DropKinds {
    let list = mimes.iter().any(|m| m == "text/uri-list");
    DropKinds {
        text: pick_mime(mimes).is_some(),
        uri: list,
        files: list,
    }
}

/// The preferred text type among `offered`.
fn pick_mime(offered: &[String]) -> Option<&'static str> {
    TEXT_MIMES
        .into_iter()
        .find(|m| offered.iter().any(|o| o == m))
}

fn pipe() -> std::io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0; 2];
    // SAFETY: `fds` is a writable pair of descriptors, filled on success.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `pipe2` returned two fresh descriptors nothing else owns.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

impl Drag {
    fn step(&self, phase: DropPhase) -> RawDrop {
        RawDrop {
            window: self.window,
            phase,
            x: self.at.0,
            y: self.at.1,
            offer: DropOffer {
                kinds: self.kinds,
                ..DropOffer::default()
            },
            reply: self.reply.clone(),
        }
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let clipboard = &mut state.clipboard;
        match event {
            wl_data_device::Event::Selection { id } => {
                if let Some(old) = std::mem::replace(&mut clipboard.selection, id) {
                    old.destroy();
                }
            }
            wl_data_device::Event::Enter {
                serial,
                surface,
                x,
                y,
                id,
            } => {
                if let Some(old) = clipboard.drag.take() {
                    old.offer.destroy();
                }
                let (Some(offer), Some(&window)) = (id, surface.data::<WindowId>()) else {
                    return;
                };
                let kinds = offer
                    .data::<OfferMimes>()
                    .and_then(|m| m.lock().ok().map(|m| kinds_of(&m.mimes)))
                    .unwrap_or_default();
                let drag = Drag {
                    offer,
                    serial,
                    window,
                    at: (x, y),
                    kinds,
                    reply: DropReply::new(),
                };
                state.pump.push(RawEvent::Drop(drag.step(DropPhase::Over)));
                clipboard.drag = Some(drag);
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                if let Some(drag) = clipboard.drag.as_mut() {
                    drag.at = (x, y);
                    state.pump.push(RawEvent::Drop(drag.step(DropPhase::Over)));
                }
            }
            wl_data_device::Event::Leave => {
                if let Some(drag) = clipboard.drag.take() {
                    state.pump.push(RawEvent::Drop(drag.step(DropPhase::Leave)));
                    drag.offer.destroy();
                }
            }
            wl_data_device::Event::Drop => match clipboard.read_drop(&state.waker) {
                Some((step, true)) => state.pump.hold_drop(step),
                Some((step, false)) => state.pump.push(RawEvent::Drop(step)),
                None => {}
            },
            _ => {}
        }
    }

    event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, OfferMimes::default()),
    ]);
}

impl Dispatch<WlDataOffer, OfferMimes> for State {
    fn event(
        _: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        mimes: &OfferMimes,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Ok(mut offered) = mimes.lock() else {
            return;
        };
        match event {
            wl_data_offer::Event::Offer { mime_type } => offered.mimes.push(mime_type),
            wl_data_offer::Event::SourceActions {
                source_actions: WEnum::Value(actions),
            } => offered.actions = actions,
            _ => {}
        }
    }
}

impl Dispatch<WlDataSource, ()> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let own = &mut state.clipboard.own;
        match event {
            wl_data_source::Event::Send { fd, .. } => {
                let Some((_, text)) = own.as_ref().filter(|(s, _)| s == source) else {
                    return;
                };
                let bytes = text.clone().into_bytes();
                // A slow reader must not stall the event loop.
                let _ = std::thread::Builder::new()
                    .name("viso-copy".into())
                    .spawn(move || {
                        let _ = File::from(fd).write_all(&bytes);
                    });
            }
            wl_data_source::Event::Cancelled => {
                if own.as_ref().is_some_and(|(s, _)| s == source) {
                    *own = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }
}

impl Dispatch<WlDataSource, DragSource> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        _: &DragSource,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(out) = state.clipboard.out.as_mut().filter(|o| o.source == *source) else {
            source.destroy();
            return;
        };
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                let bytes = if mime_type == "text/uri-list" {
                    out.uris.clone()
                } else {
                    out.text.clone()
                }
                .into_bytes();
                let _ = std::thread::Builder::new()
                    .name("viso-drag".into())
                    .spawn(move || {
                        let _ = File::from(fd).write_all(&bytes);
                    });
            }
            wl_data_source::Event::Action {
                dnd_action: WEnum::Value(action),
            } => out.action = action,
            wl_data_source::Event::DndFinished | wl_data_source::Event::Cancelled => {
                let finished = matches!(event, wl_data_source::Event::DndFinished);
                let out = state.clipboard.out.take().expect("matched above");
                out.source.destroy();
                let action = if !finished {
                    None
                } else if out.action.contains(DndAction::Move) {
                    Some(DropAction::Move)
                } else {
                    Some(DropAction::Copy)
                };
                state.drag_ended(out.window, action);
            }
            _ => {}
        }
    }
}

delegate_noop!(State: WlDataDeviceManager);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pastes_prefer_utf8_text() {
        let offered = |m: &[&str]| m.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        assert_eq!(
            pick_mime(&offered(&["STRING", "text/plain;charset=utf-8"])),
            Some("text/plain;charset=utf-8")
        );
        assert_eq!(
            pick_mime(&offered(&["TEXT", "UTF8_STRING"])),
            Some("UTF8_STRING")
        );
        assert_eq!(pick_mime(&offered(&["image/png"])), None);
    }

    #[test]
    fn a_pipe_carries_bytes_to_eof() {
        let (read, write) = pipe().unwrap();
        File::from(write).write_all(b"hello").unwrap();
        let mut out = String::new();
        File::from(read).read_to_string(&mut out).unwrap();
        assert_eq!(out, "hello");
    }
}
