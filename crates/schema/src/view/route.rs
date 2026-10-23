//! Which DSL event a node handler runs on.

use viso_ende::{DecodeError, Decoder, Encoder};

/// A DSL node event the runtime delivers, and the input sample it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EventRoute {
    /// `click` (`ClickEvent`): a pointer release on the node.
    Click = 0,
    /// `tap` (`TapEvent`): a pointer release on the node; on a node that
    /// hears double taps too, once no second press follows within the
    /// double-click time.
    Tap = 1,
    /// `pointer_down` (`PointerEvent`).
    PointerDown = 2,
    /// `pointer_move` (`PointerEvent`).
    PointerMove = 3,
    /// `pointer_up` (`PointerEvent`).
    PointerUp = 4,
    /// `hover_enter` (`HoverEvent`): the pointer entered the node.
    HoverEnter = 5,
    /// `hover_leave` (`HoverEvent`): the pointer left the node.
    HoverLeave = 6,
    /// `key_down` (`KeyEvent`): a key press routed to the node.
    KeyDown = 7,
    /// `key_up` (`KeyEvent`): a key release routed to the node.
    KeyUp = 8,
    /// A control's `changed`: the value it reports after a toggle, a slide or
    /// a settled edit.
    Changed = 9,
    /// A text field's `submitted`: Enter pressed in it.
    Submitted = 10,
    /// A tab strip's or radio group's `selected_changed`.
    SelectedChanged = 11,
    /// A `Scroll`'s `scroll_changed` (`ScrollChanged`): its offset, viewport or
    /// content moved in a frame.
    ScrollChanged = 12,
    /// A `VirtualList`'s `visible_range_changed` (`VisibleRange`): the rows it
    /// shows changed.
    VisibleRangeChanged = 13,
    /// Any node's `animation_end` (`AnimationEnd`): an animation it ran by
    /// `NodeRef::animate` ended.
    AnimationEnd = 14,
    /// `drag_start` (`DragEvent`): a press on the node or under it travelled
    /// past the slop and the node won the drag.
    DragStart = 15,
    /// `drag_move` (`DragEvent`): the drag's pointer moved.
    DragMove = 16,
    /// `drag_end` (`DragEvent`): the drag's pointer lifted or was cancelled.
    DragEnd = 17,
    /// `long_press` (`LongPressEvent`): a press on the node or under it held
    /// still past the long-press delay and the node won it.
    LongPress = 18,
    /// `link` (`LinkEvent`): a link of the node's text, or of a text under
    /// it, was activated by a click or Enter.
    Link = 19,
    /// A `Scroll`'s or `VirtualList`'s `refresh`: a pull down past its start,
    /// or an assistive technology, asked it to refresh.
    Refresh = 20,
    /// A snapping `Scroll`'s or `VirtualList`'s `snap_changed`: it came to
    /// rest on, heads for, or shows another item.
    SnapChanged = 21,
    /// A `NavigationStack`'s `current_changed`: a back gesture asked it to
    /// show another page.
    CurrentChanged = 22,
    /// `double_tap` (`TapEvent`): the release of a second press on the node
    /// soon after and near the first.
    DoubleTap = 23,
    /// `swipe` (`SwipeEvent`): a drag the node won lifted moving fast along
    /// one axis.
    Swipe = 24,
    /// `pinch_start` (`PinchEvent`): two fingers on the node or under it
    /// travelled past the slop together, or a trackpad began to pinch or
    /// turn over it, or a wheel turned with Control held, and the node won
    /// the pinch.
    PinchStart = 25,
    /// `pinch_move` (`PinchEvent`): the pinch spread, turned or moved.
    PinchMove = 26,
    /// `pinch_end` (`PinchEvent`): a finger lifted, the trackpad's gesture
    /// ended, or the wheel rested.
    PinchEnd = 27,
    /// A `SwipeRow`'s `full_swiped` (`FullSwipeEvent`): a full swipe carried
    /// its content off toward an edge.
    FullSwiped = 28,
    /// A list's `reordered` (`ReorderEvent`): a row lifted from one place
    /// came to rest at another.
    Reordered = 29,
    /// A `Sheet`'s `detent_changed`: a drag let go brought it to rest at
    /// another detent.
    DetentChanged = 30,
    /// `drop` (`DropEvent`): data a drag carried was let go over the node,
    /// a drop target taking it.
    Drop = 31,
    /// `drop_enter` (`DropEvent`): a drag carrying data the node takes came
    /// over it.
    DropEnter = 32,
    /// `drop_leave` (`DropEvent`): the drag left the node, or was put back
    /// over it.
    DropLeave = 33,
    /// `drop_end` (`DropEnd`): a drag of the node's data ended, dropped or
    /// put back.
    DropEnd = 34,
}

