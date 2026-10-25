//! Damage-driven present: what a frame redraws, and how it is shown.
//!
//! After an upload the renderer lists what its surface pass draws
//! ([`SurfaceDraw`]): one entry per paint-order slot landing on the surface,
//! keyed by its retained entry, the context it was lowered under, and what it
//! samples — the content of its offscreen pass, the content under its backdrop
//! capture, the clock a program reads. At submit each entry is placed where it
//! draws now (its root's shift, its moving clips) and the list is diffed
//! against the one last presented ([`crate::damage`]).
//!
//! - No damage: the frame is not begun, encoded or presented.
//! - Damage, and a drawable that holds an earlier frame
//!   ([`GpuBackend::frame_age`]): the surface pass keeps the drawable's pixels,
//!   clears the damaged rects and redraws only inside them — every surface draw
//!   scissored to each rect in turn — over the union of the damage of the
//!   frames the drawable missed.
//! - Otherwise (a drawable of unknown contents, damage over most of the
//!   surface, a new size or clear color): the frame is redrawn whole.
//!
//! Either way the present names the damage
//! ([`GpuBackend::present_with_damage`]), so a compositor that can update part
//! of the screen updates only that.

use viso_gpu::backend::{DrawCommand, Geometry};
use viso_gpu::{BufferDesc, BufferId, BufferUsage, Frame, GpuBackend};
use viso_math::Affine2;

use super::{
    BackdropCapture, MaterialRecord, OffscreenPass, QUAD_STRIDE, Renderer, clip_to_scissor,
    view_uniforms,
};
use crate::damage::{Drawn, History, Key, MAX_RECTS, Merger, Region, diff};
use crate::mask_pages::MaskPages;
use crate::primitive::{QuadInstance, Rect};
use crate::program::ProgramTable;
use crate::scene::Scene;
use crate::scene::key::pod_bytes;
use crate::scene::store::StoreRef;
use crate::transform::{Space, Transforms};

/// How far damage reaches past the bounds that caused it: the antialiased
/// fringe every built-in shades just outside a shape's edge.
const FRINGE: f32 = 1.0;

/// What one surface-pass entry of the last upload draws, before its root
/// moves.
#[derive(Debug, Clone, Copy)]
pub(super) struct SurfaceDraw {
    key: u64,
    /// The pixels it can touch, as painted.
    paint: Rect,
    /// Its clip as painted.
    clip: Option<Rect>,
    space: Space,
}

/// A texture the renderer rewrites in place, which the entries sampling it
/// cannot see in their own bytes: a slot can be refilled with new content at
/// the same place.
#[derive(Debug, Clone, Copy)]
pub(super) enum Atlas {
    GradientLut,
    MaskPage,
}

/// Counters of damage-driven present.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DamageStats {
    /// Submits with nothing to redraw, so never begun.
    pub skipped_frames: u64,
    /// Frames redrawn in part.
    pub partial_frames: u64,
    /// Frames redrawn whole: damage over most of the surface, a drawable of
    /// unknown contents, a new size or clear color.
    pub full_frame_fallbacks: u64,
    /// Pixels the last submitted frame redrew.
    pub damage_px: u64,
    /// Rects the last submitted frame redrew; `0` for a whole frame.
    pub damage_rects: u32,
}

/// What a presenter keys a frame by, for comparing two: entry keys and
/// surface draws, draws' keys and paint rects, passes' content, first entries
/// and composites.
#[cfg(test)]
pub(super) type Snapshot = (Vec<u64>, Vec<u32>, Vec<(u64, Rect)>, [Vec<u64>; 3]);

