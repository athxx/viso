//! The clipboard and drops over `wl_data_device`: copies offer a data source
//! the compositor asks to write out — text, `text/html` and `image/png`, the
//! PNG of an image of pixels encoded on the writing thread when first asked
//! for — and pastes read the kinds they take of the current selection's offer
//! through pipes on a background thread, while the app's own selection
//! answers at once, shared. A drag from another client
//! over a window reaches the app at each move, its offer accepted or not as
//! the app answers, and dropped, its data is read the same way before the app
//! takes it. A drag the app carries out of a window is a data source the
//! compositor runs from the button's grab, written out to whoever takes it.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
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

/// The image types a paste reads, the first the offer has.
const IMAGE_MIMES: [&str; 5] = [
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/bmp",
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
    /// Our own selection and what it holds, while the compositor holds it.
    own: Option<(WlDataSource, crate::ClipboardData)>,
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

    /// Put `data` on the clipboard, quoting the input `serial` that asked.
    pub(super) fn set(&mut self, qh: &QueueHandle<State>, data: crate::ClipboardData, serial: u32) {
        let (Some(manager), Some(device)) = (&self.manager, &self.device) else {
            return;
        };
        let source = manager.create_data_source(qh, ());
        for mime in source_mimes(&data) {
            source.offer(mime.to_owned());
        }
        device.set_selection(Some(&source), serial);
        if let Some((old, _)) = self.own.replace((source, data)) {
            old.destroy();
        }
    }

    /// Start reading what the clipboard holds of `kinds` for `window`. Our
    /// own selection is answered at once; another client's arrives later as
    /// a [`Wake::Paste`] — for a request (`token`) always, for a gesture
    /// only when it holds some.
    pub(super) fn paste(
        &mut self,
        window: WindowId,
        kinds: crate::ClipboardKinds,
        token: Option<u32>,
        waker: &Waker,
    ) -> Option<crate::ClipboardData> {
        if let Some((_, data)) = &self.own {
            let data = data.taken(kinds);
            return (token.is_some() || !data.is_empty()).then_some(data);
        }
        let empty = || token.is_some().then(crate::ClipboardData::default);
        let Some(offer) = self.selection.as_ref() else {
            return empty();
        };
        let mimes = offer
            .data::<OfferMimes>()
            .and_then(|m| m.lock().ok().map(|m| m.mimes.clone()))
            .unwrap_or_default();
        let (text, html, image) = (
            pick_mime(&mimes),
            mimes
                .iter()
                .any(|m| m == "text/html")
                .then_some("text/html"),
            IMAGE_MIMES
                .into_iter()
                .find(|i| mimes.iter().any(|m| m == i)),
        );
        let pick = kinds.pick(crate::ClipboardKinds {
            text: text.is_some(),
            html: html.is_some(),
            image: image.is_some(),
        });
        let mut reads = Vec::new();
        for (wanted, mime, kind) in [
            (pick.text, text, Part::Text),
            (pick.html, html, Part::Html),
            (pick.image, image, Part::Image),
        ] {
            let Some(mime) = mime.filter(|_| wanted) else {
                continue;
            };
            let Ok((read, write)) = pipe() else {
                continue;
            };
            offer.receive(mime.to_owned(), write.as_fd());
            // Our copy of the write end closes now, so the read ends when
            // the owner closes theirs.
            drop(write);
            reads.push((kind, read));
        }
        if reads.is_empty() {
            return empty();
        }
        let waker = waker.clone();
        let spawned = std::thread::Builder::new()
            .name("viso-paste".into())
            .spawn(move || {
                let data = read_parts(reads);
                if token.is_some() || !data.is_empty() {
                    waker.send(Wake::Paste {
                        window,
                        data,
                        token,
                    });
                }
            });
        match spawned {
            Ok(_) => None,
            Err(_) => empty(),
        }
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

/// What a paste reads through one pipe.
#[derive(Clone, Copy)]
enum Part {
    Text,
    Html,
    Image,
}

/// Read every pipe to its end at once — an owner may write them in any
/// order — into what they hold.
fn read_parts(reads: Vec<(Part, OwnedFd)>) -> crate::ClipboardData {
    let mut bytes: Vec<Vec<u8>> = vec![Vec::new(); reads.len()];
    let mut open: Vec<bool> = vec![true; reads.len()];
    let mut chunk = vec![0u8; 64 * 1024];
    while open.iter().any(|&o| o) {
        let mut polls: Vec<libc::pollfd> = reads
            .iter()
            .zip(&open)
            .map(|((_, fd), &o)| libc::pollfd {
                fd: if o { fd.as_raw_fd() } else { -1 },
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        // SAFETY: `polls` is a live array of initialized `pollfd`s; a
        // negative descriptor is skipped. A paste that stalls for ten
        // seconds is given up.
        let n = unsafe { libc::poll(polls.as_mut_ptr(), polls.len() as libc::nfds_t, 10_000) };
        if n <= 0 {
            break;
        }
        for (i, p) in polls.iter().enumerate() {
            if !open[i] || p.revents == 0 {
                continue;
            }
            // SAFETY: reading into `chunk`, writable for its length, from a
            // descriptor `reads` owns.
            let got = unsafe { libc::read(p.fd, chunk.as_mut_ptr().cast(), chunk.len()) };
            match got {
                n if n > 0 => bytes[i].extend_from_slice(&chunk[..n as usize]),
                n if n < 0 && std::io::Error::last_os_error().kind() == ErrorKind::Interrupted => {}
                _ => open[i] = false,
            }
        }
    }
    let mut data = crate::ClipboardData::default();
    for ((part, _), bytes) in reads.iter().zip(bytes) {
        if bytes.is_empty() {
            continue;
        }
        match part {
            Part::Text => data.text = Some(String::from_utf8_lossy(&bytes).into_owned()),
            Part::Html => data.html = Some(crate::clipboard::html_text(&bytes)),
            Part::Image => data.image = Some(crate::ClipboardImage::encoded(bytes)),
        }
    }
    data
}

/// The types a selection holding `data` offers: its image as a PNG, its
/// HTML, and its text in every text type.
fn source_mimes(data: &crate::ClipboardData) -> Vec<&'static str> {
    let mut mimes = Vec::new();
    if data
        .image
        .as_ref()
        .is_some_and(|i| i.png().is_some() || i.rgba_pixels().is_some())
    {
        mimes.push("image/png");
    }
    if data.html.is_some() {
        mimes.push("text/html");
    }
    if data.text.is_some() {
        mimes.extend(TEXT_MIMES);
    }
    mimes
}

/// What our selection `data` writes for `mime`; `None` for a type it
/// does not offer. An image is encoded on the writing thread.
fn source_bytes(data: &crate::ClipboardData, mime: &str) -> Option<SourceBytes> {
    match mime {
        "image/png" => data.image.clone().map(SourceBytes::Image),
        "text/html" => data
            .html
            .clone()
            .map(|h| SourceBytes::Bytes(h.into_bytes())),
        _ if TEXT_MIMES.contains(&mime) => data
            .text
            .clone()
            .map(|t| SourceBytes::Bytes(t.into_bytes())),
        _ => None,
    }
}

enum SourceBytes {
    Bytes(Vec<u8>),
    Image(crate::ClipboardImage),
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
            wl_data_source::Event::Send { mime_type, fd } => {
                let Some(bytes) = own
                    .as_ref()
                    .filter(|(s, _)| s == source)
                    .and_then(|(_, data)| source_bytes(data, &mime_type))
                else {
                    return;
                };
                // A slow reader, or encoding the image, must not stall the
                // event loop.
                let _ = std::thread::Builder::new()
                    .name("viso-copy".into())
                    .spawn(move || {
                        let mut file = File::from(fd);
                        let _ = match bytes {
                            SourceBytes::Bytes(bytes) => file.write_all(&bytes),
                            SourceBytes::Image(image) => match image.png() {
                                Some(png) => file.write_all(&png),
                                None => Ok(()),
                            },
                        };
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
    fn a_selection_offers_what_it_holds_and_pastes_read_every_pipe() {
        let image = crate::ClipboardImage::rgba(1, 1, vec![1, 2, 3, 4]).unwrap();
        let data = crate::ClipboardData {
            image: Some(image.clone()),
            ..crate::ClipboardData::html("<b>x</b>", "x")
        };
        let mimes = source_mimes(&data);
        assert_eq!(&mimes[..2], &["image/png", "text/html"]);
        assert!(mimes.contains(&"UTF8_STRING"));
        let Some(SourceBytes::Image(sent)) = source_bytes(&data, "image/png") else {
            panic!("the image goes as itself, encoded on the writing thread");
        };
        assert!(sent.same(&image));
        assert!(source_bytes(&data, "image/jpeg").is_none());

        let (a_read, a_write) = pipe().unwrap();
        let (b_read, b_write) = pipe().unwrap();
        let writer = std::thread::spawn(move || {
            // The second pipe first, and more than a pipe holds.
            File::from(b_write).write_all(&vec![7u8; 200_000]).unwrap();
            File::from(a_write).write_all(b"plain").unwrap();
        });
        let read = read_parts(vec![(Part::Text, a_read), (Part::Image, b_read)]);
        writer.join().unwrap();
        assert_eq!(read.text.as_deref(), Some("plain"));
        assert_eq!(read.image.unwrap().encoded_bytes().unwrap().len(), 200_000);
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
