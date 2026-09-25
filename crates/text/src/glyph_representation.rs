//! Adaptive glyph representation: the Coverage / MTSDF / Vector / Color retained
//! state machine and its promotion hysteresis.
//!
//! A glyph is not assigned a fixed representation lane. It starts as exact A8
//! coverage — the best answer for small, CJK, and editor text — and is promoted
//! by observed on-screen behavior (Temporal Promotion): sustained scale or
//! rotation promotes to a scalable multi-channel distance field, extreme
//! sustained zoom to a retained vector outline, and a color source resolves to
//! a color representation by what the face provides. Promotion is hysteretic so
//! transient motion does not thrash representations, and any representation can
//! fall back to exact coverage under residency pressure because coverage is
//! always correct.
//!
//! # One retained state per run, evaluated only on change
//!
//! [`RepresentationState`] lives on retained run metadata, not per glyph, and
//! [`RepresentationState::resolve`] is called once per visible run per frame. A
//! frame whose [`TransformSample`] lands in the same raster bucket, with the same
//! rotation and hints, and with no timer or completion due, returns the cached
//! decision without evaluating any policy — translation, scroll, opacity, color,
//! and clip are not inputs at all, so they can never cause a promotion (§13.4).
//!
//! # Work is requested, never done here
//!
//! The state never rasterizes, generates, or tessellates. A [`Resolution`] names
//! what to draw now — always the last-good representation — and at most one
//! representation to produce off the frame. The caller reports completion with
//! [`RepresentationState::ready`] and the switch happens at the next `resolve`,
//! so a representation never changes mid-frame (§13.3). The representation the
//! run stopped drawing is handed back as a release, so a glyph holds two
//! representations only while a transition is in flight and never keeps
//! coverage, a field, and an outline resident at once.

use std::time::Duration;

use crate::inspect::RepresentationExplanation;
use crate::mtsdf::{self, BUCKETS, MtsdfPlan};
use crate::progressive::FontRevision;

/// The five glyph image representations. Which one a glyph resolves to is a
/// runtime decision (see [`RepresentationState`]), not a per-crate lane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GlyphImageKind {
    /// Exact single-channel coverage. The default; best for small / CJK /
    /// editor text and the always-correct fallback.
    MaskA8,
    /// Scalable multi-channel signed-distance field (sharp corners plus a true
    /// distance channel). Used for glyphs promoted under sustained transform.
    ScalableMtsdf,
    /// Retained vector outline for extreme scale / high precision; the steady
    /// state does not re-tessellate it every frame.
    OutlineVector,
    /// Premultiplied RGBA bitmap strike (for example sbix / CBDT color emoji).
    ColorRgba8,
    /// Vector color glyph (for example COLR / SVG).
    ColorVector,
}

/// Which promotion thresholds a run uses (§13.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunClass {
    /// Latin and other general text.
    Text,
    /// CJK: a far larger glyph working set and distance fields that cost more to
    /// generate, so promotion needs a longer sustained transform and backs off
    /// longer after a representation is lost. Stable CJK stays on exact coverage.
    Cjk,
}

/// One representation at one resolution bucket: what a run draws, or asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Representation {
    /// Which representation.
    pub kind: GlyphImageKind,
    /// The resolution bucket, in the unit the kind is keyed by: the rounded
    /// raster px-per-em for [`GlyphImageKind::MaskA8`], the ladder bucket's
    /// px-per-em for [`GlyphImageKind::ScalableMtsdf`], and `0` for the
    /// resolution-independent [`GlyphImageKind::OutlineVector`].
    pub bucket: u16,
}

impl Representation {
    /// The retained outline, which serves every scale past the field ladder.
    pub const OUTLINE: Self = Self {
        kind: GlyphImageKind::OutlineVector,
        bucket: 0,
    };

    /// Exact coverage rasterized at `bucket` px-per-em.
    pub const fn coverage(bucket: u16) -> Self {
        Self {
            kind: GlyphImageKind::MaskA8,
            bucket,
        }
    }

    /// A distance field generated at the ladder bucket `bucket`.
    pub const fn mtsdf(bucket: u16) -> Self {
        Self {
            kind: GlyphImageKind::ScalableMtsdf,
            bucket,
        }
    }

