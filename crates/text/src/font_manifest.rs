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
//! Build-time scanning of `assets/fonts/` fills these entries; that discovery
//! is filled in later, so the manifest is constructed from declared entries
//! here.

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
}
