//! Drag and drop: data a source offers carried to a target that takes it.
//!
//! A source ([`NodeStore::set_drag_source`]) offers [`DragData`]: the
//! representations a target may take it as — text, a link, files, or a value
//! of a kind the app names ([`DragKind`]). A mouse pressed on it and moved
//! past the slop lifts it; a finger or a pen lifts it by holding still a
//! long press, so a list it lies in still pans under a brisk one. Lifted,
//! the data rides the pointer: no node sees the pointer's samples, the
//! source is `dragging`, and its preview ([`NodeStore::set_drag_preview`]) —
//! a layer, put away until then — shows over the source where it lies and
//! follows the pointer from where it was gripped. The innermost drop target
//! ([`NodeStore::set_drop_target`]) under the pointer that takes one of the
//! data's representations is the one it is over, `drop_over` among the
//! interaction states; each it comes over and leaves hears so
//! ([`DropPhase::Enter`], [`DropPhase::Leave`]). Let go over one, the target
//! takes the data ([`DropPhase::Drop`]) and the preview goes; let go over
//! none, or cancelled, the preview springs back over the source before it
//! goes. Either way the source hears how it ended ([`DragEnd`]).
//!
//! The keyboard carries data too: Space or Enter on a focused source its own
//! handler leaves them to lifts it over the first target that takes it, in
//! reading order; Tab and the arrows step it through them, Space or Enter
//! drops it on the one it is over and Escape puts it back.
//!
//! Data with text, a link or files leaves the app with the mouse: carried
//! past the window's edge it is handed to the OS ([`NodeStore::drag_out_asked`]),
//! the preview goes, and the drag goes on as one the OS runs, over other
//! apps' windows and the app's own alike; the source hears how it ended once
//! the OS tells ([`NodeStore::drag_out_ended`]).
//!
//! What a target and a source hear is told through their key handlers after
//! the input that decided it ([`KeyRouter::route_drops`](crate::KeyRouter::route_drops)).
//! The preview's motion is an offset past its own translate, marking only
//! `TRANSFORM`.

use core::any::Any;
use core::fmt;
use core::time::Duration;
use std::path::PathBuf;
use std::rc::Rc;

use crate::component::NodeStore;
use crate::hit_test::HitTestTree;
use crate::input::{Key, KeyEvent, PointerId};
use crate::layout::Vec2;
use crate::node::NodeId;
use crate::scroll_motion::{MAX_STEP, SPRING};

/// Within this many points of its rest, and slower than [`SETTLE_STILL`], a
/// preview's glide arrived.
const SETTLE_NEAR: f64 = 0.5;
const SETTLE_STILL: f64 = 10.0;

/// A kind of value the app carries: two values are of one kind when their
/// kinds' names are equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DragKind(u64);

impl DragKind {
    /// The kind named `name`.
    pub const fn named(name: &str) -> DragKind {
        // FNV-1a.
        let bytes = name.as_bytes();
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut i = 0;
        while i < bytes.len() {
            hash ^= bytes[i] as u64;
            hash = hash.wrapping_mul(0x0100_0000_01b3);
            i += 1;
        }
        DragKind(hash)
    }

    /// The kind of Rust type `T`.
    pub fn of<T: 'static>() -> DragKind {
        DragKind::named(core::any::type_name::<T>())
    }
}

/// One representation of what a drag carries.
#[derive(Clone)]
pub enum DragItem {
    /// Plain text.
    Text(Rc<str>),
    /// A link.
    Uri(Rc<str>),
    /// Files, by path.
    Files(Rc<[PathBuf]>),
    /// A value of the app's kind `.0`.
    Value(DragKind, Rc<dyn Any>),
}

impl fmt::Debug for DragItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DragItem::Text(text) => f.debug_tuple("Text").field(text).finish(),
            DragItem::Uri(uri) => f.debug_tuple("Uri").field(uri).finish(),
            DragItem::Files(files) => f.debug_tuple("Files").field(files).finish(),
            DragItem::Value(kind, _) => f.debug_tuple("Value").field(kind).finish(),
        }
    }
}

/// What a drag carries: the representations a target may take it as, the
/// richest first.
#[derive(Debug, Clone, Default)]
pub struct DragData {
    items: Vec<DragItem>,
}

impl DragData {
    /// Data with no representation yet.
    pub fn new() -> DragData {
        DragData::default()
    }

    /// Plain text.
    pub fn text(text: impl Into<Rc<str>>) -> DragData {
        DragData::new().with(DragItem::Text(text.into()))
    }

    /// A value of `T`'s kind ([`DragKind::of`]).
    pub fn value<T: 'static>(value: T) -> DragData {
        DragData::new().with_value(value)
    }

    /// It also offers `item`.
    pub fn with(mut self, item: DragItem) -> DragData {
        self.items.push(item);
        self
    }

    /// It also offers `value`, of `T`'s kind.
    pub fn with_value<T: 'static>(self, value: T) -> DragData {
        self.with(DragItem::Value(DragKind::of::<T>(), Rc::new(value)))
    }

    /// Its representations, the richest first.
    pub fn items(&self) -> &[DragItem] {
        &self.items
    }

    /// Its plain text, if it offers one.
    pub fn as_text(&self) -> Option<&str> {
        self.items.iter().find_map(|item| match item {
            DragItem::Text(text) => Some(&**text),
            _ => None,
        })
    }

    /// Its link, if it offers one.
    pub fn uri(&self) -> Option<&str> {
        self.items.iter().find_map(|item| match item {
            DragItem::Uri(uri) => Some(&**uri),
            _ => None,
        })
    }

    /// Its files, if it offers them.
    pub fn files(&self) -> Option<&[PathBuf]> {
        self.items.iter().find_map(|item| match item {
            DragItem::Files(files) => Some(&**files),
            _ => None,
        })
    }

    /// Its value of kind `kind`, if it offers one.
    pub fn value_of(&self, kind: DragKind) -> Option<&dyn Any> {
        self.items.iter().find_map(|item| match item {
            DragItem::Value(k, value) if *k == kind => Some(&**value),
            _ => None,
        })
    }

    /// Its value of `T`'s kind, if it offers one.
    pub fn get<T: 'static>(&self) -> Option<&T> {
        self.value_of(DragKind::of::<T>())?.downcast_ref()
    }

    /// Whether it offers a representation `accept` takes.
    /// Whether it offers anything another app takes: text, a link or files.
    pub fn leaves_the_app(&self) -> bool {
        self.items.iter().any(|item| {
            matches!(
                item,
                DragItem::Text(_) | DragItem::Uri(_) | DragItem::Files(_)
            )
        })
    }

    pub fn offers(&self, accept: DragAccept) -> bool {
        self.items.iter().any(|item| {
            matches!(
                (accept, item),
                (DragAccept::Text, DragItem::Text(_))
                    | (DragAccept::Uri, DragItem::Uri(_))
                    | (DragAccept::Files, DragItem::Files(_))
            ) || matches!((accept, item), (DragAccept::Value(a), DragItem::Value(k, _)) if a == *k)
        })
    }
}

