//! Shimmers: a node's drawing that holds the place of content still on its
//! way.
//!
//! A shimmer is a node's content ([`Content::Shimmer`]): a rounded block of
//! a quiet colour. Waving, a band of light sweeps across it; pulsing, it
//! fades and comes back. Every shimmer of a store runs on one clock, and a
//! wave's band stands where the clock puts it along the window's width, so
//! the blocks of a placeholder — lines of text, an avatar, a picture — light
//! up together as one band passes over them, whatever their layout. A band
//! comes round every [`TRAVEL`] dp across, so a wide placeholder lights in
//! several places at once.
//!
//! The clock beats only while a shimmer shows, repainting the shimmers that
//! show ([`NodeStore::tick_shimmers`]); under reduced motion it stands still,
//! and the blocks hold their quiet colour.

use std::f32::consts::TAU;
use std::time::Duration;

use viso_render::{
    AnalyticShadow, Border, Corners, LayerClip, Primitive, Quad, Rect, Rgba, ShadowShape,
};

use crate::component::{BuildCx, Handle, LeafStyle, NodeStore};
use crate::content::Content;
use crate::dirty::DirtyClass;
use crate::layout::Sizing;
use crate::node::NodeId;
use crate::style::BoxStyle;

/// How a shimmer moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ShimmerLook {
    /// A band of light sweeps across it.
    #[default]
    Wave,
    /// It fades and comes back.
    Pulse,
    /// It stands still.
    Still,
}

/// What a shimmer node draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shimmer {
    pub look: ShimmerLook,
    /// The block's colour, and the light a wave sweeps over it.
    pub base: Rgba,
    pub highlight: Rgba,
    /// The block's corner radius; past half its shorter side it is round.
    pub radius: f32,
    /// Where the store's clock stood when it was last painted (seconds).
    pub time: f32,
}

/// How long a wave's band takes to come round, and a pulse to fade and come
/// back (seconds).
pub const WAVE_PERIOD: f32 = 1.6;
pub const PULSE_PERIOD: f32 = 2.0;
/// How far apart a wave's bands come, across (dp).
pub const TRAVEL: f32 = 800.0;
/// How wide a band is (dp): a core of full light, blurred out to the band's
/// edges.
pub const BAND: f32 = BAND_CORE + 6.0 * BAND_SIGMA;
const BAND_CORE: f32 = 96.0;
const BAND_SIGMA: f32 = 24.0;
/// How far a pulse fades its block at its faintest.
const PULSE_DEPTH: f32 = 0.5;

impl Shimmer {
    /// A shimmer of `look`, a block of `base` rounded by `radius`, a wave's
    /// light `highlight`.
    pub fn new(look: ShimmerLook, base: Rgba, highlight: Rgba, radius: f32) -> Shimmer {
        Shimmer {
            look,
            base,
            highlight,
            radius,
            time: 0.0,
        }
    }

    /// Append its drawing at `rect` to `out`.
    pub fn paint(&self, rect: Rect, out: &mut Vec<Primitive>) {
        if rect.w <= 0.0 || rect.h <= 0.0 {
            return;
        }
        let radius = self.radius.clamp(0.0, rect.w.min(rect.h) * 0.5);
        let mut base = self.base;
        if self.look == ShimmerLook::Pulse {
            let fade = 0.5 - 0.5 * (self.time / PULSE_PERIOD * TAU).cos();
            base.a *= 1.0 - PULSE_DEPTH * fade;
        }
        out.push(Primitive::Quad(Quad {
            rect,
            color: base,
            radius,
            border: Border::NONE,
        }));
        if self.look != ShimmerLook::Wave {
            return;
        }
        // The bands' centres stand `TRAVEL` apart along the window's width.
        let lead = (self.time / WAVE_PERIOD).fract() * TRAVEL;
        let reach = BAND * 0.5;
        let first = ((rect.x - reach - lead) / TRAVEL).ceil() as i32;
        let last = ((rect.x + rect.w + reach - lead) / TRAVEL).floor() as i32;
        if first > last {
            return;
        }
        out.push(Primitive::Layer(LayerClip {
            clip: rect,
            opacity: 1.0,
            blur_sigma: 0.0,
            backdrop_sigma: 0.0,
        }));
        if radius > 0.0 {
            out.push(Primitive::ClipCorners(Corners::uniform(radius)));
        }
        // A band is a soft-edged column of light: a core blurred out to the
        // band's width, standing past the block's top and bottom so only its
        // sides fade.
        let over = 3.0 * BAND_SIGMA;
        for k in first..=last {
            let centre = lead + k as f32 * TRAVEL;
            out.push(Primitive::AnalyticShadow(AnalyticShadow {
                rect: Rect::new(
                    centre - BAND_CORE * 0.5,
                    rect.y - over,
                    BAND_CORE,
                    rect.h + 2.0 * over,
                ),
                color: self.highlight,
                radius: Corners::uniform(0.0),
                offset: [0.0, 0.0],
                sigma: BAND_SIGMA,
                spread: 0.0,
                shape: ShadowShape::RoundedBox,
                inner: false,
            }));
        }
        out.push(Primitive::LayerEnd);
    }
}

