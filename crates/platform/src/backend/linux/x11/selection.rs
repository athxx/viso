//! The CLIPBOARD selection: owning it for the app's copies — text,
//! `text/html` and `image/png`, the PNG of an image of pixels encoded when
//! first asked for — and converting it for pastes: its `TARGETS` first, then
//! the richest kind the paste takes and the text, one after another, with
//! INCR transfers both ways for data larger than one request. Pastes asked
//! for while one is converting wait their turn. A hidden utility window is
//! the owner and the requestor. It owns `XdndSelection` too while the app
//! carries a drag out, and answers for the dragged text and `text/uri-list`
//! the same way.

use x11rb::connection::{Connection, RequestConnection};
use x11rb::errors::{ConnectionError, ReplyError, ReplyOrIdError};
use x11rb::protocol::xproto::{
    self, Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux,
    EventMask, PropMode, Property, PropertyNotifyEvent, SelectionClearEvent, SelectionNotifyEvent,
    SelectionRequestEvent, Timestamp, Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;

use std::collections::VecDeque;

use super::atoms::Atoms;
use crate::clipboard::{ClipboardData, ClipboardImage, ClipboardKinds};
use crate::control::WindowId;

pub(super) struct Clipboard {
    window: Window,
    /// The text the app put on the clipboard, while it owns the selection.
    owned: Option<Owned>,
    /// What the drag the app carries out holds, while it owns
    /// `XdndSelection`.
    dragged: Option<Owned>,
    /// INCR transfers to other clients, advanced as they delete each chunk.
    outgoing: Vec<Outgoing>,
    incoming: Option<Incoming>,
    /// Pastes asked for while another converts.
    waiting: VecDeque<Paste>,
}

struct Owned {
    data: ClipboardData,
    /// A `text/uri-list`; empty for none.
    uris: String,
    time: Timestamp,
}

/// A paste asked for: of `kinds` into `window`, answering `token`.
#[derive(Clone, Copy)]
struct Paste {
    window: WindowId,
    kinds: ClipboardKinds,
    token: Option<u32>,
    time: Timestamp,
}

/// What a paste is converting the selection to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Targets,
    Text,
    Html,
    Image,
}

/// A finished paste: into `window`, what it read, answering `token`.
pub(super) type Pasted = (WindowId, ClipboardData, Option<u32>);

struct Outgoing {
    requestor: Window,
    property: Atom,
    kind: Atom,
    data: Vec<u8>,
    offset: usize,
}

/// A paste in progress.
struct Incoming {
    paste: Paste,
    step: Step,
    target: Atom,
    /// The steps left after this one.
    next: Vec<(Step, Atom)>,
    data: ClipboardData,
    /// The owner sends the data in chunks; they accumulate here.
    incr: Option<Vec<u8>>,
}

impl Clipboard {
    pub(super) fn new(conn: &XCBConnection, root: Window) -> Result<Self, ReplyOrIdError> {
        let window = conn.generate_id()?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            window,
            root,
            0,
            0,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        Ok(Self {
            window,
            owned: None,
            dragged: None,
            outgoing: Vec::new(),
            incoming: None,
            waiting: VecDeque::new(),
        })
    }

    /// Take ownership of CLIPBOARD with `data`.
    pub(super) fn set(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        data: ClipboardData,
        time: Timestamp,
    ) -> Result<(), ReplyError> {
        conn.set_selection_owner(self.window, atoms.CLIPBOARD, time)?;
        let owner = conn.get_selection_owner(atoms.CLIPBOARD)?.reply()?.owner;
        self.owned = (owner == self.window).then(|| Owned {
            data,
            uris: String::new(),
            time,
        });
        Ok(())
    }

    /// The window that owns the selections, and is a drag's source.
    pub(super) fn window(&self) -> Window {
        self.window
    }

    /// Take ownership of `XdndSelection` with a drag's `text` and `uris` (a
    /// `text/uri-list`, empty for none). Returns whether it took it.
    pub(super) fn set_drag(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        text: String,
        uris: String,
        time: Timestamp,
    ) -> Result<bool, ReplyError> {
        conn.set_selection_owner(self.window, atoms.XdndSelection, time)?;
        let owner = conn
            .get_selection_owner(atoms.XdndSelection)?
            .reply()?
            .owner;
        self.dragged = (owner == self.window).then(|| Owned {
            data: ClipboardData::text(text),
            uris,
            time,
        });
        Ok(self.dragged.is_some())
    }