/// A representation a drop target takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DragAccept {
    Text,
    Uri,
    Files,
    /// A value of this kind.
    Value(DragKind),
}

/// What a drop does with the data it takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DropOperation {
    /// The data leaves its source for the target.
    #[default]
    Move,
    /// The target takes a copy; the source keeps it.
    Copy,
    /// The target refers to the source's data.
    Link,
}

/// What a drop target is ([`NodeStore::set_drop_target`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DropTarget {
    /// The representations it takes; none for any.
    pub accepts: Vec<DragAccept>,
    /// What a drop on it does.
    pub operation: DropOperation,
}

impl DropTarget {
    /// Whether it takes `data`.
    pub fn takes(&self, data: &DragData) -> bool {
        self.accepts.is_empty() || self.accepts.iter().any(|&a| data.offers(a))
    }

    /// The index among its accepts of the representation it takes `data`
    /// as: the first it lists that the data offers.
    pub fn taken(&self, data: &DragData) -> Option<usize> {
        self.accepts.iter().position(|&a| data.offers(a))
    }
}

/// Which way a drag passed a drop target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropPhase {
    /// It came over the target.
    Enter,
    /// It left the target, or was put back over it.
    Leave,
    /// It was let go over the target, which takes the data.
    Drop,
}

/// What a drop target hears of a drag.
#[derive(Debug, Clone)]
pub struct DropEvent {
    pub phase: DropPhase,
    /// Where the pointer is, in window coordinates; the target's middle for
    /// the keyboard's drag.
    pub at: Vec2,
    pub data: Rc<DragData>,
    /// The index among the target's accepts of the representation it takes
    /// the data as ([`DropTarget::taken`]); `0` for a target taking any.
    pub taken: usize,
    /// That representation; `None` for a target taking any.
    pub taken_as: Option<DragAccept>,
    /// What a drop on the target does.
    pub operation: DropOperation,
}

/// How a source's drag ended: what the target it was dropped on did, `None`
/// for one put back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DragEnd {
    pub operation: Option<DropOperation>,
}

/// What a source or a target is told.
#[derive(Debug, Clone)]
pub(crate) enum DropTell {
    Target(DropEvent),
    Source(DragEnd),
}

#[derive(Debug)]
struct Source {
    node: NodeId,
    data: Rc<DragData>,
}

/// A mouse's press on a source, not yet past the slop.
#[derive(Debug, Clone, Copy)]
struct Armed {
    pointer: PointerId,
    node: NodeId,
    origin: [f32; 2],
}

#[derive(Debug)]
struct Session {
    /// The pointer carrying it; `None` for the keyboard.
    pointer: Option<PointerId>,
    source: NodeId,
    data: Rc<DragData>,
    /// Where it is, and where on the source it was gripped.
    at: Vec2,
    grip: Vec2,
    over: Option<NodeId>,
    outward: Outward,
}

/// Where a carried drag stands with the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outward {
    /// Inside the app.
    In,
    /// Carried past the window's edge, waiting to be handed to the OS.
    Asked,
    /// The OS runs it.
    Gone,
    /// The OS took none: it stays inside the app.
    Kept,
}

/// A drag from outside the app over the window.
#[derive(Debug)]
struct Arrival {
    data: Rc<DragData>,
    over: Option<NodeId>,
}

/// Where a drag from outside the app is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArrivalPhase {
    /// It came over the window or moved over it.
    Over,
    /// It left the window, or was given up.
    Leave,
    /// It was let go over the window.
    Drop,
}

/// How a shown preview moves: its offset past where it lays out, heading
/// for `to`, hidden on arrival when `hide`.
#[derive(Debug)]
struct Shown {
    node: NodeId,
    offset: Vec2,
    to: Vec2,
    v: Vec2,
    gliding: bool,
    hide: bool,
}

/// A window's drag sources, drop targets and the drag under way. Cold: a
/// store with none spends nothing on them.
#[derive(Debug, Default)]
pub(crate) struct DragDrops {
    sources: Vec<Source>,
    /// Each source's preview, by source; a source need not be one yet.
    previews: Vec<(NodeId, NodeId)>,
    targets: Vec<(NodeId, DropTarget)>,
    armed: Vec<Armed>,
    session: Option<Session>,
    arrival: Option<Arrival>,
    shown: Option<Shown>,
    told: Vec<(NodeId, DropTell)>,
}

impl DragDrops {
    pub(crate) fn clear(&mut self) {
        *self = DragDrops::default();
    }

    pub(crate) fn moving(&self) -> bool {
        self.shown.as_ref().is_some_and(|s| s.gliding)
    }

