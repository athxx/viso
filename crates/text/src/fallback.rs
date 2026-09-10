//! Run/cluster font fallback: when the requested face cannot cover a contiguous
//! run, choose a covering face by asking the platform *once per run* and reusing
//! the answer for later runs of the same shape.
//!
//! Fallback runs during shaping preparation, off the main thread, on the
//! coverage-miss cold path — never per glyph draw. It takes a run the requested
//! face missed (`.notdef`, see [`crate::shaping::ShapedRun::has_coverage_miss`])
//! and returns a face that covers it, plus how much of the run that face maps.
//!
//! # Run/cluster granularity, never per Unicode scalar
//!
//! The platform fallback resolver ([`crate::system_fonts::SystemFontProvider`])
//! is queried for a whole contiguous run, not for each scalar. A normal CJK page
//! must not call CoreText / DirectWrite / fontconfig / the Android matcher once
//! per Han character. The first run of a given shape pays one OS query; the
//! resolved candidate is remembered in a [`FallbackPlan`] and reused.
//!
//! # The FallbackPlan candidate cache
//!
//! A [`FallbackPlanKey`] — base face, script, locale, and style — maps to the
//! candidate face last resolved for that shape. A later run of the same shape
//! checks the candidate's *local* coverage first (its `cmap`, no OS call): if it
//! covers the run it is used directly; only a local miss re-queries the OS. This
//! is the deliberate divergence from a fixed ordered cascade: fallback is a
//! per-shape learned plan, and locale is part of the key so the same Han scalar
//! can resolve to a different face under `zh-Hans` / `zh-Hant` / `ja` / `ko`.
//!
//! Variation coordinates and presentation mode are part of the plan key's
//! eventual identity (per the runtime spec) and are added with the color/emoji
//! and variable-font slices; the key already carries the fields that are live.
//!
//! # Cluster atomicity, never a font split inside a grapheme
//!
//! Coverage and the mapped boundary are measured over extended grapheme
//! clusters (UAX#29), not scalars. A cluster is covered only when the face has a
//! glyph for *every* scalar in it, and the mapped length always lands on a
//! cluster boundary. An emoji cluster — a ZWJ sequence like `👩‍💻`, a
//! skin-tone or variation-selector sequence, a regional-indicator flag, a keycap
//! — is therefore covered as one unit or not at all: it is never split into
//! `👩` + ZWJ + `💻` and routed to three faces. A face that covers only the
//! leading scalar of a cluster contributes nothing, so the whole cluster falls
//! through to the emoji fallback face intact.

use std::collections::HashMap;

use unicode_script::{Script, UnicodeScript};
use unicode_segmentation::UnicodeSegmentation;

use crate::FontFaceId;
use crate::font_request::{FontSlant, FontWeight, FontWidth};
use crate::system_fonts::{SystemFontProvider, SystemFontQuery};

/// The style attributes that, together with the base face and locale, identify a
/// fallback plan. Two runs that differ in any of these may resolve to different
/// fallback faces, so they must not share a cached candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FallbackStyle {
    /// Requested weight.
    pub weight: FontWeight,
    /// Requested width / stretch.
    pub width: FontWidth,
    /// Requested slant.
    pub slant: FontSlant,
}

impl Default for FallbackStyle {
    fn default() -> Self {
        Self {
            weight: FontWeight::REGULAR,
            width: FontWidth::NORMAL,
            slant: FontSlant::Normal,
        }
    }
}

/// The key a resolved fallback candidate is remembered under.
///
/// A normal CJK page shares one key across all its runs, so the first run's OS
/// query serves the rest. Locale is part of the key because the same Unicode
/// scalar can prefer a different face under different languages (the runtime
/// spec's CJK locale requirement).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FallbackPlanKey {
    /// The requested/base face that missed coverage.
    pub base: FontFaceId,
    /// The run's dominant script.
    pub script: Script,
    /// BCP-47 language / locale hint (empty = none).
    pub locale: String,
    /// Requested style attributes.
    pub style: FallbackStyle,
    /// Font-source revision; a system font-set change bumps it and invalidates
    /// every remembered candidate that predates the change.
    pub source_revision: u32,
}

/// The outcome of planning fallback for one run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackPlan {
    /// A covering face was found. `mapped_len` is how many leading bytes of the
    /// run that face covers, so a partially-covered run can be split and the
    /// remainder re-planned.
    Mapped {
        /// The face to shape the covered span with.
        face: FontFaceId,
        /// Bytes of the run, from its start, the face covers.
        mapped_len: usize,
    },
    /// Neither the remembered candidate nor the platform could cover the run;
    /// the caller renders `.notdef` for it.
    Unresolved,
}