    /// The types a drag the selection holds offers, the richest first.
    pub(super) fn drag_types(&self, atoms: &Atoms) -> Vec<Atom> {
        let Some(dragged) = &self.dragged else {
            return Vec::new();
        };
        let list = (!dragged.uris.is_empty()).then_some(atoms.TEXT_URI_LIST);
        let text = dragged
            .data
            .text
            .as_ref()
            .filter(|t| !t.is_empty())
            .map(|_| {
                [
                    atoms.UTF8_STRING,
                    atoms.TEXT_PLAIN_UTF8,
                    atoms.TEXT_PLAIN,
                    Atom::from(AtomEnum::STRING),
                ]
            });
        list.into_iter().chain(text.into_iter().flatten()).collect()
    }

    /// Start reading what CLIPBOARD holds of `kinds` for `window`. The
    /// app's own copy answers at once; another owner's arrives through
    /// [`on_notify`](Self::on_notify) and [`on_property`](Self::on_property)
    /// — for a request (`token`) always, for a gesture only when it holds
    /// some.
    pub(super) fn request(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        window: WindowId,
        kinds: ClipboardKinds,
        token: Option<u32>,
        time: Timestamp,
    ) -> Result<Option<ClipboardData>, ConnectionError> {
        if let Some(owned) = &self.owned {
            let data = owned.data.taken(kinds);
            return Ok((token.is_some() || !data.is_empty()).then_some(data));
        }
        let paste = Paste {
            window,
            kinds,
            token,
            time,
        };
        if self.incoming.is_some() {
            self.waiting.push_back(paste);
            return Ok(None);
        }
        self.start(conn, atoms, paste)?;
        Ok(None)
    }

    /// Begin `paste` by asking the owner for its targets.
    fn start(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        paste: Paste,
    ) -> Result<(), ConnectionError> {
        self.convert(
            conn,
            atoms,
            Incoming {
                paste,
                step: Step::Targets,
                target: atoms.TARGETS,
                next: Vec::new(),
                data: ClipboardData::default(),
                incr: None,
            },
        )
    }

    fn convert(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        incoming: Incoming,
    ) -> Result<(), ConnectionError> {
        conn.delete_property(self.window, atoms.VISO_SELECTION)?;
        conn.convert_selection(
            self.window,
            atoms.CLIPBOARD,
            incoming.target,
            atoms.VISO_SELECTION,
            incoming.paste.time,
        )?;
        self.incoming = Some(incoming);
        Ok(())
    }

    /// The step of `incoming` read `value` of type `kind` (`None`: the owner
    /// refused it): keep it and go on to the next step, or finish.
    fn stepped(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        mut incoming: Incoming,
        read: Option<(Atom, &[u8])>,
    ) -> Result<Option<Pasted>, ConnectionError> {
        match (incoming.step, read) {
            (Step::Targets, read) => {
                let targets: Vec<Atom> = read
                    .map(|(_, v)| {
                        v.as_chunks::<4>()
                            .0
                            .iter()
                            .map(|c| u32::from_ne_bytes(*c))
                            .collect()
                    })
                    .unwrap_or_default();
                let listed = |a: Atom| targets.contains(&a);
                let string = Atom::from(AtomEnum::STRING);
                // An owner that lists no targets is asked for text alone.
                let text = if targets.is_empty() || listed(atoms.UTF8_STRING) {
                    Some(atoms.UTF8_STRING)
                } else if listed(atoms.TEXT_PLAIN_UTF8) {
                    Some(atoms.TEXT_PLAIN_UTF8)
                } else if listed(string) || listed(atoms.TEXT) || listed(atoms.TEXT_PLAIN) {
                    Some(string)
                } else {
                    None
                };
                let held = ClipboardKinds {
                    text: text.is_some(),
                    html: listed(atoms.TEXT_HTML),
                    image: listed(atoms.IMAGE_PNG),
                };
                let pick = incoming.paste.kinds.pick(held);
                if pick.image {
                    incoming.next.push((Step::Image, atoms.IMAGE_PNG));
                }
                if pick.html {
                    incoming.next.push((Step::Html, atoms.TEXT_HTML));
                }
                if let Some(target) = text.filter(|_| pick.text) {
                    incoming.next.push((Step::Text, target));
                }
                incoming.next.reverse();
            }
            (Step::Text, None) if incoming.target == atoms.UTF8_STRING => {
                // No UTF-8 form: ask once more for Latin-1.
                incoming.next.push((Step::Text, AtomEnum::STRING.into()));
            }
            // Some owners answer every target with what they hold: only
            // data of the type asked for is taken.
            (Step::Text, Some((kind, value))) if is_text(atoms, kind) => {
                incoming.data.text = Some(decode(kind, value));
            }
            (Step::Html, Some((kind, value))) if kind == atoms.TEXT_HTML => {
                incoming.data.html = Some(crate::clipboard::html_text(value));
            }
            (Step::Image, Some((kind, value))) if kind == atoms.IMAGE_PNG && !value.is_empty() => {
                incoming.data.image = Some(ClipboardImage::encoded(value));
            }
            _ => {}
        }
        if let Some((step, target)) = incoming.next.pop() {
            incoming.step = step;
            incoming.target = target;
            incoming.incr = None;
            self.convert(conn, atoms, incoming)?;
            return Ok(None);
        }
        let Incoming { paste, data, .. } = incoming;
        if let Some(next) = self.waiting.pop_front() {
            self.start(conn, atoms, next)?;
        }
        Ok(
            (paste.token.is_some() || !data.is_empty()).then_some((
                paste.window,
                data,
                paste.token,
            )),
        )
    }

