//! Glyph representations at runtime. A run drawn scaled or rotated for long
//! enough draws from distance fields, which stay sharp at any size in their
//! window, and settles back to exact coverage at the size it holds still at.
//!
//! Only runs under a turned node are looked at: the frame names them, and a
//! run under translation alone never takes a state, so it costs nothing and
//! never promotes.

use std::time::Duration;

use viso_gpu::{GpuBackend, TextureDesc};
use viso_render::{MtsdfAlloc, MtsdfAtlas, Rect};
use viso_text::mtsdf::{BUCKETS, MtsdfGlyph};
use viso_text::{
    FontRevision, GlyphImageKind, GlyphKey, Representation, RepresentationState, RunClass,
    TransformSample,
};

use super::{Fate, PLACEMENT_RETRIES, ParagraphSlot, PositionedGlyph, TextShaper, TextSlot};
use crate::text_worker::{Field, bucket};

/// Fields outlive no face change: a replaced face draws under a new id.
const REVISION: FontRevision = FontRevision(0);

/// Where one glyph's field lives in the MTSDF plane.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct FieldPlacement {
    pub(super) uv: Rect,
    /// The field's top left from the pen, in field texels, y up.
    pub(super) left: f32,
    pub(super) top: f32,
    pub(super) size: [u32; 2],
    /// The px per em the field was generated at.
    pub(super) px_per_em: f32,
    pub(super) page: usize,
}

/// The representation a run draws with and the transform history that
/// chooses it, kept while the run is drawn turned or has not settled back.
#[derive(Debug)]
pub(super) struct RunLook {
    state: RepresentationState,
    /// What the run's content draws now.
    pub(super) drawn: Representation,
    /// What it asked for, until every glyph of it is resident.
    waiting: Option<Representation>,
    /// The scale and turn the frame last saw it at.
    scale: f32,
    rotated: bool,
    /// The frame that last saw it turned.
    seen: u64,
    /// The MTSDF pages its drawn fields sit on, touched each frame it draws.
    pages: Vec<usize>,
}

/// Whether every glyph a representation draws is resident.
enum Readiness {
    Ready,
    Missing,
    /// A glyph cannot be drawn this way.
    Failed,
}

impl TextSlot {
    /// The coverage bucket the run is laid out at, untransformed.
    pub(super) fn base_bucket(&self) -> u16 {
        bucket(self.placing.font_size * self.placing.dpi, 1.0)
    }

    /// What the run draws with: its own coverage unless a look says otherwise.
    pub(super) fn representation(&self) -> Representation {
        self.look
            .as_ref()
            .map_or(Representation::coverage(self.base_bucket()), |look| {
                look.drawn
            })
    }
}

/// `glyph`'s key when its run draws `representation` from a base bucket of
/// `base`: its own bucket scaled by the run's.
pub(super) fn representation_key(
    glyph: &PositionedGlyph,
    representation: Representation,
    base: u16,
) -> GlyphKey {
    let ratio = f32::from(glyph.bucket) / f32::from(base.max(1));
    let bucket = match representation.kind {
        GlyphImageKind::ScalableMtsdf => {
            nearest_field_bucket(f32::from(representation.bucket) * ratio)
        }
        _ if representation.bucket == base => glyph.bucket,
        _ => bucket(f32::from(representation.bucket), ratio),
    };
    GlyphKey {
        face: glyph.face,
        glyph: glyph.glyph,
        bucket,
        kind: representation.kind,
    }
}

/// The field ladder's bucket closest to `px`, by ratio.
fn nearest_field_bucket(px: f32) -> u16 {
    let px = px.max(1.0);
    BUCKETS
        .iter()
        .copied()
        .min_by(|a, b| (a / px).ln().abs().total_cmp(&(b / px).ln().abs()))
        .unwrap_or(BUCKETS[0]) as u16
}

/// Han, kana and Hangul take the longer sustain: their strokes show a
/// representation switch more.
fn run_class(text: &str) -> RunClass {
    let cjk = text.chars().any(|c| {
        matches!(c as u32,
            0x1100..=0x11FF | 0x2E80..=0x2FDF | 0x3040..=0x30FF | 0x3100..=0x31FF
            | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF
            | 0x20000..=0x3FFFF)
    });
    if cjk { RunClass::Cjk } else { RunClass::Text }
}

