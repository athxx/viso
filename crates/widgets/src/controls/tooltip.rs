//! `Tooltip` — a short description that shows by the content it describes
//! while the pointer rests on it or the keyboard brings the focus into it.
//!
//! It shows after the pointer rests on the content for
//! [`TooltipStyle::delay`]; once one has shown, moving to another content
//! with a tooltip within [`TOOLTIP_GRACE`] shows the next at once, as along
//! a toolbar. Only one shows at a time. It stays while the pointer is on the
//! content or on the tooltip itself, crossing the gap between them within
//! [`TOOLTIP_LINGER`], and goes when the pointer leaves, a press lands on
//! the content, or Escape is pressed; a press keeps it away until the
//! pointer leaves. The keyboard's focus coming into the content shows it at
//! once, and the focus leaving takes it away. A long press of a finger shows
//! it, and it goes [`TOOLTIP_TOUCH`] after the finger lifts.
//!
//! It sits below the content, centred, flipped above when there is no room
//! below and shifted to stay in the window ([`TooltipStyle::placement`]).
//!
//! Semantics: the tooltip is a `tooltip` named by its text, and the first
//! control within the content is described by it.
//!
//! Invalidation: the tooltip builds once with its content; showing and
//! hiding it flips its `hidden` flag.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use viso_ui::{
    Align, AnchorAlign, Anchoring, Axis, BoxStyle, BuildCx, Component, DispatchPhase, EventCx,
    FlexStyle, FocusChange, Inset, Length, NodeId, NodeStore, PointerPhase, Rgba, Role, Semantics,
    Shadow, Side, Sizing,
};

use crate::label;

/// How long a tooltip shows at once after another one went.
pub const TOOLTIP_GRACE: Duration = Duration::from_millis(400);

/// How long a tooltip stays after the pointer left, for the pointer to
/// reach it across the gap.
pub const TOOLTIP_LINGER: Duration = Duration::from_millis(120);

/// How long a tooltip a long press showed stays after the finger lifts.
pub const TOOLTIP_TOUCH: Duration = Duration::from_millis(1500);

/// The kind a wake's token carries in its top bits.
const SHOW: u32 = 1 << 28;
const HIDE: u32 = 2 << 28;
const COOL: u32 = 3 << 28;
const TICK: u32 = (1 << 28) - 1;

/// How a tooltip looks.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TooltipStyle {
    pub bubble: BoxStyle,
    pub shadow: Option<Shadow>,
    /// The room between the bubble's edge and its text.
    pub padding: Inset,
    pub font_size: f32,
    pub ink: Rgba,
    /// The widest the bubble grows before its text wraps.
    pub max_width: f32,
    /// How long the pointer rests on the content before it shows.
    pub delay: Duration,
    /// Where it sits against the content.
    pub placement: Anchoring,
}

impl Default for TooltipStyle {
    fn default() -> Self {
        TooltipStyle {
            bubble: BoxStyle::solid(Rgba {
                r: 0.12,
                g: 0.12,
                b: 0.14,
                a: 0.96,
            })
            .with_radius(6.0),
            shadow: Some(Shadow::below(
                Rgba {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                    a: 0.18,
                },
                2.0,
                8.0,
            )),
            padding: Inset {
                left: 8.0,
                right: 8.0,
                top: 4.0,
                bottom: 5.0,
            },
            font_size: 12.0,
            ink: Rgba {
                r: 0.96,
                g: 0.96,
                b: 0.97,
                a: 1.0,
            },
            max_width: 280.0,
            delay: Duration::from_millis(500),
            placement: Anchoring {
                side: Side::Bottom,
                align: AnchorAlign::Center,
                gap: 6.0,
                margin: 4.0,
                ..Anchoring::default()
            },
        }
    }
}

