//! XDND (version 5), as a drop target: a drag from another client over one
//! of the app's windows reaches the app step by step, each `XdndPosition`
//! answered by an `XdndStatus` once the app has heard it, and the drop by an
//! `XdndFinished` once the app has taken what the selection holds.

use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::{
    self, Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, Property,
    PropertyNotifyEvent, SelectionNotifyEvent,
};
use x11rb::xcb_ffi::XCBConnection;

use super::atoms::Atoms;
use crate::control::WindowId;
use crate::drop::{DropAction, DropActions, DropKinds, DropOffer, DropPhase, DropReply, RawDrop};

/// The protocol version the app's windows speak.
pub(super) const VERSION: u32 = 5;

/// The drag over one of the app's windows.
pub(super) struct Visit {
    source: xproto::Window,
    target: xproto::Window,
    window: WindowId,
    version: u32,
    types: Vec<Atom>,
    /// The action the source asks for.
    asked: Atom,
    offer: DropOffer,
    reply: DropReply,
    /// Where it is, in logical points.
    at: (f64, f64),
    /// Let go: the selection's text being converted, in chunks for INCR.
    dropped: Option<Option<Vec<u8>>>,
}

#[derive(Default)]
pub(super) struct Xdnd {
    visit: Option<Visit>,
    /// The target, source and asked action of a drop the app is taking,
    /// which the visit no longer holds once its data is read.
    finishing: Option<(xproto::Window, xproto::Window, Atom)>,
}

/// What a client message of the protocol asks of the app.
pub(super) enum Step {
    /// Hand the app this step of the drag.
    Tell(RawDrop),
    Nothing,
}

impl Xdnd {
    /// An `XdndEnter`, `XdndPosition`, `XdndLeave` or `XdndDrop` to window
    /// `target`, the app's window `window` at `scale`; `None` for a message
    /// of another protocol.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn on_message(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        root: xproto::Window,
        e: &ClientMessageEvent,
        window: WindowId,
        scale: f64,
    ) -> Option<Step> {
        let data = e.data.as_data32();
        let source = data[0];
        let ours = |v: &Option<Visit>| v.as_ref().is_some_and(|v| v.source == source);
        if e.type_ == atoms.XdndEnter {
            let version = data[1] >> 24;
            if version > VERSION {
                return Some(Step::Nothing);
            }
            let types = if data[1] & 1 != 0 {
                type_list(conn, atoms, source).unwrap_or_default()
            } else {
                data[2..5]
                    .iter()
                    .copied()
                    .filter(|&t| t != x11rb::NONE)
                    .collect()
            };
            let kinds = kinds_of(atoms, &types);
            self.visit = Some(Visit {
                source,
                target: e.window,
                window,
                version,
                types,
                asked: atoms.XdndActionCopy,
                offer: DropOffer {
                    kinds,
                    ..DropOffer::default()
                },
                reply: DropReply::new(),
                at: (0.0, 0.0),
                dropped: None,
            });
            return Some(Step::Nothing);
        }
        if e.type_ == atoms.XdndPosition {
            if !ours(&self.visit) {
                return Some(Step::Nothing);
            }
            let (x, y) = ((data[2] >> 16) as i16, (data[2] & 0xffff) as i16);
            let local = conn
                .translate_coordinates(root, e.window, x, y)
                .ok()
                .and_then(|c| c.reply().ok())
                .map_or((0.0, 0.0), |r| (f64::from(r.dst_x), f64::from(r.dst_y)));
            let visit = self.visit.as_mut()?;
            if visit.version >= 2 && data[4] != x11rb::NONE {
                visit.asked = data[4];
            }
            visit.at = (local.0 / scale, local.1 / scale);
            return Some(Step::Tell(visit.step(DropPhase::Over)));
        }
        if e.type_ == atoms.XdndLeave {
            if !ours(&self.visit) {
                return Some(Step::Nothing);
            }
            let visit = self.visit.take()?;
            return Some(Step::Tell(visit.step(DropPhase::Leave)));
        }
        if e.type_ == atoms.XdndDrop {
            if !ours(&self.visit) {
                return Some(Step::Nothing);
            }
            let time = if data[2] != 0 {
                data[2]
            } else {
                x11rb::CURRENT_TIME
            };
            let visit = self.visit.as_mut()?;
            let target = [
                atoms.TEXT_URI_LIST,
                atoms.UTF8_STRING,
                atoms.TEXT_PLAIN_UTF8,
            ]
            .into_iter()
            .chain([atoms.TEXT_PLAIN, AtomEnum::STRING.into()])
            .find(|t| visit.types.contains(t));
            let Some(target) = target else {
                let visit = self.visit.take()?;
                self.finishing = Some((visit.target, visit.source, visit.asked));
                return Some(Step::Tell(visit.step(DropPhase::Drop)));
            };
            visit.dropped = Some(None);
            let _ = conn.delete_property(visit.target, atoms.VISO_XDND);
            let _ = conn.convert_selection(
                visit.target,
                atoms.XdndSelection,
                target,
                atoms.VISO_XDND,
                time,
            );
            let _ = conn.flush();
            return Some(Step::Nothing);
        }
        None
    }

    /// The source answered the drop's conversion: the app takes the drop.
    pub(super) fn on_notify(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        e: &SelectionNotifyEvent,
    ) -> Option<Result<Option<RawDrop>, ReplyError>> {
        let visit = self.visit.as_ref()?;
        if e.selection != atoms.XdndSelection || e.requestor != visit.target {
            return None;
        }
        Some(self.read(conn, atoms, e.property == x11rb::NONE, false))
    }

    /// A property changed on the window an INCR transfer comes to.
    pub(super) fn on_property(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        e: &PropertyNotifyEvent,
    ) -> Option<Result<Option<RawDrop>, ReplyError>> {
        let visit = self.visit.as_ref()?;
        let incr = matches!(visit.dropped, Some(Some(_)));
        if !incr
            || e.window != visit.target
            || e.atom != atoms.VISO_XDND
            || e.state != Property::NEW_VALUE
        {
            return None;
        }
        Some(self.read(conn, atoms, false, true))
    }

    /// Reads what the source put in the property; `refused` when it
    /// converted nothing. The drop goes to the app once all of it is read.
    fn read(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        refused: bool,
        chunk: bool,
    ) -> Result<Option<RawDrop>, ReplyError> {
        let Some(visit) = self.visit.as_mut() else {
            return Ok(None);
        };
        let mut done = refused;
        if !refused {
            let reply = conn
                .get_property(
                    true,
                    visit.target,
                    atoms.VISO_XDND,
                    AtomEnum::ANY,
                    0,
                    u32::MAX / 4,
                )?
                .reply()?;
            if !chunk && reply.type_ == atoms.INCR {
                // Deleting the property (done above) asks for the first chunk.
                visit.dropped = Some(Some(Vec::new()));
                return Ok(None);
            }
            let incr = matches!(visit.dropped, Some(Some(_)));
            let buf = visit
                .dropped
                .get_or_insert(None)
                .get_or_insert_with(Vec::new);
            buf.extend_from_slice(&reply.value);
            // A chunk of nothing ends an INCR transfer; a plain answer is
            // whole.
            done = !incr || reply.value.is_empty();
            if done {
                let data = std::mem::take(buf);
                let text = String::from_utf8_lossy(&data).into_owned();
                if reply.type_ == atoms.TEXT_URI_LIST {
                    let (files, uri) = crate::drop::uri_list(&text);
                    visit.offer.text = uri.clone();
                    visit.offer.uri = uri;
                    visit.offer.files = files;
                } else {
                    visit.offer.text = Some(text);
                }
            }
        }
        if !done {
            return Ok(None);
        }
        let mut visit = self.visit.take().expect("checked above");
        let offer = &mut visit.offer;
        offer.kinds = DropKinds {
            text: offer.text.is_some(),
            uri: offer.uri.is_some(),
            files: !offer.files.is_empty(),
        };
        self.finishing = Some((visit.target, visit.source, visit.asked));
        Ok(Some(visit.step(DropPhase::Drop)))
    }

    /// The app heard step `phase` of the drag over window `target` and said
    /// `reply`: the source hears so, as an `XdndStatus` for a move and an
    /// `XdndFinished` for the drop.
    pub(super) fn answer(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        target: xproto::Window,
        phase: DropPhase,
        reply: &DropReply,
    ) {
        let found = match phase {
            DropPhase::Over => self
                .visit
                .as_ref()
                .filter(|v| v.target == target)
                .map(|v| (v.source, v.asked)),
            DropPhase::Drop => self
                .finishing
                .take()
                .filter(|f| f.0 == target)
                .map(|f| (f.1, f.2)),
            DropPhase::Leave => None,
        };
        let Some((source, asked)) = found else {
            return;
        };
        let action = actions_for(atoms, asked).settle(reply.get());
        let action_atom = action.map_or(x11rb::NONE, |a| action_atom(atoms, a));
        let (kind, data) = if phase == DropPhase::Over {
            (
                atoms.XdndStatus,
                // Accepted, and every move wanted, whether or not it is in
                // the same rectangle.
                [target, u32::from(action.is_some()) | 2, 0, 0, action_atom],
            )
        } else {
            (
                atoms.XdndFinished,
                [target, u32::from(action.is_some()), action_atom, 0, 0],
            )
        };
        let message = ClientMessageEvent::new(32, source, kind, data);
        let _ = conn.send_event(false, source, EventMask::NO_EVENT, message);
    }
}