    /// The owner answered a conversion.
    pub(super) fn on_notify(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        e: &SelectionNotifyEvent,
    ) -> Result<Option<Pasted>, ReplyError> {
        if e.requestor != self.window || e.selection != atoms.CLIPBOARD {
            return Ok(None);
        }
        let Some(incoming) = self.incoming.take() else {
            return Ok(None);
        };
        if e.property == x11rb::NONE {
            return Ok(self.stepped(conn, atoms, incoming, None)?);
        }
        let reply = conn
            .get_property(
                true,
                self.window,
                atoms.VISO_SELECTION,
                AtomEnum::ANY,
                0,
                u32::MAX / 4,
            )?
            .reply()?;
        if reply.type_ == atoms.INCR {
            // Deleting the property (done above) asks for the first chunk.
            self.incoming = Some(Incoming {
                incr: Some(Vec::new()),
                ..incoming
            });
            return Ok(None);
        }
        Ok(self.stepped(conn, atoms, incoming, Some((reply.type_, &reply.value)))?)
    }

    /// A property changed on a window the clipboard is transferring with.
    pub(super) fn on_property(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        e: &PropertyNotifyEvent,
    ) -> Result<Option<Pasted>, ReplyError> {
        if e.state == Property::DELETE {
            self.advance_outgoing(conn, e.window, e.atom)?;
            return Ok(None);
        }
        if e.window != self.window || e.atom != atoms.VISO_SELECTION {
            return Ok(None);
        }
        let Some(incoming) = self.incoming.as_mut() else {
            return Ok(None);
        };
        let Some(buf) = incoming.incr.as_mut() else {
            return Ok(None);
        };
        let reply = conn
            .get_property(
                true,
                self.window,
                atoms.VISO_SELECTION,
                AtomEnum::ANY,
                0,
                u32::MAX / 4,
            )?
            .reply()?;
        if !reply.value.is_empty() {
            buf.extend_from_slice(&reply.value);
            return Ok(None);
        }
        let mut done = self.incoming.take().expect("checked above");
        let data = done.incr.take().unwrap_or_default();
        Ok(self.stepped(conn, atoms, done, Some((reply.type_, &data)))?)
    }

    fn advance_outgoing(
        &mut self,
        conn: &XCBConnection,
        window: Window,
        property: Atom,
    ) -> Result<(), ConnectionError> {
        let Some(i) = self
            .outgoing
            .iter()
            .position(|o| o.requestor == window && o.property == property)
        else {
            return Ok(());
        };
        let chunk = chunk_len(conn);
        let out = &mut self.outgoing[i];
        let end = (out.offset + chunk).min(out.data.len());
        conn.change_property8(
            PropMode::REPLACE,
            out.requestor,
            out.property,
            out.kind,
            &out.data[out.offset..end],
        )?;
        if out.offset == end {
            // The zero-length chunk just sent ends the transfer.
            self.outgoing.swap_remove(i);
        } else {
            out.offset = end;
        }
        Ok(())
    }

    /// Another client asked for the selection the app owns.
    pub(super) fn on_request(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        e: &SelectionRequestEvent,
    ) -> Result<(), ConnectionError> {
        // Obsolete clients leave the property unset and mean the target.
        let property = if e.property == x11rb::NONE {
            e.target
        } else {
            e.property
        };
        let which = if e.selection == atoms.CLIPBOARD {
            self.owned.as_ref()
        } else if e.selection == atoms.XdndSelection {
            self.dragged.as_ref()
        } else {
            None
        };
        let answered = match which.map(|o| (o.data.clone(), o.uris.clone(), o.time)) {
            Some(owned) => self.answer(conn, atoms, e.requestor, property, e.target, owned)?,
            None => false,
        };
        conn.send_event(
            false,
            e.requestor,
            EventMask::NO_EVENT,
            SelectionNotifyEvent {
                response_type: xproto::SELECTION_NOTIFY_EVENT,
                sequence: 0,
                time: e.time,
                requestor: e.requestor,
                selection: e.selection,
                target: e.target,
                property: if answered { property } else { x11rb::NONE },
            },
        )?;
        Ok(())
    }