/// A builder of the content a tooltip describes.
type Builder = Box<dyn Fn(&mut BuildCx<'_>)>;

/// Content with a tooltip. Construct one with [`tooltip`].
pub struct Tooltip {
    text: String,
    content: Option<Builder>,
    style: TooltipStyle,
}

/// A tooltip saying `text`, with no content yet.
pub fn tooltip(text: impl Into<String>) -> Tooltip {
    Tooltip {
        text: text.into(),
        content: None,
        style: TooltipStyle::default(),
    }
}

impl Tooltip {
    /// Set the content it describes.
    pub fn content(mut self, content: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.content = Some(Box::new(content));
        self
    }

    /// Replace the whole [`TooltipStyle`].
    pub fn style(mut self, style: TooltipStyle) -> Self {
        self.style = style;
        self
    }

    /// Set where it sits against the content.
    pub fn placement(mut self, placement: Anchoring) -> Self {
        self.style.placement = placement;
        self
    }

    /// Set how long the pointer rests on the content before it shows.
    pub fn delay(mut self, delay: Duration) -> Self {
        self.style.delay = delay;
        self
    }
}

impl Component for Tooltip {
    fn build(&self, cx: &mut BuildCx<'_>) {
        build(cx, self);
    }
}

/// Which tooltip shows, and whether the next shows at once: one pointer,
/// one tooltip.
#[derive(Default)]
struct Tips {
    shown: Option<Weak<TipState>>,
    warm: bool,
    tick: u32,
}

thread_local! {
    static TIPS: RefCell<Tips> = RefCell::default();
}

type Shared = Rc<OnceCell<Rc<TipState>>>;

fn with(shared: &Shared, f: impl FnOnce(&Rc<TipState>)) {
    if let Some(state) = shared.get() {
        f(state);
    }
}

fn build(cx: &mut BuildCx<'_>, tooltip: &Tooltip) {
    let style = &tooltip.style;
    let shared: Shared = Rc::new(OnceCell::new());
    let mut tip = None;
    let body = cx.flex(
        FlexStyle {
            axis: Axis::Column,
            align: Align::Stretch,
            size: Sizing {
                width: Length::Fit,
                height: Length::Fit,
            },
            ..FlexStyle::default()
        },
        |cx| {
            if let Some(content) = &tooltip.content {
                content(cx);
            }
            let bubble = cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    padding: style.padding,
                    align: Align::Start,
                    size: Sizing {
                        width: Length::Fit,
                        height: Length::Fit,
                    },
                    style: style.bubble,
                    ..FlexStyle::default()
                },
                |cx| {
                    label(tooltip.text.clone())
                        .font_size(style.font_size)
                        .color(style.ink)
                        .wrap()
                        .build_node(cx);
                },
            );
            tip = Some(bubble);
        },
    );
    let tip = tip.expect("built");
    cx.set_box_rules(
        tip,
        viso_ui::BoxRules {
            max_width: viso_ui::Span::dp(style.max_width),
            ..viso_ui::BoxRules::NONE
        },
    );
    cx.anchored(tip, body, style.placement);
    if let Some(shadow) = style.shadow {
        cx.shadow(tip, shadow);
    }
    // A press anywhere else, and Escape no one took, take it away.
    cx.light_dismiss(tip);
    let s = Rc::clone(&shared);
    cx.on_key(tip, move |ev| {
        if ev.dismissed() {
            with(&s, |state| state.dismiss(ev));
        }
    });
    cx.set_hidden(tip, true);
    cx.semantics(
        tip,
        Semantics::role(Role::Tooltip).with_label(tooltip.text.clone()),
    );
    let text = tooltip.text.clone();
    cx.structure(|s| describe(s.store, body.id(), tip.id(), &text));
    cx.long_pressable(body, true);
    let s = Rc::clone(&shared);
    cx.on_pointer(body, move |ev| with(&s, |state| state.pointer(ev)));
    let s = Rc::clone(&shared);
    cx.on_key(body, move |ev| with(&s, |state| state.key(ev)));
    let _ = shared.set(Rc::new(TipState {
        body: body.id(),
        tip: tip.id(),
        delay: style.delay,
        shown: Cell::new(false),
        by_focus: Cell::new(false),
        inside: Cell::new(false),
        held: Cell::new(false),
        tick: Cell::new(0),
    }));
}

/// Give the first control within `body` — a node that takes the focus or
/// the pointer — `text` as its description, unless it has one.
fn describe(store: &mut NodeStore, body: NodeId, tip: NodeId, text: &str) {
    let mut stack: Vec<NodeId> = store.arena().children(body).collect();
    stack.reverse();
    while let Some(id) = stack.pop() {
        if id == tip {
            continue;
        }
        if store.focusable(id) || store.has_handler(id) {
            let mut semantics = store.semantics(id).cloned().unwrap_or_default();
            if semantics.hint.is_none() {
                semantics.hint = Some(text.to_owned());
                store.set_semantics(id, semantics);
            }
            return;
        }
        let at = stack.len();
        stack.extend(store.arena().children(id));
        stack[at..].reverse();
    }
}

/// One tooltip, shared by its handlers.
struct TipState {
    body: NodeId,
    tip: NodeId,
    delay: Duration,
    shown: Cell<bool>,
    /// Whether the keyboard's focus showed it.
    by_focus: Cell<bool>,
    /// Whether the pointer is on the content or the tooltip.
    inside: Cell<bool>,
    /// Whether a press keeps it away until the pointer leaves.
    held: Cell<bool>,
    /// The latest wake armed, which a later one or a cancel outdates.
    tick: Cell<u32>,
}