    /// Whether this representation scales on the GPU without a re-raster.
    pub fn is_scalable(&self) -> bool {
        matches!(
            self.kind,
            GlyphImageKind::ScalableMtsdf | GlyphImageKind::OutlineVector
        )
    }
}

/// The on-screen transform a run was observed at this frame.
///
/// Only what can change the right representation is here. Translation, scroll,
/// opacity, color, and clip are deliberately absent, so none of them can
/// trigger a promotion (§13.4).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransformSample {
    /// Frame timestamp on a monotonic clock. Every threshold is a duration, so a
    /// refresh-rate change by itself moves nothing.
    pub now: Duration,
    /// Effective device pixels per em after every transform and the display
    /// scale.
    pub px_per_em: f32,
    /// The transform is not axis aligned (rotation or skew).
    pub rotated: bool,
    /// World-space or canvas text whose scale is expected to keep changing: the
    /// first raster-bucket change promotes without waiting for a streak.
    pub world_space: bool,
}

/// One frame's decision for a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    /// Draw this now. It is the last-good representation and already resident,
    /// with one exception: when `request == Some(draw)` the drawn representation
    /// was lost or invalidated, and `draw` is exact coverage the caller admits
    /// now on its normal coverage path — coverage is always correct, so the
    /// glyph is degraded rather than dropped (§13.11).
    pub draw: Representation,
    /// Produce this off the frame and report it with
    /// [`RepresentationState::ready`] (or [`RepresentationState::lost`] if it
    /// cannot be produced). Issued once per target, never repeated while it is
    /// pending.
    pub request: Option<Representation>,
    /// The run no longer references this representation; its residency may be
    /// reclaimed.
    pub release: Option<Representation>,
}

/// Promotion thresholds. Calibrated by benchmark, never ABI (§13.4).
#[derive(Debug, Clone, Copy)]
struct Policy {
    /// How long raster-bucket changes must keep arriving before they count as a
    /// sustained transform, and how long a rotation must persist.
    sustain: Duration,
    /// Raster-bucket changes a streak needs before it counts as sustained.
    changes: u32,
    /// How long the transform must hold still before the run counts as settled.
    settle: Duration,
    /// How long promotion stays suppressed after a promoted representation was
    /// lost, so residency pressure cannot make a run thrash.
    cooldown: Duration,
}

const TEXT_POLICY: Policy = Policy {
    sustain: Duration::from_millis(100),
    changes: 3,
    settle: Duration::from_millis(200),
    cooldown: Duration::from_millis(1000),
};

const CJK_POLICY: Policy = Policy {
    sustain: Duration::from_millis(250),
    changes: 6,
    settle: Duration::from_millis(200),
    cooldown: Duration::from_millis(2000),
};

/// Once on the outline, a run returns to the field ladder only below this
/// factor of the ladder's top, so a zoom hovering at the hand-off point does not
/// alternate between the two.
const OUTLINE_EXIT: f32 = 0.8;

/// The retained promotion state one run carries between frames: the
/// representation it draws, the one it is waiting for, and the transform
/// history the hysteresis reads.
#[derive(Debug)]
pub struct RepresentationState {
    policy: Policy,
    revision: FontRevision,
    /// The last-good representation, drawn until a switch at a frame boundary.
    drawn: Representation,
    /// The representation requested and not yet switched to.
    pending: Option<Representation>,
    /// `pending` was reported ready; it becomes `drawn` at the next resolve.
    arrived: bool,
    /// `drawn` was lost or invalidated; handled at the next resolve.
    drawn_lost: bool,
    /// A promoted pending representation was lost; the next resolve starts the
    /// cooldown.
    pending_lost: bool,
    /// Raster bucket, rotation, and hint of the last evaluated sample.
    bucket: u16,
    rotated: bool,
    world_space: bool,
    /// Start of the current run of raster-bucket changes, `None` when still.
    streak_start: Option<Duration>,
    streak_changes: u32,
    last_change: Duration,
    rotated_since: Option<Duration>,
    cooldown_until: Duration,
    /// The next time a threshold can fire with no new input.
    deadline: Option<Duration>,
    /// A completion or loss arrived since the last evaluation.
    dirty: bool,
    evaluations: u64,
}