    fn source(&self, id: NodeId) -> Option<&Source> {
        self.sources.iter().find(|s| s.node == id)
    }
}

impl NodeStore {
    /// Make node `id` a drag source offering `data`, or no longer one with
    /// `None`.
    pub fn set_drag_source(&mut self, id: NodeId, data: Option<DragData>) {
        let at = self.drag_drops.sources.iter().position(|s| s.node == id);
        match (data.filter(|_| self.arena().is_live(id)), at) {
            (Some(data), Some(i)) => self.drag_drops.sources[i].data = Rc::new(data),
            (Some(data), None) => self.drag_drops.sources.push(Source {
                node: id,
                data: Rc::new(data),
            }),
            (None, Some(i)) => {
                self.drag_drops.sources.swap_remove(i);
                if self
                    .drag_drops
                    .session
                    .as_ref()
                    .is_some_and(|s| s.source == id)
                {
                    self.cancel_drag_drop();
                }
            }
            (None, None) => {}
        }
    }

    /// Whether node `id` is a drag source.
    pub fn is_drag_source(&self, id: NodeId) -> bool {
        self.drag_drops.source(id).is_some()
    }

    /// Show layer `preview` over drag source `source` while it is carried,
    /// following the pointer; `None` for none. The preview is put away until
    /// then, and its content lies at its top start corner.
    pub fn set_drag_preview(&mut self, source: NodeId, preview: Option<NodeId>) {
        let previews = &mut self.drag_drops.previews;
        let at = previews.iter().position(|&(s, _)| s == source);
        match (preview, at) {
            (Some(preview), Some(i)) => previews[i].1 = preview,
            (Some(preview), None) => previews.push((source, preview)),
            (None, Some(i)) => {
                previews.swap_remove(i);
            }
            (None, None) => {}
        }
        if let Some(preview) = preview {
            self.set_hidden(preview, true);
            self.set_hittable(preview, false);
        }
    }

    /// Make node `id` a drop target, or no longer one with `None`.
    pub fn set_drop_target(&mut self, id: NodeId, target: Option<DropTarget>) {
        let at = self.drag_drops.targets.iter().position(|(n, _)| *n == id);
        match (target.filter(|_| self.arena().is_live(id)), at) {
            (Some(target), Some(i)) => self.drag_drops.targets[i].1 = target,
            (Some(target), None) => self.drag_drops.targets.push((id, target)),
            (None, Some(i)) => {
                self.drag_drops.targets.swap_remove(i);
                if self.drag_drops.session.as_ref().and_then(|s| s.over) == Some(id) {
                    self.drag_drops.session.as_mut().unwrap().over = None;
                    self.interaction_states.moved();
                }
            }
            (None, None) => {}
        }
    }

    /// Whether node `id` is a drop target.
    pub fn is_drop_target(&self, id: NodeId) -> bool {
        self.drag_drops.targets.iter().any(|(n, _)| *n == id)
    }

    /// Whether a drag is over drop target `id`, which takes its data.
    pub fn drop_over(&self, id: NodeId) -> bool {
        self.drag_drops.session.as_ref().and_then(|s| s.over) == Some(id)
            || self.drag_drops.arrival.as_ref().and_then(|a| a.over) == Some(id)
    }

    /// A drag from outside the app carrying `data` is at `at` under `root`,
    /// in `phase`: it comes over the innermost target there that takes it,
    /// leaving the one it was over, and dropped, that target takes it.
    /// Before the drop `data` may offer its kinds with no contents, as the
    /// OS keeps them until then. Returns what a drop where it is does, none
    /// over no target.
    pub fn drag_from_outside(
        &mut self,
        root: NodeId,
        phase: ArrivalPhase,
        at: Vec2,
        data: DragData,
    ) -> Option<DropOperation> {
        let data = Rc::new(data);
        let over = match phase {
            ArrivalPhase::Leave => None,
            _ => self.target_taking(root, at, &data),
        };
        let was = match self.drag_drops.arrival.as_mut() {
            Some(a) => {
                a.data = Rc::clone(&data);
                core::mem::replace(&mut a.over, over)
            }
            None => {
                self.drag_drops.arrival = Some(Arrival {
                    data: Rc::clone(&data),
                    over,
                });
                None
            }
        };
        if was != over {
            if let Some(was) = was {
                self.tell_target(was, DropPhase::Leave, at, &data);
            }
            if let Some(over) = over {
                self.tell_target(over, DropPhase::Enter, at, &data);
            }
            self.interaction_states.moved();
        }
        let operation = over.and_then(|o| self.target_of(o)).map(|t| t.operation);
        if phase != ArrivalPhase::Over {
            self.drag_drops.arrival = None;
            if let Some(over) = over {
                self.tell_target(over, DropPhase::Drop, at, &data);
                self.interaction_states.moved();
            }
        }
        operation
    }

    fn target_of(&self, id: NodeId) -> Option<&DropTarget> {
        self.drag_drops
            .targets
            .iter()
            .find(|(n, _)| *n == id)
            .map(|(_, t)| t)
    }

    /// Whether node `id`'s data is being carried.
    pub(crate) fn drag_lifted(&self, id: NodeId) -> bool {
        self.drag_drops
            .session
            .as_ref()
            .is_some_and(|s| s.source == id)
    }

    /// Whether data is being carried.
    pub fn drag_drop_running(&self) -> bool {
        self.drag_drops.session.is_some()
    }

    /// The preview a drag shows, which hit testing passes through.
    pub(crate) fn drag_preview_shown(&self) -> Option<NodeId> {
        self.drag_drops.shown.as_ref().map(|s| s.node)
    }

    /// The offset a drag gives node `id`, its preview, past its own
    /// translate.
    #[inline]
    pub(crate) fn drag_preview_offset_of(&self, id: NodeId) -> Vec2 {
        match &self.drag_drops.shown {
            Some(s) if s.node == id => s.offset,
            _ => Vec2::ZERO,
        }
    }

