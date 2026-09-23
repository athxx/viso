//! The clipboard over `wl_data_device`: copies offer a data source the
//! compositor asks to write out, pastes read the current selection's offer
//! through a pipe on a background thread. Drag-and-drop offers are declined.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsFd, FromRawFd, OwnedFd};
use std::sync::Mutex;

use wayland_client::protocol::{
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::WlDataDeviceManager,
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
    wl_seat::WlSeat,
};
use wayland_client::{
    Connection, Dispatch, Proxy, QueueHandle, delegate_noop, event_created_child,
};

use super::State;
use crate::backend::linux::{Wake, Waker};
use crate::control::WindowId;

/// Text types in the order a paste prefers them; a copy offers them all.
const TEXT_MIMES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "TEXT",
    "STRING",
];

/// The types an offer advertised, filled by its `offer` events.
type OfferMimes = Mutex<Vec<String>>;

pub(super) struct Clipboard {
    manager: Option<WlDataDeviceManager>,
    device: Option<WlDataDevice>,
    /// The current selection, when another client owns it.
    selection: Option<WlDataOffer>,
    /// The drag hovering one of our surfaces.
    drag: Option<WlDataOffer>,
    /// Our own selection and its text, while the compositor holds it.
    own: Option<(WlDataSource, String)>,
}

impl Clipboard {
    pub(super) fn new(manager: Option<WlDataDeviceManager>) -> Self {
        Self {
            manager,
            device: None,
            selection: None,
            drag: None,
            own: None,
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
        if let Some(offer) = self.drag.take() {
            offer.destroy();
        }
        if let Some(device) = self.device.take()
            && device.version() >= 2
        {
            device.release();
        }
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
            .and_then(|m| pick_mime(&m.lock().ok()?))?;
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
            wl_data_device::Event::Enter { id, .. } => {
                if let Some(old) = std::mem::replace(&mut clipboard.drag, id) {
                    old.destroy();
                }
            }
            wl_data_device::Event::Leave | wl_data_device::Event::Drop => {
                if let Some(old) = clipboard.drag.take() {
                    old.destroy();
                }
            }
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
        if let wl_data_offer::Event::Offer { mime_type } = event
            && let Ok(mut mimes) = mimes.lock()
        {
            mimes.push(mime_type);
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
