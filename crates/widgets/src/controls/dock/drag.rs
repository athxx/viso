//! Pointer and keyboard dragging for a dock's seams.
//!
//! Section 2 wires the resize seams: each seam bar authored by [`build`](super::build)
//! gets a pointer handler and a key handler that drive the seam's `fraction` cell,
//! reusing the reference splitter's delta-from-anchor drag model (the three
//! `Float` cells `fraction`/`down_pos`/`start_frac`, a `capture_pointer` on press,
//! a delta-over-extent move, a `release_pointer` on release, and a keyboard step).
//! The handlers only ever *write the cell* — they hold no node geometry, because an
//! [`EventCx`](viso_ui::EventCx) has no node bounds. Turning the committed fraction
//! into live pane geometry, and applying the minimum-pane floor against the
//! resolved parent extent, is the store-mutating [`reconcile`](super::reconcile)
//! step's job (the handler-writes-intent / reconcile-mutates-store split the
//! virtual list uses).
//!
//! The panel drag-to-redock half lives alongside it (section 4): [`wire_panel_drag`]
//! attaches a pointer handler to each tab leaf that, on a primary drag, resolves the
//! pointer against the warm drop-zone rectangles the build walk recorded, toggles a
//! pre-authored drop-hint overlay over the zone the pointer is over, and on release
//! pushes a [`RedockIntent`] onto the shared queue the reconcile step drains. Like
//! the seam handler it holds no node store — it reads warm rects, toggles a node's
//! visibility through the deferred `set_hidden`, and writes an intent; the tree edit
//! and the node remount are the reconcile step's job.

use viso_ui::{
    Axis, BuildCx, EventCx, Key, PointerButtons, PointerPhase, Rect, StateId, StateValue,
};

use super::build::{RedockIntents, SeamRec, SharedZones};
use super::tree::{DropPart, PanelKey};

/// The fraction step one arrow-key press moves a seam — the keyboard equivalent of
/// a drag (AGENTS section 15), matching the reference splitter's `KEY_STEP_FRACTION`
/// so a dock seam and a standalone splitter resize alike from the keyboard.
pub(super) const KEY_STEP_FRACTION: f32 = 0.02;

/// The extent, in logical pixels, a seam drag assumes for its container when the
/// live parent extent is not yet known at the moment of a raw pointer delta. The
/// handler converts a pixel delta to a fraction delta with this nominal extent and
/// writes the raw fraction; the reconcile step reclamps the committed fraction
/// against the *actual* resolved extent and the minimum-pane floor, so this value
/// only scales the drag's pixel sensitivity, never the final geometry. Matches the
/// reference splitter's default extent so the feel is identical.
const NOMINAL_EXTENT: f32 = 600.0;

/// Read a `Float` state cell through an [`EventCx`], defaulting to `0.0` for a stale
/// handle or a non-float value (neither happens in normal use — the seam cells are
/// authored as `Float` and live as long as the seam). The reference splitter's
/// `read_f32` helper, local to the dock so the two drag models stay independent.
fn read_f32(ev: &EventCx<'_>, cell: StateId) -> f32 {
    match ev.get(cell) {
        Some(StateValue::Float(v)) => v,
        _ => 0.0,
    }
}

/// The pointer sample's coordinate on the seam's main axis.
fn main_axis_pos(axis: Axis, x: f32, y: f32) -> f32 {
    match axis {
        Axis::Row => x,
        Axis::Column => y,
    }
}