/// The damage state between uploads and presents.
#[derive(Debug)]
pub(super) struct Presenter {
    draws: Vec<SurfaceDraw>,
    /// Each paint-order entry's surface draw, or [`NONE`] for one drawn into
    /// an offscreen pass.
    draw_of: Vec<u32>,
    /// Each offscreen pass's content: the sum of its entries' keys, each mixed
    /// with its place in the pass, so one entry's change updates it in place
    /// and entries moving together leave it be.
    pass_sums: Vec<u64>,
    /// Each offscreen pass's first entry, which places in it count from.
    pass_firsts: Vec<u32>,
    /// Each offscreen pass's composite entry.
    composite_of: Vec<u32>,
    /// Each backdrop capture's content key, once a composite reads it.
    capture_keys: Vec<Option<u64>>,
    /// Every paint-order entry's key, this upload.
    keys: Vec<u64>,
    /// What this frame draws, where it draws it now.
    drawn: Vec<Drawn>,
    /// What the last presented frame drew.
    presented: Vec<Drawn>,
    /// Each surface draw's entry in `presented`, or [`NONE`] where it drew
    /// nothing; valid while `placed` is.
    drawn_of: Vec<u32>,
    /// The transform epoch `presented` was placed at from this upload's
    /// draws, or `None` once either changed.
    placed: Option<u64>,
    /// New keys of `presented` entries a patch rewrote since it was shown.
    restaged: Vec<(u32, u64)>,
    /// What splices changed since the last presented frame: the rects their
    /// removed draws covered and their inserted ones cover. `presented`
    /// already lists the inserted ones in place of the removed.
    staged: Vec<Rect>,
    /// The inserted draws a splice places, and each one's entry in
    /// `presented`, reused.
    placing: Vec<Drawn>,
    placed_of: Vec<u32>,
    history: History,
    /// The next frame is redrawn whole.
    full: bool,
    /// The viewport and clear color the last frame was presented with.
    viewport: [f32; 2],
    clear: [f32; 4],
    /// What the last submit redrew.
    region: Region,
    partial: bool,
    clear_buffer: Option<BufferId>,
    stats: DamageStats,
    /// How many times each [`Atlas`] was written: part of the key of every
    /// entry sampling it, so a write damages them.
    atlas_writes: [u64; 2],
}

impl Default for Presenter {
    fn default() -> Presenter {
        Presenter {
            draws: Vec::new(),
            draw_of: Vec::new(),
            pass_sums: Vec::new(),
            pass_firsts: Vec::new(),
            composite_of: Vec::new(),
            capture_keys: Vec::new(),
            keys: Vec::new(),
            drawn: Vec::new(),
            presented: Vec::new(),
            drawn_of: Vec::new(),
            placed: None,
            restaged: Vec::new(),
            staged: Vec::new(),
            placing: Vec::new(),
            placed_of: Vec::new(),
            history: History::default(),
            full: true,
            viewport: [0.0, 0.0],
            clear: [0.0; 4],
            region: Region::Empty,
            partial: true,
            clear_buffer: None,
            stats: DamageStats::default(),
            atlas_writes: [0; 2],
        }
    }
}

impl Presenter {
    /// Stage a splice that replaced the `removed` surface draws from `first`
    /// with the `added` ones now there against the last presented frame:
    /// what the removed covered and the inserted cover is damage, and the
    /// inserted take the removed's place in `presented`.
    fn stage(&mut self, transforms: &Transforms, first: usize, removed: usize, added: usize) {
        let surface = Rect::new(0.0, 0.0, self.viewport[0], self.viewport[1]);
        let at = self.drawn_of[first..]
            .iter()
            .copied()
            .find(|&d| d != NONE)
            .map_or(self.presented.len(), |d| d as usize);
        let mut gone = 0;
        for &d in &self.drawn_of[first..first + removed] {
            if d != NONE {
                self.staged.push(self.presented[d as usize].rect);
                gone += 1;
            }
        }
        self.placing.clear();
        self.placed_of.clear();
        for d in &self.draws[first..first + added] {
            match place(transforms, d, surface) {
                Some(drawn) => {
                    self.staged.push(drawn.rect);
                    self.placed_of.push((at + self.placing.len()) as u32);
                    self.placing.push(drawn);
                }
                None => self.placed_of.push(NONE),
            }
        }
        let delta = self.placing.len() as i64 - gone as i64;
        self.presented
            .splice(at..at + gone, self.placing.iter().copied());
        self.drawn_of
            .splice(first..first + removed, self.placed_of.iter().copied());
        if delta != 0 {
            for d in &mut self.drawn_of[first + added..] {
                if *d != NONE {
                    *d = (i64::from(*d) + delta) as u32;
                }
            }
        }
        let (at, end) = (at as u32, (at + gone) as u32);
        self.restaged.retain_mut(|(i, _)| {
            if *i >= end {
                *i = (i64::from(*i) + delta) as u32;
                true
            } else {
                *i < at
            }
        });
        if self.staged.len() > STAGED {
            self.staged.clear();
            self.full = true;
        }
    }