impl EventRoute {
    /// Every route, by discriminant.
    pub const ALL: [EventRoute; 35] = [
        EventRoute::Click,
        EventRoute::Tap,
        EventRoute::PointerDown,
        EventRoute::PointerMove,
        EventRoute::PointerUp,
        EventRoute::HoverEnter,
        EventRoute::HoverLeave,
        EventRoute::KeyDown,
        EventRoute::KeyUp,
        EventRoute::Changed,
        EventRoute::Submitted,
        EventRoute::SelectedChanged,
        EventRoute::ScrollChanged,
        EventRoute::VisibleRangeChanged,
        EventRoute::AnimationEnd,
        EventRoute::DragStart,
        EventRoute::DragMove,
        EventRoute::DragEnd,
        EventRoute::LongPress,
        EventRoute::Link,
        EventRoute::Refresh,
        EventRoute::SnapChanged,
        EventRoute::CurrentChanged,
        EventRoute::DoubleTap,
        EventRoute::Swipe,
        EventRoute::PinchStart,
        EventRoute::PinchMove,
        EventRoute::PinchEnd,
        EventRoute::FullSwiped,
        EventRoute::Reordered,
        EventRoute::DetentChanged,
        EventRoute::Drop,
        EventRoute::DropEnter,
        EventRoute::DropLeave,
        EventRoute::DropEnd,
    ];

    /// The route of standard DSL event `event`, `None` for an event the runtime
    /// does not deliver. A control's own events route through
    /// [`ControlKind::route`](super::control::ControlKind::route).
    pub fn of(event: &str) -> Option<EventRoute> {
        EventRoute::ALL
            .into_iter()
            .find(|route| !route.is_control() && route.event() == event)
    }