/// A loaded fallback candidate: owned face bytes and index, so a later run can
/// test the candidate's local coverage without touching the OS. The candidate's
/// [`FontFaceId`] is its position in the planner's `faces` vector plus `id_base`.
#[derive(Debug)]
struct Candidate {
    bytes: Vec<u8>,
    index: u32,
}

impl Candidate {
    /// Bytes of `run`, from its start, this candidate's `cmap` covers, measured
    /// over extended grapheme clusters so the boundary never falls inside one.
    ///
    /// A cluster counts as covered only when the face has a glyph for *every*
    /// scalar in it; the first cluster with any missing scalar stops the mapping.
    /// This keeps emoji ZWJ / skin-tone / variation-selector / flag sequences
    /// atomic: a face that covers the base emoji but not the joiner or the
    /// trailing scalar contributes zero for that cluster, so the whole cluster is
    /// re-planned onto the emoji fallback face rather than split across faces.
    /// Zero means the candidate covers no leading cluster of the run.
    fn mapped_len(&self, run: &str) -> usize {
        let Ok(face) = ttf_parser::Face::parse(&self.bytes, self.index) else {
            return 0;
        };
        let mut mapped = 0;
        for (offset, cluster) in run.grapheme_indices(true) {
            if cluster.chars().all(|ch| face.glyph_index(ch).is_some()) {
                mapped = offset + cluster.len();
            } else {
                break;
            }
        }
        mapped
    }
}

/// The run/cluster fallback planner with its per-shape candidate cache.
///
/// It interns each distinct resolved fallback face once (so the same system face
/// reused across runs keeps one [`FontFaceId`]) and remembers, per
/// [`FallbackPlanKey`], the candidate to try first. The counters mirror the
/// runtime spec's fallback instrumentation.
#[derive(Debug, Default)]
pub struct FontFallback {
    /// Per-shape remembered candidate; the plan-cache the spec requires.
    plans: HashMap<FallbackPlanKey, FontFaceId>,
    /// Interned fallback faces: dense id -> owned bytes, so local coverage of a
    /// remembered candidate is testable without the OS.
    faces: Vec<Candidate>,
    /// System faces already interned, keyed by their bytes' identity, so the
    /// same resolved face dedups to one id across runs.
    interned: HashMap<(u64, u32), FontFaceId>,
    /// Base id space offset: fallback face ids start above the resolver's app /
    /// system faces so the two id spaces never collide.
    id_base: u32,
    /// Counter: platform fallback queries issued (runs, never scalars).
    system_fallback_query_count: u64,
    /// Counter: bytes of run text mapped by a platform fallback query.
    system_fallback_mapped_clusters: u64,
    /// Counter: plan-cache hits — a remembered candidate covered the run with no
    /// OS query.
    fallback_plan_hit: u64,
    /// Counter: plan-cache misses — no remembered candidate covered, so the OS
    /// was queried.
    fallback_plan_miss: u64,
}

impl FontFallback {
    /// A planner whose fallback face ids start at `id_base`, above the id space
    /// the resolver hands out, so the two never collide.
    pub fn new(id_base: u32) -> Self {
        Self {
            id_base,
            ..Self::default()
        }
    }

    /// The dominant script of a run: the first strongly-scripted scalar's script,
    /// ignoring `Common` / `Inherited` (spaces, digits, punctuation) which carry
    /// no fallback signal. Returns `Common` if the run has no strong script.
    pub fn run_script(run: &str) -> Script {
        run.chars()
            .map(|c| c.script())
            .find(|&s| s != Script::Common && s != Script::Inherited)
            .unwrap_or(Script::Common)
    }