/// Attach the pointer and key drag handlers to a seam's bar node, driving the
/// seam's `fraction` cell. Called once per seam during the build walk, after the
/// seam record is known.
///
/// The pointer handler mirrors the reference splitter: a primary press records the
/// press anchor (the main-axis position and the fraction at that instant) and
/// captures the pointer to the bar so subsequent samples route here even off the
/// bar; each move adds the pixel delta over the nominal extent to the recorded
/// fraction and writes the (0..1-clamped) result; the release frees the capture.
/// The key handler steps the fraction by [`KEY_STEP_FRACTION`] on the arrow keys.
/// Both only write the cell — the reconcile step reads it, applies the minimum-pane
/// floor against the real extent, and rewrites the pane weights.
pub(super) fn wire_seam(cx: &mut BuildCx<'_>, bar: viso_ui::Handle, seam: &SeamRec) {
    let axis = seam.axis;
    let fraction = seam.fraction;
    let down_pos = seam.down_pos;
    let start_frac = seam.start_frac;
    let bar_id = bar.id();

    cx.on_pointer(bar, move |ev| {
        let Some(p) = ev.pointer() else { return };
        // A primary drag, or a release of one; other samples are not seam input.
        if !p.buttons.contains(PointerButtons::PRIMARY) && p.phase != PointerPhase::Up {
            return;
        }
        match p.phase {
            PointerPhase::Down => {
                ev.set(down_pos, StateValue::Float(main_axis_pos(axis, p.x, p.y)));
                ev.set(start_frac, StateValue::Float(read_f32(ev, fraction)));
                ev.capture_pointer(bar_id);
            }
            PointerPhase::Move => {
                let dpos = main_axis_pos(axis, p.x, p.y) - read_f32(ev, down_pos);
                let delta = dpos / NOMINAL_EXTENT;
                // Write the raw fraction clamped to the open range; the reconcile
                // step reclamps it to the minimum-pane floor once the real extent
                // is resolved (the handler has no node geometry to floor against).
                let next = (read_f32(ev, start_frac) + delta).clamp(0.0, 1.0);
                ev.set(fraction, StateValue::Float(next));
            }
            PointerPhase::Up => {
                ev.release_pointer();
            }
            // Hover enter/leave are not drag input; the no-button early return
            // above already skips them, but the match stays exhaustive.
            PointerPhase::Enter | PointerPhase::Leave => {}
        }
    });

    cx.on_key(bar, move |ev| {
        let Some(k) = ev.key() else { return };
        if !k.pressed {
            return;
        }
        // For a row split Left/Right are the natural axis and for a column split
        // Up/Down are; both pairs are accepted so either orientation resizes from
        // the keyboard (the reference splitter's convention).
        let dir = match k.key {
            Key::Right | Key::Down => 1.0,
            Key::Left | Key::Up => -1.0,
            _ => return,
        };
        let next = (read_f32(ev, fraction) + dir * KEY_STEP_FRACTION).clamp(0.0, 1.0);
        ev.set(fraction, StateValue::Float(next));
    });
}

/// The fraction of a drop zone's shorter edge that forms an edge band. A pointer
/// within this fraction of the target's left/right/top/bottom edge resolves to the
/// matching split; anywhere more central resolves to a center (tab) drop. Matches
/// the reference dock's edge-band proportion so the drop feel is identical.
const DROP_EDGE_BAND: f32 = 0.25;

/// A committed redock: what a panel drop asks the reconcile step to do to the tree.
/// A panel-drag handler cannot edit the tree itself (a tree edit needs
/// `&mut NodeStore`, which an [`EventCx`](viso_ui::EventCx) does not hold), so on
/// drop it pushes one of these onto the shared [`RedockIntents`] queue and the
/// reconcile step drains it — the same handler-writes-intent / reconcile-mutates-
/// store split the seam drag uses for its `fraction` cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RedockIntent {
    /// Redock `key` relative to `target` per `part` (an edge splits, a center/tab
    /// joins the target's group). The reconcile step edits the tree, re-authorizes
    /// the changed subtree, and remounts `key`'s existing panel node.
    Dock {
        /// The dragged panel.
        key: PanelKey,
        /// The panel whose zone the pointer was over at drop.
        target: PanelKey,
        /// Where in the target's zone the pointer dropped.
        part: DropPart,
    },
}

/// Resolve a pointer at `(x, y)` against a drop zone rectangle into a [`DropPart`]:
/// the nearest edge band splits, and the central region joins the target's tab
/// group. Returns `None` when the pointer is outside the rectangle. The edge bands
/// are proportional (a fraction of the zone's shorter side, [`DROP_EDGE_BAND`]) so a
/// small and a large zone read alike; ties between an over-corner's two edges resolve
/// to whichever band the pointer is proportionally deepest into.
fn resolve_part(rect: Rect, x: f32, y: f32) -> Option<DropPart> {
    if x < rect.x || x > rect.x + rect.w || y < rect.y || y > rect.y + rect.h {
        return None;
    }
    // Proportional depth into each edge (0 at the edge, 1 at the far side); the band
    // is the same fraction of the shorter side on both axes so the bands stay square.
    let left = (x - rect.x) / rect.w;
    let right = (rect.x + rect.w - x) / rect.w;
    let top = (y - rect.y) / rect.h;
    let bottom = (rect.y + rect.h - y) / rect.h;
    let nearest = left.min(right).min(top).min(bottom);
    if nearest >= DROP_EDGE_BAND {
        // Central region: join the target's tab group.
        return Some(DropPart::Center);
    }
    // Whichever edge the pointer is proportionally closest to.
    Some(if nearest == left {
        DropPart::Left
    } else if nearest == right {
        DropPart::Right
    } else if nearest == top {
        DropPart::Top
    } else {
        DropPart::Bottom
    })
}