impl TextShaper {
    /// Observe this frame's turned runs, each with the scale and turn it
    /// draws at, and resolve what every run the shaper looks after draws. A
    /// run whose representation switched is pushed onto `updated`, for the
    /// caller to draw again; its extent is unchanged. A frame with no turned
    /// run and no run settling back does nothing.
    pub(crate) fn represent(
        &mut self,
        now: Duration,
        turned: &[(ParagraphSlot, f32, bool)],
        updated: &mut Vec<ParagraphSlot>,
    ) {
        if turned.is_empty() && self.looks.is_empty() {
            return;
        }
        self.look_frame += 1;
        let frame = self.look_frame;
        for &(slot, scale, rotated) in turned {
            let Some(entry) = self.slots.get_mut(&slot) else {
                continue;
            };
            let Some(drawn) = &entry.drawn else {
                continue;
            };
            let look = entry.look.get_or_insert_with(|| {
                let at_rest = TransformSample {
                    now,
                    px_per_em: entry.placing.font_size * entry.placing.dpi,
                    rotated: false,
                    world_space: false,
                };
                let state =
                    RepresentationState::new(run_class(&drawn.target.text), REVISION, &at_rest);
                self.looks.push(slot);
                Box::new(RunLook {
                    drawn: state.drawn(),
                    state,
                    waiting: None,
                    scale: 1.0,
                    rotated: false,
                    seen: 0,
                    pages: Vec::new(),
                })
            });
            look.scale = scale;
            look.rotated = rotated;
            look.seen = frame;
        }
        let mut looks = std::mem::take(&mut self.looks);
        looks.retain(|&slot| self.resolve_look(slot, now, frame, updated));
        self.looks = looks;
        self.fields_reclaimed = false;
    }

    /// Resolve `slot`'s look for this frame; `false` once it no longer needs
    /// one.
    fn resolve_look(
        &mut self,
        slot: ParagraphSlot,
        now: Duration,
        frame: u64,
        updated: &mut Vec<ParagraphSlot>,
    ) -> bool {
        let Some(mut entry) = self.slots.remove(&slot) else {
            return false;
        };
        let keep = match entry.look.take() {
            Some(mut look) => {
                let keep = self.resolve_with(&entry, &mut look, slot, now, frame, updated);
                if keep {
                    entry.look = Some(look);
                }
                keep
            }
            None => false,
        };
        self.slots.insert(slot, entry);
        keep
    }

    fn resolve_with(
        &mut self,
        entry: &TextSlot,
        look: &mut RunLook,
        slot: ParagraphSlot,
        now: Duration,
        frame: u64,
        updated: &mut Vec<ParagraphSlot>,
    ) -> bool {
        if entry.drawn.is_none() {
            return false;
        }
        if look.seen != frame {
            // No longer under a turn: it draws as laid out.
            look.scale = 1.0;
            look.rotated = false;
        }
        let base = entry.base_bucket();
        if self.fields_reclaimed
            && look.drawn.kind == GlyphImageKind::ScalableMtsdf
            && !matches!(
                self.readiness(entry, look.drawn, base, false),
                Readiness::Ready
            )
        {
            look.state.lost(look.drawn);
        }
        if let Some(waiting) = look.waiting {
            match self.readiness(entry, waiting, base, true) {
                Readiness::Ready => {
                    look.state.ready(waiting, REVISION);
                    look.waiting = None;
                }
                Readiness::Missing => {}
                Readiness::Failed => {
                    look.state.lost(waiting);
                    look.waiting = None;
                }
            }
        }
        let sample = TransformSample {
            now,
            px_per_em: entry.placing.font_size * entry.placing.dpi * look.scale,
            rotated: look.rotated,
            world_space: false,
        };
        let evaluations = look.state.evaluations();
        let resolution = look.state.resolve(&sample);
        self.counters
            .record_evaluations(look.state.evaluations() - evaluations);
        if resolution.draw != look.drawn {
            self.counters.record_switch(look.drawn, resolution.draw);
            look.drawn = resolution.draw;
            look.pages.clear();
            if look.drawn.kind == GlyphImageKind::ScalableMtsdf {
                self.field_pages(entry, look.drawn, base, &mut look.pages);
            }
            if !updated.contains(&slot) {
                updated.push(slot);
            }
        }
        match resolution.request {
            // The drawn representation was lost: its coverage is admitted as
            // it draws.
            Some(request) if request == resolution.draw => {}
            // No outline lane draws at runtime: the run keeps its top field.
            Some(request) if request.kind == GlyphImageKind::OutlineVector => {
                look.state.lost(request);
            }
            Some(request) => {
                look.waiting = Some(request);
                let _ = self.readiness(entry, request, base, true);
            }
            None if look.state.pending().is_none() => look.waiting = None,
            None => {}
        }
        for &page in &look.pages {
            self.residency
                .touch_page(GlyphImageKind::ScalableMtsdf, page);
        }
        // Back at rest: it draws its own coverage, nothing is pending and no
        // threshold is armed.
        let at_rest = look.seen != frame
            && look.drawn == Representation::coverage(base)
            && look.state.pending().is_none()
            && look.state.deadline().is_none();
        !at_rest
    }

