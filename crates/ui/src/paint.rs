//! Paint: lower a laid-out subtree to renderer primitives. UI produces paint
//! data; the renderer owns batching and ordering downstream.
//!
//! [`paint_tree`] walks the tree in pre-order (parent before children, so a
//! container's background paints behind its contents) and emits one
//! [`viso_render::Primitive::Quad`] per visible node from its resolved `world`
//! rect and [`crate::style::BoxStyle`]. A node with a transparent, borderless
//! style contributes nothing — a pure layout container. The walk pushes into a
//! caller-owned `Vec` so frames reuse the buffer. The renderer decides
//! batching/ordering downstream; this only produces the primitive data.
//!
//! Node rects are on-screen rects: `bounds` displaced by the scrolls and
//! translates around the node, carried down the walk (see
//! [`crate::transform`]). A scroll viewport additionally wraps its children in a
//! [`viso_render::Primitive::Layer`]/[`viso_render::Primitive::LayerEnd`] pair
//! clipped to the viewport's own rect, so content scrolled past the edge is
//! clipped away. The viewport's own background quad paints before the clip is
//! pushed, so it fills the whole box regardless of scroll.
//!
//! Each transform root — a scroll viewport's content, a translated node — is
//! bracketed by [`viso_render::Primitive::Transform`] and recorded in a
//! [`TransformTable`], so a frame that only scrolls or slides moves the
//! retained draws instead of painting again.

use std::sync::Arc;

use crate::asset::{AssetDraw, AssetFit};
use crate::check_mark::CheckMark;
use crate::component::NodeStore;
use crate::content::Content;
use crate::field_marks::{FieldMarks, HANDLE, HandleShape};
use crate::layout::Vec2;
use crate::node::NodeId;
use crate::style::BoxStyle;
use crate::theme::Palette;
use crate::transform::TransformTable;
use viso_render::{
    AddressMode, AnalyticEllipse, AnalyticRRect, AnalyticShadow, Border, Corners,
    GlyphInstanceData, GlyphRunDraw, ImageDraw, LayerClip, Path, PathCmd, Point, Primitive,
    ProgramTextures, Quad, Rect, Rgba, RoundMask, SamplerDesc, ShaderDraw, ShadowShape,
    TransformSlot,
};

/// The opaque white tint that passes a premultiplied color-atlas texel through
/// the Image shader unchanged (`texel * float4(1,1,1,1)`), so a color-bitmap
/// glyph paints exactly as its emoji artwork stores it.
const WHITE: Rgba = Rgba {
    r: 1.0,
    g: 1.0,
    b: 1.0,
    a: 1.0,
};

/// Emit primitives for the subtree rooted at `root` into `out`, in pre-order.
/// `out` is not cleared — append semantics let a caller compose multiple trees.
///
/// Overlay nodes (see [`NodeStore::is_overlay`]) paint in a deferred top layer:
/// the main walk skips an overlay subtree where it meets it and records the root;
/// once the main walk finishes, each recorded overlay root paints, in the order
/// they were met (author order). So a popup declared deep in the tree still draws
/// over the whole scene, with no z-index sort. A tree with no overlay node emits
/// exactly the pre-order stream it did before — the deferred pass is empty.
///
/// The transform roots met are recorded into a table this call discards; the
/// frame loop paints through [`paint_with_transforms`] to keep it.
pub fn paint_tree(store: &NodeStore, root: NodeId, out: &mut Vec<Primitive>) {
    paint_with_transforms(store, root, out, &mut TransformTable::default());
}

/// [`paint_tree`], recording every transform root met into `table` (cleared
/// first) in paint order, each root's primitives bracketed by a
/// [`Primitive::Transform`] naming its slot.
pub fn paint_with_transforms(
    store: &NodeStore,
    root: NodeId,
    out: &mut Vec<Primitive>,
    table: &mut TransformTable,
) {
    paint_walk(store, root, out, table, &mut ());
}

/// [`paint_with_transforms`], recording into `owned` each node painted and
/// the stream range of its own drawables — its box and its content, not its
/// descendants' — in paint order.
pub fn paint_owned(
    store: &NodeStore,
    root: NodeId,
    out: &mut Vec<Primitive>,
    table: &mut TransformTable,
    owned: &mut Vec<(NodeId, core::ops::Range<usize>)>,
) {
    struct Owned<'a>(&'a mut Vec<(NodeId, core::ops::Range<usize>)>);
    impl PaintSpans for Owned<'_> {
        fn open(&mut self, _: NodeId, _: usize) -> usize {
            0
        }
        fn close(&mut self, _: usize, node: NodeId, span: Span) {
            self.0.push((node, span.own..span.own_end));
        }
    }
    paint_walk(store, root, out, table, &mut Owned(owned));
}

mod splice;

pub(crate) use splice::PaintIndex;

/// What the last paint left: its transform roots, where each node's
/// primitives sit, and what the last incremental paint rewrote. One box, so a
/// walk that reads the store beside it moves one pointer.
#[derive(Debug, Default)]
pub(crate) struct Painting {
    pub(crate) transforms: TransformTable,
    pub(crate) index: PaintIndex,
    /// The stream positions the last patch rewrote, ascending.
    pub(crate) patched: Vec<u32>,
    /// The runs of the stream the last splice replaced.
    pub(crate) splices: Vec<viso_render::Splice>,
}

/// A store's [`Painting`], lent out while a walk reads the store beside it.
#[derive(Debug)]
pub(crate) struct PaintSlot(Option<Box<Painting>>);

impl Default for PaintSlot {
    fn default() -> Self {
        PaintSlot(Some(Box::default()))
    }
}

impl PaintSlot {
    const LENT: &str = "the paint state is lent only for the length of a walk";

    pub(crate) fn get(&self) -> &Painting {
        self.0.as_deref().expect(Self::LENT)
    }

    pub(crate) fn get_mut(&mut self) -> &mut Painting {
        self.0.as_deref_mut().expect(Self::LENT)
    }

    pub(crate) fn lend(&mut self) -> Box<Painting> {
        self.0.take().expect(Self::LENT)
    }

    pub(crate) fn give_back(&mut self, painting: Box<Painting>) {
        self.0 = Some(painting);
    }
}

/// What a paint walk records besides its primitives: the span of the stream
/// each node emitted, its own primitives through its last descendant's.
pub(crate) trait PaintSpans {
    /// `node` starts emitting at `start`; the returned token closes its span.
    fn open(&mut self, node: NodeId, start: usize) -> usize;
    /// The span `node` opened as `token` is out.
    fn close(&mut self, token: usize, node: NodeId, span: Span);
}

/// Where one node's primitives landed.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Span {
    /// Its own drawables — its box and its content.
    pub(crate) own: usize,
    pub(crate) own_end: usize,
    /// One past its last primitive, its closing markers included; its
    /// overlays paint later.
    pub(crate) end: usize,
    /// The transform root its own drawables are painted in.
    pub(crate) slot: Option<u32>,
    /// Its parent's slot, [`NodeIndex::NONE`](crate::node::NodeIndex::NONE)'s
    /// at the top, and how deep in the paint it is.
    pub(crate) parent: u32,
    pub(crate) depth: u32,
    pub(crate) flags: u8,
}

impl Span {
    /// It fades through a `Layer` just before its own drawables.
    pub(crate) const FADED: u8 = 1;
    /// It is a scroll viewport: its content's `Transform` follows its clip.
    pub(crate) const SCROLL: u8 = 2;
    /// It is a transform root: a `Transform` opens its span.
    pub(crate) const ROOT: u8 = 4;
    /// It clips its children: a `Layer` over its box follows its own
    /// drawables. Every scroll viewport does.
    pub(crate) const CLIP: u8 = 8;
    /// Its clip is rounded: a `ClipCorners` follows the clip's `Layer`.
    pub(crate) const CORNERS: u8 = 16;
}

/// A walk that records nothing; the calls compile away.
impl PaintSpans for () {
    #[inline(always)]
    fn open(&mut self, _: NodeId, _: usize) -> usize {
        0
    }

    #[inline(always)]
    fn close(&mut self, _: usize, _: NodeId, _: Span) {}
}

pub(crate) use splice::{Spliced, paint_indexed, splice_walk};

/// The one paint walk, shared by the frame and the inspector.
pub(crate) fn paint_walk<S: PaintSpans>(
    store: &NodeStore,
    root: NodeId,
    out: &mut Vec<Primitive>,
    table: &mut TransformTable,
    spans: &mut S,
) {
    table.clear();
    // The top-layer roots met during the main walk, each with the space it was
    // met in; the table's scratch, so a retained table allocates nothing.
    let mut overlays = core::mem::take(&mut table.overlays);
    overlays.clear();
    let top = top_space(store, root);
    // The `root` itself paints even if flagged overlay — it is the entry, not a
    // child boundary — so the main walk starts unconditionally.
    paint_subtree(store, root, top, out, table, &mut overlays, spans);
    // Deferred top layer: paint each overlay root in the order it was met, inside
    // the root it was met in so it moves with it. An overlay's own subtree may in
    // turn contain further overlays, appended past the current end, so the index
    // walk naturally drains nested ones too.
    let mut i = 0;
    while i < overlays.len() {
        let (node, space) = overlays[i];
        i += 1;
        // One that paints nothing opens no transform either.
        if store.hidden(node) || store.opacity(node) <= 0.0 {
            continue;
        }
        if let Some(slot) = space.slot {
            out.push(Primitive::Transform(TransformSlot(slot)));
        }
        paint_subtree(store, node, space, out, table, &mut overlays, spans);
        if space.slot.is_some() {
            out.push(Primitive::TransformEnd);
        }
    }
    table.overlays = overlays;
}

/// Where a node sits: the innermost transform root around it, that root's
/// displacement, unsnapped and as painted, and how deep in the paint the
/// node is.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Space {
    slot: Option<u32>,
    raw: Vec2,
    snapped: Vec2,
    depth: u32,
}

impl Space {
    /// This space moved to `raw`, as transform root `slot`.
    fn moved(self, store: &NodeStore, slot: u32, raw: Vec2) -> Space {
        Space {
            slot: Some(slot),
            raw,
            snapped: store.snap_shift(raw),
            ..self
        }
    }
}

/// The space of the nodes outside every transform root of a paint from
/// `root`.
fn top_space(store: &NodeStore, root: NodeId) -> Space {
    let raw = if store.arena().is_live(root) {
        store.shift_above(root)
    } else {
        Vec2::ZERO
    };
    Space {
        slot: None,
        raw,
        snapped: store.snap_shift(raw),
        depth: 0,
    }
}

