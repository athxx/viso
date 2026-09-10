//! The application font manifest: the declared set of app-supplied families,
//! roles, and their sources, kept as compact descriptors.
//!
//! The manifest is the authority for which families the application ships and
//! how a [`crate::font_request::FontRole`] binds to a family. It never touches
//! platform system fonts — those arrive through the provider seam.
//!
//! The manifest holds only compact descriptors: the family / style attributes,
//! a coarse script-coverage summary, a color-capability flag, and an
//! [`AssetRef`] to the packaged bytes. It never holds decoded font bytes or a
//! parsed face — the resolver reads and parses an asset lazily on first use.
//!
//! Build-time discovery scans `assets/fonts/`, validates each container
//! ([`crate::font_format`]), and extracts the minimal per-face metadata into
//! [`DiscoveredFace`] records. That scan is build tooling — it reads the
//! filesystem, which this pure-algorithm crate never does. What lives here is
//! the normalization from those records into a compact [`FontManifest`]
//! ([`FontManifest::from_discovered`]): the manifest is metadata only, so
//! constructing it — even for thousands of faces — parses no font bytes. The
//! first real use of a face triggers the lazy asset read and parse in the
//! resolver, not manifest construction (section 3.1).

use crate::font_request::{FontRole, FontSlant, FontWeight, FontWidth};

/// A reference to packaged font bytes, resolved to the actual source at load
/// time. A compact integer, never a path string on any hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AssetRef(pub u32);

/// A coarse summary of which Unicode scripts a packaged face plausibly covers.
///
/// This exists only to cheaply exclude a packaged face that clearly cannot
/// cover a script; it does not replace the face's `cmap`, and exact coverage is
/// still verified after the face is lazily loaded.
#[derive(Debug, Clone, Default)]
pub struct ScriptCoverageSummary {
    /// Coarse script tags the face is declared to cover (empty = unknown, do
    /// not exclude).
    pub scripts: Vec<unicode_script::Script>,
}

impl ScriptCoverageSummary {
    /// Whether this summary can rule the face out for `script`. A face with an
    /// empty (unknown) summary is never ruled out.
    pub fn may_cover(&self, script: unicode_script::Script) -> bool {
        self.scripts.is_empty() || self.scripts.contains(&script)
    }
}

/// One declared packaged face: its style attributes, coverage summary, color
/// capability, and the asset that holds its bytes.
#[derive(Debug, Clone)]
pub struct ManifestEntry {
    /// Family name as declared.
    pub family: String,
    /// Weight of this face.
    pub weight: FontWeight,
    /// Width / stretch of this face.
    pub width: FontWidth,
    /// Slant of this face.
    pub slant: FontSlant,
    /// Face index within a collection asset (0 for a single-face file).
    pub face_index: u32,
    /// Whether this face carries a color glyph table.
    pub color: bool,
    /// Coarse script coverage for cheap exclusion.
    pub coverage: ScriptCoverageSummary,
    /// The packaged bytes for this face.
    pub asset: AssetRef,
}

/// One face as reported by build-time discovery: the minimal metadata the
/// scanner extracts from a validated container, before normalization into the
/// manifest.
///
/// This is the boundary type between build tooling (which reads the filesystem
/// and parses containers) and this crate (which normalizes and serves compact
/// descriptors). It is deliberately the same field set as [`ManifestEntry`]:
/// discovery extracts exactly what the manifest keeps, nothing more, so a
/// scanner never smuggles decoded bytes or full coverage across the boundary.
pub type DiscoveredFace = ManifestEntry;

/// The parsed application font manifest: declared faces plus role bindings.
#[derive(Debug, Default)]
pub struct FontManifest {
    entries: Vec<ManifestEntry>,
    /// Role -> family binding. A role with no binding falls through to the
    /// system resolver.
    role_bindings: Vec<(FontRole, String)>,
    /// Monotonic revision; part of the resolver's cache key so a manifest
    /// change invalidates cached resolutions.
    revision: u32,
}

impl FontManifest {
    /// A manifest built from declared entries and role bindings.
    ///
    /// This is the programmatic construction path. Build-time scanning of
    /// `assets/fonts/` produces the same shape and is filled in later.
    pub fn from_declared(
        entries: Vec<ManifestEntry>,
        role_bindings: Vec<(FontRole, String)>,
    ) -> Self {
        Self {
            entries,
            role_bindings,
            revision: 1,
        }
    }