    /// Pointer `pointer`, not `direct`, pressed `target` at `at`: arms a
    /// drag of the nearest source at or above it.
    pub(crate) fn arm_drag_source(
        &mut self,
        pointer: PointerId,
        target: NodeId,
        at: [f32; 2],
        direct: bool,
    ) {
        self.drag_drops.armed.retain(|a| a.pointer != pointer);
        if direct || self.drag_drops.sources.is_empty() {
            return;
        }
        let mut node = Some(target);
        while let Some(n) = node {
            if self.is_drag_source(n) {
                self.drag_drops.armed.push(Armed {
                    pointer,
                    node: n,
                    origin: at,
                });
                return;
            }
            node = self.parent(n);
        }
    }

    /// Pointer `pointer`'s sample `phase` at `at` under `root`: a drag it
    /// carries follows it or ends — a contact taken away puts it back, a
    /// mouse leaving the window does not — and a press it armed on a source
    /// lifts once past `slop`. Returns whether the drag took the sample,
    /// which no node then sees.
    pub(crate) fn steer_drag_drop(
        &mut self,
        root: NodeId,
        pointer: PointerId,
        phase: crate::input::PointerPhase,
        at: [f32; 2],
        slop: f32,
        direct: bool,
    ) -> bool {
        use crate::input::PointerPhase;
        let carried = self
            .drag_drops
            .session
            .as_ref()
            .is_some_and(|s| s.pointer == Some(pointer));
        if !carried {
            match phase {
                PointerPhase::Move => {
                    let Some(armed) = self
                        .drag_drops
                        .armed
                        .iter()
                        .find(|a| a.pointer == pointer)
                        .copied()
                    else {
                        return false;
                    };
                    let travel = (at[0] - armed.origin[0]).hypot(at[1] - armed.origin[1]);
                    if travel < slop {
                        return false;
                    }
                    self.drag_drops.armed.retain(|a| a.pointer != pointer);
                    self.lift_drag(pointer, armed.node, armed.origin);
                }
                PointerPhase::Up | PointerPhase::Leave | PointerPhase::Down => {
                    self.drag_drops.armed.retain(|a| a.pointer != pointer);
                    return false;
                }
                _ => return false,
            }
        }
        let at = Vec2::new(at[0], at[1]);
        let Some(s) = self.drag_drops.session.as_mut() else {
            return true;
        };
        if s.outward == Outward::Gone {
            // The OS has the drag; its samples are nobody's.
            return true;
        }
        if !direct
            && s.outward == Outward::In
            && phase == PointerPhase::Move
            && s.data.leaves_the_app()
        {
            let window = self.world(root);
            if !(window.x <= at.x
                && at.x < window.x + window.w
                && window.y <= at.y
                && at.y < window.y + window.h)
            {
                self.drag_drops.session.as_mut().unwrap().outward = Outward::Asked;
            }
        }
        match phase {
            PointerPhase::Move => self.carry_to(root, at),
            PointerPhase::Up => {
                self.carry_to(root, at);
                self.drop_carried();
            }
            PointerPhase::Leave if direct => self.cancel_drag_drop(),
            _ => {}
        }
        true
    }

    /// Pointer `pointer` lifts the data of source `node`, gripped at `at`:
    /// no other gesture keeps the pointer.
    pub(crate) fn lift_drag(&mut self, pointer: PointerId, node: NodeId, at: [f32; 2]) {
        self.release_hold(pointer);
        self.release_press(pointer);
        self.drop_drag(pointer);
        self.set_capture(pointer, Some(node));
        let at = Vec2::new(at[0], at[1]);
        let w = self.world(node);
        self.begin_carry(Some(pointer), node, at, at - Vec2::new(w.x, w.y));
    }

    fn begin_carry(&mut self, pointer: Option<PointerId>, node: NodeId, at: Vec2, grip: Vec2) {
        if self.drag_drops.session.is_some() {
            self.cancel_drag_drop();
        }
        let Some(source) = self.drag_drops.source(node) else {
            return;
        };
        let data = Rc::clone(&source.data);
        let preview = self
            .drag_drops
            .previews
            .iter()
            .find(|&&(s, _)| s == node)
            .map(|&(_, p)| p);
        self.drag_drops.session = Some(Session {
            pointer,
            source: node,
            data,
            at,
            grip,
            over: None,
            outward: Outward::In,
        });
        // A preview a drop left still gliding back goes at once.
        if let Some(old) = self.drag_drops.shown.take() {
            self.put_preview(&old, Vec2::ZERO);
            self.set_hidden(old.node, true);
        }
        if let Some(preview) = preview.filter(|&p| self.arena().is_live(p)) {
            self.set_hidden(preview, false);
            let place = self.preview_place(preview, at - grip);
            let shown = Shown {
                node: preview,
                offset: Vec2::ZERO,
                to: place,
                v: Vec2::ZERO,
                gliding: false,
                hide: false,
            };
            self.put_preview(&shown, place);
            self.drag_drops.shown = Some(Shown {
                offset: place,
                ..shown
            });
        }
        self.interaction_states.moved();
    }

    /// The offset that puts preview `preview`'s top start corner at `corner`.
    fn preview_place(&self, preview: NodeId, corner: Vec2) -> Vec2 {
        let w = self.world(preview);
        let own = self.drag_preview_offset_of(preview);
        corner - (Vec2::new(w.x, w.y) - own)
    }

    /// Writes `offset` into the preview `shown` names, past its own
    /// translate.
    fn put_preview(&mut self, shown: &Shown, offset: Vec2) {
        if !self.arena().is_live(shown.node) {
            return;
        }
        let own = self.translate(shown.node) - shown.offset;
        self.put_translate(shown.node, own + offset);
    }

