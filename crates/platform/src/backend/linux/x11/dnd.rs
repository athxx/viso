//! XDND (version 5), as a drop target: a drag from another client over one
//! of the app's windows reaches the app step by step, each `XdndPosition`
//! answered by an `XdndStatus` once the app has heard it, and the drop by an
//! `XdndFinished` once the app has taken what the selection holds.
//!
//! And as a source, for the drags the app carries out of a window: the
//! selection window owns `XdndSelection` and speaks for the drag, the
//! pointer's moves find the aware window under it, each told `XdndPosition`
//! once it answered the last, and let go over one that took it the drag
//! ends with that window's `XdndFinished`.

use x11rb::connection::Connection;
use x11rb::errors::ReplyError;
use x11rb::protocol::xproto::{
    self, Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, EventMask, Property,
    PropertyNotifyEvent, SelectionNotifyEvent,
};
use x11rb::wrapper::ConnectionExt as _;
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
    outbound: Option<Outbound>,
}

/// A drag the app carries out of window `window`.
struct Outbound {
    window: WindowId,
    /// The window that owns `XdndSelection` and is named as the source.
    source: xproto::Window,
    types: Vec<Atom>,
    /// The action asked of each target.
    asked: Atom,
    allowed: DropActions,
    target: Option<Target>,
    /// Let go over a target that took it, waiting for its `XdndFinished`.
    dropped: bool,
}

/// The aware window a drag the app carries out is over.
struct Target {
    window: xproto::Window,
    version: u32,
    /// The action its last `XdndStatus` took; `None` while it takes none.
    accepted: Option<Atom>,
    /// A position told and not yet answered.
    waiting: bool,
    /// The latest position and its time, held until the last is answered.
    pending: Option<(u32, u32)>,
}

/// What a step of a drag the app carries out came to.
pub(super) enum Outcome {
    /// Not a step of one.
    Ignored,
    Going,
    /// It ended, doing this.
    Ended(Option<DropAction>),
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

impl Xdnd {
    /// Whether the app carries a drag out.
    pub(super) fn carrying(&self) -> bool {
        self.outbound.is_some()
    }

    /// Starts carrying a drag out of `window`, offering `types`, which the
    /// selection window `source` owns `XdndSelection` for.
    pub(super) fn start(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        window: WindowId,
        source: xproto::Window,
        types: Vec<Atom>,
        allowed: DropActions,
    ) {
        let _ = conn.change_property32(
            xproto::PropMode::REPLACE,
            source,
            atoms.XdndTypeList,
            AtomEnum::ATOM,
            &types,
        );
        let asked = [
            (allowed.copies, atoms.XdndActionCopy),
            (allowed.moves, atoms.XdndActionMove),
            (allowed.links, atoms.XdndActionLink),
        ]
        .into_iter()
        .find_map(|(yes, a)| yes.then_some(a))
        .unwrap_or(atoms.XdndActionCopy);
        self.outbound = Some(Outbound {
            window,
            source,
            types,
            asked,
            allowed,
            target: None,
            dropped: false,
        });
    }

    /// The window the drag carried out started from.
    pub(super) fn carried_from(&self) -> Option<WindowId> {
        self.outbound.as_ref().map(|o| o.window)
    }

    /// The pointer carrying the drag out moved to (`x`, `y`) on the root
    /// window at `time`.
    pub(super) fn carry(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        root: xproto::Window,
        (x, y): (i16, i16),
        time: u32,
    ) {
        let Some(out) = self.outbound.as_mut().filter(|o| !o.dropped) else {
            return;
        };
        let under = aware_under(conn, atoms, root, x, y);
        if out.target.as_ref().map(|t| t.window) != under.map(|u| u.0) {
            if let Some(old) = out.target.take() {
                tell(conn, old.window, atoms.XdndLeave, [out.source, 0, 0, 0, 0]);
            }
            if let Some((window, version)) = under {
                let version = version.min(VERSION);
                let more = u32::from(out.types.len() > 3);
                let mut data = [out.source, version << 24 | more, 0, 0, 0];
                for (slot, &t) in data[2..].iter_mut().zip(&out.types) {
                    *slot = t;
                }
                tell(conn, window, atoms.XdndEnter, data);
                out.target = Some(Target {
                    window,
                    version,
                    accepted: None,
                    waiting: false,
                    pending: None,
                });
            }
        }
        let Some(target) = out.target.as_mut() else {
            return;
        };
        let at = (u32::from(x as u16) << 16) | u32::from(y as u16);
        if target.waiting {
            target.pending = Some((at, time));
            return;
        }
        let asked = if target.version >= 2 { out.asked } else { 0 };
        tell(
            conn,
            target.window,
            atoms.XdndPosition,
            [out.source, 0, at, time, asked],
        );
        target.waiting = true;
        let _ = conn.flush();
    }

