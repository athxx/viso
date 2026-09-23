//! The CLIPBOARD selection: owning it for the app's copies, converting it
//! for pastes, with INCR transfers both ways for text larger than one
//! request. A hidden utility window is the owner and the requestor.

use x11rb::connection::{Connection, RequestConnection};
use x11rb::errors::{ConnectionError, ReplyError, ReplyOrIdError};
use x11rb::protocol::xproto::{
    self, Atom, AtomEnum, ChangeWindowAttributesAux, ConnectionExt as _, CreateWindowAux,
    EventMask, PropMode, Property, PropertyNotifyEvent, SelectionClearEvent, SelectionNotifyEvent,
    SelectionRequestEvent, Timestamp, Window, WindowClass,
};
use x11rb::wrapper::ConnectionExt as _;
use x11rb::xcb_ffi::XCBConnection;

use super::atoms::Atoms;
use crate::control::WindowId;

pub(super) struct Clipboard {
    window: Window,
    /// The text the app put on the clipboard, while it owns the selection.
    owned: Option<Owned>,
    /// INCR transfers to other clients, advanced as they delete each chunk.
    outgoing: Vec<Outgoing>,
    incoming: Option<Incoming>,
}

struct Owned {
    text: String,
    time: Timestamp,
}

struct Outgoing {
    requestor: Window,
    property: Atom,
    kind: Atom,
    data: Vec<u8>,
    offset: usize,
}

/// A paste in progress.
struct Incoming {
    window: WindowId,
    target: Atom,
    /// The owner sends the text in chunks; they accumulate here.
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
            outgoing: Vec::new(),
            incoming: None,
        })
    }

    /// Take ownership of CLIPBOARD with `text`.
    pub(super) fn set(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        text: &str,
        time: Timestamp,
    ) -> Result<(), ReplyError> {
        conn.set_selection_owner(self.window, atoms.CLIPBOARD, time)?;
        let owner = conn.get_selection_owner(atoms.CLIPBOARD)?.reply()?.owner;
        self.owned = (owner == self.window).then(|| Owned {
            text: text.to_owned(),
            time,
        });
        Ok(())
    }

    /// Start reading CLIPBOARD for `window`. The app's own copy answers at
    /// once; another owner's text arrives through [`on_notify`](Self::on_notify).
    pub(super) fn request(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        window: WindowId,
        time: Timestamp,
    ) -> Result<Option<String>, ConnectionError> {
        if let Some(owned) = &self.owned {
            return Ok(Some(owned.text.clone()));
        }
        self.convert(conn, atoms, window, atoms.UTF8_STRING, time)?;
        Ok(None)
    }

    fn convert(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        window: WindowId,
        target: Atom,
        time: Timestamp,
    ) -> Result<(), ConnectionError> {
        conn.delete_property(self.window, atoms.VISO_SELECTION)?;
        conn.convert_selection(
            self.window,
            atoms.CLIPBOARD,
            target,
            atoms.VISO_SELECTION,
            time,
        )?;
        self.incoming = Some(Incoming {
            window,
            target,
            incr: None,
        });
        Ok(())
    }

    /// The owner answered a conversion.
    pub(super) fn on_notify(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        e: &SelectionNotifyEvent,
    ) -> Result<Option<(WindowId, String)>, ReplyError> {
        if e.requestor != self.window || e.selection != atoms.CLIPBOARD {
            return Ok(None);
        }
        let Some(incoming) = self.incoming.take() else {
            return Ok(None);
        };
        if e.property == x11rb::NONE {
            // No UTF-8 form: ask once more for Latin-1.
            if incoming.target == atoms.UTF8_STRING {
                self.convert(
                    conn,
                    atoms,
                    incoming.window,
                    AtomEnum::STRING.into(),
                    e.time,
                )?;
            }
            return Ok(None);
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
        Ok(Some((incoming.window, decode(reply.type_, &reply.value))))
    }

    /// A property changed on a window the clipboard is transferring with.
    pub(super) fn on_property(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        e: &PropertyNotifyEvent,
    ) -> Result<Option<(WindowId, String)>, ReplyError> {
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
        let done = self.incoming.take().expect("checked above");
        let data = done.incr.unwrap_or_default();
        Ok(Some((done.window, decode(reply.type_, &data))))
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
        let answered = e.selection == atoms.CLIPBOARD
            && self.answer(conn, atoms, e.requestor, property, e.target)?;
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
    ) -> Result<bool, ConnectionError> {
        let Some(owned) = &self.owned else {
            return Ok(false);
        };
        let string = Atom::from(AtomEnum::STRING);
        if target == atoms.TARGETS {
            conn.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                AtomEnum::ATOM,
                &[
                    atoms.TARGETS,
                    atoms.TIMESTAMP,
                    atoms.UTF8_STRING,
                    atoms.TEXT_PLAIN_UTF8,
                    atoms.TEXT,
                    string,
                    atoms.TEXT_PLAIN,
                ],
            )?;
            return Ok(true);
        }
        if target == atoms.TIMESTAMP {
            conn.change_property32(
                PropMode::REPLACE,
                requestor,
                property,
                AtomEnum::INTEGER,
                &[owned.time],
            )?;
            return Ok(true);
        }
        let (kind, data) = if target == atoms.UTF8_STRING
            || target == atoms.TEXT_PLAIN_UTF8
            || target == atoms.TEXT
        {
            (atoms.UTF8_STRING, owned.text.as_bytes().to_vec())
        } else if target == string || target == atoms.TEXT_PLAIN {
            (string, latin1(&owned.text))
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
    }
}

/// The largest property chunk one request carries.
fn chunk_len(conn: &XCBConnection) -> usize {
    conn.maximum_request_bytes()
        .saturating_sub(256)
        .min(1 << 20)
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