    /// The DSL event name.
    pub fn event(self) -> &'static str {
        match self {
            EventRoute::Click => "click",
            EventRoute::Tap => "tap",
            EventRoute::PointerDown => "pointer_down",
            EventRoute::PointerMove => "pointer_move",
            EventRoute::PointerUp => "pointer_up",
            EventRoute::HoverEnter => "hover_enter",
            EventRoute::HoverLeave => "hover_leave",
            EventRoute::KeyDown => "key_down",
            EventRoute::KeyUp => "key_up",
            EventRoute::Changed => "changed",
            EventRoute::Submitted => "submitted",
            EventRoute::SelectedChanged => "selected_changed",
            EventRoute::ScrollChanged => "scroll_changed",
            EventRoute::VisibleRangeChanged => "visible_range_changed",
            EventRoute::AnimationEnd => "animation_end",
            EventRoute::DragStart => "drag_start",
            EventRoute::DragMove => "drag_move",
            EventRoute::DragEnd => "drag_end",
            EventRoute::LongPress => "long_press",
            EventRoute::Link => "link",
            EventRoute::Refresh => "refresh",
            EventRoute::SnapChanged => "snap_changed",
            EventRoute::CurrentChanged => "current_changed",
            EventRoute::DoubleTap => "double_tap",
            EventRoute::Swipe => "swipe",
            EventRoute::PinchStart => "pinch_start",
            EventRoute::PinchMove => "pinch_move",
            EventRoute::PinchEnd => "pinch_end",
            EventRoute::FullSwiped => "full_swiped",
            EventRoute::Reordered => "reordered",
            EventRoute::DetentChanged => "detent_changed",
            EventRoute::Drop => "drop",
            EventRoute::DropEnter => "drop_enter",
            EventRoute::DropLeave => "drop_leave",
            EventRoute::DropEnd => "drop_end",
        }
    }

    /// The Rust variant name, for code that names a route in tokens.
    pub fn variant(self) -> &'static str {
        match self {
            EventRoute::Click => "Click",
            EventRoute::Tap => "Tap",
            EventRoute::PointerDown => "PointerDown",
            EventRoute::PointerMove => "PointerMove",
            EventRoute::PointerUp => "PointerUp",
            EventRoute::HoverEnter => "HoverEnter",
            EventRoute::HoverLeave => "HoverLeave",
            EventRoute::KeyDown => "KeyDown",
            EventRoute::KeyUp => "KeyUp",
            EventRoute::Changed => "Changed",
            EventRoute::Submitted => "Submitted",
            EventRoute::SelectedChanged => "SelectedChanged",
            EventRoute::ScrollChanged => "ScrollChanged",
            EventRoute::VisibleRangeChanged => "VisibleRangeChanged",
            EventRoute::AnimationEnd => "AnimationEnd",
            EventRoute::DragStart => "DragStart",
            EventRoute::DragMove => "DragMove",
            EventRoute::DragEnd => "DragEnd",
            EventRoute::LongPress => "LongPress",
            EventRoute::Link => "Link",
            EventRoute::Refresh => "Refresh",
            EventRoute::SnapChanged => "SnapChanged",
            EventRoute::CurrentChanged => "CurrentChanged",
            EventRoute::DoubleTap => "DoubleTap",
            EventRoute::Swipe => "Swipe",
            EventRoute::PinchStart => "PinchStart",
            EventRoute::PinchMove => "PinchMove",
            EventRoute::PinchEnd => "PinchEnd",
            EventRoute::FullSwiped => "FullSwiped",
            EventRoute::Reordered => "Reordered",
            EventRoute::DetentChanged => "DetentChanged",
            EventRoute::Drop => "Drop",
            EventRoute::DropEnter => "DropEnter",
            EventRoute::DropLeave => "DropLeave",
            EventRoute::DropEnd => "DropEnd",
        }
    }

    /// The route with discriminant `raw`.
    pub fn from_u8(raw: u8) -> Option<EventRoute> {
        EventRoute::ALL.get(usize::from(raw)).copied()
    }

    /// Whether a key sample delivers it; otherwise a pointer sample does.
    pub fn is_key(self) -> bool {
        matches!(self, EventRoute::KeyDown | EventRoute::KeyUp)
    }

    /// Whether the node's key handler delivers it: a key sample, an
    /// activated link, which reaches a text and its ancestors that way, a
    /// refresh asked of a viewport, the item a snapping one settles on, a
    /// swipe row's full swipe, a list's reorder, or what a drag and drop
    /// tells its target and its source.
    pub fn by_key_handler(self) -> bool {
        self.is_key()
            || matches!(
                self,
                EventRoute::Link
                    | EventRoute::Refresh
                    | EventRoute::SnapChanged
                    | EventRoute::FullSwiped
                    | EventRoute::Reordered
            )
            || self.is_drop()
    }

    /// Whether a drag and drop tells it.
    pub fn is_drop(self) -> bool {
        matches!(
            self,
            EventRoute::Drop | EventRoute::DropEnter | EventRoute::DropLeave | EventRoute::DropEnd
        )
    }

    /// Whether a drag the node wins delivers it.
    pub fn is_drag(self) -> bool {
        matches!(
            self,
            EventRoute::DragStart | EventRoute::DragMove | EventRoute::DragEnd | EventRoute::Swipe
        )
    }

    /// Whether a pinch the node wins delivers it.
    pub fn is_pinch(self) -> bool {
        matches!(
            self,
            EventRoute::PinchStart | EventRoute::PinchMove | EventRoute::PinchEnd
        )
    }

    /// Whether the node reports it after a frame's layout rather than an
    /// input sample delivering it.
    pub fn is_notice(self) -> bool {
        matches!(
            self,
            EventRoute::ScrollChanged | EventRoute::VisibleRangeChanged | EventRoute::AnimationEnd
        )
    }

    /// Whether a control reports it, from whichever sample drives the control.
    pub fn is_control(self) -> bool {
        matches!(
            self,
            EventRoute::Changed
                | EventRoute::Submitted
                | EventRoute::SelectedChanged
                | EventRoute::CurrentChanged
                | EventRoute::DetentChanged
        )
    }
}