    /// Moves the preview to offset `to`, gliding there or at once.
    fn move_preview(&mut self, to: Vec2, glide: bool, hide: bool) {
        let Some(mut shown) = self.drag_drops.shown.take() else {
            return;
        };
        shown.to = to;
        shown.hide = hide;
        if glide && !self.reduced_motion() {
            shown.gliding = true;
        } else {
            self.put_preview(&shown, to);
            shown.offset = to;
            shown.gliding = false;
            shown.v = Vec2::ZERO;
            if hide {
                self.set_hidden(shown.node, true);
                self.put_preview(&shown, Vec2::ZERO);
                return;
            }
        }
        self.drag_drops.shown = Some(shown);
    }

    /// The drag under way moves to `at` under `root`: its preview follows,
    /// and it comes over the target there.
    fn carry_to(&mut self, root: NodeId, at: Vec2) {
        let Some(s) = self.drag_drops.session.as_mut() else {
            return;
        };
        s.at = at;
        let corner = at - s.grip;
        if let Some(node) = self.drag_preview_shown() {
            let to = self.preview_place(node, corner);
            self.move_preview(to, false, false);
        }
        let over = self.target_at(root, at);
        self.come_over(over, at);
    }

    /// The innermost drop target under `at` that takes the data carried.
    fn target_at(&self, root: NodeId, at: Vec2) -> Option<NodeId> {
        let data = &self.drag_drops.session.as_ref()?.data;
        self.target_taking(root, at, data)
    }

    /// The innermost drop target under `at` that takes `data`.
    fn target_taking(&self, root: NodeId, at: Vec2, data: &DragData) -> Option<NodeId> {
        if self.drag_drops.targets.is_empty() {
            return None;
        }
        let mut node = HitTestTree::hit(self, root, at.x, at.y);
        while let Some(n) = node {
            if self.target_of(n).is_some_and(|t| t.takes(data)) && !self.inert(n) {
                return Some(n);
            }
            node = self.parent(n);
        }
        None
    }

    /// The drag comes over `over`, leaving the target it was over.
    fn come_over(&mut self, over: Option<NodeId>, at: Vec2) {
        let Some(s) = self.drag_drops.session.as_mut() else {
            return;
        };
        if s.over == over {
            return;
        }
        let was = core::mem::replace(&mut s.over, over);
        let data = Rc::clone(&s.data);
        if let Some(was) = was {
            self.tell_target(was, DropPhase::Leave, at, &data);
        }
        if let Some(over) = over {
            self.tell_target(over, DropPhase::Enter, at, &data);
        }
        self.interaction_states.moved();
    }

    fn tell_target(&mut self, target: NodeId, phase: DropPhase, at: Vec2, data: &Rc<DragData>) {
        let (taken, taken_as, operation) = self
            .drag_drops
            .targets
            .iter()
            .find(|(n, _)| *n == target)
            .map(|(_, t)| {
                let taken = t.taken(data);
                (taken.unwrap_or(0), taken.map(|i| t.accepts[i]), t.operation)
            })
            .unwrap_or_default();
        self.drag_drops.told.push((
            target,
            DropTell::Target(DropEvent {
                phase,
                at,
                data: Rc::clone(data),
                taken,
                taken_as,
                operation,
            }),
        ));
    }

    /// Lets the drag under way go: over a target, the target takes it;
    /// over none, it is put back.
    fn drop_carried(&mut self) {
        let Some(s) = self.drag_drops.session.as_ref() else {
            return;
        };
        let Some(over) = s.over else {
            self.cancel_drag_drop();
            return;
        };
        let s = self.drag_drops.session.take().unwrap();
        self.tell_target(over, DropPhase::Drop, s.at, &s.data);
        let operation = self
            .drag_drops
            .targets
            .iter()
            .find(|(n, _)| *n == over)
            .map(|(_, t)| t.operation);
        self.end_carry(&s, DragEnd { operation });
        if let Some(shown) = self.drag_drops.shown.as_ref() {
            let to = shown.offset;
            self.move_preview(to, false, true);
        }
    }

    /// The data carried past the window's edge, to hand to the OS; answer
    /// with [`NodeStore::drag_out_started`].
    pub fn drag_out_asked(&self) -> Option<Rc<DragData>> {
        let s = self.drag_drops.session.as_ref()?;
        (s.outward == Outward::Asked).then(|| Rc::clone(&s.data))
    }

    /// Whether the OS took the drag [`NodeStore::drag_out_asked`] named:
    /// taken, the target it was over hears it leave and its preview goes;
    /// not, it stays inside the app.
    pub fn drag_out_started(&mut self, started: bool) {
        let Some(s) = self
            .drag_drops
            .session
            .as_mut()
            .filter(|s| s.outward == Outward::Asked)
        else {
            return;
        };
        if !started {
            s.outward = Outward::Kept;
            return;
        }
        s.outward = Outward::Gone;
        let (over, at, data) = (s.over.take(), s.at, Rc::clone(&s.data));
        if let Some(over) = over {
            self.tell_target(over, DropPhase::Leave, at, &data);
        }
        if let Some(shown) = self.drag_drops.shown.take()
            && self.arena().is_live(shown.node)
        {
            self.put_preview(&shown, Vec2::ZERO);
            self.set_hidden(shown.node, true);
        }
        self.interaction_states.moved();
    }

    /// The drag the OS took ended, doing `operation` where it was let go,
    /// or put back (`None`): its source hears so.
    pub fn drag_out_ended(&mut self, operation: Option<DropOperation>) {
        if !self
            .drag_drops
            .session
            .as_ref()
            .is_some_and(|s| s.outward == Outward::Gone)
        {
            return;
        }
        let s = self.drag_drops.session.take().unwrap();
        self.end_carry(&s, DragEnd { operation });
    }