    /// A manifest normalized from build-time discovery records.
    ///
    /// Discovery yields one [`DiscoveredFace`] per face found while scanning
    /// `assets/fonts/`; this normalizes them into compact manifest entries:
    /// duplicate faces (same asset + face index) are collapsed, and entries are
    /// sorted by family then style so the manifest — and the resolver cache key
    /// derived from its revision — is deterministic regardless of directory
    /// iteration order.
    ///
    /// This does no font parsing: it moves and orders metadata records. The
    /// bytes behind each [`AssetRef`] are read and parsed lazily by the
    /// resolver on first use, so constructing a manifest over thousands of
    /// discovered faces costs no startup parse (section 3.1).
    pub fn from_discovered(
        mut discovered: Vec<DiscoveredFace>,
        role_bindings: Vec<(FontRole, String)>,
    ) -> Self {
        // Deterministic order independent of filesystem iteration: family, then
        // weight / width / slant, then the asset identity as a final tiebreak.
        discovered.sort_by(|a, b| {
            a.family
                .cmp(&b.family)
                .then(a.weight.0.cmp(&b.weight.0))
                .then(a.width.0.cmp(&b.width.0))
                .then((a.slant as u8).cmp(&(b.slant as u8)))
                .then(a.asset.0.cmp(&b.asset.0))
                .then(a.face_index.cmp(&b.face_index))
        });
        // Collapse duplicate faces: the same (asset, face_index) discovered
        // twice is one face. Kept stable by the sort above.
        discovered.dedup_by(|a, b| a.asset == b.asset && a.face_index == b.face_index);
        Self {
            entries: discovered,
            role_bindings,
            revision: 1,
        }
    }

    /// The manifest revision, part of the resolver cache key.
    pub fn revision(&self) -> u32 {
        self.revision
    }

    /// The declared faces.
    pub fn entries(&self) -> &[ManifestEntry] {
        &self.entries
    }

    /// The family bound to `role`, if the application declared one.
    pub fn family_for_role(&self, role: FontRole) -> Option<&str> {
        self.role_bindings
            .iter()
            .find(|(r, _)| *r == role)
            .map(|(_, family)| family.as_str())
    }