impl RepresentationState {
    /// A run first laid out at `sample`, drawing exact coverage at its raster
    /// bucket — the representation its initial layout rasterizes.
    pub fn new(class: RunClass, revision: FontRevision, sample: &TransformSample) -> Self {
        let bucket = raster_bucket(sample.px_per_em);
        let policy = match class {
            RunClass::Text => TEXT_POLICY,
            RunClass::Cjk => CJK_POLICY,
        };
        Self {
            policy,
            revision,
            drawn: Representation::coverage(bucket),
            pending: None,
            arrived: false,
            drawn_lost: false,
            pending_lost: false,
            bucket,
            rotated: sample.rotated,
            world_space: sample.world_space,
            streak_start: None,
            streak_changes: 0,
            last_change: sample.now,
            rotated_since: sample.rotated.then_some(sample.now),
            cooldown_until: Duration::ZERO,
            deadline: sample.rotated.then_some(sample.now + policy.sustain),
            dirty: false,
            evaluations: 0,
        }
    }

    /// Resolve the representation to draw this frame from the observed
    /// transform. Call once per frame, at the frame boundary; this is the only
    /// place the drawn representation changes. Mutates no residency pool.
    pub fn resolve(&mut self, sample: &TransformSample) -> Resolution {
        let bucket = raster_bucket(sample.px_per_em);
        let quiet = !self.dirty
            && bucket == self.bucket
            && sample.rotated == self.rotated
            && sample.world_space == self.world_space
            && self.deadline.is_none_or(|due| sample.now < due);
        if quiet {
            return Resolution {
                draw: self.drawn,
                request: None,
                release: None,
            };
        }
        self.evaluations += 1;
        self.dirty = false;
        self.observe(sample, bucket);
        let now = sample.now;
        if self.drawn_lost {
            self.drawn_lost = false;
            self.pending_lost = false;
            if self.drawn.is_scalable() {
                self.cooldown_until = now + self.policy.cooldown;
            }
            self.pending = None;
            self.arrived = false;
            self.drawn = Representation::coverage(bucket);
            self.arm(now);
            return Resolution {
                draw: self.drawn,
                request: Some(self.drawn),
                release: None,
            };
        }
        if self.pending_lost {
            self.pending_lost = false;
            self.cooldown_until = now + self.policy.cooldown;
        }
        let mut release = None;
        if self.arrived {
            self.arrived = false;
            if let Some(next) = self.pending.take() {
                release = Some(self.drawn);
                self.drawn = next;
            }
        }
        let request = self.decide(sample);
        self.arm(now);
        Resolution {
            draw: self.drawn,
            request,
            release,
        }
    }

    /// Report that `representation` was produced and validated against
    /// `revision`. Returns whether the run still wants it; a stale revision or a
    /// target the run has since moved away from is refused, and the caller may
    /// let it be reclaimed. The switch waits for the next [`resolve`].
    ///
    /// [`resolve`]: Self::resolve
    pub fn ready(&mut self, representation: Representation, revision: FontRevision) -> bool {
        if revision != self.revision || self.pending != Some(representation) || self.arrived {
            return false;
        }
        self.arrived = true;
        self.dirty = true;
        true
    }

    /// Report that `representation` is gone or cannot be produced: evicted
    /// under residency pressure, failed generation or validation, a missed
    /// deadline, or an unsupported backend (§13.11). A lost drawn representation
    /// falls back to exact coverage at the next resolve; a lost pending one is
    /// abandoned. Either way promotion backs off for a while.
    pub fn lost(&mut self, representation: Representation) {
        if representation == self.drawn {
            self.drawn_lost = true;
            self.dirty = true;
        }
        if self.pending == Some(representation) {
            self.pending = None;
            self.arrived = false;
            self.pending_lost = representation.is_scalable();
            self.dirty = true;
        }
    }