/// Paint the children of the layered stack `root` by ascending layer, those
/// of one layer in source order.
#[cold]
#[inline(never)]
fn paint_layered<S: PaintSpans>(
    store: &NodeStore,
    root: NodeId,
    space: Space,
    out: &mut Vec<Primitive>,
    table: &mut TransformTable,
    overlays: &mut Vec<(NodeId, Space)>,
    spans: &mut S,
) {
    let start = table.layered.len();
    for (k, c) in store.arena().children(root).enumerate() {
        let key = (i64::from(store.stack_layer(c)) << 32) + k as i64;
        table.layered.push((key, c));
    }
    table.layered[start..].sort_unstable_by_key(|&(key, _)| key);
    let end = table.layered.len();
    // Painting a child pushes past this run and truncates back to it.
    for k in start..end {
        let c = table.layered[k].1;
        if store.is_overlay(c) {
            overlays.push((c, space));
        } else {
            paint_subtree(store, c, space, out, table, overlays, spans);
        }
    }
    table.layered.truncate(start);
}

/// Paint one subtree in pre-order, diverting any overlay child into `overlays`
/// instead of descending into it (the caller drains that list into a top layer).
/// A stack paints its children by ascending layer, those of one layer in
/// source order.
fn paint_subtree<S: PaintSpans>(
    store: &NodeStore,
    root: NodeId,
    space: Space,
    out: &mut Vec<Primitive>,
    table: &mut TransformTable,
    overlays: &mut Vec<(NodeId, Space)>,
    spans: &mut S,
) {
    let arena = store.arena();
    if !arena.is_live(root) {
        return;
    }
    // A hidden node paints neither its own quad/content nor any descendant, and
    // neither does a fully transparent one.
    let opacity = store.opacity(root);
    if store.hidden(root) || opacity <= 0.0 {
        return;
    }

    let span = spans.open(root, out.len());
    // A translated node is a transform root: it and its subtree move together.
    let mut space = space;
    let own = store.is_transform_root(root);
    if own {
        let raw = space.raw + store.self_shift(root);
        let slot = table.push(store, root, space.slot, false, store.snap_shift(raw));
        space = space.moved(store, slot, raw);
        out.push(Primitive::Transform(TransformSlot(slot)));
    }

    let world = store.bounds(root).translate(space.snapped);
    // A glass box's surface lies under everything the node draws and outside
    // its fade, which would drop the blur: it fades by its own opacity.
    if let Some(backdrop) = store.backdrop(root) {
        let radius = corner_radius(&store.style(root), world);
        out.push(Primitive::Frosted(Box::new(backdrop.surface(
            world,
            radius,
            opacity,
            store.backdrops.access,
        ))));
    }
    // A translucent node composites itself and its subtree as one layer over
    // all its subtree draws: its reach.
    let faded = opacity < 1.0;
    if faded {
        out.push(Primitive::Layer(LayerClip {
            clip: store.reach(root).translate(space.snapped),
            opacity,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }));
    }
    let own_start = out.len();
    paint_own(store, root, world, out);
    let own_end = out.len();

    // A clipping node — a scroll viewport, or one asked to `clip` — clips its
    // children to its own box, rounded by its corners; its own drawables
    // already painted above, so they are not clipped. A scroll viewport's
    // content is a transform root moved by the scroll. An ordinary container
    // pushes nothing.
    let scroll = store.is_scroll(root);
    let clips = store.clips_children(root);
    let mut corners = None;
    let mut inner = space;
    if clips {
        corners = clip_corners(store, root, world);
        out.push(Primitive::Layer(LayerClip {
            clip: world,
            opacity: 1.0,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }));
        if let Some(corners) = corners {
            out.push(Primitive::ClipCorners(corners));
        }
    }
    if scroll {
        let raw = space.raw + store.content_shift(root);
        let slot = table.push(store, root, space.slot, true, store.snap_shift(raw));
        inner = space.moved(store, slot, raw);
        out.push(Primitive::Transform(TransformSlot(slot)));
    }

    // Recurse into children in sibling order (pre-order: parent already painted).
    // An overlay child is diverted to the deferred top layer rather than painted
    // in place — it draws after the whole scene, so it must not descend here.
    inner.depth += 1;
    if store.is_layered(root) {
        paint_layered(store, root, inner, out, table, overlays, spans);
    } else if scroll {
        // A viewport's chrome does not scroll: it paints after the content's
        // transform ends.
        for c in arena.children(root).filter(|&c| !store.is_chrome(c)) {
            if store.is_overlay(c) {
                overlays.push((c, inner));
            } else {
                paint_subtree(store, c, inner, out, table, overlays, spans);
            }
        }
        out.push(Primitive::TransformEnd);
        let mut outer = space;
        outer.depth += 1;
        for c in arena.children(root).filter(|&c| store.is_chrome(c)) {
            paint_subtree(store, c, outer, out, table, overlays, spans);
        }
    } else {
        for c in arena.children(root) {
            if store.is_overlay(c) {
                overlays.push((c, inner));
            } else {
                paint_subtree(store, c, inner, out, table, overlays, spans);
            }
        }
    }

    if scroll && store.is_layered(root) {
        out.push(Primitive::TransformEnd);
    }
    if clips {
        out.push(Primitive::LayerEnd);
    }
    if faded {
        out.push(Primitive::LayerEnd);
    }
    // The focus ring goes round the node over its subtree, outside its own
    // clip, moving with it.
    if let Some(progress) = store.focus_ring_on(root) {
        paint_focus_ring(store, root, world, progress, out);
    }
    if own {
        out.push(Primitive::TransformEnd);
    }
    let flag = |on: bool, flag: u8| if on { flag } else { 0 };
    spans.close(
        span,
        root,
        Span {
            own: own_start,
            own_end,
            end: out.len(),
            slot: space.slot,
            parent: arena.parent_of(root.node_index()).get(),
            depth: space.depth,
            flags: flag(faded, Span::FADED)
                | flag(scroll, Span::SCROLL)
                | flag(own, Span::ROOT)
                | flag(clips, Span::CLIP)
                | flag(corners.is_some(), Span::CORNERS),
        },
    );
}

/// The corner radius `style` rounds a box of `rect` with: its fixed part and
/// its share of the shorter side, no further than half that side (a pill).
#[inline]
fn corner_radius(style: &BoxStyle, rect: Rect) -> f32 {
    let side = rect.w.min(rect.h);
    (style.radius + style.radius_pct * side)
        .min(side * 0.5)
        .max(0.0)
}

/// The corners a clipping `node` at `world` rounds its clip with, `None`
/// when they are sharp.
#[inline]
pub(crate) fn clip_corners(store: &NodeStore, node: NodeId, world: Rect) -> Option<Corners> {
    let radius = corner_radius(&store.style(node), world);
    (radius > 0.0).then_some(Corners::uniform(radius))
}

/// A node's own drawables at `world`: its box, then its content over it.
#[inline(always)]
fn paint_own(store: &NodeStore, node: NodeId, world: Rect, out: &mut Vec<Primitive>) {
    let style = store.style(node);
    let radius = corner_radius(&style, world);
    let shadows = store.shadows(node);
    for &shadow in shadows.iter().filter(|s| !s.inset) {
        paint_shadow(shadow, world, radius, out);
    }
    let layers = store.background_layers(node);
    if layers.is_empty() {
        if style.is_visible() {
            out.push(Primitive::Quad(Quad {
                rect: world,
                color: style.fill,
                radius,
                border: style.border,
            }));
        }
    } else {
        // The layers lie between the fill and the border.
        if style.fill.a > 0.0 {
            out.push(Primitive::Quad(Quad {
                rect: world,
                color: style.fill,
                radius,
                border: Border::NONE,
            }));
        }
        crate::background::paint_layers(store, layers, world, radius, out);
        if style.border.width > 0.0 {
            out.push(Primitive::Quad(Quad {
                rect: world,
                color: Rgba::TRANSPARENT,
                radius,
                border: style.border,
            }));
        }
    }
    if shadows.iter().any(|s| s.inset) {
        let width = style.border.width.max(0.0);
        let inner = Rect {
            x: world.x + width,
            y: world.y + width,
            w: (world.w - 2.0 * width).max(0.0),
            h: (world.h - 2.0 * width).max(0.0),
        };
        for &shadow in shadows.iter().filter(|s| s.inset) {
            paint_shadow(shadow, inner, (radius - width).max(0.0), out);
        }
    }
    if store.is_rippling() {
        let body = store.check_mark(node).and_then(|m| m.body_radius(world));
        store.paint_ripples(node, world, body.unwrap_or(radius), style.fill, out);
    }
    // Drawable content (text/image/path) paints over the node's background. Its
    // coordinates are node-local, so shift them by the node's world origin — a
    // scrolled or repositioned node then draws its content in the right place.
    // An image is rounded by the box's corners.
    // A check's caption is its content: one with none draws nothing until
    // its caption is shaped.
    if let Some(content) = store.content_payload(node) {
        if let Some(mark) = store.check_mark(node) {
            paint_check(store, node, mark, content, world, out);
            return;
        }
        // Its content lies in its box less its padding.
        let inner = crate::content_inset::inset_rect(world, store.content_inset(node));
        let ring = if let Some(marks) = store.field_marks(node) {
            paint_field(
                marks,
                store.palette(),
                content,
                inner,
                store.text_shadows(node),
                out,
            );
            if marks.handles.is_some() {
                paint_handles(store.field_handles(node), store.palette(), out);
            }
            marks.ring
        } else {
            if let Content::Progress(progress) = content {
                let rtl = store.layout_direction(node) == crate::adaptive::LayoutDirection::Rtl;
                progress.paint(world, rtl, out);
            } else if let Content::Stepper(stepper) = content {
                stepper.paint(world, store.reads_rtl(node), out);
            } else if let Content::Slider(slider) = content {
                slider.paint(world, store.reads_rtl(node), out);
            } else if let Content::ColorPlane(plane) = content {
                plane.paint(world, out);
            } else if let Content::Plot(plot) = content {
                plot.paint(world, store.reads_rtl(node), out);
            } else if let Content::Segments(segments) = content {
                segments.paint(store, node, world, out);
            } else if let Content::Days(days) = content {
                days.paint(store, node, world, out);
            } else {
                let mask = (radius > 0.0).then_some(RoundMask {
                    rect: world,
                    radius: Corners::uniform(radius),
                });
                let shadows = store.text_shadows(node);
                if !shadows.is_empty() {
                    crate::text_shadow::paint_text_shadows(content, inner, shadows, false, out);
                }
                paint_content(content, inner, mask, store.access_cues(), out);
                if !shadows.is_empty() {
                    crate::text_shadow::paint_text_shadows(content, inner, shadows, true, out);
                }
            }
            false
        };
        let link = store
            .focused_link(node)
            .filter(|_| store.in_state(node, crate::interaction::Interaction::FocusVisible));
        match link {
            Some(link) => {
                for rect in store.link_boxes(node, link) {
                    paint_ring(store, rect, out);
                }
            }
            None if ring => paint_ring(store, world, out),
            None => {}
        }
    }
}