    /// Requires the last presented frame, while placed, to list exactly
    /// where each surface draw draws now, `drawn_of` leading to it.
    #[cfg(test)]
    #[track_caller]
    pub(super) fn check_placed(&self, transforms: &Transforms) {
        if self.placed.is_none() {
            return;
        }
        let surface = Rect::new(0.0, 0.0, self.viewport[0], self.viewport[1]);
        let mut placed = Vec::new();
        assert_eq!(self.drawn_of.len(), self.draws.len());
        for (d, &at) in self.draws.iter().zip(&self.drawn_of) {
            match place(transforms, d, surface) {
                Some(drawn) => {
                    assert_eq!(at as usize, placed.len(), "where the draw was shown");
                    placed.push(drawn);
                }
                None => assert_eq!(at, NONE),
            }
        }
        let keyed = |v: &[Drawn]| v.iter().map(|d| d.rect).collect::<Vec<_>>();
        assert_eq!(keyed(&self.presented), keyed(&placed), "the shown rects");
    }

    pub(super) fn atlas_written(&mut self, atlas: Atlas) {
        self.atlas_writes[atlas as usize] += 1;
    }

    /// Every entry's key and surface draw, every surface draw's key and
    /// paint rect, and each offscreen pass's content, first entry and
    /// composite.
    #[cfg(test)]
    pub(super) fn snapshot(&self) -> Snapshot {
        let draws = self.draws.iter().map(|d| (d.key, d.paint)).collect();
        let wide = |v: &[u32]| v.iter().map(|&x| u64::from(x)).collect();
        let passes = [
            self.pass_sums.clone(),
            wide(&self.pass_firsts),
            wide(&self.composite_of),
        ];
        (self.keys.clone(), self.draw_of.clone(), draws, passes)
    }
}

/// No entry.
const NONE: u32 = u32::MAX;

/// How many rects splices stage before the next frame is redrawn whole.
const STAGED: usize = 256;

/// Where surface draw `d` draws now, within `surface`, or `None` where it
/// draws nothing.
fn place(transforms: &Transforms, d: &SurfaceDraw, surface: Rect) -> Option<Drawn> {
    let place = transforms.place(d.space.slot);
    let mut rect = if place.is_identity() {
        d.paint
    } else {
        d.paint.transform_bounds(&place)
    };
    if let Some(clip) = transforms.clip(d.space, d.clip) {
        rect = rect.intersect(clip);
    }
    let rect = rect.intersect(surface);
    rect.has_area().then(|| Drawn {
        key: placed_key(d.key, place),
        rect,
    })
}

/// What the entry keyed `key`, `place` entries into its offscreen pass, adds
/// to the pass's content.
fn pass_term(key: u64, place: u32) -> u64 {
    Key::new(key).word(u64::from(place)).finish()
}

/// The key of a surface draw keyed `key` whose root went to `place`.
fn placed_key(key: u64, place: Affine2) -> u64 {
    if place.is_identity() {
        key
    } else {
        Key::new(key)
            .f32s(&[place.a, place.b, place.c, place.d, place.e, place.f])
            .finish()
    }
}

impl Renderer {
    /// Lists what the uploaded frame's surface pass draws (see the module
    /// docs). Runs at the end of every upload.
    pub(super) fn collect_surface_draws(&mut self) {
        let (keying, p) = self.keying();
        p.draws.clear();
        p.keys.clear();
        p.draw_of.clear();
        p.pass_sums.clear();
        p.pass_sums.resize(keying.offscreen_passes.len(), 0);
        p.pass_firsts.clear();
        p.pass_firsts.resize(keying.offscreen_passes.len(), NONE);
        p.composite_of.clear();
        p.composite_of.resize(keying.offscreen_passes.len(), NONE);
        p.capture_keys.clear();
        p.capture_keys.resize(keying.backdrop_captures.len(), None);
        p.placed = None;
        p.restaged.clear();
        for (i, entry) in keying.scene.paint_order.iter().enumerate() {
            let key = keying.draw_key(p, i);
            p.keys.push(key);
            if let StoreRef::Composite { pass, .. } = entry.store
                && let Some(slot) = p.composite_of.get_mut(pass as usize)
            {
                *slot = i as u32;
            }
            if let Some(pass) = entry.context.offscreen {
                let pass = pass as usize;
                if let Some(sum) = p.pass_sums.get_mut(pass) {
                    let first = &mut p.pass_firsts[pass];
                    if *first == NONE {
                        *first = i as u32;
                    }
                    *sum = sum.wrapping_add(pass_term(key, i as u32 - *first));
                }
                p.draw_of.push(NONE);
                continue;
            }
            p.draw_of.push(p.draws.len() as u32);
            p.draws.push(keying.surface_draw(i, key));
        }
    }

