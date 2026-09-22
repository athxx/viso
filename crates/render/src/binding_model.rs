//! How a draw addresses the textures it samples (§20.2).
//!
//! A draw fixes its resources. Bind one texture per draw and a frame that paints
//! twenty images from twenty textures costs twenty draws, because each texture
//! change ends the batch. §20.2 names the fast path out of that: where the backend
//! offers a resource table — Metal argument/resource tables, a D3D12 descriptor
//! heap, Vulkan descriptor indexing, WebGPU binding arrays — the instance carries
//! a `texture_index` into the table and the texture stops being draw state at all.
//!
//! And then it names the fallback, which is the part that has to be good: **atlas,
//! a small texture set, and bind-group batching**. That is not a consolation
//! prize. Almost every real UI frame samples a handful of distinct resources — one
//! glyph atlas, one icon atlas, a gradient LUT page — so once adjacent draws that
//! share a resource *merge*, the fallback reaches the same draw count bindless
//! would, without any backend support at all. Bindless pays only past the point
//! where the resource set stops being small.
//!
//! The rule §20.2 states about both halves is that the **public paint API does not
//! change with the answer**. A host draws [`crate::Primitive::Image`]; it never
//! selects a binding model, never sees a `texture_index`, and never writes a
//! different scene for a backend without tables. So this module is the decision
//! and nothing else: [`BindingModel::select`] says which model a frame's texture
//! workload belongs in, and the renderer's actual batching — the fallback — is
//! implemented where batches are formed, not here.
//!
//! The thresholds are internal benchmark parameters (§20.2 fixes no ABI): they are
//! the crossover of one atlas policy against one table's indexing cost, and both
//! move. What is stable is the shape — a capability veto, a resource set too large
//! to atlas, and a batch-break count that is actually costing draws.

/// How a draw names the textures it samples (§20.2).
///
/// Not a quality ranking. Both models produce identical pixels; they differ in how
/// many draws a frame of images costs, and only once the resource set is large.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BindingModel {
    /// One bind group per draw: the sampled texture is draw state, so changing it
    /// ends the batch.
    ///
    /// The default and the answer for every backend this renderer has. Its cost is
    /// a function of how many *distinct* resources a frame touches, not of how many
    /// images it paints — which is why atlasing and merging adjacent same-resource
    /// draws is the whole fallback, and why that fallback is usually enough.
    #[default]
    PerDraw,
    /// The draw binds a table of `slots` textures and each instance selects one by
    /// index (§20.2).
    ///
    /// Wins where the resource set is genuinely large and unatlasable — a document
    /// of hundreds of independent images, a tile grid streaming distinct pages —
    /// because a texture change then costs nothing at all. `slots` is the table's
    /// capacity: a frame needing more distinct textures than that would have to
    /// page the table per draw, which is the per-draw cost again under another
    /// name, so it is not selected.
    Bindless {
        /// How many textures one draw can address through the table.
        slots: u32,
    },
}

/// Distinct textures a frame can sample before atlasing and bind-group batching
/// stop being the cheaper answer.
///
/// A UI frame's resource set is small by construction: a glyph atlas, an icon
/// atlas, a gradient LUT page, a handful of offscreen layer targets. At that size
/// merging adjacent same-resource draws already collapses the frame to a few
/// batches, and a table would add indexing to save nothing. Internal benchmark
/// parameter (§20.2).
const SMALL_TEXTURE_SET: u32 = 16;

/// Texture-caused batch breaks per frame below which the per-draw model is not
/// costing anything worth fixing.
///
/// The count that matters is not how many textures exist but how often paint order
/// *alternates* between them — a hundred images drawn atlas-first cost one break.
/// Below this floor the draws bindless would remove are too few to pay for the
/// table. Internal benchmark parameter (§20.2).
const COSTLY_BATCH_BREAKS: u32 = 64;