/// The shimmers of a store that move, and the clock they share.
#[derive(Debug, Default)]
pub(crate) struct Shimmers {
    nodes: Vec<NodeId>,
    time: f32,
}

impl Shimmers {
    pub(crate) fn clear(&mut self) {
        self.nodes.clear();
        self.time = 0.0;
    }

    /// Forgets the shimmer of the slot `index`, freed or reused.
    pub(crate) fn forget(&mut self, index: u32) {
        self.nodes.retain(|id| id.index() != index);
    }
}

impl BuildCx<'_> {
    /// A shimmer leaf of `size` drawing `shimmer`.
    pub fn shimmer(&mut self, size: Sizing, shimmer: Shimmer) -> Handle {
        let handle = self.leaf(LeafStyle {
            size,
            style: BoxStyle::NONE,
        });
        self.store.set_shimmer(handle.id(), shimmer);
        handle
    }
}

impl NodeStore {
    /// Node `id`'s shimmer, if it draws one.
    pub fn shimmer(&self, id: NodeId) -> Option<Shimmer> {
        match self.content_payload(id) {
            Some(Content::Shimmer(shimmer)) => Some(*shimmer),
            _ => None,
        }
    }

    /// Make node `id` draw `shimmer`, on the store's clock.
    pub fn set_shimmer(&mut self, id: NodeId, shimmer: Shimmer) {
        if !self.arena().is_live(id) {
            return;
        }
        let shimmer = Shimmer {
            time: self.shimmers.time,
            ..shimmer
        };
        if self.shimmer(id) == Some(shimmer) {
            return;
        }
        self.set_content_payload(id, Content::Shimmer(shimmer));
        self.set_hittable(id, false);
        let nodes = &mut self.shimmers.nodes;
        nodes.retain(|&n| n != id);
        if shimmer.look != ShimmerLook::Still {
            nodes.push(id);
        }
    }

    /// Whether a shimmer that moves shows: the frame loop beats while one
    /// does, unless motion is reduced.
    #[inline]
    pub fn is_shimmering(&self) -> bool {
        !self.reduced_motion() && self.shimmers.nodes.iter().any(|&id| self.shows(id))
    }