/// The panel-drag half of the drag subsystem: attach a pointer handler to a tab leaf
/// that drags its panel to a new dock. Called once per tab during the build walk,
/// after the shared drag channel (the drop-hint node, the intent queue, the zone
/// registry) exists.
///
/// The handler mirrors the seam handler's guard/capture-release lifecycle but drives
/// a *structural* drop instead of a scalar fraction: a primary press captures the
/// pointer to the tab so subsequent samples route here; each move resolves the
/// pointer against the warm drop-zone rectangles (an `EventCx` cannot hit-test
/// arbitrary nodes, so it reads the rects the reconcile step refreshed into `zones`),
/// and toggles the pre-authored drop-hint overlay visible over the resolved zone (a
/// deferred `set_hidden`, the only visibility face a handler has); the release hides
/// the hint, frees the capture, and — if the pointer was over a zone — pushes a
/// [`RedockIntent::Dock`] onto `intents` for the reconcile step to apply. The handler
/// never touches the tree or any node geometry; the tree edit and the node remount
/// are the reconcile step's job.
pub(super) fn wire_panel_drag(
    cx: &mut BuildCx<'_>,
    tab: viso_ui::Handle,
    key: PanelKey,
    hint: viso_ui::NodeId,
    intents: &RedockIntents,
    zones: &SharedZones,
) {
    let tab_id = tab.id();
    let intents = intents.clone();
    let zones = zones.clone();

    cx.on_pointer(tab, move |ev| {
        // Copy the sample's fields up front: the Up arm calls `set_hidden` (a mutable
        // borrow of `ev`) and then reads the pointer coords, so it cannot hold the
        // `ev.pointer()` borrow across that call.
        let (phase, buttons, px, py) = match ev.pointer() {
            Some(p) => (p.phase, p.buttons, p.x, p.y),
            None => return,
        };
        // A primary drag, or a release of one; other samples are not drag input.
        if !buttons.contains(PointerButtons::PRIMARY) && phase != PointerPhase::Up {
            return;
        }
        match phase {
            PointerPhase::Down => {
                // Capture so a drag that leaves the tab keeps routing here; the hint
                // stays hidden until the first move resolves a zone.
                ev.capture_pointer(tab_id);
            }
            PointerPhase::Move => {
                // Resolve the pointer against the warm zones. Show the hint over the
                // resolved zone (the reconcile step positions the hint node); hide it
                // when the pointer is over no zone or over the dragged panel's own.
                let over = resolve_zone(&zones.borrow(), key, px, py);
                ev.set_hidden(hint, over.is_none());
            }
            PointerPhase::Up => {
                ev.set_hidden(hint, true);
                if let Some((target, part)) = resolve_zone(&zones.borrow(), key, px, py) {
                    intents
                        .borrow_mut()
                        .push(RedockIntent::Dock { key, target, part });
                }
                ev.release_pointer();
            }
            // Hover enter/leave are not drag input; the no-button early return above
            // already skips them, but the match stays exhaustive.
            PointerPhase::Enter | PointerPhase::Leave => {}
        }
    });
}