/// What a frame asks of the texture binding path, as the §20.2 entry conditions
/// name it.
///
/// Every field but the capability is a property of the *frame*, so the decision is
/// portable: a backend without tables simply never offers the fast path, and the
/// same scene keeps working.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextureWorkload {
    /// Table capacity the backend reports, or `0` when it binds per draw — the
    /// [`viso_gpu::Caps::bindless_texture_slots`] veto.
    pub bindless_slots: u32,
    /// Distinct textures sampled this frame, after atlasing. Counting *after*
    /// atlasing is the point: atlasing is the first answer to a large resource set,
    /// and bindless is only the answer to what atlasing could not merge.
    pub distinct_textures: u32,
    /// Draws this frame that ended because the next primitive sampled a different
    /// texture — the cost the fast path removes, measured rather than assumed
    /// (§7.3).
    pub texture_batch_breaks: u32,
}

impl TextureWorkload {
    /// Whether the frame's resource set is past the size atlasing and a small
    /// texture set handle well.
    pub fn resource_set_is_large(&self) -> bool {
        self.distinct_textures > SMALL_TEXTURE_SET
    }

    /// Whether per-draw binding is *measurably* costing this frame draws.
    ///
    /// Separate from the set size because the two come apart: a frame can sample
    /// many textures and break almost no batches if paint order groups them, and
    /// that frame has nothing for a table to fix.
    pub fn binding_costs_draws(&self) -> bool {
        self.texture_batch_breaks >= COSTLY_BATCH_BREAKS
    }

    /// Whether the table could hold the frame's whole resource set.
    ///
    /// A frame that overflows the table has to rebind it mid-frame, which is the
    /// per-draw cost with extra indexing on top.
    pub fn fits_in_the_table(&self) -> bool {
        self.bindless_slots > 0 && self.distinct_textures <= self.bindless_slots
    }
}

impl BindingModel {
    /// Pick the binding model for a frame's texture workload (§20.2).
    ///
    /// Conjunctive, like every other §20 lane decision: the backend must offer a
    /// table large enough for the frame, the resource set must be past what
    /// atlasing answers, and per-draw binding must actually be breaking batches.
    /// Any one missing yields [`BindingModel::PerDraw`], so every incomplete
    /// description of a frame lands on the model that works on every backend —
    /// which is what makes "the public paint API does not change across backends"
    /// (§20.2) true by construction rather than by review.
    pub fn select(workload: TextureWorkload) -> BindingModel {
        if workload.fits_in_the_table()
            && workload.resource_set_is_large()
            && workload.binding_costs_draws()
        {
            BindingModel::Bindless {
                slots: workload.bindless_slots,
            }
        } else {
            BindingModel::PerDraw
        }
    }

