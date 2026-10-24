//! Placeholders — [`Skeleton`] blocks that hold the place of content on its
//! way, and a [`Placeholder`] that shows them until it comes.
//!
//! A skeleton is a block of a quiet colour shaped as what it stands for: a
//! rectangle ([`skeleton_rect`]), a circle ([`skeleton_circle`]) or lines of
//! text ([`skeleton_text`]), the last line short. A band of light sweeps
//! across the blocks ([`ShimmerLook::Wave`]), or they fade and come back
//! ([`ShimmerLook::Pulse`]); every block of a window moves on one clock, so a
//! band crosses the blocks of a placeholder together. Under reduced motion,
//! or out of sight, they stand still and beat no frames.
//!
//! A [`Placeholder`] holds a skeleton and the content it stands for, both
//! built once: while its state cell reads `true` the skeleton shows,
//! announced as a `progressindicator` named by its label (default
//! "Loading"), with no value; once the cell reads `false` the content shows
//! instead, fading in (at once under reduced motion). Skeleton blocks take no
//! focus or input and have no semantics of their own.
//!
//! ```
//! use viso_widgets::{placeholder, skeleton_circle, skeleton_text};
//! use viso_ui::{Axis, BuildCx, BindingTable, Component, FlexStyle, NodeStore, SemanticProjector, StateStore, StateValue, TextEdits, VirtualLists};
//!
//! let mut store = NodeStore::new();
//! let mut states = StateStore::new();
//! let mut bindings = BindingTable::new();
//! let mut lists = VirtualLists::new();
//! let mut text_edits = TextEdits::new();
//! let mut projectors = SemanticProjector::new();
//! let mut cx = BuildCx::with_reactive(&mut store, &mut states, &mut bindings, &mut lists, &mut text_edits, &mut projectors);
//! let loading = cx.state(StateValue::Bool(true));
//! placeholder(loading)
//!     .skeleton(|cx| {
//!         cx.flex(FlexStyle { axis: Axis::Row, gap: 12.0, ..FlexStyle::default() }, |cx| {
//!             skeleton_circle(40.0).build(cx);
//!             skeleton_text(3).build(cx);
//!         });
//!     })
//!     .content(|_cx| {})
//!     .build(&mut cx);
//! ```

use std::time::Duration;

use viso_ui::{
    Align, Axis, BuildCx, Component, FlexStyle, Justify, Keyframe, Keyframes, Length, LookValue,
    NodeId, Rgba, Role, Semantics, Shimmer, ShimmerLook, Size, StateId, StateValue, StructureCx,
};

/// How skeleton blocks look.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SkeletonStyle {
    /// A block's colour, and the light a wave sweeps over it.
    pub base: Rgba,
    pub highlight: Rgba,
    /// A rectangle's and a line's corner radius.
    pub radius: f32,
    /// A line of text's height, the gap between lines, and the share of the
    /// width the last line takes.
    pub line_height: f32,
    pub line_gap: f32,
    pub last_line: f32,
}

impl Default for SkeletonStyle {
    fn default() -> Self {
        SkeletonStyle {
            base: Rgba {
                r: 0.0,
                g: 0.0,
                b: 0.0,
                a: 0.2,
            },
            highlight: Rgba {
                r: 1.0,
                g: 1.0,
                b: 1.0,
                a: 0.5,
            },
            radius: 4.0,
            line_height: 12.0,
            line_gap: 8.0,
            last_line: 0.6,
        }
    }
}

/// What a skeleton stands for.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Shape {
    Rect(Size),
    Circle(f32),
    Text(u32),
}

/// A block holding the place of content on its way. Construct one with
/// [`skeleton_rect`], [`skeleton_circle`] or [`skeleton_text`].
///
/// Invalidation: a moving block repaints itself each frame while it shows
/// (`PAINT`); nothing re-lays out.
#[derive(Debug, Clone, PartialEq)]
pub struct Skeleton {
    shape: Shape,
    look: ShimmerLook,
    style: SkeletonStyle,
}

fn shape(shape: Shape) -> Skeleton {
    Skeleton {
        shape,
        look: ShimmerLook::Wave,
        style: SkeletonStyle::default(),
    }
}

/// A rectangle of `size`: a picture's, a card's, a button's place.
pub fn skeleton_rect(size: Size) -> Skeleton {
    shape(Shape::Rect(size))
}

/// A circle `diameter` across: an avatar's place.
pub fn skeleton_circle(diameter: f32) -> Skeleton {
    shape(Shape::Circle(diameter))
}

/// `lines` lines of text filling the width, the last one short.
pub fn skeleton_text(lines: u32) -> Skeleton {
    shape(Shape::Text(lines.max(1)))
}

impl Skeleton {
    /// Move as `look` says (default a wave).
    pub fn look(mut self, look: ShimmerLook) -> Self {
        self.look = look;
        self
    }