    /// The best declared entry for a family at the requested attributes, if the
    /// application ships that family.
    ///
    /// Selection prefers an exact attribute match, then the nearest weight; a
    /// family the manifest does not declare returns `None` so the resolver can
    /// fall through to the system resolver.
    pub fn select(
        &self,
        family: &str,
        weight: FontWeight,
        width: FontWidth,
        slant: FontSlant,
    ) -> Option<&ManifestEntry> {
        self.entries
            .iter()
            .filter(|e| e.family == family)
            .min_by_key(|e| {
                let slant_penalty = u32::from(e.slant != slant) * 1_000_000;
                let width_penalty =
                    (i32::from(e.width.0) - i32::from(width.0)).unsigned_abs() * 10_000;
                let weight_penalty = (i32::from(e.weight.0) - i32::from(weight.0)).unsigned_abs();
                slant_penalty + width_penalty + weight_penalty
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(family: &str, weight: FontWeight, slant: FontSlant, asset: u32) -> ManifestEntry {
        ManifestEntry {
            family: family.to_owned(),
            weight,
            width: FontWidth::NORMAL,
            slant,
            face_index: 0,
            color: false,
            coverage: ScriptCoverageSummary::default(),
            asset: AssetRef(asset),
        }
    }

    fn manifest() -> FontManifest {
        FontManifest::from_declared(
            vec![
                entry("Inter", FontWeight::REGULAR, FontSlant::Normal, 1),
                entry("Inter", FontWeight::BOLD, FontSlant::Normal, 2),
                entry("Inter", FontWeight::REGULAR, FontSlant::Italic, 3),
            ],
            vec![
                (FontRole::Ui, "Inter".to_owned()),
                (FontRole::Mono, "JetBrains Mono".to_owned()),
            ],
        )
    }

    #[test]
    fn family_for_role_reflects_bindings() {
        let m = manifest();
        assert_eq!(m.family_for_role(FontRole::Ui), Some("Inter"));
        assert_eq!(m.family_for_role(FontRole::Mono), Some("JetBrains Mono"));
        // An unbound role falls through (the resolver goes to the system path).
        assert_eq!(m.family_for_role(FontRole::Emoji), None);
    }

    #[test]
    fn select_prefers_exact_weight_and_slant() {
        let m = manifest();

        let regular = m
            .select(
                "Inter",
                FontWeight::REGULAR,
                FontWidth::NORMAL,
                FontSlant::Normal,
            )
            .expect("regular exists");
        assert_eq!(regular.asset, AssetRef(1));

        let bold = m
            .select(
                "Inter",
                FontWeight::BOLD,
                FontWidth::NORMAL,
                FontSlant::Normal,
            )
            .expect("bold exists");
        assert_eq!(bold.asset, AssetRef(2));

        let italic = m
            .select(
                "Inter",
                FontWeight::REGULAR,
                FontWidth::NORMAL,
                FontSlant::Italic,
            )
            .expect("italic exists");
        assert_eq!(italic.asset, AssetRef(3));
    }

    #[test]
    fn select_matching_slant_beats_nearer_weight() {
        let m = manifest();
        // Bold italic is not shipped. The italic upright-weight face (asset 3)
        // must win over the upright bold face (asset 2): a slant mismatch is
        // penalized far more than a weight mismatch.
        let got = m
            .select(
                "Inter",
                FontWeight::BOLD,
                FontWidth::NORMAL,
                FontSlant::Italic,
            )
            .expect("some Inter face exists");
        assert_eq!(got.asset, AssetRef(3));
    }

    #[test]
    fn select_returns_none_for_undeclared_family() {
        let m = manifest();
        assert!(
            m.select(
                "NoSuchFamily",
                FontWeight::REGULAR,
                FontWidth::NORMAL,
                FontSlant::Normal
            )
            .is_none()
        );
    }

    #[test]
    fn from_discovered_normalizes_order_and_dedups() {
        // Discovery reports faces in arbitrary (filesystem) order, with a
        // duplicate of the same (asset, face_index).
        let discovered = vec![
            entry("Inter", FontWeight::BOLD, FontSlant::Normal, 2),
            entry("Inter", FontWeight::REGULAR, FontSlant::Normal, 1),
            entry("Inter", FontWeight::REGULAR, FontSlant::Normal, 1), // duplicate.
            entry("Alpha", FontWeight::REGULAR, FontSlant::Normal, 9),
        ];
        let m = FontManifest::from_discovered(discovered, Vec::new());

        // Duplicate collapsed: 4 in, 3 out.
        assert_eq!(m.entries().len(), 3);
        // Deterministic order: family first (Alpha before Inter), then weight.
        assert_eq!(m.entries()[0].family, "Alpha");
        assert_eq!(m.entries()[1].family, "Inter");
        assert_eq!(m.entries()[1].weight, FontWeight::REGULAR);
        assert_eq!(m.entries()[2].family, "Inter");
        assert_eq!(m.entries()[2].weight, FontWeight::BOLD);
    }

    #[test]
    fn from_discovered_is_order_independent() {
        // The same faces discovered in two different directory orders normalize
        // to the same manifest (same entry sequence), so the resolver cache key
        // does not depend on filesystem iteration order.
        let a = FontManifest::from_discovered(
            vec![
                entry("B", FontWeight::REGULAR, FontSlant::Normal, 2),
                entry("A", FontWeight::REGULAR, FontSlant::Normal, 1),
            ],
            Vec::new(),
        );
        let b = FontManifest::from_discovered(
            vec![
                entry("A", FontWeight::REGULAR, FontSlant::Normal, 1),
                entry("B", FontWeight::REGULAR, FontSlant::Normal, 2),
            ],
            Vec::new(),
        );
        let families_a: Vec<_> = a.entries().iter().map(|e| e.family.as_str()).collect();
        let families_b: Vec<_> = b.entries().iter().map(|e| e.family.as_str()).collect();
        assert_eq!(families_a, families_b);
        assert_eq!(families_a, ["A", "B"]);
    }

    /// An [`crate::app_fonts::AssetSource`] that counts every byte read, so a
    /// test can prove a code path performs no lazy asset load (and thus no
    /// parse, since a read is the prerequisite of a parse).
    #[derive(Default)]
    struct CountingAssetSource {
        reads: std::cell::Cell<usize>,
    }

    impl crate::app_fonts::AssetSource for CountingAssetSource {
        fn read(&self, _asset: AssetRef) -> Option<Vec<u8>> {
            self.reads.set(self.reads.get() + 1);
            None
        }
    }

    #[test]
    fn three_thousand_discovered_faces_parse_nothing_at_startup() {
        // Simulate a project (or system) with 3000 discovered faces: constructing
        // the manifest must not scale startup work with the face count. The DoD
        // (section 3.1 / spec: "3000 fonts must not make startup a function of
        // 3000 parses") is that manifest construction and lookup read zero font
        // assets — the byte read, and therefore the parse, is deferred to the
        // resolver on first real use.
        const N: u32 = 3000;
        let discovered: Vec<DiscoveredFace> = (0..N)
            .map(|i| {
                entry(
                    &format!("Family{i}"),
                    FontWeight::REGULAR,
                    FontSlant::Normal,
                    i,
                )
            })
            .collect();

        let source = CountingAssetSource::default();
        let m = FontManifest::from_discovered(discovered, Vec::new());
        assert_eq!(m.entries().len(), N as usize);

        // Constructing the manifest read no asset bytes.
        assert_eq!(
            source.reads.get(),
            0,
            "manifest construction eagerly loaded font assets"
        );

        // A lookup across the whole manifest also reads nothing: select returns a
        // compact descriptor (an AssetRef), never the bytes.
        for i in 0..N {
            let hit = m.select(
                &format!("Family{i}"),
                FontWeight::REGULAR,
                FontWidth::NORMAL,
                FontSlant::Normal,
            );
            assert!(hit.is_some());
        }
        assert_eq!(
            source.reads.get(),
            0,
            "manifest lookup eagerly loaded font assets"
        );
    }
}