    /// Bring the surface draws to a splice that replaced the `removed`
    /// paint-order entries from `at`, leaves drawn into the surface pass,
    /// with the `added` ones now there: those are keyed and drawn, every
    /// other draw keeps its key, and the entries after them move.
    pub(super) fn splice_surface_draws(&mut self, at: usize, removed: usize, added: usize) {
        let (keying, p) = self.keying();
        let shift = added as i64 - removed as i64;
        let first = p.draw_of[at..]
            .iter()
            .copied()
            .find(|&d| d != NONE)
            .map_or(p.draws.len(), |d| d as usize);
        p.keys
            .splice(at..at + removed, std::iter::repeat_n(0, added));
        for i in at..at + added {
            p.keys[i] = keying.draw_key(p, i);
        }
        let keys = &p.keys;
        p.draws.splice(
            first..first + removed,
            (at..at + added).map(|i| keying.surface_draw(i, keys[i])),
        );
        p.draw_of
            .splice(at..at + removed, (first..first + added).map(|d| d as u32));
        if p.placed.is_some() {
            p.stage(keying.transforms, first, removed, added);
        } else {
            p.restaged.clear();
        }
        if shift != 0 {
            let end = (at + removed) as i64;
            let moved = |i: &mut u32| {
                if *i != NONE && i64::from(*i) >= end {
                    *i = (i64::from(*i) + shift) as u32;
                }
            };
            for d in &mut p.draw_of[at + added..] {
                if *d != NONE {
                    *d = (i64::from(*d) + shift) as u32;
                }
            }
            p.pass_firsts.iter_mut().for_each(moved);
            p.composite_of.iter_mut().for_each(moved);
        }
    }

    /// The presenter, and what its keys read of the rest of the renderer.
    fn keying(&mut self) -> (Keying<'_>, &mut Presenter) {
        let keying = Keying {
            scene: &self.scene,
            offscreen_passes: &self.offscreen_passes,
            backdrop_captures: &self.backdrop_captures,
            material_records: &self.material_records,
            mask_pages: &self.mask_pages,
            programs: &self.programs,
            transforms: &self.transforms,
        };
        (keying, &mut self.present)
    }

    /// Re-key paint-order entry `index` after a patch rewrote it, and every
    /// composite its change reaches up to the surface draw. When the last
    /// presented frame was placed as things stand, the surface draw's new key
    /// is staged against it, so the next frame's damage is the rewritten draws
    /// alone.
    pub(super) fn rekey(&mut self, index: usize) {
        let (keying, p) = self.keying();
        let mut i = index;
        loop {
            let old = p.keys[i];
            let new = keying.draw_key(p, i);
            if new == old {
                break;
            }
            p.keys[i] = new;
            if let Some(pass) = keying.scene.paint_order[i].context.offscreen {
                let pass = pass as usize;
                let place = i as u32 - p.pass_firsts[pass];
                let sum = &mut p.pass_sums[pass];
                *sum = sum
                    .wrapping_sub(pass_term(old, place))
                    .wrapping_add(pass_term(new, place));
                match p.composite_of[pass] {
                    NONE => break,
                    composite => i = composite as usize,
                }
                continue;
            }
            let d = p.draw_of[i] as usize;
            p.draws[d].key = new;
            if p.placed.is_some() {
                match p.drawn_of[d] {
                    NONE => {}
                    at => {
                        let place = keying.transforms.place(p.draws[d].space.slot);
                        p.restaged.push((at, placed_key(new, place)));
                    }
                }
            }
            break;
        }
    }
}

/// What draw keys read of the renderer.
struct Keying<'a> {
    scene: &'a Scene,
    offscreen_passes: &'a [OffscreenPass],
    backdrop_captures: &'a [BackdropCapture],
    material_records: &'a [MaterialRecord],
    mask_pages: &'a MaskPages,
    programs: &'a ProgramTable,
    transforms: &'a Transforms,
}