    /// Puts the drag under way back: the target it was over hears it leave
    /// and its preview springs back over the source before it goes.
    pub fn cancel_drag_drop(&mut self) {
        let Some(s) = self.drag_drops.session.take() else {
            return;
        };
        if let Some(over) = s.over {
            self.tell_target(over, DropPhase::Leave, s.at, &s.data);
        }
        self.end_carry(&s, DragEnd { operation: None });
        if let Some(node) = self.drag_preview_shown() {
            let home = self.world(s.source);
            let to = self.preview_place(node, Vec2::new(home.x, home.y));
            self.move_preview(to, true, true);
        }
    }

    fn end_carry(&mut self, s: &Session, end: DragEnd) {
        if let Some(pointer) = s.pointer
            && self.capture_of(pointer) == Some(s.source)
        {
            self.set_capture(pointer, None);
        }
        if self.arena().is_live(s.source) {
            self.drag_drops.told.push((s.source, DropTell::Source(end)));
        }
        self.interaction_states.moved();
    }

    /// A finger's or a pen's hold on node `node` came due at `at`: one on a
    /// source lifts its data. Returns whether it did.
    pub(crate) fn hold_lifts(&mut self, pointer: PointerId, node: NodeId, at: [f32; 2]) -> bool {
        if !self.is_drag_source(node) {
            return false;
        }
        self.lift_drag(pointer, node, at);
        true
    }

    /// Whether any node takes a finger's hold as a lift.
    pub(crate) fn drag_drops_armable(&self) -> bool {
        !self.drag_drops.sources.is_empty()
    }

    /// Whether node `id` takes a finger's hold as a lift.
    pub(crate) fn holds_lift(&self, id: NodeId) -> bool {
        !self.drag_drops.sources.is_empty() && self.is_drag_source(id)
    }

    /// The drop targets that take the data carried, in reading order.
    fn targets_taking(&self) -> Vec<NodeId> {
        let Some(s) = self.drag_drops.session.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<NodeId> = self
            .drag_drops
            .targets
            .iter()
            .filter(|(n, t)| {
                *n != s.source && t.takes(&s.data) && !self.inert(*n) && !self.hidden(*n)
            })
            .map(|(n, _)| *n)
            .collect();
        out.sort_by(|&a, &b| {
            let (a, b) = (self.world(a), self.world(b));
            (a.y, a.x)
                .partial_cmp(&(b.y, b.x))
                .unwrap_or(core::cmp::Ordering::Equal)
        });
        out
    }

    /// The keyboard's drag comes over target `over`, its preview gliding
    /// there; over none it sits over its source.
    fn key_over(&mut self, over: Option<NodeId>) {
        let Some(s) = self.drag_drops.session.as_ref() else {
            return;
        };
        let source = self.world(s.source);
        let grip = s.grip;
        let at = match over {
            Some(t) => {
                let w = self.world(t);
                Vec2::new(w.x + w.w / 2.0, w.y + w.h / 2.0)
            }
            None => Vec2::new(source.x, source.y) + grip,
        };
        if let Some(s) = self.drag_drops.session.as_mut() {
            s.at = at;
        }
        if let Some(node) = self.drag_preview_shown() {
            let to = self.preview_place(node, at - grip);
            self.move_preview(to, true, false);
        }
        self.come_over(over, at);
    }

    /// The keyboard's drag: Space or Enter on a focused source lifts it, the
    /// arrows and Tab step it through the targets that take it, Space or
    /// Enter drops it and Escape puts it back. Returns whether it took `ev`.
    pub(crate) fn drag_key(&mut self, ev: &KeyEvent) -> bool {
        if !ev.pressed || (self.drag_drops.sources.is_empty() && self.drag_drops.session.is_none())
        {
            return false;
        }
        let keyboard = self
            .drag_drops
            .session
            .as_ref()
            .is_some_and(|s| s.pointer.is_none());
        if !keyboard {
            if self.drag_drops.session.is_some() {
                // A pointer's drag: Escape puts it back.
                if ev.key == Key::Escape {
                    self.cancel_drag_drop();
                    return true;
                }
                return false;
            }
            let Some(focused) = self.focused().filter(|&f| self.is_drag_source(f)) else {
                return false;
            };
            if ev.repeat || !matches!(ev.key, Key::Space | Key::Enter) {
                return false;
            }
            let w = self.world(focused);
            let grip = Vec2::new(w.w / 2.0, w.h / 2.0);
            self.begin_carry(None, focused, Vec2::new(w.x, w.y) + grip, grip);
            let first = self.targets_taking().first().copied();
            self.key_over(first);
            return true;
        }
        match ev.key {
            Key::Escape => self.cancel_drag_drop(),
            Key::Space | Key::Enter if !ev.repeat => self.drop_carried(),
            Key::Tab | Key::Up | Key::Down | Key::Left | Key::Right => {
                let back = matches!(ev.key, Key::Up | Key::Left)
                    || (ev.key == Key::Tab && ev.modifiers.shift);
                let order = self.targets_taking();
                if order.is_empty() {
                    return true;
                }
                let over = self.drag_drops.session.as_ref().and_then(|s| s.over);
                let at = over.and_then(|o| order.iter().position(|&n| n == o));
                let next = match (at, back) {
                    (None, false) => 0,
                    (None, true) => order.len() - 1,
                    (Some(i), false) => (i + 1) % order.len(),
                    (Some(i), true) => (i + order.len() - 1) % order.len(),
                };
                self.key_over(Some(order[next]));
            }
            _ => {}
        }
        true
    }

    /// The focus moved to `to`: a keyboard's drag of another node is put
    /// back.
    pub(crate) fn drag_focus(&mut self, to: Option<NodeId>) {
        if self
            .drag_drops
            .session
            .as_ref()
            .is_some_and(|s| s.pointer.is_none() && Some(s.source) != to)
        {
            self.cancel_drag_drop();
        }
    }

    /// Whether what a drag decided waits to be told.
    pub fn drops_due(&self) -> bool {
        !self.drag_drops.told.is_empty()
    }