    /// Replace the whole [`SkeletonStyle`].
    pub fn style(mut self, style: SkeletonStyle) -> Self {
        self.style = style;
        self
    }

    fn block(&self, cx: &mut BuildCx<'_>, size: Size, radius: f32) {
        let s = &self.style;
        cx.shimmer(size, Shimmer::new(self.look, s.base, s.highlight, radius));
    }
}

impl Component for Skeleton {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let s = self.style;
        match self.shape {
            Shape::Rect(size) => self.block(cx, size, s.radius),
            Shape::Circle(d) => self.block(cx, Size::fixed(d, d), d * 0.5),
            Shape::Text(lines) => {
                cx.flex(
                    FlexStyle {
                        axis: Axis::Column,
                        gap: s.line_gap,
                        align: Align::Start,
                        justify: Justify::Start,
                        size: Size {
                            width: Length::fill(),
                            height: Length::Fit,
                        },
                        ..FlexStyle::default()
                    },
                    |cx| {
                        for line in 0..lines {
                            let width = if lines > 1 && line + 1 == lines {
                                Length::pct(s.last_line * 100.0)
                            } else {
                                Length::fill()
                            };
                            let size = Size {
                                width,
                                height: Length::Fixed(s.line_height),
                            };
                            self.block(cx, size, s.radius.min(s.line_height * 0.5));
                        }
                    },
                );
            }
        }
    }
}

/// A part's builder.
type Build = Box<dyn Fn(&mut BuildCx<'_>)>;

/// A skeleton and the content it stands for, built once and shown in turn.
/// Construct one with [`placeholder`].
///
/// Invalidation: the loading cell's change flips the two parts' `hidden`
/// flags in a structure hook and fades the content in.
pub struct Placeholder {
    loading: StateId,
    label: String,
    skeleton: Option<Build>,
    content: Option<Build>,
}

/// A placeholder showing its skeleton while `loading` reads `true`, its
/// content once it reads `false`.
pub fn placeholder(loading: StateId) -> Placeholder {
    Placeholder {
        loading,
        label: "Loading".to_owned(),
        skeleton: None,
        content: None,
    }
}

impl Placeholder {
    /// Build the skeleton shown while loading.
    pub fn skeleton(mut self, build: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.skeleton = Some(Box::new(build));
        self
    }

    /// Build the content shown once loaded.
    pub fn content(mut self, build: impl Fn(&mut BuildCx<'_>) + 'static) -> Self {
        self.content = Some(Box::new(build));
        self
    }

    /// Name the skeleton `label` while it shows (default "Loading").
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }
}

/// How long the content takes to fade in.
const FADE_IN: Duration = Duration::from_millis(200);

impl Component for Placeholder {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let part = |cx: &mut BuildCx<'_>, build: &Option<Build>| {
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    align: Align::Stretch,
                    justify: Justify::Start,
                    size: Size {
                        width: Length::fill(),
                        height: Length::Fit,
                    },
                    ..FlexStyle::default()
                },
                |cx| {
                    if let Some(build) = build {
                        build(cx);
                    }
                },
            )
        };
        let mut parts = None;
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                align: Align::Stretch,
                justify: Justify::Start,
                size: Size {
                    width: Length::fill(),
                    height: Length::Fit,
                },
                ..FlexStyle::default()
            },
            |cx| {
                let skeleton = part(cx, &self.skeleton);
                let content = part(cx, &self.content);
                parts = Some((skeleton, content));
            },
        );
        let (skeleton, content) = parts.expect("both parts are built");
        let mut semantics = Semantics::role(Role::ProgressIndicator);
        semantics.label = Some(self.label.clone());
        cx.semantics(skeleton, semantics);
        let (skeleton, content, loading) = (skeleton.id(), content.id(), self.loading);
        cx.structure(|s| {
            show(s, loading, skeleton, content, false);
            s.store.add_structure_hook([loading], move |s, _| {
                if s.store.arena().is_live(content) {
                    show(s, loading, skeleton, content, true);
                }
            });
        });
    }
}

/// Show the skeleton while `loading` reads anything but `false`, the content
/// otherwise; content newly shown fades in when `fade` says.
fn show(s: &mut StructureCx<'_>, loading: StateId, skeleton: NodeId, content: NodeId, fade: bool) {
    let on = !matches!(s.states.get(loading), Some(StateValue::Bool(false)));
    let was_hidden = s.store.hidden(content);
    s.store.set_hidden(skeleton, !on);
    s.store.set_hidden(content, on);
    if fade && !on && was_hidden {
        let track = Keyframes::new(
            FADE_IN,
            [
                Keyframe::new(0.0, LookValue::Opacity(0.0)),
                Keyframe::new(1.0, LookValue::Opacity(1.0)),
            ],
        );
        s.store.play(content, track, None);
    }
}