impl Keying<'_> {
    /// The surface draw of paint-order entry `i`, keyed `key`.
    fn surface_draw(&self, i: usize, key: u64) -> SurfaceDraw {
        let entry = &self.scene.paint_order[i];
        let (_, extent) = self.scene.key(i);
        let bounds = entry.bounds.paint;
        let paint = match extent {
            Some(extent) if !bounds.has_area() => extent,
            _ => bounds,
        };
        SurfaceDraw {
            key,
            paint,
            clip: entry.context.clip,
            space: entry.context.space,
        }
    }

    /// The key of paint-order entry `i`: its retained content, the context it
    /// was lowered under, and whatever it samples.
    fn draw_key(&self, p: &mut Presenter, i: usize) -> u64 {
        let entry = &self.scene.paint_order[i];
        let (content, _) = self.scene.key(i);
        let ctx = entry.context;
        let mut key = Key::new(content)
            .f32s(&ctx.origin)
            .word(u64::from(ctx.space.slot) << 32 | u64::from(ctx.space.chain));
        key = match ctx.clip {
            Some(clip) => key.rect(clip),
            None => key.word(u64::MAX),
        };
        match entry.store {
            StoreRef::Composite { pass, .. } => {
                if let (Some(sum), Some(pass)) = (
                    p.pass_sums.get(pass as usize),
                    self.offscreen_passes.get(pass as usize),
                ) {
                    key = key.word(pass.key).word(*sum);
                }
            }
            StoreRef::BackdropComposite { capture, .. } => {
                key = key.word(self.capture_key(p, capture as usize));
            }
            StoreRef::MaterialComposite { capture, material } => {
                key = key.word(self.capture_key(p, capture as usize));
                if let Some(m) = self.material_records.get(material as usize) {
                    key = key.rect(m.rect).f32s(&m.radius).f32s(&[m.noise, m.opacity]);
                    for row in &m.op.matrix.rows {
                        key = key.f32s(row);
                    }
                    key = key.f32s(&[m.op.gamma]);
                }
            }
            StoreRef::Gradient(_) => {
                key = key.word(p.atlas_writes[Atlas::GradientLut as usize]);
            }
            StoreRef::GlyphRun(run)
                if self
                    .scene
                    .glyph_runs
                    .run(run)
                    .is_some_and(|e| self.mask_pages.contains(e.atlas)) =>
            {
                key = key.word(p.atlas_writes[Atlas::MaskPage as usize]);
            }
            StoreRef::Shader(id) => {
                // The slot's program can be replaced under the same entry,
                // and a program that reads the clock follows it.
                if let Some(e) = self.scene.shaders.get(id) {
                    if let Some((pipeline, _)) = self.programs.get(e.program) {
                        let raw: viso_gpu::slots::RawId = pipeline.into();
                        key = key.word(u64::from(raw.index) << 32 | u64::from(raw.generation));
                    }
                    if let Some(time) = self.programs.clock(e.program) {
                        key = key.f32s(&[time]);
                    }
                }
            }
            _ => {}
        }
        key.finish()
    }

    /// The content key of backdrop capture `capture`: every surface entry
    /// under it whose pixels reach its region.
    fn capture_key(&self, p: &mut Presenter, capture: usize) -> u64 {
        if let Some(Some(key)) = p.capture_keys.get(capture) {
            return *key;
        }
        let Some(cap) = self.backdrop_captures.get(capture) else {
            return 0;
        };
        let mut key = Key::new(capture as u64).rect(cap.roi);
        for (entry, &k) in self.scene.paint_order[..cap.under].iter().zip(&p.keys) {
            if entry.context.offscreen.is_some() {
                continue;
            }
            let paint = entry.bounds.paint;
            if paint.has_area() && !paint.intersects(cap.roi) {
                continue;
            }
            key = key.word(k);
        }
        let key = key.finish();
        if let Some(slot) = p.capture_keys.get_mut(capture) {
            *slot = Some(key);
        }
        key
    }
}