    /// Whether an instance carries a texture index under this model — the §20.2
    /// `instance.texture_index` fast path.
    ///
    /// `false` for [`PerDraw`](Self::PerDraw), which is why no instance ABI in this
    /// renderer has that field: adding it to a frozen layout no backend can index
    /// would cost every draw four bytes of bandwidth to carry a constant.
    pub fn indexes_per_instance(self) -> bool {
        matches!(self, BindingModel::Bindless { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The workload §20.2 names the fast path for: a large unatlasable resource set
    /// interleaved in paint order, on a backend with a table big enough for it.
    fn a_document_of_hundreds_of_images() -> TextureWorkload {
        TextureWorkload {
            bindless_slots: 4_096,
            distinct_textures: 600,
            texture_batch_breaks: 600,
        }
    }

    #[test]
    fn the_model_every_backend_has_is_the_default_one() {
        assert_eq!(BindingModel::default(), BindingModel::PerDraw);
        assert_eq!(
            BindingModel::select(TextureWorkload::default()),
            BindingModel::PerDraw,
            "a frame nobody described must not require a resource table"
        );
        assert!(!BindingModel::PerDraw.indexes_per_instance());
    }

    #[test]
    fn a_large_interleaved_resource_set_is_what_bindless_is_for() {
        let model = BindingModel::select(a_document_of_hundreds_of_images());
        assert_eq!(model, BindingModel::Bindless { slots: 4_096 });
        assert!(model.indexes_per_instance());
    }

    /// Each condition alone returns the answer to the portable model, so the
    /// conjunction cannot decay into a disjunction.
    #[test]
    fn every_condition_is_individually_necessary() {
        let base = a_document_of_hundreds_of_images();
        for (missing, workload) in [
            (
                "the backend has no table",
                TextureWorkload {
                    bindless_slots: 0,
                    ..base
                },
            ),
            (
                "the table is smaller than the frame's set",
                TextureWorkload {
                    bindless_slots: 64,
                    ..base
                },
            ),
            (
                "atlasing left a small set",
                TextureWorkload {
                    distinct_textures: SMALL_TEXTURE_SET,
                    ..base
                },
            ),
            (
                "paint order broke no batches",
                TextureWorkload {
                    texture_batch_breaks: 0,
                    ..base
                },
            ),
        ] {
            assert_eq!(
                BindingModel::select(workload),
                BindingModel::PerDraw,
                "{missing}: the bindless path must not be selected"
            );
        }
    }

    /// The §20.2 hard rule's own shape: an ordinary UI frame — one glyph atlas, one
    /// icon atlas, a gradient page, a few layer targets — stays on the portable
    /// model even on a backend with a huge table, because atlasing already did the
    /// work a table would do.
    #[test]
    fn an_ordinary_frame_does_not_need_a_resource_table() {
        for (label, distinct, breaks) in [
            ("a text-heavy screen", 1, 0),
            ("icons, text and a gradient", 3, 2),
            ("the same, under four translucent layers", 7, 6),
            ("a hundred icons from one atlas", 2, 1),
        ] {
            let frame = TextureWorkload {
                bindless_slots: 500_000,
                distinct_textures: distinct,
                texture_batch_breaks: breaks,
            };
            assert_eq!(
                BindingModel::select(frame),
                BindingModel::PerDraw,
                "{label} must not require a resource table"
            );
        }
    }

    /// Many textures, grouped in paint order: the set is large but nothing is
    /// breaking, so there are no draws for a table to save. This is the case that
    /// separates "large" from "costly" and keeps the decision honest (§7.3).
    #[test]
    fn a_large_but_well_ordered_set_has_nothing_to_fix() {
        let grouped = TextureWorkload {
            texture_batch_breaks: 4,
            ..a_document_of_hundreds_of_images()
        };
        assert!(grouped.resource_set_is_large());
        assert!(!grouped.binding_costs_draws());
        assert_eq!(BindingModel::select(grouped), BindingModel::PerDraw);
    }

    /// Both floors are floors, not ranges.
    #[test]
    fn the_floors_are_exact() {
        let at_set = TextureWorkload {
            distinct_textures: SMALL_TEXTURE_SET + 1,
            ..a_document_of_hundreds_of_images()
        };
        let below_set = TextureWorkload {
            distinct_textures: SMALL_TEXTURE_SET,
            ..a_document_of_hundreds_of_images()
        };
        assert!(matches!(
            BindingModel::select(at_set),
            BindingModel::Bindless { .. }
        ));
        assert_eq!(BindingModel::select(below_set), BindingModel::PerDraw);

        let at_breaks = TextureWorkload {
            texture_batch_breaks: COSTLY_BATCH_BREAKS,
            ..a_document_of_hundreds_of_images()
        };
        let below_breaks = TextureWorkload {
            texture_batch_breaks: COSTLY_BATCH_BREAKS - 1,
            ..a_document_of_hundreds_of_images()
        };
        assert!(matches!(
            BindingModel::select(at_breaks),
            BindingModel::Bindless { .. }
        ));
        assert_eq!(BindingModel::select(below_breaks), BindingModel::PerDraw);
    }

    /// A table exactly the size of the frame's set fits; one slot short does not.
    #[test]
    fn the_table_must_hold_the_whole_set() {
        let exact = TextureWorkload {
            bindless_slots: 600,
            ..a_document_of_hundreds_of_images()
        };
        let short = TextureWorkload {
            bindless_slots: 599,
            ..a_document_of_hundreds_of_images()
        };
        assert_eq!(
            BindingModel::select(exact),
            BindingModel::Bindless { slots: 600 }
        );
        assert_eq!(BindingModel::select(short), BindingModel::PerDraw);
    }
}