    /// Plan fallback for one contiguous coverage-miss run.
    ///
    /// `key` identifies the shape (base face, script, locale, style, revision);
    /// `run` is the run text the base face could not cover; `provider` is the
    /// platform system-font seam. The remembered candidate is tried first with a
    /// local coverage check (no OS call); only a local miss queries the OS, once
    /// for the whole run. A newly resolved candidate is remembered under `key`.
    pub fn plan_run(
        &mut self,
        key: &FallbackPlanKey,
        run: &str,
        provider: &dyn SystemFontProvider,
    ) -> FallbackPlan {
        // Try the remembered candidate's local coverage first: a normal CJK page
        // takes this path for every run after the first, with no OS query.
        if let Some(&face) = self.plans.get(key) {
            let mapped = self.faces[(face.0 - self.id_base) as usize].mapped_len(run);
            if mapped > 0 {
                self.fallback_plan_hit += 1;
                return FallbackPlan::Mapped {
                    face,
                    mapped_len: mapped,
                };
            }
        }

        // No remembered candidate covered the run: ask the platform once.
        self.fallback_plan_miss += 1;
        let query = SystemFontQuery {
            role: crate::font_request::FontRole::Ui,
            weight: key.style.weight,
            width: key.style.width,
            slant: key.style.slant,
            lang: key.locale.clone(),
            sample: run.to_owned(),
        };
        self.system_fallback_query_count += 1;
        let Some(result) = provider.resolve_system_face(&query) else {
            return FallbackPlan::Unresolved;
        };

        let face = self.intern(result.bytes, result.index);
        let mapped = self.faces[(face.0 - self.id_base) as usize].mapped_len(run);
        if mapped == 0 {
            return FallbackPlan::Unresolved;
        }

        self.system_fallback_mapped_clusters += mapped as u64;
        self.plans.insert(key.clone(), face);
        FallbackPlan::Mapped {
            face,
            mapped_len: mapped,
        }
    }

    /// Owned sfnt bytes and face index for a resolved fallback face, for the Face
    /// Cache to build a `ttf-parser` / `rustybuzz` face. Cold path only.
    pub fn face_bytes(&self, face: FontFaceId) -> Option<(&[u8], u32)> {
        face.0
            .checked_sub(self.id_base)
            .and_then(|i| self.faces.get(i as usize))
            .map(|c| (c.bytes.as_slice(), c.index))
    }

    /// Intern a resolved system face to a stable fallback id, deduping identical
    /// bytes so the same face reused across runs keeps one id.
    fn intern(&mut self, bytes: Vec<u8>, index: u32) -> FontFaceId {
        let ident = (fnv1a(&bytes), index);
        if let Some(&id) = self.interned.get(&ident) {
            return id;
        }
        let id = FontFaceId(self.id_base + self.faces.len() as u32);
        self.faces.push(Candidate { bytes, index });
        self.interned.insert(ident, id);
        id
    }

    /// Platform fallback queries issued (per run, never per scalar).
    pub fn system_fallback_query_count(&self) -> u64 {
        self.system_fallback_query_count
    }

    /// Bytes of run text mapped by platform fallback queries.
    pub fn system_fallback_mapped_clusters(&self) -> u64 {
        self.system_fallback_mapped_clusters
    }

    /// Plan-cache hits: a remembered candidate covered a run with no OS query.
    pub fn fallback_plan_hit(&self) -> u64 {
        self.fallback_plan_hit
    }

    /// Plan-cache misses: no remembered candidate covered, so the OS was queried.
    pub fn fallback_plan_miss(&self) -> u64 {
        self.fallback_plan_miss
    }
}