impl Renderer {
    /// What this frame must redraw, given what was last presented: the
    /// surface pass placed where it draws now, diffed against the last
    /// presented frame's. Leaves the placed list in `present.drawn`.
    fn frame_region(&mut self, clear: [f32; 4], viewport: [f32; 2]) -> Region {
        let p = &mut self.present;
        let surface = Rect::new(0.0, 0.0, viewport[0], viewport[1]);
        let same = p.partial && !p.full && p.viewport == viewport && p.clear == clear;
        if same && p.placed == Some(self.transforms.epoch()) {
            let mut merger = Merger::new(FRINGE, surface);
            p.staged.iter().for_each(|&r| merger.add(r));
            for &(at, key) in &p.restaged {
                let shown = p.presented[at as usize];
                if shown.key != key {
                    merger.add(shown.rect);
                }
            }
            return merger.finish();
        }
        p.placed = None;
        p.restaged.clear();
        p.drawn.clear();
        p.drawn_of.clear();
        for d in &p.draws {
            match place(&self.transforms, d, surface) {
                Some(drawn) => {
                    p.drawn_of.push(p.drawn.len() as u32);
                    p.drawn.push(drawn);
                }
                None => p.drawn_of.push(NONE),
            }
        }
        if !same {
            return Region::Full;
        }
        let mut merger = Merger::new(FRINGE, surface);
        p.staged.iter().for_each(|&r| merger.add(r));
        diff(&p.presented, &p.drawn, |r| merger.add(r));
        merger.finish()
    }

    /// Whether to keep each frame's unchanged pixels and redraw only what
    /// changed (the default), or redraw and present every submitted frame
    /// whole.
    pub fn set_partial_redraw(&mut self, on: bool) {
        self.present.partial = on;
        self.present.full = true;
    }

    /// Redraw the next frame whole: the surface's pixels changed behind the
    /// renderer's back — a texture an image draws was rewritten in place, the
    /// surface was recreated or its device lost.
    pub fn damage_all(&mut self) {
        self.present.full = true;
    }

    /// What the last submit redrew: nothing (it was skipped), the whole
    /// surface, or these rects, in viewport units.
    pub fn last_damage(&self) -> &Region {
        &self.present.region
    }

    /// The damage-driven present counters.
    pub fn damage_stats(&self) -> DamageStats {
        self.present.stats
    }

