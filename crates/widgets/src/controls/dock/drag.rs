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
//! The panel drag-to-redock half of the drag subsystem lands in a later section;
//! this file is the seam half only.

use viso_ui::{Axis, BuildCx, EventCx, Key, PointerButtons, PointerPhase, StateId, StateValue};

use super::build::SeamRec;

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
}