    /// The face's revision changed, or the device was recreated: everything the
    /// run holds is stale. The next resolve falls back to exact coverage.
    pub fn invalidate(&mut self, revision: FontRevision) {
        self.revision = revision;
        self.pending = None;
        self.arrived = false;
        self.drawn_lost = true;
        self.dirty = true;
    }

    /// The representation the run draws, as of the last resolve.
    pub fn drawn(&self) -> Representation {
        self.drawn
    }

    /// The representation requested and not yet switched to.
    pub fn pending(&self) -> Option<Representation> {
        self.pending
    }

    /// What the run draws and waits for, and the observed transform that
    /// drove it, for the Inspector.
    pub fn explain(&self) -> RepresentationExplanation {
        RepresentationExplanation {
            drawn: self.drawn,
            pending: self.pending,
            pending_ready: self.arrived,
            bucket: self.bucket,
            rotated: self.rotated,
            world_space: self.world_space,
            streak: self.streak_start.map(|start| (self.streak_changes, start)),
            rotated_since: self.rotated_since,
            cooldown_until: (self.cooldown_until > Duration::ZERO).then_some(self.cooldown_until),
        }
    }

    /// How many resolves evaluated the policy rather than returning the cached
    /// decision.
    pub fn evaluations(&self) -> u64 {
        self.evaluations
    }

    fn observe(&mut self, sample: &TransformSample, bucket: u16) {
        let now = sample.now;
        if bucket != self.bucket {
            if self.streak_start.is_none() {
                self.streak_start = Some(now);
                self.streak_changes = 0;
            }
            self.streak_changes += 1;
            self.last_change = now;
            self.bucket = bucket;
        }
        if self.streak_start.is_some() && now.saturating_sub(self.last_change) >= self.policy.settle
        {
            self.streak_start = None;
            self.streak_changes = 0;
        }
        self.rotated_since = match (sample.rotated, self.rotated_since) {
            (false, _) => None,
            (true, None) => Some(now),
            (true, since) => since,
        };
        self.rotated = sample.rotated;
        self.world_space = sample.world_space;
    }

    fn sustained(&self, now: Duration) -> bool {
        // The motion itself must have lasted: a streak that stopped short does
        // not become sustained while it waits to settle.
        let streak = self.streak_start.is_some_and(|start| {
            self.world_space
                || (self.streak_changes >= self.policy.changes
                    && self.last_change.saturating_sub(start) >= self.policy.sustain)
        });
        let turned = self
            .rotated_since
            .is_some_and(|since| now.saturating_sub(since) >= self.policy.sustain);
        streak || turned
    }

    fn settled(&self) -> bool {
        self.streak_start.is_none() && !self.rotated
    }

    fn decide(&mut self, sample: &TransformSample) -> Option<Representation> {
        let px = sample.px_per_em;
        if self.drawn.kind == GlyphImageKind::MaskA8 {
            if let Some(pending) = self.pending {
                let moot = if pending.is_scalable() {
                    self.settled()
                } else {
                    pending.bucket != self.bucket
                };
                if !moot {
                    return None;
                }
                self.pending = None;
            }
            if self.sustained(sample.now) && sample.now >= self.cooldown_until {
                return self.ask(scaled_target(px));
            }
            if self.settled() && self.bucket != self.drawn.bucket {
                return self.ask(Representation::coverage(self.bucket));
            }
            return None;
        }
        let target = if self.settled() {
            if covers(self.drawn, px) && self.drawn.kind == GlyphImageKind::OutlineVector {
                None
            } else if mtsdf::plan(px) == MtsdfPlan::Outline {
                Some(Representation::OUTLINE)
            } else {
                Some(Representation::coverage(self.bucket))
            }
        } else if covers(self.drawn, px) {
            None
        } else {
            Some(scaled_target(px))
        };
        match target {
            None => {
                self.pending = None;
                None
            }
            Some(target) if self.pending == Some(target) => None,
            Some(target) => self.ask(target),
        }
    }

    fn ask(&mut self, target: Representation) -> Option<Representation> {
        self.pending = Some(target);
        self.arrived = false;
        Some(target)
    }