/// The prelude records a payload is built from, with their fields in order.
pub const PAYLOAD_RECORDS: &[(&str, &[&str])] = &[
    ("Point", &["x", "y"]),
    ("Modifiers", &["shift", "control", "alt", "logo"]),
    ("PointerButtons", &["primary", "secondary", "middle"]),
    ("ClickEvent", &["position", "modifiers"]),
    ("TapEvent", &["position", "pointer_kind"]),
    ("LongPressEvent", &["position", "pointer_kind"]),
    ("HoverEvent", &["position", "pointer_kind"]),
    (
        "PointerEvent",
        &["position", "button", "buttons", "pointer_kind", "modifiers"],
    ),
    ("KeyEvent", &["key", "repeat", "modifiers"]),
    ("DragEvent", &["position", "delta", "total", "velocity"]),
    ("SwipeEvent", &["direction", "position", "velocity"]),
    (
        "PinchEvent",
        &[
            "position",
            "scale",
            "rotation",
            "scale_delta",
            "rotation_delta",
            "pan",
        ],
    ),
    ("Offset", &["x", "y"]),
    ("SizeDp", &["width", "height"]),
    ("ScrollChanged", &["offset", "viewport", "content"]),
    ("VisibleRange", &["first", "last", "count"]),
    ("AnimationEnd", &["target", "finished"]),
    ("LinkEvent", &["url"]),
    ("SelectionChanged", &["value"]),
    ("FullSwipeEvent", &["edge"]),
    ("ReorderEvent", &["from", "to"]),
    (
        "DropEvent",
        &["position", "kind", "text", "files", "operation"],
    ),
    ("DropEnd", &["operation"]),
];

/// The prelude enums a payload names a variant of, with their variants in order.
pub const PAYLOAD_ENUMS: &[(&str, &[&str])] = &[
    ("PointerButton", &["primary", "secondary", "middle"]),
    ("PointerKind", &["mouse", "touch", "pen"]),
    ("SwipeDirection", &["left", "right", "up", "down"]),
    ("SwipeEdge", &["leading", "trailing"]),
    ("DropOperation", &["move", "copy", "link"]),
    ("Animate", &["translate", "scale", "rotation", "opacity"]),
    (
        "Key",
        &[
            "char",
            "enter",
            "escape",
            "tab",
            "backspace",
            "delete",
            "space",
            "arrow_up",
            "arrow_down",
            "arrow_left",
            "arrow_right",
            "home",
            "end",
            "page_up",
            "page_down",
            "function",
            "unidentified",
        ],
    ),
];

/// One handler a node declares: the event it runs on, its index in the view's
/// handler table, and whether it is an `on capture` handler.
///
/// A capture handler runs as the sample walks down to its target — on the
/// node's ancestors' capture leg, and on the target itself; any other handler
/// runs on the target and as the sample bubbles back up (§52).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    /// The event it runs on.
    pub event: EventRoute,
    /// Its index in the component's handler table.
    pub handler: u32,
    /// Whether it runs on the capture leg instead of the bubble leg.
    pub capture: bool,
}

/// The tag bit marking a capture route; event discriminants stay below it.
const CAPTURE: u8 = 0x80;

impl Route {
    /// The route's tag byte: its event's discriminant with the high bit set
    /// for a capture handler.
    #[inline]
    pub fn tag(self) -> u8 {
        self.event as u8 | if self.capture { CAPTURE } else { 0 }
    }

    /// The route of tag byte `tag` running handler `handler`, `None` for a
    /// tag naming no event.
    #[inline]
    pub fn from_tag(tag: u8, handler: u32) -> Option<Route> {
        Some(Route {
            event: EventRoute::from_u8(tag & !CAPTURE)?,
            handler,
            capture: tag & CAPTURE != 0,
        })
    }

    /// Writes the route: its [`tag`](Self::tag), then its handler index. A
    /// route written before capture handlers existed reads back as a bubble
    /// one.
    pub fn encode(self, enc: &mut Encoder) {
        enc.write_u8(self.tag());
        enc.write_varint(u64::from(self.handler));
    }

    /// Reads a route [`Route::encode`] wrote.
    pub fn decode(dec: &mut Decoder<'_>) -> Result<Route, DecodeError> {
        let offset = dec.position();
        let tag = dec.read_u8()?;
        let at = dec.position();
        let handler =
            u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset: at })?;
        Route::from_tag(tag, handler).ok_or(DecodeError::Malformed { offset })
    }
}