    /// Run the shimmers' clock on by a frame of `delta`, repainting the ones
    /// that show.
    pub fn tick_shimmers(&mut self, delta: Duration) {
        if self.reduced_motion() {
            return;
        }
        let period = WAVE_PERIOD * PULSE_PERIOD;
        let time = (self.shimmers.time + delta.as_secs_f32().min(0.1)) % period;
        self.shimmers.time = time;
        let mut i = self.shimmers.nodes.len();
        while i > 0 {
            i -= 1;
            let id = self.shimmers.nodes[i];
            if !self.arena().is_live(id) || self.shimmer(id).is_none() {
                self.shimmers.nodes.swap_remove(i);
                continue;
            }
            if self.shows(id)
                && let Some(Content::Shimmer(s)) = self.content_payload_mut(id)
            {
                s.time = time;
                self.mark_dirty(id, DirtyClass::PAINT);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: Rgba = Rgba::new(0.5, 0.5, 0.5, 1.0);
    const LIGHT: Rgba = Rgba::new(1.0, 1.0, 1.0, 0.5);

    fn painted(s: &Shimmer, rect: Rect) -> Vec<Primitive> {
        let mut out = Vec::new();
        s.paint(rect, &mut out);
        out
    }

    /// The bands' centres across.
    fn bands(out: &[Primitive]) -> Vec<f32> {
        out.iter()
            .filter_map(|p| match p {
                Primitive::AnalyticShadow(b) => Some(b.rect.x + b.rect.w * 0.5),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_wave_lights_where_the_clock_puts_its_band_across_the_window() {
        let mut s = Shimmer::new(ShimmerLook::Wave, BASE, LIGHT, 4.0);
        s.time = WAVE_PERIOD * 0.25;
        // The band's centre at 200 dp: a block under it lights, the band
        // clipped round the block's corners.
        let under = painted(&s, Rect::new(150.0, 0.0, 100.0, 12.0));
        assert!(matches!(under[0], Primitive::Quad(_)));
        assert!(matches!(under[1], Primitive::Layer(_)));
        assert!(matches!(under[2], Primitive::ClipCorners(_)));
        assert_eq!(bands(&under), [200.0]);
        assert!(matches!(under.last(), Some(Primitive::LayerEnd)));
        // A block the band is away from draws itself alone.
        assert_eq!(painted(&s, Rect::new(400.0, 0.0, 100.0, 12.0)).len(), 1);
        // A band comes round TRAVEL further on.
        let next = painted(&s, Rect::new(950.0, 0.0, 100.0, 12.0));
        assert_eq!(bands(&next), [200.0 + TRAVEL]);
    }

    #[test]
    fn a_pulse_fades_and_comes_back_and_a_still_block_stands() {
        let mut s = Shimmer::new(ShimmerLook::Pulse, BASE, LIGHT, 0.0);
        let alpha = |s: &Shimmer| match painted(s, Rect::new(0.0, 0.0, 10.0, 10.0))[0] {
            Primitive::Quad(q) => q.color.a,
            _ => unreachable!(),
        };
        assert_eq!(alpha(&s), 1.0);
        s.time = PULSE_PERIOD * 0.5;
        assert!((alpha(&s) - (1.0 - PULSE_DEPTH)).abs() < 1e-5);
        let still = Shimmer::new(ShimmerLook::Still, BASE, LIGHT, 100.0);
        let out = painted(&still, Rect::new(0.0, 0.0, 40.0, 40.0));
        assert_eq!(out.len(), 1);
        match out[0] {
            Primitive::Quad(q) => assert_eq!(q.radius, 20.0, "round past half its side"),
            _ => unreachable!(),
        }
    }

    #[test]
    fn the_clock_beats_while_a_shimmer_shows_and_not_under_reduced_motion() {
        let mut store = NodeStore::new();
        let id = {
            let mut cx = BuildCx::new(&mut store);
            let h = cx.shimmer(
                Sizing::fixed(100.0, 12.0),
                Shimmer::new(ShimmerLook::Wave, BASE, LIGHT, 4.0),
            );
            h.id()
        };
        assert!(store.is_shimmering());
        store.clear_dirty();
        store.tick_shimmers(Duration::from_millis(100));
        assert!(store.dirty(id).contains(DirtyClass::PAINT));
        assert!((store.shimmer(id).unwrap().time - 0.1).abs() < 1e-6);
        store.set_hidden(id, true);
        assert!(!store.is_shimmering(), "hidden, it beats no frames");
        store.set_hidden(id, false);
        store.set_reduced_motion(true);
        assert!(!store.is_shimmering());
        let still = Shimmer::new(ShimmerLook::Still, BASE, LIGHT, 0.0);
        store.set_reduced_motion(false);
        store.set_shimmer(id, still);
        assert!(!store.is_shimmering(), "a still block beats nothing");
    }
}