/// A small FNV-1a hash over face bytes for candidate dedup identity. Only used
/// on the cold intern path, not on any steady-state path.
fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::system_fonts::SystemFontResult;

    /// A subset of DejaVu Sans: covers Latin, misses CJK / emoji / Cyrillic. Used
    /// both as the base face and as the fallback face a counting provider hands
    /// back, so a run's local coverage against it is real, not stubbed.
    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");

    /// A provider that hands back the DejaVu subset for every query, counting
    /// calls so a test can assert the OS is not re-queried per scalar.
    struct CountingProvider {
        calls: Cell<u32>,
        answer: bool,
    }

    impl CountingProvider {
        fn new(answer: bool) -> Self {
            Self {
                calls: Cell::new(0),
                answer,
            }
        }
    }

    impl SystemFontProvider for CountingProvider {
        fn resolve_system_face(&self, _query: &SystemFontQuery) -> Option<SystemFontResult> {
            self.calls.set(self.calls.get() + 1);
            self.answer.then(|| SystemFontResult {
                bytes: DEJAVU.to_vec(),
                index: 0,
            })
        }
    }

    /// A provider that routes by the query's locale, as a real platform matcher
    /// does: the same Han text resolves to a different face under zh-Hans / ja /
    /// ko. It records the locale of each query so a test can assert the planner
    /// threads locale through to the OS. Each locale returns byte-distinct
    /// fallback bytes, standing in for the distinct faces a platform matcher
    /// selects per language (PingFang for zh vs Hiragino for ja); the fixture
    /// carries no real CJK face, so each locale is the Latin-covering DejaVu
    /// with a per-locale count of trailing padding bytes appended — parsers
    /// ignore the tail, so all faces parse at index 0 and cover Latin, while
    /// distinct bytes intern to distinct face identities, which is what the
    /// plan cache turns on.
    struct LocaleRoutingProvider {
        seen_locales: RefCell<Vec<String>>,
    }

    impl LocaleRoutingProvider {
        fn new() -> Self {
            Self {
                seen_locales: RefCell::new(Vec::new()),
            }
        }

        /// The byte-distinct fallback face a locale routes to, mimicking a
        /// platform matcher's per-language face selection. `None` for a locale
        /// the matcher has no face for.
        fn face_bytes_for(locale: &str) -> Option<Vec<u8>> {
            let pad = match locale {
                "zh-Hans" => 1,
                "zh-Hant" => 2,
                "ja" => 3,
                "ko" => 4,
                _ => return None,
            };
            let mut bytes = DEJAVU.to_vec();
            bytes.extend(std::iter::repeat_n(0u8, pad));
            Some(bytes)
        }
    }

    impl SystemFontProvider for LocaleRoutingProvider {
        fn resolve_system_face(&self, query: &SystemFontQuery) -> Option<SystemFontResult> {
            self.seen_locales.borrow_mut().push(query.lang.clone());
            Self::face_bytes_for(&query.lang).map(|bytes| SystemFontResult { bytes, index: 0 })
        }
    }

    fn key(base: u32, script: Script, locale: &str) -> FallbackPlanKey {
        FallbackPlanKey {
            base: FontFaceId(base),
            script,
            locale: locale.to_owned(),
            style: FallbackStyle::default(),
            source_revision: 0,
        }
    }

    #[test]
    fn dominant_script_ignores_common_scalars() {
        // A leading space and digit carry no fallback signal; the first strong
        // scalar's script wins.
        assert_eq!(FontFallback::run_script(" 1你"), Script::Han);
        assert_eq!(FontFallback::run_script("AV"), Script::Latin);
        assert_eq!(FontFallback::run_script("   "), Script::Common);
    }

    #[test]
    fn first_run_queries_os_and_maps_the_run() {
        let provider = CountingProvider::new(true);
        let mut fb = FontFallback::new(1000);
        // A Latin run (the DejaVu fallback covers it fully).
        let plan = fb.plan_run(&key(0, Script::Latin, ""), "AVn", &provider);

        match plan {
            FallbackPlan::Mapped { face, mapped_len } => {
                assert!(face.0 >= 1000, "fallback ids start above the base");
                assert_eq!(mapped_len, "AVn".len(), "the whole run mapped");
            }
            FallbackPlan::Unresolved => panic!("the fallback covers Latin"),
        }
        assert_eq!(provider.calls.get(), 1);
        assert_eq!(fb.system_fallback_query_count(), 1);
        assert_eq!(fb.fallback_plan_miss(), 1);
        assert_eq!(fb.fallback_plan_hit(), 0);
        assert_eq!(fb.system_fallback_mapped_clusters(), "AVn".len() as u64);
    }

    #[test]
    fn common_page_reuses_candidate_without_per_char_os_query() {
        // The DoD: a page of many runs of the same shape queries the OS once and
        // serves every later run from the remembered candidate's local coverage.
        let provider = CountingProvider::new(true);
        let mut fb = FontFallback::new(1000);
        let k = key(0, Script::Latin, "");

        // 200 runs, each covered by the same fallback face.
        for _ in 0..200 {
            let plan = fb.plan_run(&k, "AVWxn", &provider);
            assert!(matches!(plan, FallbackPlan::Mapped { .. }));
        }

        // Exactly one OS query for 200 runs: no per-run, and certainly no
        // per-scalar, platform query.
        assert_eq!(provider.calls.get(), 1);
        assert_eq!(fb.system_fallback_query_count(), 1);
        assert_eq!(fb.fallback_plan_miss(), 1, "only the first run missed");
        assert_eq!(fb.fallback_plan_hit(), 199, "the rest hit the plan cache");
    }

    #[test]
    fn same_face_reused_across_runs_interns_to_one_id() {
        let provider = CountingProvider::new(true);
        let mut fb = FontFallback::new(1000);

        // Two distinct shapes (different locale) both resolve to the DejaVu
        // fallback bytes; the identical bytes must intern to one face id.
        let first = fb.plan_run(&key(0, Script::Latin, "en"), "AV", &provider);
        let second = fb.plan_run(&key(0, Script::Latin, "fr"), "AV", &provider);
        let (FallbackPlan::Mapped { face: a, .. }, FallbackPlan::Mapped { face: b, .. }) =
            (first, second)
        else {
            panic!("both runs map");
        };
        assert_eq!(a, b, "identical fallback bytes dedup to one id");

        // Two OS queries (distinct keys), but one interned face.
        assert_eq!(provider.calls.get(), 2);
        assert_eq!(fb.face_bytes(a).map(|(b, _)| b.len()), Some(DEJAVU.len()));
    }

    #[test]
    fn local_coverage_miss_requeries_os() {
        // A remembered candidate that does not cover a later run must re-query
        // the OS rather than wrongly reusing itself. The DejaVu fallback covers
        // Latin but misses CJK, so a CJK run under the same key re-queries.
        let provider = CountingProvider::new(true);
        let mut fb = FontFallback::new(1000);
        let k = key(0, Script::Latin, "");

        fb.plan_run(&k, "AV", &provider); // remembers DejaVu
        assert_eq!(provider.calls.get(), 1);

        // Same key, but a run the candidate cannot cover: local check fails, so
        // the OS is queried again (and here still cannot cover, so Unresolved).
        let plan = fb.plan_run(&k, "\u{4F60}\u{597D}", &provider);
        assert!(matches!(plan, FallbackPlan::Unresolved));
        assert_eq!(provider.calls.get(), 2, "local miss re-queried the OS");
    }

    #[test]
    fn locale_is_part_of_the_plan_key() {
        // The same base face and script under different locales are distinct
        // plans, each resolved against the platform once (CJK locale rule).
        let provider = CountingProvider::new(true);
        let mut fb = FontFallback::new(1000);

        fb.plan_run(&key(0, Script::Han, "ja"), "AV", &provider);
        fb.plan_run(&key(0, Script::Han, "zh-Hans"), "AV", &provider);
        fb.plan_run(&key(0, Script::Han, "ja"), "AV", &provider);

        // ja and zh-Hans are separate keys (2 misses -> 2 OS queries); the second
        // ja run hits the ja plan.
        assert_eq!(provider.calls.get(), 2);
        assert_eq!(fb.fallback_plan_miss(), 2);
        assert_eq!(fb.fallback_plan_hit(), 1);
    }

    #[test]
    fn source_revision_change_invalidates_the_plan() {
        // Bumping the font-source revision changes the key, so a stale candidate
        // is not reused after a system font-set change.
        let provider = CountingProvider::new(true);
        let mut fb = FontFallback::new(1000);

        let mut k = key(0, Script::Latin, "");
        fb.plan_run(&k, "AV", &provider);
        k.source_revision = 1;
        fb.plan_run(&k, "AV", &provider);

        assert_eq!(provider.calls.get(), 2, "revision bump re-queried the OS");
    }

    #[test]
    fn unresolvable_run_is_unresolved() {
        let provider = CountingProvider::new(false);
        let mut fb = FontFallback::new(1000);
        let plan = fb.plan_run(&key(0, Script::Han, "zh-Hans"), "\u{4F60}", &provider);
        assert_eq!(plan, FallbackPlan::Unresolved);
        assert_eq!(fb.system_fallback_query_count(), 1);
    }

    #[test]
    fn same_han_run_selects_a_different_face_per_locale() {
        // The CJK locale rule: one base face and one Han script, but zh-Hans /
        // zh-Hant / ja / ko each resolve to a *different* face, because the same
        // Unicode Han scalar has a different preferred face per language. The run
        // text is identical across the four plans; only the locale differs.
        let provider = LocaleRoutingProvider::new();
        let mut fb = FontFallback::new(1000);

        let mut faces = Vec::new();
        for locale in ["zh-Hans", "zh-Hant", "ja", "ko"] {
            // The fixture is Latin-only, so the covered run stands in for a Han
            // run; the discriminator under test is locale -> face, not coverage.
            let plan = fb.plan_run(&key(0, Script::Han, locale), "AV", &provider);
            match plan {
                FallbackPlan::Mapped { face, .. } => faces.push(face),
                FallbackPlan::Unresolved => panic!("{locale} resolves a face"),
            }
        }

        // Four distinct faces for the four locales: no two collapse together.
        let mut deduped = faces.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(deduped.len(), 4, "each locale selected a distinct face");

        // The planner faithfully threaded each locale into the OS query rather
        // than dropping or normalizing it.
        assert_eq!(
            *provider.seen_locales.borrow(),
            ["zh-Hans", "zh-Hant", "ja", "ko"],
        );
    }

    #[test]
    fn per_locale_plans_are_independently_cached() {
        // Each locale's face is remembered under its own key: a second run of the
        // same locale hits its plan with no OS query, and does not bleed into
        // another locale's plan.
        let provider = LocaleRoutingProvider::new();
        let mut fb = FontFallback::new(1000);

        let ja = fb.plan_run(&key(0, Script::Han, "ja"), "AV", &provider);
        let ko = fb.plan_run(&key(0, Script::Han, "ko"), "AV", &provider);
        let ja_again = fb.plan_run(&key(0, Script::Han, "ja"), "AVn", &provider);

        let (
            FallbackPlan::Mapped { face: ja_face, .. },
            FallbackPlan::Mapped { face: ko_face, .. },
            FallbackPlan::Mapped {
                face: ja_again_face,
                ..
            },
        ) = (ja, ko, ja_again)
        else {
            panic!("all three resolve");
        };

        assert_ne!(ja_face, ko_face, "ja and ko keep distinct faces");
        assert_eq!(ja_again_face, ja_face, "the second ja run reused ja's plan");
        // Two OS queries (ja, ko); the second ja run was a plan-cache hit.
        assert_eq!(provider.seen_locales.borrow().len(), 2);
        assert_eq!(fb.fallback_plan_hit(), 1);
        assert_eq!(fb.fallback_plan_miss(), 2);
    }

    #[test]
    fn emoji_zwj_sequence_is_never_split_across_faces() {
        // "A" + the woman-technologist ZWJ sequence. The DejaVu fallback covers
        // the leading "A" but none of the emoji cluster's scalars, so the whole
        // cluster must fall through as one unit: the mapped length stops exactly
        // at the emoji cluster boundary, never partway into 👩‍💻 (which would
        // route 👩 / ZWJ / 💻 to three faces).
        let provider = CountingProvider::new(true);
        let mut fb = FontFallback::new(1000);
        let run = "A\u{1F469}\u{200D}\u{1F4BB}";

        let plan = fb.plan_run(&key(0, Script::Common, ""), run, &provider);
        let FallbackPlan::Mapped { mapped_len, .. } = plan else {
            panic!("the leading 'A' maps");
        };
        assert_eq!(
            mapped_len,
            "A".len(),
            "mapping stops at the emoji cluster boundary, not inside it"
        );
        // The boundary is a real grapheme edge: the remainder is exactly the one
        // atomic emoji cluster, not a fragment of it.
        let (_, rest) = run.split_at(mapped_len);
        assert_eq!(
            rest.graphemes(true).count(),
            1,
            "the unmapped remainder is one whole emoji cluster"
        );
    }

    #[test]
    fn partly_covered_cluster_maps_as_uncovered() {
        // A base letter the fixture covers, combined with a combining acute it
        // does not: "a" + U+0301 is one grapheme. A per-scalar walk would map the
        // 'a' and split the cluster; cluster-atomic coverage rejects the whole
        // composed cluster, so a leading covered letter followed by such a cluster
        // maps only the standalone letter and stops at the cluster edge.
        let provider = CountingProvider::new(true);
        let mut fb = FontFallback::new(1000);
        // "e" (standalone, covered) then "a" + combining acute (one cluster).
        let run = "e\u{0061}\u{0301}";

        let plan = fb.plan_run(&key(0, Script::Latin, ""), run, &provider);
        let FallbackPlan::Mapped { mapped_len, .. } = plan else {
            panic!("the leading 'e' maps");
        };
        assert_eq!(
            mapped_len,
            "e".len(),
            "the composed cluster is not split: only the standalone letter mapped"
        );
    }

    #[test]
    fn fully_covered_multi_scalar_run_maps_whole() {
        // Every scalar (each its own grapheme here) is covered, so cluster-atomic
        // mapping still maps the entire run — atomicity does not shrink coverage
        // when the face genuinely covers everything.
        let provider = CountingProvider::new(true);
        let mut fb = FontFallback::new(1000);
        let run = "Aenx";

        let plan = fb.plan_run(&key(0, Script::Latin, ""), run, &provider);
        assert_eq!(
            plan,
            FallbackPlan::Mapped {
                face: FontFaceId(1000),
                mapped_len: run.len(),
            }
        );
    }
}