/// Resolve a pointer at `(x, y)` against the warm zone registry into the target panel
/// and drop part it is over, skipping the dragged panel's own zone (a panel does not
/// redock onto itself). The first zone the pointer hits wins — zones do not overlap
/// (each covers a distinct tab-group region), so order does not matter beyond the
/// self-skip.
fn resolve_zone(
    zones: &[super::build::ZoneRec],
    dragged: PanelKey,
    x: f32,
    y: f32,
) -> Option<(PanelKey, DropPart)> {
    for zone in zones {
        if zone.target == dragged {
            continue;
        }
        if let Some(part) = resolve_part(zone.rect, x, y) {
            return Some((zone.target, part));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use viso_ui::{
        BindingTable, BoxStyle, BuildCx, Key, KeyEvent, LeafStyle, Modifiers, NodeId, NodeStore,
        PointerEvent, SemanticProjector, Size, StateStore, TextEdits, VirtualLists,
    };

    /// The reactive stores a seam's handlers are wired into, held together so a test
    /// can build a lone seam bar and then drive its pointer/key handlers against the
    /// same state — the seam half of the drag subsystem in isolation, without the
    /// full dock build walk (which the reconcile module's harness already exercises).
    struct Reactive {
        store: NodeStore,
        states: StateStore,
        bindings: BindingTable,
        lists: VirtualLists,
        text_edits: TextEdits,
        projectors: SemanticProjector,
    }

    /// A wired seam: the bar node its handlers hang on, and the three drag cells they
    /// write, so a test can drive the bar and read the fraction back.
    struct WiredSeam {
        bar: NodeId,
        fraction: StateId,
    }

    impl Reactive {
        fn new() -> Self {
            Reactive {
                store: NodeStore::new(),
                states: StateStore::new(),
                bindings: BindingTable::new(),
                lists: VirtualLists::new(),
                text_edits: TextEdits::new(),
                projectors: SemanticProjector::new(),
            }
        }

        /// Author a bare seam bar leaf and its three drag cells through a reactive
        /// build cx, wire the seam's handlers onto the bar, and return the bar node
        /// plus the fraction cell. `frac0` is the seam's initial fraction.
        fn wire(&mut self, axis: Axis, frac0: f32) -> WiredSeam {
            let mut cx = BuildCx::with_reactive(
                &mut self.store,
                &mut self.states,
                &mut self.bindings,
                &mut self.lists,
                &mut self.text_edits,
                &mut self.projectors,
            );
            let fraction = cx.state(StateValue::Float(frac0));
            let down_pos = cx.state(StateValue::Float(0.0));
            let start_frac = cx.state(StateValue::Float(frac0));
            let bar = cx.leaf(LeafStyle {
                size: Size::fixed(
                    super::super::build::SEAM_SIZE,
                    super::super::build::SEAM_SIZE,
                ),
                style: BoxStyle::NONE,
            });
            // A minimal seam record: the drag handlers read only the axis and the
            // three cells (never the pane/container ids), so placeholder node ids are
            // fine for exercising the handler in isolation.
            let seam = SeamRec {
                container: bar.id(),
                pane_a: bar.id(),
                pane_b: bar.id(),
                axis,
                fraction,
                down_pos,
                start_frac,
            };
            let bar_id = bar.id();
            wire_seam(&mut cx, bar, &seam);
            WiredSeam {
                bar: bar_id,
                fraction,
            }
        }

        /// Feed a pointer sample to the bar's pointer handler, restoring it after, and
        /// return any pending capture request the handler made.
        fn pointer(&mut self, bar: NodeId, ev: PointerEvent) -> Option<Option<NodeId>> {
            let mut handler = self.store.take_handler(bar).expect("pointer handler");
            let capture = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
                cx.__take_capture_request()
            };
            self.store.restore_handler(bar, handler);
            capture
        }

        /// Feed a key sample to the bar's key handler, restoring it after.
        fn key(&mut self, bar: NodeId, ev: KeyEvent) {
            let mut handler = self.store.take_key_handler(bar).expect("key handler");
            {
                let mut cx = EventCx::__new_key(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
            }
            self.store.restore_key_handler(bar, handler);
        }

        /// The current value of a seam's `fraction` cell.
        fn read(&mut self, cell: StateId) -> f32 {
            let ev = still();
            let cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
            match cx.get(cell) {
                Some(StateValue::Float(v)) => v,
                _ => f32::NAN,
            }
        }
    }

    /// A still (no-button) pointer sample, for read-only state peeks.
    fn still() -> PointerEvent {
        PointerEvent {
            x: 0.0,
            y: 0.0,
            phase: PointerPhase::Move,
            buttons: PointerButtons::NONE,
            modifiers: Modifiers::default(),
        }
    }

    /// A primary-button pointer sample at `(x, y)` in the given phase.
    fn primary_at(x: f32, y: f32, phase: PointerPhase) -> PointerEvent {
        PointerEvent {
            x,
            y,
            phase,
            buttons: PointerButtons::PRIMARY,
            modifiers: Modifiers::default(),
        }
    }

    /// A key press/release sample.
    fn key_ev(key: Key, pressed: bool) -> KeyEvent {
        KeyEvent {
            key,
            pressed,
            repeat: false,
            modifiers: Modifiers::default(),
        }
    }

    /// Test 3 — the seam drag tape: a primary press on the bar records the anchor and
    /// captures the pointer to the bar; a move shifts the fraction by the pixel delta
    /// over the nominal extent; a release frees the capture. The handler is
    /// floor-agnostic (it clamps only to `[0, 1]`), so the move lands at the raw
    /// delta — the minimum-pane floor is the reconcile step's job, proven separately.
    #[test]
    fn seam_drag_anchors_moves_and_releases() {
        let mut rx = Reactive::new();
        let seam = rx.wire(Axis::Row, 0.5);

        assert!((rx.read(seam.fraction) - 0.5).abs() < 1e-6, "starts at 0.5");

        // Press at x=300: records the anchor and requests capture to the bar.
        let cap = rx.pointer(seam.bar, primary_at(300.0, 0.0, PointerPhase::Down));
        assert_eq!(cap, Some(Some(seam.bar)), "the press captures to the bar");
        assert!(
            (rx.read(seam.fraction) - 0.5).abs() < 1e-6,
            "the press alone does not move the fraction"
        );

        // Move right by +NOMINAL_EXTENT * 0.2 px over the nominal extent: +0.2 -> 0.7.
        let dx = NOMINAL_EXTENT * 0.2;
        rx.pointer(seam.bar, primary_at(300.0 + dx, 0.0, PointerPhase::Move));
        assert!(
            (rx.read(seam.fraction) - 0.7).abs() < 1e-6,
            "the move shifts the fraction by dpx/NOMINAL_EXTENT: got {}",
            rx.read(seam.fraction)
        );

        // Move far past the end: the handler clamps the raw fraction to 1.0 (the
        // reconcile step, not the handler, applies the min-pane floor).
        rx.pointer(
            seam.bar,
            primary_at(300.0 + 10.0 * dx, 0.0, PointerPhase::Move),
        );
        assert!(
            (rx.read(seam.fraction) - 1.0).abs() < 1e-6,
            "the handler clamps to the open range, deferring the floor to reconcile"
        );

        // Release frees the capture.
        let cap = rx.pointer(seam.bar, primary_at(0.0, 0.0, PointerPhase::Up));
        assert_eq!(cap, Some(None), "the release frees the capture");
    }

    /// A column seam drags on the y axis: the same pixel delta on y moves the
    /// fraction and the x jump is ignored.
    #[test]
    fn column_seam_drags_on_the_y_axis() {
        let mut rx = Reactive::new();
        let seam = rx.wire(Axis::Column, 0.5);

        rx.pointer(seam.bar, primary_at(999.0, 200.0, PointerPhase::Down));
        let dy = NOMINAL_EXTENT * 0.2;
        rx.pointer(seam.bar, primary_at(0.0, 200.0 + dy, PointerPhase::Move));
        assert!(
            (rx.read(seam.fraction) - 0.7).abs() < 1e-6,
            "a column seam reads the y delta and ignores x"
        );
    }

    /// Arrow keys step the fraction by `KEY_STEP_FRACTION`: Right/Down increment,
    /// Left/Up decrement; a key-up and an unrelated key do nothing.
    #[test]
    fn arrow_keys_step_the_fraction() {
        let mut rx = Reactive::new();
        let seam = rx.wire(Axis::Row, 0.5);

        rx.key(seam.bar, key_ev(Key::Right, true));
        assert!(
            (rx.read(seam.fraction) - (0.5 + KEY_STEP_FRACTION)).abs() < 1e-6,
            "Right steps up one step"
        );
        rx.key(seam.bar, key_ev(Key::Down, true));
        assert!(
            (rx.read(seam.fraction) - (0.5 + 2.0 * KEY_STEP_FRACTION)).abs() < 1e-6,
            "Down steps up too"
        );
        rx.key(seam.bar, key_ev(Key::Left, true));
        rx.key(seam.bar, key_ev(Key::Up, true));
        assert!(
            (rx.read(seam.fraction) - 0.5).abs() < 1e-6,
            "Left and Up step back down"
        );

        // A key-up and an unrelated key leave the fraction alone.
        rx.key(seam.bar, key_ev(Key::Right, false));
        rx.key(seam.bar, key_ev(Key::Enter, true));
        assert!(
            (rx.read(seam.fraction) - 0.5).abs() < 1e-6,
            "key-up and Enter do not step"
        );
    }

    /// A non-primary press neither anchors nor captures, and a no-button move does
    /// not drag — only a primary drag (or the release of one) is seam input.
    #[test]
    fn non_primary_pointer_does_not_drag() {
        let mut rx = Reactive::new();
        let seam = rx.wire(Axis::Row, 0.5);

        let non_primary_down = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary_at(300.0, 0.0, PointerPhase::Down)
        };
        let cap = rx.pointer(seam.bar, non_primary_down);
        assert_eq!(cap, None, "a non-primary press does not capture");
        let non_primary_move = PointerEvent {
            buttons: PointerButtons::NONE,
            ..primary_at(999.0, 0.0, PointerPhase::Move)
        };
        rx.pointer(seam.bar, non_primary_move);
        assert!(
            (rx.read(seam.fraction) - 0.5).abs() < 1e-6,
            "a no-button move does not drag"
        );
    }

    /// The outcome of feeding one pointer sample to a panel-drag handler: any capture
    /// request it made, and every deferred visibility request it queued. Test 4 asserts
    /// against both — the hint toggles as the pointer enters and leaves a zone.
    struct DragStep {
        capture: Option<Option<NodeId>>,
        hidden: Vec<(NodeId, bool)>,
    }

    /// A wired panel drag: the tab node its handler hangs on, the drop-hint node it
    /// toggles, and the intent queue it pushes onto. The handler owns its own clones of
    /// the zone registry and the intent queue, so the test reads back through `intents`
    /// while the seeded zones live inside the handler's clone.
    struct WiredPanel {
        tab: NodeId,
        hint: NodeId,
        intents: RedockIntents,
    }

    impl Reactive {
        /// Author a bare tab leaf and a drop-hint leaf, seed the shared zone registry
        /// with one target zone, wire the panel-drag handler onto the tab, and return
        /// the handle bundle. `dragged` is the key the tab drags; `target`/`rect` name
        /// the one zone the pointer can drop onto.
        fn wire_panel(&mut self, dragged: PanelKey, target: PanelKey, rect: Rect) -> WiredPanel {
            let intents: RedockIntents = Default::default();
            let zones: SharedZones = Default::default();
            let (tab_id, hint_id);
            {
                let mut cx = BuildCx::with_reactive(
                    &mut self.store,
                    &mut self.states,
                    &mut self.bindings,
                    &mut self.lists,
                    &mut self.text_edits,
                    &mut self.projectors,
                );
                let hint = cx.leaf(LeafStyle {
                    size: Size::fill(),
                    style: BoxStyle::NONE,
                });
                let tab = cx.leaf(LeafStyle {
                    size: Size::fixed(60.0, 24.0),
                    style: BoxStyle::NONE,
                });
                hint_id = hint.id();
                tab_id = tab.id();
                wire_panel_drag(&mut cx, tab, dragged, hint_id, &intents, &zones);
            }
            // Seed the one drop zone the handler hit-tests against (the build walk's
            // ZoneRec, here for the target panel only).
            zones.borrow_mut().push(super::super::build::ZoneRec {
                node: tab_id,
                target,
                rect,
            });
            WiredPanel {
                tab: tab_id,
                hint: hint_id,
                intents,
            }
        }

        /// Feed a pointer sample to the tab's panel-drag handler, restoring it after,
        /// and return the capture request plus the deferred visibility requests it made.
        fn panel_pointer(&mut self, tab: NodeId, ev: PointerEvent) -> DragStep {
            let mut handler = self.store.take_handler(tab).expect("panel pointer handler");
            let (capture, hidden) = {
                let mut cx = EventCx::__new_pointer(&mut self.states, &self.bindings, &ev);
                handler(&mut cx);
                (cx.__take_capture_request(), cx.__take_hidden_requests())
            };
            self.store.restore_handler(tab, handler);
            DragStep { capture, hidden }
        }
    }

    /// Test 4 — the panel drag-to-redock tape. A primary press on a tab captures the
    /// pointer to the tab (the hint stays hidden). A move into the target zone's central
    /// region shows the hint; a move back outside every zone hides it again. A release
    /// over the zone hides the hint, frees the capture, and pushes the resolved
    /// [`RedockIntent::Dock`] for the reconcile step to apply — the handler itself never
    /// touches the tree or any node, only writes the intent (verified by the empty queue
    /// staying empty until release, then holding exactly the resolved drop).
    #[test]
    fn panel_drag_shows_hint_and_pushes_the_redock_intent() {
        let mut rx = Reactive::new();
        let dragged = PanelKey(0);
        let target = PanelKey(1);
        // A zone well away from the origin so a press at the origin is over no zone.
        let zone = Rect {
            x: 100.0,
            y: 100.0,
            w: 400.0,
            h: 300.0,
        };
        let panel = rx.wire_panel(dragged, target, zone);

        // Press: captures to the tab, queues no visibility change, pushes no intent.
        let down = rx.panel_pointer(panel.tab, primary_at(20.0, 20.0, PointerPhase::Down));
        assert_eq!(
            down.capture,
            Some(Some(panel.tab)),
            "the press captures the pointer to the tab"
        );
        assert!(
            down.hidden.is_empty(),
            "the press queues no visibility change"
        );
        assert!(
            panel.intents.borrow().is_empty(),
            "the press pushes no redock intent"
        );

        // Move into the zone's centre: the hint is requested visible over the zone.
        let center = rx.panel_pointer(panel.tab, primary_at(300.0, 250.0, PointerPhase::Move));
        assert_eq!(
            center.hidden,
            vec![(panel.hint, false)],
            "a move over a zone shows the hint"
        );

        // Move back outside every zone: the hint is requested hidden again.
        let outside = rx.panel_pointer(panel.tab, primary_at(10.0, 10.0, PointerPhase::Move));
        assert_eq!(
            outside.hidden,
            vec![(panel.hint, true)],
            "a move off every zone hides the hint"
        );
        assert!(
            panel.intents.borrow().is_empty(),
            "no intent is pushed until the release"
        );

        // Release over the zone's centre: hides the hint, frees the capture, and pushes
        // the resolved Center-join intent.
        let up = rx.panel_pointer(panel.tab, primary_at(300.0, 250.0, PointerPhase::Up));
        assert_eq!(up.capture, Some(None), "the release frees the capture");
        assert_eq!(
            up.hidden,
            vec![(panel.hint, true)],
            "the release hides the hint"
        );
        assert_eq!(
            &*panel.intents.borrow(),
            &[RedockIntent::Dock {
                key: dragged,
                target,
                part: DropPart::Center,
            }],
            "the release pushes the resolved Center-join redock intent"
        );
    }

    /// A release over an edge band resolves to that edge's split part, and a release
    /// over no zone pushes nothing — the drop is discarded when the pointer is off
    /// every zone.
    #[test]
    fn panel_drag_resolves_the_edge_band_and_discards_an_off_zone_drop() {
        let mut rx = Reactive::new();
        let dragged = PanelKey(0);
        let target = PanelKey(1);
        let zone = Rect {
            x: 100.0,
            y: 100.0,
            w: 400.0,
            h: 300.0,
        };

        // A press then a release deep in the left band: resolves to a Left split.
        let mut rx_left = Reactive::new();
        let left = rx_left.wire_panel(dragged, target, zone);
        rx_left.panel_pointer(left.tab, primary_at(20.0, 20.0, PointerPhase::Down));
        rx_left.panel_pointer(left.tab, primary_at(110.0, 250.0, PointerPhase::Up));
        assert_eq!(
            &*left.intents.borrow(),
            &[RedockIntent::Dock {
                key: dragged,
                target,
                part: DropPart::Left,
            }],
            "a release in the left band resolves to a Left split"
        );

        // A press then a release outside every zone: nothing is pushed.
        let off = rx.wire_panel(dragged, target, zone);
        rx.panel_pointer(off.tab, primary_at(20.0, 20.0, PointerPhase::Down));
        let up = rx.panel_pointer(off.tab, primary_at(10.0, 10.0, PointerPhase::Up));
        assert_eq!(
            up.capture,
            Some(None),
            "the release still frees the capture"
        );
        assert!(
            off.intents.borrow().is_empty(),
            "a drop off every zone pushes no intent"
        );
    }
}