    /// Whether every glyph `entry` draws as `representation` is resident;
    /// when `ask`, the missing ones are asked of the worker.
    fn readiness(
        &mut self,
        entry: &TextSlot,
        representation: Representation,
        base: u16,
        ask: bool,
    ) -> Readiness {
        let Some(drawn) = &entry.drawn else {
            return Readiness::Failed;
        };
        let field = representation.kind == GlyphImageKind::ScalableMtsdf;
        let mut readiness = Readiness::Ready;
        for glyph in &drawn.placed.glyphs {
            if glyph.emoji {
                continue;
            }
            let key = representation_key(glyph, representation, base);
            let resident = if field {
                self.field_uv.contains_key(&key)
            } else {
                self.coverage_uv.contains_key(&key)
            };
            if resident {
                continue;
            }
            match self.fate.get(&key) {
                // Nothing to draw, or coverage the platform draws.
                Some(Fate::Blank) => {}
                Some(Fate::Platform | Fate::Refused) if !field => {}
                Some(Fate::Platform | Fate::Refused) => return Readiness::Failed,
                Some(Fate::Requested) => readiness = Readiness::Missing,
                None => {
                    readiness = Readiness::Missing;
                    if ask {
                        self.fate.insert(key, Fate::Requested);
                        if field {
                            self.fields_wanted.push(key);
                        } else {
                            self.raster_wanted.push(key);
                        }
                    }
                }
            }
        }
        readiness
    }

    /// The MTSDF pages `entry`'s fields sit on, each once.
    fn field_pages(
        &self,
        entry: &TextSlot,
        representation: Representation,
        base: u16,
        pages: &mut Vec<usize>,
    ) {
        let Some(drawn) = &entry.drawn else {
            return;
        };
        for glyph in &drawn.placed.glyphs {
            let key = representation_key(glyph, representation, base);
            if let Some(placement) = self.field_uv.get(&key)
                && !pages.contains(&placement.page)
            {
                pages.push(placement.page);
            }
        }
    }

    /// The earliest time a looked-after run decides anew with nothing
    /// moving, for the loop to wake at.
    pub(crate) fn representation_deadline(&self) -> Option<Duration> {
        self.looks
            .iter()
            .filter_map(|slot| self.slots.get(slot)?.look.as_ref()?.state.deadline())
            .min()
    }

    /// Whether a looked-after run waits for a representation, so frames must
    /// keep coming until it switches.
    pub(crate) fn representation_waiting(&self) -> bool {
        self.looks.iter().any(|slot| {
            self.slots
                .get(slot)
                .and_then(|entry| entry.look.as_ref())
                .is_some_and(|look| look.waiting.is_some())
        })
    }

    /// The MTSDF pages the looked-after runs draw from.
    pub(super) fn drawn_field_pages(&self) -> Vec<usize> {
        let mut pages = Vec::new();
        for slot in &self.looks {
            if let Some(look) = self.slots.get(slot).and_then(|e| e.look.as_ref()) {
                pages.extend(&look.pages);
            }
        }
        pages
    }

    /// Admit the fields the worker generated.
    pub(super) fn admit_fields<B: GpuBackend>(&mut self, backend: &mut B, fields: Vec<Field>) {
        for (key, field) in fields {
            if !self.worker_faces.contains(&key.face) || self.field_uv.contains_key(&key) {
                self.fate.remove(&key);
                continue;
            }
            match field {
                Some((glyph, _)) if glyph.is_empty() => {
                    self.fate.insert(key, Fate::Blank);
                }
                Some((glyph, texels)) => {
                    if self.admit_field(backend, key, &glyph, &texels) {
                        self.fate.remove(&key);
                        self.counters.record_field();
                    } else {
                        self.fate.insert(key, Fate::Refused);
                    }
                }
                None => {
                    self.fate.insert(key, Fate::Platform);
                }
            }
        }
    }

    fn admit_field<B: GpuBackend>(
        &mut self,
        backend: &mut B,
        key: GlyphKey,
        glyph: &MtsdfGlyph,
        texels: &[u8],
    ) -> bool {
        let bytes = glyph.byte_len();
        for _ in 0..PLACEMENT_RETRIES {
            let (page, fresh) = self.page_for(key, bytes);
            let (size, page_edge) = (self.field_size, self.atlas_page);
            let atlas = self.mtsdf_atlas.get_or_insert_with(|| {
                let texture = backend.create_texture(&TextureDesc {
                    width: size,
                    height: size,
                    format: MtsdfAtlas::FORMAT,
                    render_target: false,
                    label: "ui-glyph-fields",
                });
                MtsdfAtlas::new(size, page_edge, texture)
            });
            match atlas.alloc_in_page(page, glyph, texels) {
                MtsdfAlloc::Placed(uv) => {
                    self.field_uv.insert(
                        key,
                        FieldPlacement {
                            uv,
                            left: glyph.left,
                            top: glyph.top,
                            size: [glyph.width, glyph.height],
                            px_per_em: glyph.px_per_em,
                            page,
                        },
                    );
                    return true;
                }
                MtsdfAlloc::PageFull if fresh => self.revoke(key, page, bytes),
                MtsdfAlloc::PageFull | MtsdfAlloc::Empty | MtsdfAlloc::TooLarge => return false,
            }
        }
        false
    }

    /// The bytes of the MTSDF plane allocated on the device.
    pub(super) fn field_plane_bytes(&self) -> u64 {
        let edge = u64::from(self.field_size);
        self.mtsdf_atlas.as_ref().map_or(0, |_| edge * edge * 4)
    }
}