/// The shadow a box of `world` rounded by `radius` casts under itself, or
/// within itself when the shadow is inset.
#[cold]
fn paint_shadow(shadow: crate::shadow::Shadow, world: Rect, radius: f32, out: &mut Vec<Primitive>) {
    out.push(Primitive::AnalyticShadow(AnalyticShadow {
        rect: world,
        color: shadow.color,
        radius: Corners::uniform(radius),
        offset: [shadow.offset.x, shadow.offset.y],
        sigma: shadow.blur * 0.5,
        spread: shadow.spread,
        shape: ShadowShape::RoundedBox,
        inner: shadow.inset,
    }));
}

/// The focus ring round `rect`, worn by a focused link (round each of its
/// line boxes) or a read-only field while the keyboard moved the focus.
#[cold]
fn paint_ring(store: &NodeStore, rect: Rect, out: &mut Vec<Primitive>) {
    store.focus_ring_style().paint(rect, 0.0, 1.0, out);
}

/// The focus ring round focused `node` at `world`, `progress` of the way in,
/// rounded as its corners are.
#[cold]
fn paint_focus_ring(
    store: &NodeStore,
    node: NodeId,
    world: Rect,
    progress: f32,
    out: &mut Vec<Primitive>,
) {
    let radius = corner_radius(&store.style(node), world);
    store.focus_ring_style().paint(world, radius, progress, out);
}

/// A text field at `world`, clipped to it: its selection, its text `content`,
/// its composition's underline and its caret, all shifted by its scroll.
#[cold]
fn paint_field(
    marks: &FieldMarks,
    palette: &Palette,
    content: &Content,
    world: Rect,
    shadows: &[crate::text_shadow::TextShadow],
    out: &mut Vec<Primitive>,
) {
    out.push(Primitive::Layer(LayerClip {
        clip: world,
        opacity: 1.0,
        blur_sigma: 0.0,
        backdrop_sigma: 0.0,
    }));
    let (ox, oy) = (world.x - marks.scroll.x, world.y - marks.scroll.y);
    let quad = |r: &Rect, color: Rgba| {
        Primitive::Quad(Quad {
            rect: Rect::new(ox + r.x, oy + r.y, r.w, r.h),
            color,
            radius: 0.0,
            border: Border::NONE,
        })
    };
    out.extend(
        marks
            .selection
            .iter()
            .map(|r| quad(r, palette.resolve(marks.selection_color))),
    );
    let text = Rect {
        x: ox,
        y: oy,
        ..world
    };
    if !shadows.is_empty() {
        crate::text_shadow::paint_text_shadows(content, text, shadows, false, out);
    }
    paint_content(content, text, None, false, out);
    if !shadows.is_empty() {
        crate::text_shadow::paint_text_shadows(content, text, shadows, true, out);
    }
    let ink = palette.resolve(marks.ink);
    out.extend(marks.underlines.iter().map(|r| quad(r, ink)));
    out.extend(marks.caret.iter().map(|r| quad(r, ink)));
    out.push(Primitive::LayerEnd);
}

/// A field's selection handles, over everything it draws and unclipped by
/// its box.
#[cold]
fn paint_handles(handles: [Option<HandleShape>; 2], palette: &Palette, out: &mut Vec<Primitive>) {
    let color = palette.resolve(HANDLE);
    for shape in handles.into_iter().flatten() {
        if let Some(stem) = shape.stem {
            out.push(Primitive::Quad(Quad {
                rect: stem,
                color,
                radius: 0.0,
                border: Border::NONE,
            }));
        }
        out.push(match shape.tear {
            Some(radius) => Primitive::AnalyticRRect(AnalyticRRect {
                rect: shape.knob,
                color,
                radius,
                border: Border::NONE,
            }),
            None => Primitive::AnalyticEllipse(AnalyticEllipse {
                rect: shape.knob,
                color,
                border: Border::NONE,
            }),
        });
    }
}

/// A check at `world`: its mark, then its caption `content` beside it, the
/// mark inked with the caption's color.
#[cold]
fn paint_check(
    store: &NodeStore,
    node: NodeId,
    mark: CheckMark,
    content: &Content,
    world: Rect,
    out: &mut Vec<Primitive>,
) {
    let (text, baseline) = crate::component::caption_of(Some(content));
    let rtl = store.layout_direction(node) == crate::adaptive::LayoutDirection::Rtl;
    let size = Vec2 {
        x: world.w,
        y: world.h,
    };
    let shown = store.check_progress(node);
    let place = mark.place(text, baseline, size, rtl, shown);
    let ink = match content {
        Content::Text { color, .. } => *color,
        _ => WHITE,
    };
    let mark_rect = Rect {
        x: world.x + place.mark.x,
        y: world.y + place.mark.y,
        ..place.mark
    };
    mark.paint(mark_rect, ink, shown, rtl, out);
    let caption = Rect {
        x: world.x + place.text.x,
        y: world.y + place.text.y,
        w: text.x,
        h: text.y,
    };
    paint_content(content, caption, None, store.access_cues(), out);
}

/// Lower one node's [`Content`] to a primitive, translating its node-local
/// coordinates by the node's `world` origin. The image variant fills the node's
/// whole `world` box (a content leaf sizes to the image's intrinsic size, so the
/// box already matches unless a fixed size overrides it). `mask` rounds the
/// images it draws; a text's access-key underlines draw when `cues`.
pub(crate) fn paint_content(
    content: &Content,
    world: Rect,
    mask: Option<RoundMask>,
    cues: bool,
    out: &mut Vec<Primitive>,
) {
    let ox = world.x;
    let oy = world.y;
    match content {
        Content::Text {
            glyphs,
            atlas,
            lane,
            color_glyphs,
            color_atlas,
            color,
            ink,
            ..
        } => {
            let shifted = |glyphs: &[GlyphInstanceData]| {
                glyphs
                    .iter()
                    .map(|g| GlyphInstanceData {
                        rect: Rect {
                            x: g.rect.x + ox,
                            y: g.rect.y + oy,
                            w: g.rect.w,
                            h: g.rect.h,
                        },
                        ..*g
                    })
                    .collect()
            };
            let mark = |mark: &crate::content::TextMark| {
                Primitive::Quad(Quad {
                    rect: Rect::new(ox + mark.rect.x, oy + mark.rect.y, mark.rect.w, mark.rect.h),
                    color: mark.color.unwrap_or(*color),
                    radius: 0.0,
                    border: Border::NONE,
                })
            };
            match ink.as_deref() {
                None => out.push(Primitive::GlyphRun(GlyphRunDraw {
                    glyphs: shifted(glyphs),
                    atlas: *atlas,
                    color: *color,
                    lane: *lane,
                })),
                Some(ink) => {
                    out.extend(ink.fills.iter().map(mark));
                    let mut from = 0;
                    let tail = crate::content::InkRun {
                        end: glyphs.len() as u32,
                        color: None,
                    };
                    for run in ink.runs.iter().chain([&tail]) {
                        let to = (run.end as usize).min(glyphs.len());
                        if from < to {
                            out.push(Primitive::GlyphRun(GlyphRunDraw {
                                glyphs: shifted(&glyphs[from..to]),
                                atlas: *atlas,
                                color: run.color.unwrap_or(*color),
                                lane: *lane,
                            }));
                        }
                        from = from.max(to);
                    }
                    out.extend(ink.lines.iter().map(mark));
                    if cues {
                        out.extend(ink.cues.iter().map(mark));
                    }
                }
            }
            // Color-bitmap glyphs (emoji) reuse the Image pipeline: each is one
            // textured quad sampling the RGBA color atlas at its glyph sub-rect.
            // A white, opaque tint leaves the premultiplied emoji texel unchanged
            // (`texel * float4(1,1,1,1)`), so the color glyph paints as authored.
            // `color_atlas` is `Some` whenever `color_glyphs` is non-empty.
            if let Some(color_atlas) = color_atlas {
                for g in color_glyphs {
                    out.push(Primitive::Image(ImageDraw {
                        rect: Rect {
                            x: g.rect.x + ox,
                            y: g.rect.y + oy,
                            w: g.rect.w,
                            h: g.rect.h,
                        },
                        uv: g.uv,
                        tint: WHITE,
                        texture: *color_atlas,
                        sampler: SamplerDesc::LINEAR_CLAMP,
                        mask: None,
                    }));
                }
            }
        }
        Content::Image {
            texture, uv, tint, ..
        } => {
            out.push(Primitive::Image(ImageDraw {
                rect: world,
                uv: *uv,
                tint: *tint,
                texture: *texture,
                sampler: SamplerDesc::LINEAR_CLAMP,
                mask,
            }));
        }
        Content::Path {
            cmds, fill, stroke, ..
        } => {
            let cmds = cmds.iter().map(|c| translate_cmd(*c, ox, oy)).collect();
            out.push(Primitive::Path(Box::new(Path {
                cmds,
                fill: *fill,
                stroke: *stroke,
                shadow: None,
            })));
        }
        Content::Shader {
            program,
            params,
            images,
        } => {
            let mut textures: ProgramTextures = Default::default();
            for (slot, image) in textures.iter_mut().zip(images.iter()) {
                *slot = image.texture;
            }
            // An image not yet resident: the node draws nothing until it is.
            if images.len() <= textures.len() && images.iter().all(|i| i.texture.is_some()) {
                out.push(Primitive::Shader(Box::new(ShaderDraw {
                    rect: world,
                    program: *program,
                    params: Arc::clone(params),
                    textures,
                })));
            }
        }
        Content::Scene { slot, revision } => out.push(Primitive::Scene(viso_render::SceneDraw {
            rect: world,
            slot: *slot,
            revision: *revision,
            mask,
        })),
        Content::Asset {
            draw,
            natural,
            fit,
            tint,
            mirrored,
            ..
        } => paint_asset(draw, *natural, *fit, *tint, *mirrored, world, mask, out),
        Content::Spinner(spinner) => spinner.paint(world, out),
        Content::Progress(progress) => progress.paint(world, false, out),
        Content::PageDots(dots) => dots.paint(world, out),
        Content::Stepper(stepper) => stepper.paint(world, false, out),
        Content::Slider(slider) => slider.paint(world, false, out),
        Content::ColorPlane(plane) => plane.paint(world, out),
        Content::Shimmer(shimmer) => shimmer.paint(world, out),
        Content::Plot(plot) => plot.paint(world, false, out),
        Content::Drawing(drawing) => drawing.paint(world, out),
        // Segments read their container's children, which only a node's own
        // paint reaches.
        Content::Segments(_) | Content::Days(_) => {}
    }
}