    /// Moves what a drag decided into `out`.
    pub(crate) fn take_drop_tells(&mut self, out: &mut Vec<(NodeId, DropTell)>) {
        out.append(&mut self.drag_drops.told);
    }

    /// Advances a preview's glide by `delta`.
    pub(crate) fn tick_drag_drops(&mut self, delta: Duration) {
        let Some(mut shown) = self.drag_drops.shown.take() else {
            return;
        };
        if !shown.gliding {
            self.drag_drops.shown = Some(shown);
            return;
        }
        let dt = delta.as_secs_f64().min(MAX_STEP);
        let decay = (-SPRING * dt).exp();
        // A critically damped spring about its rest, on each axis:
        // e(t) = (e + (v + λe)t)·e^(−λt).
        let step = |e: f32, v: f32| {
            let (e, v) = (f64::from(e), f64::from(v));
            let c = v + SPRING * e;
            ((e + c * dt) * decay, (v - SPRING * c * dt) * decay)
        };
        let (ex, vx) = step(shown.offset.x - shown.to.x, shown.v.x);
        let (ey, vy) = step(shown.offset.y - shown.to.y, shown.v.y);
        let arrived = ex.hypot(ey) < SETTLE_NEAR && vx.hypot(vy) < SETTLE_STILL;
        let next = if arrived {
            shown.to
        } else {
            shown.to + Vec2::new(ex as f32, ey as f32)
        };
        self.put_preview(&shown, next);
        shown.offset = next;
        shown.v = if arrived {
            Vec2::ZERO
        } else {
            Vec2::new(vx as f32, vy as f32)
        };
        if arrived {
            shown.gliding = false;
            if shown.hide {
                self.put_preview(&shown, Vec2::ZERO);
                self.set_hidden(shown.node, true);
                return;
            }
        }
        self.drag_drops.shown = Some(shown);
    }