impl TipState {
    /// Arm a wake doing `kind` after `delay`, outdating the one before.
    fn arm(&self, ev: &mut EventCx<'_>, kind: u32, delay: Duration) {
        let tick = (self.tick.get() + 1) & TICK;
        self.tick.set(tick);
        ev.wake_after(self.body, delay, kind | tick);
    }

    /// Outdate the wake armed.
    fn cancel(&self) {
        self.tick.set((self.tick.get() + 1) & TICK);
    }

    fn show(self: &Rc<Self>, ev: &mut EventCx<'_>) {
        self.cancel();
        if self.shown.replace(true) {
            return;
        }
        let was = TIPS.with(|t| {
            let mut t = t.borrow_mut();
            t.warm = false;
            t.shown.replace(Rc::downgrade(self))
        });
        if let Some(was) = was.and_then(|w| w.upgrade())
            && !Rc::ptr_eq(&was, self)
        {
            was.hide(ev, false);
        }
        ev.set_hidden(self.tip, false);
    }

    /// Take it away; when `warm`, the next one shows at once for a while.
    fn hide(&self, ev: &mut EventCx<'_>, warm: bool) {
        self.cancel();
        self.by_focus.set(false);
        if !self.shown.replace(false) {
            return;
        }
        ev.set_hidden(self.tip, true);
        let tick = TIPS.with(|t| {
            let mut t = t.borrow_mut();
            let mine = t
                .shown
                .as_ref()
                .is_some_and(|w| std::ptr::eq(w.as_ptr(), self));
            if mine {
                t.shown = None;
            }
            t.warm = warm;
            t.tick = (t.tick + 1) & TICK;
            t.tick
        });
        if warm {
            ev.wake_after(self.body, TOOLTIP_GRACE, COOL | tick);
        }
    }

    fn pointer(self: &Rc<Self>, ev: &mut EventCx<'_>) {
        if ev.phase() == DispatchPhase::Capture {
            return;
        }
        let Some(p) = ev.pointer().copied() else {
            return;
        };
        let direct = ev.direct();
        match p.phase {
            PointerPhase::Enter if !direct => {
                self.inside.set(true);
                if self.held.get() {
                    return;
                }
                if self.shown.get() {
                    // Back across the gap, or onto the tooltip.
                    self.cancel();
                    return;
                }
                let warm = TIPS.with(|t| {
                    let t = t.borrow();
                    t.warm || t.shown.as_ref().is_some_and(|w| w.strong_count() > 0)
                });
                if warm {
                    self.show(ev);
                } else {
                    self.arm(ev, SHOW, self.delay);
                }
            }
            PointerPhase::Leave if !direct => {
                self.inside.set(false);
                self.held.set(false);
                if self.shown.get() && !self.by_focus.get() {
                    self.arm(ev, HIDE, TOOLTIP_LINGER);
                } else if !self.shown.get() {
                    self.cancel();
                }
            }
            PointerPhase::Down if direct => self.cancel(),
            PointerPhase::Down => {
                self.held.set(true);
                self.hide(ev, false);
            }
            PointerPhase::Up if direct && self.shown.get() => {
                self.arm(ev, HIDE, TOOLTIP_TOUCH);
            }
            _ if direct && ev.long_press().is_some() => self.show(ev),
            _ => {}
        }
    }

    /// A press elsewhere, or Escape no one took: it goes, and stays away
    /// while the pointer stays.
    fn dismiss(&self, ev: &mut EventCx<'_>) {
        self.hide(ev, false);
        self.held.set(self.inside.get());
    }

    fn key(self: &Rc<Self>, ev: &mut EventCx<'_>) {
        if let Some(token) = ev.woken() {
            let tick = token & TICK;
            match token & !TICK {
                SHOW if tick == self.tick.get() && self.inside.get() && !self.held.get() => {
                    self.show(ev);
                }
                HIDE if tick == self.tick.get() && !self.inside.get() => self.hide(ev, true),
                COOL => TIPS.with(|t| {
                    let mut t = t.borrow_mut();
                    if t.tick == tick {
                        t.warm = false;
                    }
                }),
                _ => {}
            }
            return;
        }
        if let Some(change) = ev.focus_change() {
            let within = ev.focused().is_some_and(|f| ev.is_within(f, self.body));
            match change {
                FocusChange::Gained if within && ev.focus_visible() => {
                    self.show(ev);
                    self.by_focus.set(true);
                }
                FocusChange::Lost if !within && self.by_focus.get() => self.hide(ev, false),
                _ => {}
            }
        }
    }
}