    fn answer(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        requestor: Window,
        property: Atom,
        target: Atom,
        (data, uris, time): (ClipboardData, String, Timestamp),
    ) -> Result<bool, ConnectionError> {
        let string = Atom::from(AtomEnum::STRING);
        let image = data
            .image
            .as_ref()
            .filter(|i| i.rgba_pixels().is_some() || i.png().is_some());
        if target == atoms.TARGETS {
            let mut targets = vec![atoms.TARGETS, atoms.TIMESTAMP];
            if image.is_some() {
                targets.push(atoms.IMAGE_PNG);
            }
            if data.html.is_some() {
                targets.push(atoms.TEXT_HTML);
            }
            if data.text.is_some() {
                targets.extend([
                    atoms.UTF8_STRING,
                    atoms.TEXT_PLAIN_UTF8,
                    atoms.TEXT,
                    string,
                    atoms.TEXT_PLAIN,
                ]);
            }
            if !uris.is_empty() {
                targets.push(atoms.TEXT_URI_LIST);
            }
            conn.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                AtomEnum::ATOM,
                &targets,
            )?;
            return Ok(true);
        }
        if target == atoms.TIMESTAMP {
            conn.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                AtomEnum::INTEGER,
                &[time],
            )?;
            return Ok(true);
        }
        let text = data.text.as_deref();
        let (kind, data) = if target == atoms.TEXT_URI_LIST && !uris.is_empty() {
            (atoms.TEXT_URI_LIST, uris.into_bytes())
        } else if target == atoms.IMAGE_PNG
            && let Some(png) = image.and_then(ClipboardImage::png)
        {
            (atoms.IMAGE_PNG, png.to_vec())
        } else if target == atoms.TEXT_HTML
            && let Some(html) = data.html
        {
            (atoms.TEXT_HTML, html.into_bytes())
        } else if let Some(text) = text
            && (target == atoms.UTF8_STRING
                || target == atoms.TEXT_PLAIN_UTF8
                || target == atoms.TEXT)
        {
            (atoms.UTF8_STRING, text.as_bytes().to_vec())
        } else if let Some(text) = text
            && (target == string || target == atoms.TEXT_PLAIN)
        {
            (string, latin1(text))
        } else {
            return Ok(false);
        };
        if data.len() <= chunk_len(conn) {
            conn.change_property8(PropMode::REPLACE, requestor, property, kind, &data)?;
            return Ok(true);
        }
        // Too large for one request: announce the size, then send chunks
        // each time the requestor deletes the property.
        conn.change_window_attributes(
            requestor,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::PROPERTY_CHANGE),
        )?;
        let len = u32::try_from(data.len()).unwrap_or(u32::MAX);
        conn.change_property32(PropMode::REPLACE, requestor, property, atoms.INCR, &[len])?;
        self.outgoing
            .retain(|o| o.requestor != requestor || o.property != property);
        self.outgoing.push(Outgoing {
            requestor,
            property,
            kind,
            data,
            offset: 0,
        });
        Ok(true)
    }

    pub(super) fn on_clear(&mut self, atoms: &Atoms, e: &SelectionClearEvent) {
        if e.selection == atoms.CLIPBOARD && e.owner == self.window {
            self.owned = None;
        }
        if e.selection == atoms.XdndSelection && e.owner == self.window {
            self.dragged = None;
        }
    }
}

/// The largest property chunk one request carries.
fn chunk_len(conn: &XCBConnection) -> usize {
    conn.maximum_request_bytes()
        .saturating_sub(256)
        .min(1 << 20)
}

/// Whether a converted selection of type `kind` is text.
fn is_text(atoms: &Atoms, kind: Atom) -> bool {
    [
        atoms.UTF8_STRING,
        AtomEnum::STRING.into(),
        atoms.TEXT,
        atoms.TEXT_PLAIN_UTF8,
        atoms.TEXT_PLAIN,
    ]
    .contains(&kind)
}

fn decode(kind: Atom, data: &[u8]) -> String {
    if kind == Atom::from(AtomEnum::STRING) {
        data.iter().map(|&b| char::from(b)).collect()
    } else {
        String::from_utf8_lossy(data).into_owned()
    }
}

fn latin1(text: &str) -> Vec<u8> {
    text.chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin1_replaces_what_it_cannot_encode() {
        assert_eq!(latin1("café"), b"caf\xe9");
        assert_eq!(latin1("中a"), b"?a");
    }
}