    /// The button carrying the drag out was let go at `time`: over a
    /// target that took it, it is dropped there; anywhere else it ends.
    pub(super) fn let_go(&mut self, conn: &XCBConnection, atoms: &Atoms, time: u32) -> Outcome {
        let Some(out) = self.outbound.as_mut().filter(|o| !o.dropped) else {
            return Outcome::Ignored;
        };
        match &out.target {
            Some(t) if t.accepted.is_some() => {
                tell(conn, t.window, atoms.XdndDrop, [out.source, 0, time, 0, 0]);
                out.dropped = true;
                let _ = conn.flush();
                Outcome::Going
            }
            _ => self.give_up(conn, atoms),
        }
    }

    /// The drag carried out is put back: the target it was over hears it
    /// leave.
    pub(super) fn give_up(&mut self, conn: &XCBConnection, atoms: &Atoms) -> Outcome {
        let Some(out) = self.outbound.take() else {
            return Outcome::Ignored;
        };
        if let Some(t) = &out.target {
            tell(conn, t.window, atoms.XdndLeave, [out.source, 0, 0, 0, 0]);
            let _ = conn.flush();
        }
        Outcome::Ended(None)
    }

    /// An `XdndStatus` or `XdndFinished` to the selection window, from the
    /// target of the drag the app carries out.
    pub(super) fn on_source_message(
        &mut self,
        conn: &XCBConnection,
        atoms: &Atoms,
        e: &ClientMessageEvent,
    ) -> Outcome {
        let data = e.data.as_data32();
        let Some(out) = self.outbound.as_mut() else {
            return Outcome::Ignored;
        };
        let Some(target) = out.target.as_mut().filter(|t| t.window == data[0]) else {
            return Outcome::Ignored;
        };
        if e.type_ == atoms.XdndStatus {
            target.accepted = (data[1] & 1 != 0).then(|| {
                if target.version >= 2 && data[4] != x11rb::NONE {
                    data[4]
                } else {
                    atoms.XdndActionCopy
                }
            });
            target.waiting = false;
            if let Some((at, time)) = target.pending.take() {
                let asked = if target.version >= 2 { out.asked } else { 0 };
                tell(
                    conn,
                    target.window,
                    atoms.XdndPosition,
                    [out.source, 0, at, time, asked],
                );
                target.waiting = true;
                let _ = conn.flush();
            }
            return Outcome::Going;
        }
        if e.type_ == atoms.XdndFinished && out.dropped {
            let done = if target.version >= 5 {
                (data[1] & 1 != 0).then_some(data[2])
            } else {
                target.accepted
            };
            let allowed = out.allowed;
            self.outbound = None;
            let action = done
                .and_then(|a| action_of(atoms, a))
                .filter(|&a| allowed.allows(a))
                .or_else(|| done.map(|_| DropAction::Copy));
            return Outcome::Ended(action);
        }
        Outcome::Ignored
    }
}

/// Sends client message `kind` with `data` to `window`.
fn tell(conn: &XCBConnection, window: xproto::Window, kind: Atom, data: [u32; 5]) {
    let message = ClientMessageEvent::new(32, window, kind, data);
    let _ = conn.send_event(false, window, EventMask::NO_EVENT, message);
}

/// The innermost window under (`x`, `y`) on `root` that speaks XDND, and
/// its version.
fn aware_under(
    conn: &XCBConnection,
    atoms: &Atoms,
    root: xproto::Window,
    x: i16,
    y: i16,
) -> Option<(xproto::Window, u32)> {
    let mut at = root;
    loop {
        let child = conn
            .translate_coordinates(root, at, x, y)
            .ok()?
            .reply()
            .ok()?
            .child;
        if child == x11rb::NONE {
            return None;
        }
        let version = conn
            .get_property(false, child, atoms.XdndAware, AtomEnum::ATOM, 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32()?.next());
        if let Some(version) = version {
            // A frame's client is the aware window; nothing inside it is.
            return Some((child, version));
        }
        at = child;
    }
}

/// The action named by `atom`.
fn action_of(atoms: &Atoms, atom: Atom) -> Option<DropAction> {
    [
        (atoms.XdndActionCopy, DropAction::Copy),
        (atoms.XdndActionMove, DropAction::Move),
        (atoms.XdndActionLink, DropAction::Link),
    ]
    .into_iter()
    .find(|&(a, _)| a == atom)
    .map(|(_, action)| action)
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