    /// Encode the staged draws against `surface` and present them — or, when
    /// nothing on the surface changed since the last presented frame, do
    /// nothing at all. `clear` is the background color (premultiplied RGBA);
    /// `viewport` is the surface size `[width, height]` the draws are laid out
    /// in.
    ///
    /// This frame's translucent layers each become an offscreen pass, emitted
    /// before the surface pass so their textures are ready when it composites
    /// them.
    pub fn submit<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        surface: viso_gpu::SurfaceId,
        clear: [f32; 4],
        viewport: [f32; 2],
    ) {
        let region = self.frame_region(clear, viewport);
        if region.is_empty() {
            let epoch = self.transforms.epoch();
            let p = &mut self.present;
            // Nothing differs from what is shown, so what is shown is placed
            // as things stand.
            p.restaged.clear();
            p.staged.clear();
            p.placed = Some(epoch);
            p.stats.skipped_frames += 1;
            p.stats.damage_px = 0;
            p.stats.damage_rects = 0;
            p.region = Region::Empty;
            return;
        }
        // The drawable can be momentarily unavailable (surface out of date after a
        // resize/DPI change, or the drawable pool is exhausted). That is transient:
        // skip this frame; the next one diffs against the same presented frame, so
        // it redraws what this one would have.
        let Some(frame) = backend.begin_frame(surface) else {
            return;
        };
        let bounds = Rect::new(0.0, 0.0, viewport[0], viewport[1]);
        let redraw = match region {
            Region::Full => Region::Full,
            _ => self.present.history.redraw(
                &region,
                self.frame_age(backend, frame, viewport),
                bounds,
            ),
        };
        let stats = &mut self.present.stats;
        stats.damage_px = redraw.area(bounds) as u64;
        match &redraw {
            Region::Rects(rects) => {
                stats.partial_frames += 1;
                stats.damage_rects = rects.len() as u32;
                self.write_clears(backend, rects, clear);
            }
            _ => {
                stats.full_frame_fallbacks += 1;
                stats.damage_rects = 0;
            }
        }
        self.encode(
            backend,
            viso_gpu::backend::RenderTarget::Surface(frame),
            clear,
            viewport,
            &redraw,
        );
        let mut damage = [(0, 0, 0, 0); MAX_RECTS];
        let shown = match &region {
            Region::Rects(rects) => {
                for (out, r) in damage.iter_mut().zip(rects.iter()) {
                    *out = clip_to_scissor(*r, viewport, self.scale);
                }
                rects.len()
            }
            _ => 0,
        };
        backend.present_with_damage(frame, &damage[..shown]);
        let epoch = self.transforms.epoch();
        let p = &mut self.present;
        p.history.push(region);
        p.staged.clear();
        if p.placed.is_some() {
            for &(at, key) in &p.restaged {
                p.presented[at as usize].key = key;
            }
            p.restaged.clear();
        } else {
            std::mem::swap(&mut p.presented, &mut p.drawn);
            p.placed = Some(epoch);
        }
        p.full = false;
        p.viewport = viewport;
        p.clear = clear;
        p.region = redraw;
    }

    /// How many presents ago what the surface pass draws over was presented:
    /// the drawable's age, or the working target's for a surface that cannot
    /// encode, whose last pass writes the drawable whole.
    fn frame_age<B: GpuBackend>(&self, backend: &B, frame: Frame, viewport: [f32; 2]) -> u32 {
        match &self.output {
            Some(output) => output.age(super::physical_size(viewport, self.scale)),
            None => backend.frame_age(frame),
        }
    }

    /// Writes one clear quad per damaged rect, each the rect grown past its
    /// scissor so the scissored pixels are all fully covered.
    fn write_clears<B: GpuBackend>(&mut self, backend: &mut B, rects: &[Rect], clear: [f32; 4]) {
        let buffer = *self.present.clear_buffer.get_or_insert_with(|| {
            backend.create_buffer(&BufferDesc {
                size: MAX_RECTS * QUAD_STRIDE,
                usage: BufferUsage::INSTANCE | BufferUsage::CPU_WRITE,
                label: "damage-clear",
            })
        });
        // The quad shades a straight color and premultiplies it.
        let a = clear[3];
        let color = if a > 0.0 {
            [clear[0] / a, clear[1] / a, clear[2] / a, a]
        } else {
            [0.0; 4]
        };
        let mut quads = [QuadInstance {
            rect_pos: [0.0; 2],
            rect_size: [0.0; 2],
            color,
            radius: 0.0,
            border_width: 0.0,
            border_color: [0.0; 4],
        }; MAX_RECTS];
        for (quad, r) in quads.iter_mut().zip(rects) {
            let r = r.inflate(4.0);
            quad.rect_pos = [r.x, r.y];
            quad.rect_size = [r.w, r.h];
        }
        backend.write_buffer(buffer, 0, pod_bytes(&quads[..rects.len()]));
    }

    /// The surface pass of a partial redraw: each damaged rect cleared, then
    /// every surface draw scissored to it.
    pub(super) fn push_partial_surface(
        &self,
        commands: &mut Vec<DrawCommand>,
        rects: &[Rect],
        viewport: [f32; 2],
        surface: &[DrawCommand],
    ) {
        let Some(buffer) = self.present.clear_buffer else {
            return;
        };
        for (i, r) in rects.iter().enumerate() {
            let cut = clip_to_scissor(*r, viewport, self.scale);
            if cut.2 == 0 || cut.3 == 0 {
                continue;
            }
            commands.push(DrawCommand {
                pipeline: self.clear_pipeline,
                bind_group: None,
                geometry: Geometry::Generated { count: 1 },
                instance_buffer: buffer,
                instance_offset: i * QUAD_STRIDE,
                uniforms: view_uniforms(viewport, Affine2::IDENTITY),
                scissor: Some(cut),
            });
            for c in surface {
                let scissor = match c.scissor {
                    Some(s) => intersect_scissor(s, cut),
                    None => Some(cut),
                };
                let Some(scissor) = scissor else {
                    continue;
                };
                commands.push(DrawCommand {
                    scissor: Some(scissor),
                    ..*c
                });
            }
        }
    }
}

fn intersect_scissor(
    a: (u32, u32, u32, u32),
    b: (u32, u32, u32, u32),
) -> Option<(u32, u32, u32, u32)> {
    let x0 = a.0.max(b.0);
    let y0 = a.1.max(b.1);
    let x1 = (a.0 + a.2).min(b.0 + b.2);
    let y1 = (a.1 + a.3).min(b.1 + b.3);
    (x1 > x0 && y1 > y0).then(|| (x0, y0, x1 - x0, y1 - y0))
}