impl Visit {
    fn step(&self, phase: DropPhase) -> RawDrop {
        RawDrop {
            window: self.window,
            phase,
            x: self.at.0,
            y: self.at.1,
            offer: self.offer.clone(),
            reply: self.reply.clone(),
        }
    }
}

/// The types a source with more than three lists in its `XdndTypeList`.
fn type_list(conn: &XCBConnection, atoms: &Atoms, source: xproto::Window) -> Option<Vec<Atom>> {
    let reply = conn
        .get_property(false, source, atoms.XdndTypeList, AtomEnum::ATOM, 0, 1024)
        .ok()?
        .reply()
        .ok()?;
    Some(reply.value32()?.collect())
}

/// The kinds a source offering `types` offers: a `text/uri-list` may hold
/// files, links, or both.
fn kinds_of(atoms: &Atoms, types: &[Atom]) -> DropKinds {
    let has = |t: Atom| types.contains(&t);
    let list = has(atoms.TEXT_URI_LIST);
    DropKinds {
        text: has(atoms.UTF8_STRING)
            || has(atoms.TEXT_PLAIN_UTF8)
            || has(atoms.TEXT_PLAIN)
            || has(AtomEnum::STRING.into()),
        uri: list,
        files: list,
    }
}

/// The actions a source asking for `asked` allows: what it asks, and a copy
/// besides, which every source takes.
fn actions_for(atoms: &Atoms, asked: Atom) -> DropActions {
    DropActions {
        moves: asked == atoms.XdndActionMove,
        copies: true,
        links: asked == atoms.XdndActionLink,
    }
}

fn action_atom(atoms: &Atoms, action: DropAction) -> Atom {
    match action {
        DropAction::Move => atoms.XdndActionMove,
        DropAction::Copy => atoms.XdndActionCopy,
        DropAction::Link => atoms.XdndActionLink,
    }
}