/// An asset of `natural` size placed into `world` by `fit`, a raster
/// flipped left to right when `mirrored`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn paint_asset(
    draw: &AssetDraw,
    natural: Vec2,
    fit: AssetFit,
    tint: Option<Rgba>,
    mirrored: bool,
    world: Rect,
    mask: Option<RoundMask>,
    out: &mut Vec<Primitive>,
) {
    if natural.x <= 0.0 || natural.y <= 0.0 || world.w <= 0.0 || world.h <= 0.0 {
        return;
    }
    let scale = match fit {
        AssetFit::Contain => (world.w / natural.x).min(world.h / natural.y),
        AssetFit::Cover => (world.w / natural.x).max(world.h / natural.y),
        AssetFit::Fill | AssetFit::Tile => 0.0,
    };
    // The box the whole asset takes, centered on the node's; a tile's
    // first copy, at the node's top-left corner.
    let placed = if fit == AssetFit::Fill {
        world
    } else if fit == AssetFit::Tile {
        Rect {
            x: world.x,
            y: world.y,
            w: natural.x,
            h: natural.y,
        }
    } else {
        let (w, h) = (natural.x * scale, natural.y * scale);
        Rect {
            x: world.x + (world.w - w) * 0.5,
            y: world.y + (world.h - h) * 0.5,
            w,
            h,
        }
    };
    match draw {
        AssetDraw::Nothing | AssetDraw::Failed => {}
        AssetDraw::Raster(texture) => {
            // A cover crops in the texture rather than drawing past the box;
            // a tile reads past it, the sampler wrapping it round.
            let (rect, mut uv) = if fit == AssetFit::Tile {
                let uv = Rect {
                    x: 0.0,
                    y: 0.0,
                    w: world.w / natural.x,
                    h: world.h / natural.y,
                };
                (world, uv)
            } else if fit == AssetFit::Cover {
                let (uw, uh) = (world.w / placed.w, world.h / placed.h);
                let uv = Rect {
                    x: (1.0 - uw) * 0.5,
                    y: (1.0 - uh) * 0.5,
                    w: uw,
                    h: uh,
                };
                (world, uv)
            } else {
                let full = Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                };
                (placed, full)
            };
            // Mirrored, the texture is read from its right edge leftward.
            if mirrored {
                uv.x += uv.w;
                uv.w = -uv.w;
            }
            out.push(Primitive::Image(ImageDraw {
                rect,
                uv,
                tint: tint.unwrap_or(WHITE),
                texture: *texture,
                sampler: if fit == AssetFit::Tile {
                    SamplerDesc {
                        address: AddressMode::Repeat,
                        ..SamplerDesc::LINEAR_CLAMP
                    }
                } else {
                    SamplerDesc::LINEAR_CLAMP
                },
                mask,
            }));
        }
        AssetDraw::Vector(scene) => {
            let crop = matches!(fit, AssetFit::Cover | AssetFit::Tile);
            if crop {
                out.push(Primitive::Layer(LayerClip {
                    clip: world,
                    opacity: 1.0,
                    blur_sigma: 0.0,
                    backdrop_sigma: 0.0,
                }));
            }
            if fit == AssetFit::Tile {
                // A copy a cell, as many as reach into the box.
                let columns = (world.w / natural.x).ceil() as u32;
                let rows = (world.h / natural.y).ceil() as u32;
                for row in 0..rows {
                    for column in 0..columns {
                        let cell = Rect {
                            x: placed.x + column as f32 * natural.x,
                            y: placed.y + row as f32 * natural.y,
                            ..placed
                        };
                        out.extend(scene.place(cell, tint).map(Primitive::from));
                    }
                }
            } else {
                out.extend(scene.place(placed, tint).map(Primitive::from));
            }
            if crop {
                out.push(Primitive::LayerEnd);
            }
        }
    }
}