    fn arm(&mut self, now: Duration) {
        let mut next = None;
        let mut at = |due: Duration| {
            if due > now {
                next = Some(next.map_or(due, |soonest: Duration| soonest.min(due)));
            }
        };
        if self.streak_start.is_some() {
            at(self.last_change + self.policy.settle);
        }
        if let Some(since) = self.rotated_since {
            at(since + self.policy.sustain);
        }
        at(self.cooldown_until);
        self.deadline = next;
    }
}

/// The coverage raster bucket for a px-per-em: whole pixels, so subpixel
/// movement inside one bucket is not a change.
fn raster_bucket(px_per_em: f32) -> u16 {
    px_per_em.round().clamp(1.0, f32::from(u16::MAX)) as u16
}

/// The scalable representation that serves `px_per_em`.
fn scaled_target(px_per_em: f32) -> Representation {
    match mtsdf::plan(px_per_em) {
        MtsdfPlan::Bucket(bucket) => Representation::mtsdf(bucket as u16),
        MtsdfPlan::Outline => Representation::OUTLINE,
    }
}

/// Whether a drawn scalable representation still serves `px_per_em` inside its
/// quality window (§13.5).
fn covers(representation: Representation, px_per_em: f32) -> bool {
    match representation.kind {
        GlyphImageKind::ScalableMtsdf => {
            let (lo, hi) = mtsdf::window(f32::from(representation.bucket));
            (lo..=hi).contains(&px_per_em)
        }
        GlyphImageKind::OutlineVector => {
            let top = BUCKETS[BUCKETS.len() - 1];
            px_per_em >= mtsdf::window(top).1 * OUTLINE_EXIT
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FontFaceId;
    use crate::glyph_cache::{GlyphKey, GlyphResidency};
    use crate::mtsdf::{MtsdfGenerator, MtsdfRequest};
    use crate::raster_a8::rasterize_coverage;

    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");
    const FACE: FontFaceId = FontFaceId(1);
    const REVISION: FontRevision = FontRevision(4);

    fn glyph() -> u16 {
        ttf_parser::Face::parse(DEJAVU, 0)
            .expect("fixture parses")
            .glyph_index('A')
            .expect("fixture covers 'A'")
            .0
    }

    /// One run driven frame by frame, with a worker that finishes each request
    /// `latency` frames after it was issued. Every request is really produced —
    /// coverage through the A8 rasterizer, fields through the generator — and
    /// admitted into a real residency, so the counters are the work a frame did.
    struct Driver {
        state: RepresentationState,
        residency: GlyphResidency,
        fields: MtsdfGenerator,
        field_bytes: Vec<u8>,
        glyph: u16,
        frame: u64,
        interval: Duration,
        now: Duration,
        latency: u64,
        inflight: Vec<(u64, Representation)>,
        /// Representations this run holds resident.
        held: Vec<Representation>,
        rasters: u64,
        generations: u64,
        outlines: u64,
        requests: Vec<Representation>,
        promotions: u32,
        demotions: u32,
        most_held: usize,
    }

    impl Driver {
        fn new(class: RunClass, px_per_em: f32) -> Self {
            let sample = TransformSample {
                now: Duration::ZERO,
                px_per_em,
                rotated: false,
                world_space: false,
            };
            let state = RepresentationState::new(class, REVISION, &sample);
            let held = vec![state.drawn()];
            Self {
                state,
                residency: GlyphResidency::new(64),
                fields: MtsdfGenerator::default(),
                field_bytes: Vec::new(),
                glyph: glyph(),
                frame: 0,
                interval: Duration::from_micros(16_667),
                now: Duration::ZERO,
                latency: 2,
                inflight: Vec::new(),
                held,
                rasters: 0,
                generations: 0,
                outlines: 0,
                requests: Vec::new(),
                promotions: 0,
                demotions: 0,
                most_held: 1,
            }
        }

        fn work(&self) -> u64 {
            self.rasters + self.generations + self.outlines
        }

        fn produce(&mut self, representation: Representation) {
            let bytes = match representation.kind {
                GlyphImageKind::MaskA8 => {
                    self.rasters += 1;
                    let bitmap =
                        rasterize_coverage(DEJAVU, 0, self.glyph, f32::from(representation.bucket))
                            .expect("coverage rasterizes");
                    bitmap.coverage.len()
                }
                GlyphImageKind::ScalableMtsdf => {
                    self.generations += 1;
                    let request = MtsdfRequest {
                        sfnt: DEJAVU,
                        index: 0,
                        face: FACE,
                        glyph: self.glyph,
                        revision: REVISION,
                        px_per_em: f32::from(representation.bucket),
                    };
                    self.fields
                        .generate(request, &mut self.field_bytes)
                        .expect("field generates")
                        .byte_len()
                }
                _ => {
                    self.outlines += 1;
                    1
                }
            };
            let key = GlyphKey {
                face: FACE,
                glyph: self.glyph,
                kind: representation.kind,
                bucket: representation.bucket,
            };
            self.residency.get_or_admit(key, bytes);
        }

        /// Deliver finished work (between frames), then run one frame at
        /// `px_per_em`.
        fn frame(&mut self, px_per_em: f32, rotated: bool) -> Resolution {
            let frame = self.frame;
            let mut i = 0;
            while i < self.inflight.len() {
                if self.inflight[i].0 <= frame {
                    let (_, done) = self.inflight.swap_remove(i);
                    if self.state.ready(done, REVISION) {
                        self.held.push(done);
                    }
                } else {
                    i += 1;
                }
            }
            let before = self.state.drawn();
            let sample = TransformSample {
                now: self.now,
                px_per_em,
                rotated,
                world_space: false,
            };
            let resolution = self.state.resolve(&sample);
            if let Some(request) = resolution.request {
                self.requests.push(request);
                self.produce(request);
                if request == resolution.draw {
                    self.held.push(request);
                } else {
                    self.inflight.push((frame + self.latency, request));
                }
            }
            if let Some(release) = resolution.release {
                self.held.retain(|held| *held != release);
            }
            match (before.is_scalable(), resolution.draw.is_scalable()) {
                (false, true) => self.promotions += 1,
                (true, false) => self.demotions += 1,
                _ => {}
            }
            self.most_held = self.most_held.max(self.held.len());
            self.residency.advance_epoch();
            self.frame += 1;
            self.now += self.interval;
            resolution
        }

        fn hold(&mut self, px_per_em: f32, frames: u32) {
            for _ in 0..frames {
                self.frame(px_per_em, false);
            }
        }

        /// Zoom geometrically from `from` to `to` over `frames` frames.
        fn zoom(&mut self, from: f32, to: f32, frames: u32) {
            let step = (to / from).powf(1.0 / frames as f32);
            let mut px = from;
            for _ in 0..frames {
                px *= step;
                self.frame(px, false);
            }
        }
    }

    #[test]
    fn the_explanation_follows_the_observed_transform() {
        let mut run = Driver::new(RunClass::Text, 16.0);
        let still = run.state.explain();
        assert_eq!(still.drawn, Representation::coverage(16));
        assert_eq!((still.pending, still.pending_ready), (None, false));
        assert_eq!(
            (still.bucket, still.rotated, still.world_space),
            (16, false, false)
        );
        assert_eq!((still.streak, still.cooldown_until), (None, None));

        run.zoom(16.0, 100.0, 90);
        let zoomed = run.state.explain();
        assert_eq!(zoomed.drawn, run.state.drawn());
        assert!(zoomed.drawn.is_scalable());
        assert_eq!(zoomed.bucket, 100);
        assert!(zoomed.streak.is_some_and(|(changes, _)| changes > 0));
    }

    #[test]
    fn a_one_frame_scale_spike_promotes_nothing() {
        let mut run = Driver::new(RunClass::Text, 16.0);
        run.hold(16.0, 10);
        let spike = run.frame(24.0, false);
        assert_eq!(spike.draw, Representation::coverage(16));
        run.hold(16.0, 60);
        assert_eq!(run.promotions, 0);
        assert!(run.requests.is_empty(), "{:?}", run.requests);
        assert_eq!(run.work(), 0);
        assert_eq!(run.state.drawn(), Representation::coverage(16));
    }

    #[test]
    fn a_sustained_zoom_promotes_once_and_only_once() {
        let mut run = Driver::new(RunClass::Text, 16.0);
        run.zoom(16.0, 100.0, 90);
        assert_eq!(run.promotions, 1);
        assert_eq!(run.demotions, 0);
        assert!(run.state.drawn().is_scalable());
        // Each ladder bucket the zoom crossed was asked for exactly once.
        let mut asked = run.requests.clone();
        asked.dedup();
        assert_eq!(asked, run.requests);
        assert!(run.requests.iter().all(Representation::is_scalable));
        assert!(run.rasters == 0, "a zoom rasterizes no coverage");
    }

    #[test]
    fn settling_demotes_once_at_a_frame_boundary() {
        let mut run = Driver::new(RunClass::Text, 16.0);
        run.latency = 3;
        run.zoom(16.0, 50.0, 60);
        let scaled = run.state.drawn();
        assert!(scaled.is_scalable());
        // Hold still until the settle request goes out.
        let mut settle = None;
        for _ in 0..60 {
            if let Some(request) = run.frame(50.0, false).request {
                settle = Some(request);
                break;
            }
        }
        assert_eq!(settle, Some(Representation::coverage(50)));
        // The coverage finishes between frames: nothing switches until the next
        // resolve.
        let rasters = run.rasters;
        for _ in 0..run.latency - 1 {
            assert_eq!(run.frame(50.0, false).draw, scaled);
        }
        assert!(run.state.ready(Representation::coverage(50), REVISION));
        assert_eq!(run.state.drawn(), scaled, "no switch mid-frame");
        run.inflight.clear();
        run.held.push(Representation::coverage(50));
        let switch = run.frame(50.0, false);
        assert_eq!(switch.draw, Representation::coverage(50));
        assert_eq!(switch.release, Some(scaled));
        assert_eq!(run.demotions, 1);
        assert_eq!(run.rasters, rasters, "one raster for the settled bucket");
        let evaluations = run.state.evaluations();
        run.hold(50.0, 200);
        assert_eq!(run.demotions, 1);
        assert_eq!(
            run.state.evaluations(),
            evaluations,
            "settled runs are not re-evaluated"
        );
        assert_eq!(run.held, vec![Representation::coverage(50)]);
    }

    #[test]
    fn a_pending_promotion_draws_the_previous_representation() {
        let mut run = Driver::new(RunClass::Text, 16.0);
        run.latency = 6;
        let step = 1.03f32;
        let mut px = 16.0;
        let mut requested = None;
        while requested.is_none() {
            px *= step;
            let resolution = run.frame(px, false);
            assert_eq!(resolution.draw, Representation::coverage(16));
            requested = resolution.request;
        }
        let target = requested.unwrap();
        assert!(target.is_scalable());
        let work = run.work();
        for _ in 0..run.latency {
            px *= step;
            let resolution = run.frame(px, false);
            if resolution.draw == target {
                break;
            }
            assert_eq!(
                resolution.draw,
                Representation::coverage(16),
                "last-good coverage"
            );
            assert_eq!(run.work(), work, "a pending frame rasterizes nothing");
        }
        assert_eq!(run.state.drawn(), target);
        assert_eq!(run.rasters, 0);
    }

    #[test]
    fn residency_pressure_demotes_to_coverage_rather_than_dropping_the_glyph() {
        let mut run = Driver::new(RunClass::Text, 16.0);
        run.zoom(16.0, 40.0, 60);
        let scaled = run.state.drawn();
        assert_eq!(scaled.kind, GlyphImageKind::ScalableMtsdf);
        assert!(
            run.residency
                .pool_resident_glyphs(GlyphImageKind::ScalableMtsdf)
                > 0
        );
        assert!(
            run.residency
                .shed_pool_to_pressure(GlyphImageKind::ScalableMtsdf, 0)
                > 0
        );
        let mut reclaimed = Vec::new();
        run.residency.take_reclaims(&mut reclaimed);
        assert!(!reclaimed.is_empty());
        for held in run.held.clone() {
            if held.kind == GlyphImageKind::ScalableMtsdf {
                run.state.lost(held);
                run.held.retain(|h| *h != held);
            }
        }
        let fallback = run.frame(42.0, false);
        let coverage = Representation::coverage(42);
        assert_eq!(fallback.draw, coverage, "degraded, not dropped");
        assert_eq!(
            fallback.request,
            Some(coverage),
            "admitted on the coverage path"
        );
        // Still zooming: the cooldown keeps the run from re-promoting into the
        // same pressure.
        let generations = run.generations;
        run.zoom(42.0, 60.0, 30);
        assert_eq!(run.generations, generations);
        assert_eq!(run.state.drawn().kind, GlyphImageKind::MaskA8);
    }

    #[test]
    fn cjk_needs_a_longer_sustained_transform() {
        // A 150 ms zoom promotes general text but leaves CJK on coverage.
        let mut text = Driver::new(RunClass::Text, 16.0);
        let mut cjk = Driver::new(RunClass::Cjk, 16.0);
        text.zoom(16.0, 26.0, 9);
        cjk.zoom(16.0, 26.0, 9);
        text.hold(26.0, 30);
        cjk.hold(26.0, 30);
        assert_eq!(text.promotions, 1);
        assert_eq!(cjk.promotions, 0);
        assert_eq!(cjk.generations, 0);
        // It settles instead: one exact raster at the new bucket.
        assert_eq!(cjk.state.drawn(), Representation::coverage(26));
        assert_eq!(cjk.rasters, 1);
        // A sustained CJK zoom does promote.
        cjk.zoom(26.0, 80.0, 60);
        assert_eq!(cjk.promotions, 1);
    }

    #[test]
    fn a_steady_run_is_never_re_evaluated() {
        let mut run = Driver::new(RunClass::Text, 16.0);
        // Pure translation and style changes are not inputs; same-bucket subpixel
        // scale jitter and a refresh-rate change are not changes.
        for i in 0..500 {
            run.frame(16.0 + if i % 2 == 0 { 0.3 } else { -0.3 }, false);
        }
        run.interval = Duration::from_micros(4_167);
        run.hold(16.0, 500);
        assert_eq!(run.state.evaluations(), 0);
        assert_eq!(run.work(), 0);
    }

    #[test]
    fn a_persisting_rotation_promotes_and_unrotating_demotes() {
        let mut run = Driver::new(RunClass::Text, 16.0);
        for _ in 0..3 {
            run.frame(16.0, true);
        }
        assert_eq!(run.promotions, 0, "a brief rotation promotes nothing");
        for _ in 0..30 {
            run.frame(16.0, true);
        }
        assert_eq!(run.promotions, 1);
        assert_eq!(run.state.drawn(), Representation::mtsdf(16));
        run.hold(16.0, 30);
        assert_eq!(run.demotions, 1);
        assert_eq!(run.state.drawn(), Representation::coverage(16));
    }

    #[test]
    fn a_stale_revision_is_refused_and_invalidation_falls_back_to_coverage() {
        let mut run = Driver::new(RunClass::Text, 16.0);
        run.latency = 100;
        run.zoom(16.0, 30.0, 30);
        let pending = run.state.pending().expect("promotion requested");
        assert!(!run.state.ready(pending, FontRevision(REVISION.0 + 1)));
        run.state.invalidate(FontRevision(REVISION.0 + 1));
        assert!(!run.state.ready(pending, FontRevision(REVISION.0 + 1)));
        let resolution = run.frame(30.0, false);
        assert_eq!(resolution.draw, Representation::coverage(30));
        assert_eq!(resolution.request, Some(Representation::coverage(30)));
        assert_eq!(run.state.pending(), None);
    }

    #[test]
    fn a_glyph_never_holds_coverage_a_field_and_an_outline_at_once() {
        let mut run = Driver::new(RunClass::Text, 16.0);
        run.zoom(16.0, 600.0, 180);
        assert_eq!(run.state.drawn(), Representation::OUTLINE);
        run.zoom(600.0, 20.0, 180);
        run.hold(20.0, 60);
        assert_eq!(run.promotions, 1);
        assert_eq!(run.demotions, 1);
        assert!(run.most_held <= 2, "held {} at once", run.most_held);
        assert_eq!(run.held, vec![Representation::coverage(20)]);
    }
}