    /// Forgets the sources and targets that left the tree; a drag whose
    /// source left is put back.
    pub(crate) fn fit_drag_drops(&mut self) {
        if self.drag_drops.sources.is_empty()
            && self.drag_drops.targets.is_empty()
            && self.drag_drops.previews.is_empty()
        {
            return;
        }
        let mut sources = core::mem::take(&mut self.drag_drops.sources);
        sources.retain(|s| self.arena().is_live(s.node));
        self.drag_drops.sources = sources;
        let mut previews = core::mem::take(&mut self.drag_drops.previews);
        previews.retain(|&(s, p)| self.arena().is_live(s) && self.arena().is_live(p));
        self.drag_drops.previews = previews;
        let mut targets = core::mem::take(&mut self.drag_drops.targets);
        targets.retain(|(n, _)| self.arena().is_live(*n));
        self.drag_drops.targets = targets;
        if let Some(s) = self.drag_drops.session.as_ref() {
            if !self.arena().is_live(s.source) {
                self.drag_drops.session = None;
                if let Some(shown) = self.drag_drops.shown.take()
                    && self.arena().is_live(shown.node)
                {
                    self.put_preview(&shown, Vec2::ZERO);
                    self.set_hidden(shown.node, true);
                }
                self.interaction_states.moved();
            } else if s.over.is_some_and(|o| !self.arena().is_live(o)) {
                self.drag_drops.session.as_mut().unwrap().over = None;
            }
        }
        let over = self.drag_drops.arrival.as_ref().and_then(|a| a.over);
        if over.is_some_and(|o| !self.arena().is_live(o)) {
            self.drag_drops.arrival.as_mut().unwrap().over = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::component::{BuildCx, FlexStyle, LeafStyle, StackStyle};
    use crate::input::{Modifiers, PointerPhase};
    use crate::layout::{Axis, Sizing};
    use crate::style::BoxStyle;
    use viso_render::Rect;

    #[derive(Debug, PartialEq)]
    struct Card(u32);

    /// A 300×300 column: a 100×50 source carrying `Card(7)`, a 300×100
    /// target taking cards, a 300×100 one taking links, and the source's
    /// preview, a layer holding a 100×50 leaf.
    fn scene() -> (NodeStore, NodeId, [NodeId; 4]) {
        let mut store = NodeStore::new();
        let mut parts = Vec::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Sizing::fixed(300.0, 300.0),
                    ..FlexStyle::default()
                },
                |cx| {
                    for (w, h) in [(100.0, 50.0), (300.0, 100.0), (300.0, 100.0)] {
                        let leaf = cx.leaf(LeafStyle {
                            size: Sizing::fixed(w, h),
                            style: BoxStyle::NONE,
                        });
                        parts.push(leaf.id());
                    }
                    let preview = cx.stack(StackStyle::default(), |cx| {
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(100.0, 50.0),
                            style: BoxStyle::NONE,
                        });
                    });
                    parts.push(preview.id());
                },
            )
            .id()
        };
        let [source, cards, links, preview] = parts[..] else {
            unreachable!()
        };
        store.set_layer(preview, true);
        store.set_drag_source(
            source,
            Some(DragData::value(Card(7)).with(DragItem::Text("seven".into()))),
        );
        store.set_drag_preview(source, Some(preview));
        store.set_drop_target(
            cards,
            Some(DropTarget {
                accepts: vec![DragAccept::Value(DragKind::of::<Card>())],
                operation: DropOperation::Move,
            }),
        );
        store.set_drop_target(
            links,
            Some(DropTarget {
                accepts: vec![DragAccept::Uri],
                operation: DropOperation::Copy,
            }),
        );
        store.layout(root, Rect::new(0.0, 0.0, 300.0, 300.0), &mut Vec::new());
        (store, root, [source, cards, links, preview])
    }

    fn told(store: &mut NodeStore) -> Vec<(NodeId, DropTell)> {
        let mut out = Vec::new();
        store.take_drop_tells(&mut out);
        out
    }

    #[test]
    fn a_mouse_carries_the_data_past_the_slop_and_the_target_takes_it() {
        let (mut store, root, [source, cards, links, _]) = scene();
        let p = PointerId::MOUSE;
        store.arm_drag_source(p, source, [10.0, 10.0], false);
        assert!(!store.steer_drag_drop(root, p, PointerPhase::Move, [11.0, 11.0], 4.0, false));
        assert!(store.steer_drag_drop(root, p, PointerPhase::Move, [20.0, 100.0], 4.0, false));
        assert!(store.drag_lifted(source) && store.drop_over(cards));
        assert!(store.steer_drag_drop(root, p, PointerPhase::Move, [20.0, 200.0], 4.0, false));
        assert!(!store.drop_over(links), "it takes no card");
        store.steer_drag_drop(root, p, PointerPhase::Move, [20.0, 120.0], 4.0, false);
        store.steer_drag_drop(root, p, PointerPhase::Up, [20.0, 120.0], 4.0, false);
        assert!(!store.drag_drop_running());
        let tells = told(&mut store);
        let phases: Vec<_> = tells
            .iter()
            .map(|(n, t)| match t {
                DropTell::Target(e) => (*n, Some(e.phase), None),
                DropTell::Source(end) => (*n, None, end.operation),
            })
            .collect();
        assert_eq!(
            phases,
            [
                (cards, Some(DropPhase::Enter), None),
                (cards, Some(DropPhase::Leave), None),
                (cards, Some(DropPhase::Enter), None),
                (cards, Some(DropPhase::Drop), None),
                (source, None, Some(DropOperation::Move)),
            ]
        );
        let DropTell::Target(drop) = &tells[3].1 else {
            panic!("a drop");
        };
        assert_eq!(drop.data.get::<Card>(), Some(&Card(7)));
        assert_eq!(drop.data.as_text(), Some("seven"));
    }

    #[test]
    fn let_go_over_nothing_it_is_put_back_and_the_preview_glides_home() {
        let (mut store, root, [source, _, _, preview]) = scene();
        let p = PointerId::MOUSE;
        store.arm_drag_source(p, source, [10.0, 10.0], false);
        store.steer_drag_drop(root, p, PointerPhase::Move, [60.0, 280.0], 4.0, false);
        assert!(!store.hidden(preview));
        assert_eq!(
            store.drag_preview_offset_of(preview),
            Vec2::new(50.0, 270.0)
        );
        store.steer_drag_drop(root, p, PointerPhase::Up, [60.0, 280.0], 4.0, false);
        assert!(matches!(
            &told(&mut store)[..],
            [(n, DropTell::Source(DragEnd { operation: None }))] if *n == source
        ));
        assert!(store.drag_drops.moving(), "it glides back");
        for _ in 0..120 {
            store.tick_drag_drops(Duration::from_millis(16));
        }
        assert!(store.hidden(preview) && store.drag_preview_shown().is_none());
        assert_eq!(store.translate(preview), Vec2::ZERO);
    }

    #[test]
    fn carried_past_the_window_s_edge_text_goes_on_as_the_os_s_drag() {
        let (mut store, root, [source, cards, _, preview]) = scene();
        let p = PointerId::MOUSE;
        store.arm_drag_source(p, source, [10.0, 10.0], false);
        store.steer_drag_drop(root, p, PointerPhase::Move, [60.0, 100.0], 4.0, false);
        assert!(store.drop_over(cards));
        assert!(store.drag_out_asked().is_none(), "inside the window");
        told(&mut store);
        store.steer_drag_drop(root, p, PointerPhase::Move, [320.0, 100.0], 4.0, false);
        let data = store.drag_out_asked().expect("past the edge");
        assert_eq!(data.as_text(), Some("seven"));
        store.drag_out_started(true);
        assert!(matches!(
            &told(&mut store)[..],
            [(n, DropTell::Target(e))] if *n == cards && e.phase == DropPhase::Leave
        ));
        assert!(store.hidden(preview) && store.drag_out_asked().is_none());
        // The OS has the pointer: its samples reach nothing.
        store.steer_drag_drop(root, p, PointerPhase::Move, [60.0, 100.0], 4.0, false);
        assert!(!store.drop_over(cards) && told(&mut store).is_empty());
        store.drag_out_ended(Some(DropOperation::Copy));
        assert!(matches!(
            &told(&mut store)[..],
            [(n, DropTell::Source(DragEnd { operation: Some(DropOperation::Copy) }))]
                if *n == source
        ));
        assert!(!store.drag_drop_running());

        // Where the OS takes none, it stays inside.
        store.arm_drag_source(p, source, [10.0, 10.0], false);
        store.steer_drag_drop(root, p, PointerPhase::Move, [320.0, 100.0], 4.0, false);
        store.drag_out_started(false);
        store.steer_drag_drop(root, p, PointerPhase::Move, [60.0, 100.0], 4.0, false);
        assert!(store.drop_over(cards) && store.drag_out_asked().is_none());
    }

    #[test]
    fn the_keyboard_steps_through_the_targets_that_take_it() {
        let (mut store, _root, [source, cards, _, _]) = scene();
        store.set_focusable(source, true);
        store.set_focused(Some(source));
        let key = |store: &mut NodeStore, key: Key| {
            store.drag_key(&KeyEvent {
                key,
                pressed: true,
                repeat: false,
                modifiers: Modifiers::default(),
            })
        };
        assert!(key(&mut store, Key::Space));
        assert!(store.drop_over(cards), "the one target that takes a card");
        assert!(key(&mut store, Key::Tab));
        assert!(store.drop_over(cards), "it wraps to it");
        assert!(key(&mut store, Key::Enter));
        let tells = told(&mut store);
        assert!(matches!(
            tells.last(),
            Some((n, DropTell::Source(DragEnd { operation: Some(DropOperation::Move) }))) if *n == source
        ));
        assert!(!key(&mut store, Key::Down), "nothing carried, no key taken");
    }
}