/// Translate a path command's points by `(dx, dy)`.
#[inline]
fn translate_cmd(cmd: PathCmd, dx: f32, dy: f32) -> PathCmd {
    let t = |p: Point| Point::new(p.x + dx, p.y + dy);
    match cmd {
        PathCmd::MoveTo(p) => PathCmd::MoveTo(t(p)),
        PathCmd::LineTo(p) => PathCmd::LineTo(t(p)),
        PathCmd::QuadTo(c, p) => PathCmd::QuadTo(t(c), t(p)),
        PathCmd::CubicTo(c1, c2, p) => PathCmd::CubicTo(t(c1), t(c2), t(p)),
        PathCmd::Close => PathCmd::Close,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset::AssetBytes;
    use crate::component::{BuildCx, FlexStyle, LeafStyle, NodeStore};
    use crate::dirty::DirtyClass;
    use crate::layout::{Axis, Sizing};
    use crate::style::BoxStyle;
    use viso_render::{Rect, Rgba};

    const RED: Rgba = Rgba {
        r: 1.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    };

    #[test]
    fn a_translucent_node_paints_its_subtree_as_one_layer() {
        let mut store = NodeStore::new();
        let (root, leaf) = {
            let mut cx = BuildCx::new(&mut store);
            let mut leaf = None;
            cx.flex(FlexStyle::default(), |cx| {
                leaf = Some(
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(10.0, 10.0),
                        style: BoxStyle::solid(RED),
                    })
                    .id(),
                );
            });
            (cx.root().unwrap(), leaf.unwrap())
        };
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 100.0,
        };
        store.layout(root, surface, &mut Vec::new());
        store.set_opacity(root, 0.5);
        assert!(store.dirty(root).contains(DirtyClass::PAINT));
        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        assert_eq!(out.len(), 3, "layer, leaf, layer end");
        assert!(matches!(out[0], Primitive::Layer(l) if l.opacity == 0.5));
        assert!(matches!(out[2], Primitive::LayerEnd));

        let green = Rgba { g: 1.0, ..RED };
        store.set_fill(leaf, green);
        store.set_opacity(root, 0.0);
        out.clear();
        paint_tree(&store, root, &mut out);
        assert!(out.is_empty(), "a transparent node paints nothing");
        store.set_opacity(root, f32::NAN);
        out.clear();
        paint_tree(&store, root, &mut out);
        assert!(matches!(out[..], [Primitive::Quad(q)] if q.color == green));
    }

    #[test]
    fn transparent_container_emits_only_visible_leaves() {
        let mut store = NodeStore::new();
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    // Transparent container: no background quad.
                    ..Default::default()
                },
                |cx| {
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(10.0, 10.0),
                        style: BoxStyle::solid(RED),
                    });
                    // A transparent leaf contributes nothing.
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(10.0, 10.0),
                        style: BoxStyle::NONE,
                    });
                },
            );
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            &mut scratch,
        );
        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        // Only the one solid leaf paints.
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Primitive::Quad(q) if q.color == RED));
    }

    #[test]
    fn a_hidden_node_emits_no_primitive_for_itself_or_its_subtree() {
        // A row of two solid leaves; hiding the first must drop both its own quad
        // and (were it a container) its descendants — the paint walk early-returns.
        let mut store = NodeStore::new();
        let mut first = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    ..Default::default()
                },
                |cx| {
                    let h = cx.leaf(LeafStyle {
                        size: Sizing::fixed(10.0, 10.0),
                        style: BoxStyle::solid(RED),
                    });
                    first = Some(h.id());
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(10.0, 10.0),
                        style: BoxStyle::solid(RED),
                    });
                },
            );
            cx.root().unwrap()
        };
        store.set_hidden(first.unwrap(), true);
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            &mut scratch,
        );
        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        // Only the still-shown leaf paints; the hidden one contributes nothing.
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], Primitive::Quad(q) if q.color == RED));
    }

    #[test]
    fn a_clipping_node_rounds_the_layer_its_children_paint_in() {
        let mut store = NodeStore::new();
        let mut child = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    size: Sizing::fixed(100.0, 100.0),
                    style: BoxStyle {
                        radius: 8.0,
                        ..BoxStyle::solid(RED)
                    },
                    ..Default::default()
                },
                |cx| {
                    child = Some(
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(100.0, 300.0),
                            style: BoxStyle::solid(RED),
                        })
                        .id(),
                    );
                },
            );
            cx.root().unwrap()
        };
        store.set_clip(root, true);
        store.layout(root, Rect::new(0.0, 0.0, 100.0, 100.0), &mut Vec::new());

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        // Its own box, then the clip rounded by its corners, then the child
        // inside it.
        assert_eq!(out.len(), 5);
        assert!(matches!(out[0], Primitive::Quad(q) if q.radius == 8.0));
        assert!(matches!(out[1], Primitive::Layer(l) if l.clip == store.world(root)));
        assert!(matches!(out[2], Primitive::ClipCorners(c) if c == Corners::uniform(8.0)));
        assert!(matches!(out[3], Primitive::Quad(q) if q.rect == store.world(child.unwrap())));
        assert!(matches!(out[4], Primitive::LayerEnd));

        store.write_corner_radius(root, 0.0, 0.0);
        out.clear();
        paint_tree(&store, root, &mut out);
        assert_eq!(out.len(), 4, "a sharp clip needs no corners");
        assert!(matches!(out[1], Primitive::Layer(_)));
        assert!(matches!(out[2], Primitive::Quad(_)));
    }

    #[test]
    fn scroll_viewport_clips_content_and_offsets_by_scroll() {
        use crate::component::ScrollStyle;
        use crate::layout::Vec2;

        let mut store = NodeStore::new();
        let mut content = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.scroll(
                ScrollStyle {
                    axis: Axis::Column,
                    across: false,
                    size: Sizing::fixed(100.0, 100.0),
                    // A solid viewport background so its quad paints before the clip.
                    style: BoxStyle::solid(RED),
                    bars: crate::scroll_bar::ScrollBars::Hidden,
                    tail: false,
                    refresh: false,
                    snap: None,
                    dots: None,
                },
                |cx| {
                    content = Some(
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(100.0, 300.0),
                            style: BoxStyle::solid(RED),
                        })
                        .id(),
                    );
                },
            );
            cx.root().unwrap()
        };
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            &mut scratch,
        );
        store.scroll_by(root, Vec2 { x: 0.0, y: 40.0 });

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);

        // viewport background quad, then the clip layer, then the content in its
        // transform root, then the layer end — the background is outside the
        // clip, the content in.
        assert_eq!(out.len(), 6);
        assert!(matches!(out[0], Primitive::Quad(q) if q.rect == store.world(root)));
        let clip = match out[1] {
            Primitive::Layer(l) => l,
            _ => panic!("expected a clip layer around the content"),
        };
        assert_eq!(clip.clip, store.world(root), "clip is the viewport box");
        assert!(matches!(out[2], Primitive::Transform(TransformSlot(0))));
        // The content quad is shifted up by the scroll offset.
        let cw = store.world(content.unwrap());
        assert!(matches!(out[3], Primitive::Quad(q) if q.rect == cw));
        assert_eq!(cw.y, store.bounds(content.unwrap()).y - 40.0);
        assert!(matches!(out[4], Primitive::TransformEnd));
        assert!(matches!(out[5], Primitive::LayerEnd));
    }

    #[test]
    fn a_field_draws_its_selection_text_and_caret_scrolled_and_clipped_to_its_box() {
        use crate::content::Content;
        use crate::field_marks::FieldMarks;
        use crate::layout::{Length, Vec2};
        use viso_render::{GlyphInstanceData, TextureId};

        let mut store = NodeStore::new();
        let mut field = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                let size = Sizing {
                    width: Length::Fixed(50.0),
                    height: Length::Fixed(20.0),
                };
                field = Some(
                    cx.leaf(LeafStyle {
                        size,
                        style: BoxStyle::NONE,
                    })
                    .id(),
                );
            });
            cx.root().unwrap()
        };
        let field = field.unwrap();
        store.set_content_payload(
            field,
            Content::Text {
                ink: None,
                glyphs: vec![GlyphInstanceData {
                    rect: Rect::new(60.0, 3.0, 8.0, 10.0),
                    uv: Rect::new(0.0, 0.0, 1.0, 1.0),
                }],
                atlas: TextureId::new(7),
                lane: viso_render::GlyphLane::CoverageA8,
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: RED,
                natural: Vec2 { x: 70.0, y: 20.0 },
                baseline: 16.0,
                shaped_at_width: None,
                width_use: crate::WidthUse::Ignored,
                min_content: 70.0,
                max_content: 70.0,
            },
        );
        store.make_field(field);
        let mut marks = FieldMarks {
            scroll: Vec2 { x: 30.0, y: 0.0 },
            content: Vec2 { x: 72.0, y: 20.0 },
            selection: vec![Rect::new(40.0, 0.0, 10.0, 20.0)],
            selection_color: RED,
            caret: Some(Rect::new(68.0, 0.0, 1.5, 20.0)),
            ink: RED,
            ..FieldMarks::default()
        };
        store.swap_field_marks(field, &mut marks);
        store.layout(root, Rect::new(0.0, 0.0, 200.0, 100.0), &mut Vec::new());

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        let world = store.world(field);
        let at = out
            .iter()
            .position(|p| matches!(p, Primitive::Layer(l) if l.clip == world))
            .expect("clipped to its box");
        let Primitive::Quad(selection) = out[at + 1] else {
            panic!("the selection behind the text: {out:?}");
        };
        assert_eq!(selection.rect.x, world.x + 10.0);
        let Primitive::GlyphRun(run) = &out[at + 2] else {
            panic!("the text: {out:?}");
        };
        assert_eq!(
            run.glyphs[0].rect.x,
            world.x + 30.0,
            "shifted by the scroll"
        );
        let Primitive::Quad(caret) = out[at + 3] else {
            panic!("the caret over it: {out:?}");
        };
        assert_eq!(caret.rect.x, world.x + 38.0);
        assert!(matches!(out[at + 4], Primitive::LayerEnd));

        assert_eq!(
            store.scroll_field(field, Vec2 { x: 100.0, y: 5.0 }),
            Vec2 { x: -8.0, y: 0.0 },
            "past the text's end it scrolls back to it"
        );
    }

    #[test]
    fn a_check_draws_its_mark_then_its_caption_after_it() {
        use crate::check_mark::{CheckMark, CheckShape};
        use crate::content::Content;
        use crate::layout::{Length, Vec2};
        use viso_render::{GlyphInstanceData, TextureId};

        let mut store = NodeStore::new();
        let mut check = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                let size = Sizing {
                    width: Length::Fit,
                    height: Length::Fit,
                };
                let style = BoxStyle::NONE;
                check = Some(cx.leaf(LeafStyle { size, style }).id());
            });
            cx.root().unwrap()
        };
        let check = check.unwrap();
        store.set_content_payload(
            check,
            Content::Text {
                ink: None,
                glyphs: vec![GlyphInstanceData {
                    rect: Rect {
                        x: 2.0,
                        y: 3.0,
                        w: 8.0,
                        h: 10.0,
                    },
                    uv: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 1.0,
                        h: 1.0,
                    },
                }],
                atlas: TextureId::new(7),
                lane: viso_render::GlyphLane::CoverageA8,
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: RED,
                natural: Vec2 { x: 30.0, y: 20.0 },
                baseline: 16.0,
                shaped_at_width: None,
                width_use: crate::WidthUse::Ignored,
                min_content: 30.0,
                max_content: 30.0,
            },
        );
        let mark = |checked| {
            Some(CheckMark {
                shape: CheckShape::Box,
                checked,
            })
        };
        store.set_check_mark(check, mark(true));
        store.layout(root, Rect::new(0.0, 0.0, 200.0, 100.0), &mut Vec::new());
        // A 20 box, a 10 gap, then the caption.
        assert_eq!((store.bounds(check).w, store.bounds(check).h), (60.0, 20.0));

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        let world = store.world(check);
        let Primitive::Quad(mark_box) = out[0] else {
            panic!("the box first: {out:?}");
        };
        assert_eq!(mark_box.rect, Rect::new(world.x, world.y, 20.0, 20.0));
        assert_eq!(
            mark_box.color, RED,
            "a checked box filled with the caption's ink"
        );
        assert!(matches!(out[1], Primitive::Path(_)), "its tick");
        let Primitive::GlyphRun(run) = &out[2] else {
            panic!("the caption after: {out:?}");
        };
        assert_eq!(run.glyphs[0].rect.x, world.x + 30.0 + 2.0);

        // Unchecking fades the fill and draws the tick back along its stroke.
        store.set_check_mark(check, mark(false));
        store.tick_transitions(core::time::Duration::from_millis(50));
        out.clear();
        paint_tree(&store, root, &mut out);
        let Primitive::Quad(fading) = out[0] else {
            panic!("the box first: {out:?}");
        };
        assert!(fading.color.a > 0.0 && fading.color.a < 1.0, "{fading:?}");
        let Primitive::Path(tick) = &out[1] else {
            panic!("a shorter tick: {out:?}");
        };
        assert!(tick.cmds.len() <= 3);

        // Unchecked, the box is an outline with no tick.
        store.tick_transitions(core::time::Duration::from_millis(200));
        out.clear();
        paint_tree(&store, root, &mut out);
        assert!(matches!(out[0], Primitive::Quad(q) if q.color.a == 0.0 && q.border.width > 0.0));
        assert!(matches!(out[1], Primitive::GlyphRun(_)));
        assert!(!store.is_transitioning());
    }

    #[test]
    fn content_emits_after_background_and_offsets_by_world() {
        use crate::content::Content;
        use crate::layout::{Length, Vec2};
        use viso_render::{GlyphInstanceData, PathCmd, Point, TextureId};

        // A row so the leaf sits at a nonzero world x, exercising the local→world
        // translation of glyph and path coordinates.
        let mut store = NodeStore::new();
        let mut text_id = None;
        let mut path_id = None;
        let mut image_id = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                text_id = Some(
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(30.0, 20.0),
                        style: BoxStyle::solid(RED),
                    })
                    .id(),
                );
                image_id = Some(
                    cx.leaf(LeafStyle {
                        size: Sizing {
                            width: Length::Fit,
                            height: Length::Fit,
                        },
                        style: BoxStyle::NONE,
                    })
                    .id(),
                );
                path_id = Some(
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(16.0, 16.0),
                        style: BoxStyle::NONE,
                    })
                    .id(),
                );
            });
            cx.root().unwrap()
        };
        let text_id = text_id.unwrap();
        let image_id = image_id.unwrap();
        let path_id = path_id.unwrap();

        // A one-glyph run positioned at local (2,3).
        store.set_content_payload(
            text_id,
            Content::Text {
                ink: None,
                glyphs: vec![GlyphInstanceData {
                    rect: Rect {
                        x: 2.0,
                        y: 3.0,
                        w: 8.0,
                        h: 10.0,
                    },
                    uv: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 1.0,
                        h: 1.0,
                    },
                }],
                atlas: TextureId::new(7),
                lane: viso_render::GlyphLane::CoverageA8,
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: RED,
                natural: Vec2 { x: 30.0, y: 20.0 },
                baseline: 16.0,
                shaped_at_width: None,
                width_use: crate::WidthUse::Ignored,
                min_content: 30.0,
                max_content: 30.0,
            },
        );
        // An image whose Fit box comes from its intrinsic size.
        store.set_content_payload(
            image_id,
            Content::Image {
                texture: TextureId::new(9),
                uv: Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                },
                tint: Rgba {
                    r: 1.0,
                    g: 1.0,
                    b: 1.0,
                    a: 1.0,
                },
                natural: Vec2 { x: 40.0, y: 20.0 },
            },
        );
        // A path with one moveto/lineto at local coords.
        store.set_content_payload(
            path_id,
            Content::Path {
                cmds: vec![
                    PathCmd::MoveTo(Point::new(1.0, 1.0)),
                    PathCmd::LineTo(Point::new(5.0, 5.0)),
                ],
                fill: Some(RED),
                stroke: None,
                natural: Vec2 { x: 16.0, y: 16.0 },
            },
        );

        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 100.0,
            },
            &mut scratch,
        );

        // The image leaf's box is its intrinsic size (Fit both axes).
        let img_box = store.bounds(image_id);
        assert_eq!(img_box.w, 40.0);
        assert_eq!(img_box.h, 20.0);

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);

        // text leaf: background quad, then a glyph run shifted to world origin.
        let text_world = store.world(text_id);
        let run = out
            .iter()
            .find_map(|p| match p {
                Primitive::GlyphRun(r) => Some(r),
                _ => None,
            })
            .expect("a glyph run");
        assert_eq!(run.atlas, TextureId::new(7));
        assert_eq!(run.glyphs[0].rect.x, text_world.x + 2.0);
        assert_eq!(run.glyphs[0].rect.y, text_world.y + 3.0);

        // image: fills its world box.
        let image = out
            .iter()
            .find_map(|p| match p {
                Primitive::Image(i) => Some(i),
                _ => None,
            })
            .expect("an image");
        assert_eq!(image.texture, TextureId::new(9));
        assert_eq!(image.rect, store.world(image_id));

        // path: commands translated by the path leaf's world origin.
        let path_world = store.world(path_id);
        let path = out
            .iter()
            .find_map(|p| match p {
                Primitive::Path(p) => Some(p),
                _ => None,
            })
            .expect("a path");
        assert_eq!(
            path.cmds[0],
            PathCmd::MoveTo(Point::new(path_world.x + 1.0, path_world.y + 1.0))
        );
        assert_eq!(
            path.cmds[1],
            PathCmd::LineTo(Point::new(path_world.x + 5.0, path_world.y + 5.0))
        );
    }

    #[test]
    fn color_glyphs_lower_to_white_tinted_images() {
        use crate::content::Content;
        use crate::layout::Vec2;
        use viso_render::{GlyphInstanceData, TextureId};

        // A text run mixing one SDF outline glyph and one color-bitmap glyph.
        // The SDF glyph must still emit the single GlyphRun; the color glyph must
        // additionally lower to one white-tinted Image sampling the color atlas,
        // both shifted to the node's world origin.
        let mut store = NodeStore::new();
        let mut text_id = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                text_id = Some(
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(40.0, 20.0),
                        style: BoxStyle::NONE,
                    })
                    .id(),
                );
            });
            cx.root().unwrap()
        };
        let text_id = text_id.unwrap();

        store.set_content_payload(
            text_id,
            Content::Text {
                ink: None,
                glyphs: vec![GlyphInstanceData {
                    rect: Rect {
                        x: 1.0,
                        y: 2.0,
                        w: 8.0,
                        h: 10.0,
                    },
                    uv: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 0.1,
                        h: 0.1,
                    },
                }],
                atlas: TextureId::new(3),
                lane: viso_render::GlyphLane::CoverageA8,
                color_glyphs: vec![GlyphInstanceData {
                    rect: Rect {
                        x: 12.0,
                        y: 4.0,
                        w: 16.0,
                        h: 16.0,
                    },
                    uv: Rect {
                        x: 0.25,
                        y: 0.5,
                        w: 0.125,
                        h: 0.125,
                    },
                }],
                color_atlas: Some(TextureId::new(5)),
                color: RED,
                natural: Vec2 { x: 40.0, y: 20.0 },
                baseline: 16.0,
                shaped_at_width: None,
                width_use: crate::WidthUse::Ignored,
                min_content: 40.0,
                max_content: 40.0,
            },
        );

        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 100.0,
            },
            &mut scratch,
        );

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        let world = store.world(text_id);

        // The SDF run still lands once, sampling the R8 atlas.
        let run = out
            .iter()
            .find_map(|p| match p {
                Primitive::GlyphRun(r) => Some(r),
                _ => None,
            })
            .expect("the outline run");
        assert_eq!(run.atlas, TextureId::new(3));
        assert_eq!(run.glyphs.len(), 1);

        // The one color glyph lowers to one Image: white opaque tint (passes the
        // premultiplied emoji texel through unchanged), the color atlas texture,
        // its UV sub-rect verbatim, and its rect shifted to the world origin.
        let images: Vec<_> = out
            .iter()
            .filter_map(|p| match p {
                Primitive::Image(i) => Some(i),
                _ => None,
            })
            .collect();
        assert_eq!(images.len(), 1, "one color glyph → one image quad");
        let img = images[0];
        assert_eq!(img.texture, TextureId::new(5));
        assert_eq!(img.tint, WHITE);
        assert_eq!(img.uv.x, 0.25);
        assert_eq!(img.uv.y, 0.5);
        assert_eq!(img.uv.w, 0.125);
        assert_eq!(img.uv.h, 0.125);
        assert_eq!(img.rect.x, world.x + 12.0);
        assert_eq!(img.rect.y, world.y + 4.0);
        assert_eq!(img.rect.w, 16.0);
        assert_eq!(img.rect.h, 16.0);
    }

    #[test]
    fn pure_text_run_emits_no_image() {
        use crate::content::Content;
        use crate::layout::Vec2;
        use viso_render::{GlyphInstanceData, TextureId};

        // A pure-text run (empty color_glyphs, no color atlas) must not emit any
        // Image primitive — the color path costs nothing when unused.
        let mut store = NodeStore::new();
        let mut text_id = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                text_id = Some(
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(20.0, 20.0),
                        style: BoxStyle::NONE,
                    })
                    .id(),
                );
            });
            cx.root().unwrap()
        };
        let text_id = text_id.unwrap();
        store.set_content_payload(
            text_id,
            Content::Text {
                ink: None,
                glyphs: vec![GlyphInstanceData {
                    rect: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 8.0,
                        h: 10.0,
                    },
                    uv: Rect {
                        x: 0.0,
                        y: 0.0,
                        w: 0.1,
                        h: 0.1,
                    },
                }],
                atlas: TextureId::new(3),
                lane: viso_render::GlyphLane::CoverageA8,
                color_glyphs: Vec::new(),
                color_atlas: None,
                color: RED,
                natural: Vec2 { x: 20.0, y: 20.0 },
                baseline: 16.0,
                shaped_at_width: None,
                width_use: crate::WidthUse::Ignored,
                min_content: 20.0,
                max_content: 20.0,
            },
        );

        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            &mut scratch,
        );

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        assert!(
            !out.iter().any(|p| matches!(p, Primitive::Image(_))),
            "a pure-text run emits no image quad"
        );
    }

    const GREEN: Rgba = Rgba {
        r: 0.0,
        g: 1.0,
        b: 0.0,
        a: 1.0,
    };
    const BLUE: Rgba = Rgba {
        r: 0.0,
        g: 0.0,
        b: 1.0,
        a: 1.0,
    };

    // The fill colors of every quad in the primitive stream, in paint order —
    // enough to assert draw order for the overlay tests below.
    fn quad_colors(out: &[Primitive]) -> Vec<Rgba> {
        out.iter()
            .filter_map(|p| match p {
                Primitive::Quad(q) => Some(q.color),
                _ => None,
            })
            .collect()
    }

    // Author a row of three solid leaves (RED, GREEN, BLUE) into a fresh store,
    // returning the store, the root, and the three leaf ids in author order. The
    // caller flags whichever leaves it wants as overlay before laying out.
    fn three_leaf_row() -> (NodeStore, NodeId, [NodeId; 3]) {
        let mut store = NodeStore::new();
        let mut ids = [None; 3];
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Row,
                    ..Default::default()
                },
                |cx| {
                    for (i, color) in [RED, GREEN, BLUE].into_iter().enumerate() {
                        ids[i] = Some(
                            cx.leaf(LeafStyle {
                                size: Sizing::fixed(10.0, 10.0),
                                style: BoxStyle::solid(color),
                            })
                            .id(),
                        );
                    }
                },
            );
            cx.root().unwrap()
        };
        (store, root, ids.map(|id| id.unwrap()))
    }

    fn layout_full(store: &mut NodeStore, root: NodeId) {
        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 100.0,
                h: 100.0,
            },
            &mut scratch,
        );
    }

    #[test]
    fn an_overlay_child_paints_after_its_later_siblings() {
        // Flag the first leaf (RED) as overlay: it is authored before GREEN and
        // BLUE, so pre-order would paint it first — instead it must paint last,
        // after both later siblings, in the deferred top layer.
        let (mut store, root, [red, _green, _blue]) = three_leaf_row();
        store.set_overlay(red, true);
        layout_full(&mut store, root);

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        assert_eq!(quad_colors(&out), vec![GREEN, BLUE, RED]);
    }

    #[test]
    fn an_overlay_paints_over_an_earlier_non_overlay_node() {
        // Flag only the last leaf (BLUE) as overlay. It already paints last in
        // pre-order, so the stream is unchanged — but this pins the guarantee
        // that a top-layer node draws over the nodes authored before it.
        let (mut store, root, [_red, _green, blue]) = three_leaf_row();
        store.set_overlay(blue, true);
        layout_full(&mut store, root);

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        let colors = quad_colors(&out);
        assert_eq!(*colors.last().unwrap(), BLUE, "overlay draws on top");
        assert_eq!(colors, vec![RED, GREEN, BLUE]);
    }

    #[test]
    fn a_percent_radius_reads_the_shorter_side_and_none_rounds_past_a_pill() {
        use crate::length::LengthTerms;
        let (mut store, root, [red, green, blue]) = three_leaf_row();
        let terms = |dp: f32, pct: f32| LengthTerms {
            dp,
            pct,
            ..LengthTerms::ZERO
        };
        store.set_corner_radius(red, terms(4.0, 0.1));
        store.set_corner_radius(green, terms(0.0, 0.5));
        store.set_corner_radius(blue, terms(1000.0, 0.0));
        layout_full(&mut store, root);
        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        let radii: Vec<f32> = out
            .iter()
            .filter_map(|p| match p {
                Primitive::Quad(q) => Some(q.radius),
                _ => None,
            })
            .collect();
        let side = |id| {
            let world = store.world(id);
            world.w.min(world.h)
        };
        assert_eq!(
            radii,
            [4.0 + 0.1 * side(red), side(green) / 2.0, side(blue) / 2.0]
        );
    }

    #[test]
    fn no_overlay_emits_the_same_stream_as_the_plain_pre_order_walk() {
        // The regression guard: with no node flagged overlay, the primitive
        // stream must be byte-identical to the pre-order walk it produced before
        // the top-layer pass existed.
        let (mut store, root, _ids) = three_leaf_row();
        layout_full(&mut store, root);

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        assert_eq!(quad_colors(&out), vec![RED, GREEN, BLUE]);
        // No layer/clip primitives crept in for a plain row.
        assert_eq!(out.len(), 3);
    }

    #[test]
    fn nested_overlays_paint_in_author_order() {
        // A top-layer node whose own subtree contains a further overlay: the
        // outer overlay is collected during the main walk and drained; draining
        // it re-enters paint_subtree, which collects the inner overlay past the
        // current end, so the index walk paints it after — author (collection)
        // order, with no z sort.
        let mut store = NodeStore::new();
        let mut outer = None;
        let mut inner = None;
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(
                FlexStyle {
                    axis: Axis::Column,
                    ..Default::default()
                },
                |cx| {
                    // A plain in-place leaf, painted in the main walk.
                    cx.leaf(LeafStyle {
                        size: Sizing::fixed(10.0, 10.0),
                        style: BoxStyle::solid(RED),
                    });
                    // An overlay panel (GREEN) that itself holds an overlay child
                    // (BLUE) — both must land in the deferred top layer.
                    let panel = cx.flex(
                        FlexStyle {
                            axis: Axis::Column,
                            style: BoxStyle::solid(GREEN),
                            ..Default::default()
                        },
                        |cx| {
                            inner = Some(
                                cx.leaf(LeafStyle {
                                    size: Sizing::fixed(10.0, 10.0),
                                    style: BoxStyle::solid(BLUE),
                                })
                                .id(),
                            );
                        },
                    );
                    outer = Some(panel.id());
                },
            );
            cx.root().unwrap()
        };
        store.set_overlay(outer.unwrap(), true);
        store.set_overlay(inner.unwrap(), true);
        layout_full(&mut store, root);

        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        // RED (in place) first, then the outer overlay panel GREEN, then its
        // nested overlay BLUE — the whole top layer over the main content.
        assert_eq!(quad_colors(&out), vec![RED, GREEN, BLUE]);
    }

    #[test]
    fn set_overlay_marks_paint_only_not_layout() {
        // An overlay lays out in place; flipping the flag must repaint but must
        // not force a re-measure/re-layout of the node. Contrast set_hidden,
        // which marks LAYOUT | PAINT.
        let (mut store, root, [red, _green, _blue]) = three_leaf_row();
        layout_full(&mut store, root);
        // Layout clears the dirty flags; a fresh set_overlay is the only mark.
        store.set_overlay(red, true);
        let d = store.dirty(red);
        assert!(d.contains(DirtyClass::PAINT), "overlay flip repaints");
        assert!(
            !d.intersects(DirtyClass::LAYOUT | DirtyClass::MEASURE),
            "overlay lays out in place — no relayout"
        );
    }

    #[test]
    fn a_shader_paints_the_box_and_a_parameter_change_is_paint_only() {
        use std::sync::Arc;
        use viso_render::{BrushLayout, BrushProgram, ProgramSlot, ShaderDraw};

        let program = Arc::new(BrushProgram {
            name: "Ripple".into(),
            layout: BrushLayout {
                stride: 20,
                rect_pos: 0,
                rect_size: 8,
                uniform_size: 16,
                viewport_size: 0,
                time: None,
                textures: Vec::new(),
                samplers: Vec::new(),
                vertex_textures: false,
                params: Vec::new(),
            },
            code: Vec::new(),
        });
        let mut store = NodeStore::new();
        let (mut a, mut b) = (None, None);
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                for slot in [&mut a, &mut b] {
                    *slot = Some(
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(30.0, 20.0),
                            style: BoxStyle::NONE,
                        })
                        .id(),
                    );
                }
            });
            cx.root().unwrap()
        };
        let (a, b) = (a.unwrap(), b.unwrap());
        let params: Arc<[u8]> = Arc::from(vec![7u8; 20]);
        store.set_shader(a, &program, Arc::clone(&params), &[]);
        store.set_shader(b, &program, Arc::clone(&params), &[]);
        assert_eq!(store.shaders().iter().count(), 1, "one program, one slot");
        assert!(store.dirty(a).contains(DirtyClass::MEASURE));

        let mut scratch = Vec::new();
        store.layout(
            root,
            Rect {
                x: 0.0,
                y: 0.0,
                w: 200.0,
                h: 100.0,
            },
            &mut scratch,
        );
        let mut out = Vec::new();
        paint_tree(&store, root, &mut out);
        let draws: Vec<&ShaderDraw> = out
            .iter()
            .filter_map(|p| match p {
                Primitive::Shader(d) => Some(&**d),
                _ => None,
            })
            .collect();
        assert_eq!(draws.len(), 2);
        assert_eq!(draws[1].rect, store.world(b));
        assert_eq!(draws[1].program, ProgramSlot(0));
        assert!(
            Arc::ptr_eq(&draws[0].params, &params),
            "re-emitted, not copied"
        );

        store.clear_dirty();
        store.set_shader_params(b, Arc::from(vec![9u8; 20]));
        assert_eq!(store.dirty(b), DirtyClass::PAINT);
        store.clear_dirty();
        store.set_shader(a, &program, Arc::from(vec![1u8; 20]), &[]);
        assert_eq!(
            store.dirty(a),
            DirtyClass::PAINT,
            "the same program, new params"
        );

        // The program leaves its slot with the last node painting with it.
        let r = store.shaders().revision();
        store.set_content_payload(
            a,
            crate::content::Content::Path {
                cmds: Vec::new(),
                fill: None,
                stroke: None,
                natural: crate::layout::Vec2::ZERO,
            },
        );
        assert_eq!(store.shaders().revision(), r, "b still paints with it");
        let mut effects = crate::reactive::EffectStore::default();
        store.free_tree(b, &mut effects, &mut Vec::new());
        assert!(store.shaders().revision() > r);
        assert_eq!(store.shaders().iter().count(), 0);
        // The slot is the next program's.
        let next = Arc::new(BrushProgram {
            name: "Next".into(),
            ..(*program).clone()
        });
        store.set_shader(a, &next, Arc::clone(&params), &[]);
        assert_eq!(
            store.content_payload(a).map(|c| match c {
                crate::content::Content::Shader { program, .. } => *program,
                _ => ProgramSlot(99),
            }),
            Some(ProgramSlot(0))
        );
    }

    #[test]
    fn a_tile_repeats_the_asset_at_its_natural_size_from_the_corner() {
        use crate::asset::{AssetDraw, AssetFit};
        use viso_render::TextureId;
        let world = Rect {
            x: 10.0,
            y: 10.0,
            w: 50.0,
            h: 30.0,
        };
        let natural = Vec2::new(20.0, 10.0);
        let mut out = Vec::new();
        let draw = AssetDraw::Raster(TextureId::new(1));
        paint_asset(
            &draw,
            natural,
            AssetFit::Tile,
            None,
            false,
            world,
            None,
            &mut out,
        );
        let [Primitive::Image(image)] = &out[..] else {
            panic!("{out:?}");
        };
        assert_eq!(image.rect, world, "one draw covers the box");
        assert_eq!(
            (image.uv.x, image.uv.y, image.uv.w, image.uv.h),
            (0.0, 0.0, 2.5, 3.0)
        );
        assert_eq!(image.sampler.address, AddressMode::Repeat);

        let scene = Arc::new(viso_render::asset::VectorScene {
            paths: Arc::from(vec![Path {
                cmds: vec![PathCmd::MoveTo(Point::new(0.0, 0.0))],
                fill: Some(WHITE),
                stroke: None,
                shadow: None,
            }]),
            size: [20.0, 10.0],
        });
        let mut out = Vec::new();
        paint_asset(
            &AssetDraw::Vector(scene),
            natural,
            AssetFit::Tile,
            None,
            false,
            world,
            None,
            &mut out,
        );
        assert!(
            matches!(out.first(), Some(Primitive::Layer(l)) if l.clip == world)
                && matches!(out.last(), Some(Primitive::LayerEnd)),
            "a vector's copies are clipped to the box"
        );
        assert_eq!(out.len(), 2 + 3 * 3, "a copy a cell reaching into the box");
    }

    #[test]
    fn a_mirrored_raster_reads_its_texture_from_the_right_edge() {
        use crate::asset::{AssetDraw, AssetFit};
        use viso_render::TextureId;
        let world = Rect {
            x: 0.0,
            y: 0.0,
            w: 40.0,
            h: 20.0,
        };
        let uv = |fit, mirrored| {
            let mut out = Vec::new();
            let draw = AssetDraw::Raster(TextureId::new(1));
            let natural = Vec2::new(20.0, 20.0);
            paint_asset(&draw, natural, fit, None, mirrored, world, None, &mut out);
            match &out[..] {
                [Primitive::Image(image)] => (image.rect, image.uv),
                other => panic!("{other:?}"),
            }
        };
        let (rect, plain) = uv(AssetFit::Contain, false);
        let (same, flipped) = uv(AssetFit::Contain, true);
        assert_eq!(rect, same, "placed where it would be");
        assert_eq!((plain.x, plain.w), (0.0, 1.0));
        assert_eq!((flipped.x, flipped.w), (1.0, -1.0));
        assert_eq!((flipped.y, flipped.h), (plain.y, plain.h));
        // A cover's crop is flipped about its middle.
        let (_, crop) = uv(AssetFit::Cover, true);
        assert_eq!((crop.x, crop.w), (1.0, -1.0));
        assert_eq!((crop.y, crop.h), (0.25, 0.5));
    }

    #[test]
    fn an_asset_draws_what_its_table_holds_and_a_new_payload_dirties_only_its_nodes() {
        use crate::asset::{AssetFit, AssetPayload};
        use std::sync::Arc;
        use viso_render::TextureId;
        use viso_render::asset::{RasterImage, ResourceId, ResourceRevision, VectorScene};

        let raster = |w: u32, h: u32| {
            AssetPayload::Raster(Arc::new(RasterImage::rgba8(
                w,
                h,
                vec![255; (w * h * 4) as usize],
            )))
        };
        let logo = ResourceId::of("images/logo.png");
        let other = ResourceId::of("images/other.png");
        let mut store = NodeStore::new();
        let (mut a, mut b, mut c) = (None, None, None);
        let root = {
            let mut cx = BuildCx::new(&mut store);
            cx.flex(FlexStyle::default(), |cx| {
                for slot in [&mut a, &mut b, &mut c] {
                    *slot = Some(
                        cx.leaf(LeafStyle {
                            size: Sizing::fixed(40.0, 40.0),
                            style: BoxStyle::NONE,
                        })
                        .id(),
                    );
                }
            });
            cx.root().unwrap()
        };
        let (a, b, c) = (a.unwrap(), b.unwrap(), c.unwrap());
        let r1 = ResourceRevision(1);
        store
            .assets_mut()
            .offer(logo, r1, AssetBytes::Static(b"png"));
        let offered = store.assets().revision();
        store
            .assets_mut()
            .offer(logo, r1, AssetBytes::Static(b"png"));
        assert_eq!(store.assets().revision(), offered, "the same bytes again");
        store.set_asset(a, logo, AssetFit::Fill, None);
        store.set_asset(b, logo, AssetFit::Contain, None);
        store.set_asset(c, other, AssetFit::Fill, None);
        assert_eq!(
            store
                .content_payload(a)
                .map_or(Vec2::ZERO, Content::natural),
            Vec2::ZERO,
            "nothing decoded yet"
        );
        assert_eq!(store.assets().undecoded().count(), 1);

        // Decoded: its size lays out, but no pixels draw before they are resident.
        store.clear_dirty();
        store.provide_asset(logo, r1, raster(20, 10));
        assert_eq!(
            store.assets().undecoded().count(),
            0,
            "the bytes are dropped"
        );
        assert_eq!(
            store
                .content_payload(a)
                .map_or(Vec2::ZERO, Content::natural),
            Vec2::new(20.0, 10.0)
        );
        assert!(store.dirty(a).contains(DirtyClass::MEASURE));
        assert_eq!(store.dirty(c), DirtyClass::EMPTY, "another resource");
        assert_eq!(store.assets().unresident().count(), 1);
        let layout = |store: &mut NodeStore| {
            let mut scratch = Vec::new();
            store.layout(
                root,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 200.0,
                    h: 100.0,
                },
                &mut scratch,
            );
            let mut out = Vec::new();
            paint_tree(store, root, &mut out);
            out
        };
        let images = |out: &[Primitive]| -> Vec<ImageDraw> {
            out.iter()
                .filter_map(|p| match p {
                    Primitive::Image(d) => Some(*d),
                    _ => None,
                })
                .collect()
        };
        assert!(images(&layout(&mut store)).is_empty());

        // Resident: the pixels draw, PAINT only, the size unchanged.
        store.clear_dirty();
        let first = TextureId::new(1);
        assert_eq!(store.attach_texture(logo, r1, first), None);
        assert!(
            store.assets().payload(logo).is_none(),
            "the texture holds the pixels now"
        );
        assert_eq!(store.dirty(a), DirtyClass::PAINT);
        assert_eq!(store.dirty(c), DirtyClass::EMPTY);
        let drawn = images(&layout(&mut store));
        assert_eq!(drawn.len(), 2);
        assert_eq!(drawn[0].rect, store.world(a), "fill stretches to the box");
        let wb = store.world(b);
        assert_eq!(
            drawn[1].rect,
            Rect {
                x: wb.x,
                y: wb.y + 10.0,
                w: 40.0,
                h: 20.0
            },
            "contain keeps the aspect, centered"
        );

        // A new payload of another size: the old pixels draw until the new
        // ones are resident, then the nodes remeasure and the old texture
        // comes back to release.
        store.clear_dirty();
        let r2 = ResourceRevision(2);
        store.provide_asset(logo, r2, raster(10, 10));
        assert_eq!(
            store.dirty(a),
            DirtyClass::EMPTY,
            "the old pixels still draw"
        );
        let second = TextureId::new(2);
        assert_eq!(store.attach_texture(logo, r2, second), Some(first));
        assert!(store.dirty(a).contains(DirtyClass::MEASURE));
        assert!(store.dirty(b).contains(DirtyClass::MEASURE));
        assert_eq!(store.dirty(c), DirtyClass::EMPTY);
        assert_eq!(
            store
                .content_payload(a)
                .map_or(Vec2::ZERO, Content::natural),
            Vec2::new(10.0, 10.0)
        );
        // A texture of a revision the table moved past is handed back.
        assert_eq!(store.attach_texture(logo, r1, first), Some(first));
        assert_eq!(store.assets().texture(logo), Some((r2, second)));

        // The same size again: PAINT only.
        store.clear_dirty();
        let r3 = ResourceRevision(3);
        store.provide_asset(logo, r3, raster(10, 10));
        store.attach_texture(logo, r3, TextureId::new(3));
        assert_eq!(store.dirty(a), DirtyClass::PAINT);
        store.clear_dirty();
        store.provide_asset(logo, r2, raster(30, 30));
        assert_eq!(
            store.dirty(a),
            DirtyClass::EMPTY,
            "older than the resident one"
        );
        assert!(store.assets().payload(logo).is_none());

        // A vector draws as soon as it is decoded, cover cropped to the box.
        let scene = Arc::new(VectorScene {
            paths: Arc::from(vec![Path {
                cmds: vec![
                    PathCmd::MoveTo(Point::new(0.0, 0.0)),
                    PathCmd::LineTo(Point::new(20.0, 10.0)),
                ],
                fill: Some(WHITE),
                stroke: None,
                shadow: None,
            }]),
            size: [20.0, 10.0],
        });
        store.set_asset(c, other, AssetFit::Cover, None);
        store.provide_asset(other, r1, AssetPayload::Vector(scene));
        let out = layout(&mut store);
        let wc = store.world(c);
        let layer = out
            .iter()
            .position(|p| matches!(p, Primitive::Layer(l) if l.clip == wc))
            .expect("a cover clips to the box");
        let Primitive::Path(path) = &out[layer + 1] else {
            panic!("{out:?}");
        };
        assert_eq!(
            path.cmds[1],
            PathCmd::LineTo(Point::new(wc.x + 60.0, wc.y + 40.0)),
            "scaled by 4 to cover the 40x40 box, centered: x from -20"
        );
        assert!(matches!(out[layer + 2], Primitive::LayerEnd));
    }

    /// A scroll viewport over `n` rows, each a card with a leaf inside: the
    /// root, the viewport, and every card and leaf.
    fn cards(store: &mut NodeStore, n: usize) -> (NodeId, NodeId, Vec<NodeId>, Vec<NodeId>) {
        use crate::component::ScrollStyle;
        let mut cards = Vec::new();
        let mut leaves = Vec::new();
        let mut viewport = None;
        let root = {
            let mut cx = BuildCx::new(store);
            cx.flex(FlexStyle::default(), |cx| {
                viewport = Some(
                    cx.scroll(
                        ScrollStyle {
                            axis: Axis::Column,
                            across: false,
                            size: Sizing::fixed(100.0, 100.0),
                            style: BoxStyle::solid(RED),
                            bars: crate::scroll_bar::ScrollBars::Hidden,
                            tail: false,
                            refresh: false,
                            snap: None,
                            dots: None,
                        },
                        |cx| {
                            for _ in 0..n {
                                let card = cx.flex(
                                    FlexStyle {
                                        size: Sizing::fixed(100.0, 20.0),
                                        style: BoxStyle::solid(RED),
                                        ..Default::default()
                                    },
                                    |cx| {
                                        leaves.push(
                                            cx.leaf(LeafStyle {
                                                size: Sizing::fixed(10.0, 10.0),
                                                style: BoxStyle::solid(RED),
                                            })
                                            .id(),
                                        );
                                    },
                                );
                                cards.push(card.id());
                            }
                        },
                    )
                    .id(),
                );
            });
            cx.root().unwrap()
        };
        let surface = Rect {
            x: 0.0,
            y: 0.0,
            w: 200.0,
            h: 200.0,
        };
        store.layout(root, surface, &mut Vec::new());
        store.clear_dirty();
        (root, viewport.unwrap(), cards, leaves)
    }

    /// The stream a whole paint of `store` emits now.
    fn whole(store: &NodeStore, root: NodeId) -> Vec<Primitive> {
        let mut out = Vec::new();
        paint_tree(store, root, &mut out);
        out
    }

    #[test]
    fn a_paint_only_frame_rewrites_the_dirty_primitives_in_place() {
        use crate::component::Painted;
        let mut store = NodeStore::new();
        let (root, _, cards, leaves) = cards(&mut store, 6);
        let mut out = Vec::new();
        assert_eq!(store.repaint_dirty(root, &mut out), Painted::Nothing);
        store.repaint(root, &mut out);
        store.clear_dirty();

        let green = Rgba { g: 1.0, ..RED };
        store.set_fill(cards[2], green);
        store.set_fill(leaves[4], green);
        assert_eq!(store.repaint_dirty(root, &mut out), Painted::Patched(2));
        assert_eq!(out, whole(&store, root), "what a whole paint emits");
        let quads: Vec<u32> = store.patched().to_vec();
        assert_eq!(quads.len(), 2);
        assert!(quads[0] < quads[1], "ascending");
        for &at in &quads {
            assert!(matches!(out[at as usize], Primitive::Quad(q) if q.color == green));
        }
        store.clear_dirty();

        // A rewrite to the same value rewrites nothing.
        store.set_fill(cards[2], green);
        assert_eq!(store.repaint_dirty(root, &mut out), Painted::Nothing);
    }

    #[test]
    fn a_paint_only_frame_under_a_scrolled_root_keeps_the_painted_displacement() {
        use crate::component::Painted;
        use crate::layout::Vec2;
        let mut store = NodeStore::new();
        let (root, viewport, cards, _) = cards(&mut store, 6);
        let mut out = Vec::new();
        store.repaint(root, &mut out);
        let painted = out.clone();
        store.clear_dirty();
        // The scroll moves the root's retained draws; nothing repaints.
        store.scroll_by(viewport, Vec2 { x: 0.0, y: 30.0 });
        store.update_transforms();
        assert_eq!(store.repaint_dirty(root, &mut out), Painted::Nothing);
        store.clear_dirty();

        let green = Rgba { g: 1.0, ..RED };
        store.set_fill(cards[3], green);
        assert_eq!(store.repaint_dirty(root, &mut out), Painted::Patched(1));
        let at = store.patched()[0] as usize;
        assert!(
            matches!((&painted[at], &out[at]), (Primitive::Quad(a), Primitive::Quad(b))
                if a.rect == b.rect && b.color == green),
            "the same box as painted, recolored"
        );
    }

    #[test]
    fn a_fade_that_keeps_the_layer_rewrites_its_opacity_in_place() {
        use crate::component::Painted;
        let mut store = NodeStore::new();
        let (root, _, cards, _) = cards(&mut store, 4);
        let mut out = Vec::new();
        store.set_opacity(cards[1], 0.5);
        assert!(matches!(
            store.repaint_dirty(root, &mut out),
            Painted::Whole(_)
        ));
        store.clear_dirty();

        store.set_opacity(cards[1], 0.8);
        assert_eq!(store.repaint_dirty(root, &mut out), Painted::Patched(1));
        assert_eq!(out, whole(&store, root), "what a whole paint emits");
        let at = store.patched()[0] as usize;
        assert!(matches!(out[at], Primitive::Layer(l) if l.opacity == 0.8));
        store.clear_dirty();

        // Opaque again, the node stops fading: another stream.
        store.set_opacity(cards[1], 1.0);
        assert!(matches!(
            store.repaint_dirty(root, &mut out),
            Painted::Whole(_)
        ));
        assert_eq!(out, whole(&store, root));
    }

    #[test]
    fn a_change_of_shape_splices() {
        use crate::component::Painted;
        let mut store = NodeStore::new();
        let (root, _, cards, leaves) = cards(&mut store, 4);
        let mut out = Vec::new();
        store.repaint(root, &mut out);
        store.clear_dirty();

        // A box that stops drawing takes its quad out.
        store.set_fill(leaves[1], Rgba { a: 0.0, ..RED });
        let before = out.len();
        assert_eq!(store.repaint_dirty(root, &mut out), Painted::Spliced(0));
        assert_eq!(out, whole(&store, root));
        assert_eq!(out.len(), before - 1);
        store.clear_dirty();

        // A hidden card takes its subtree out; shown again, it paints fresh.
        store.set_hidden(cards[2], true);
        assert!(matches!(
            store.repaint_dirty(root, &mut out),
            Painted::Spliced(_)
        ));
        assert_eq!(out, whole(&store, root));
        store.clear_dirty();
        store.set_fill(leaves[2], Rgba { g: 1.0, ..RED });
        assert_eq!(
            store.repaint_dirty(root, &mut out),
            Painted::Patched(0),
            "a node a hidden ancestor keeps from painting changes nothing"
        );
        store.clear_dirty();
        store.set_hidden(cards[2], false);
        assert_eq!(store.repaint_dirty(root, &mut out), Painted::Spliced(2));
        assert_eq!(out, whole(&store, root));
    }

    #[test]
    fn a_fade_an_overlay_or_a_transform_root_starting_paints_whole() {
        use crate::component::Painted;
        let mut store = NodeStore::new();
        let (root, _, cards, _) = cards(&mut store, 4);
        let mut out = Vec::new();
        store.repaint(root, &mut out);
        store.clear_dirty();

        // A fade opens a layer.
        store.set_opacity(cards[0], 0.5);
        assert!(matches!(
            store.repaint_dirty(root, &mut out),
            Painted::Whole(_)
        ));
        assert_eq!(out, whole(&store, root));
        store.clear_dirty();

        // An overlay paints elsewhere in the stream.
        store.set_overlay(cards[1], true);
        assert!(matches!(
            store.repaint_dirty(root, &mut out),
            Painted::Whole(_)
        ));
        assert_eq!(out, whole(&store, root));
        store.clear_dirty();

        // A first translate makes a transform root.
        store.set_translate(cards[2], crate::layout::Vec2 { x: 4.0, y: 0.0 });
        assert!(matches!(
            store.repaint_dirty(root, &mut out),
            Painted::Whole(_)
        ));
    }
}
