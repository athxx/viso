//! Face resolution: turn a [`FontRequest`] into a concrete [`FontFaceId`],
//! interning each distinct face once and reusing its id thereafter.
//!
//! The resolver owns the request -> face mapping and the registry of loaded
//! faces. String / family lookup happens here, at resolution time only; it
//! never appears on a steady-state shaping or paint path — a resolved
//! [`FontFaceId`] is a dense index into the face registry.
//!
//! Resolution is App-first / System-second: a request binds to an app manifest
//! face when the application ships one, otherwise it falls through to the
//! platform system-font provider, otherwise it is [`Missing`]. A small warm
//! Resolve Cache remembers outcomes — including negative ones — so an
//! unsatisfiable family is not queried against the OS on every request.
//!
//! Face identity is assigned by interning a [`FaceKey`]: the first time a
//! distinct key is seen it takes the next dense [`FontFaceId`], and the same
//! key always maps back to that id. Interning gives deterministic dedup (the
//! same fallback face reused across runs keeps one id) with no hash-collision
//! aliasing, and the id doubles as a dense registry index.
//!
//! [`FontRequest`]: crate::font_request::FontRequest
//! [`Missing`]: Resolved::Missing

use std::collections::HashMap;

use crate::FontFaceId;
use crate::font_manifest::{AssetRef, FontManifest};
use crate::font_request::{FontRequest, FontRole, FontSlant, FontTarget, FontWeight, FontWidth};
use crate::system_fonts::{SystemFontProvider, SystemFontQuery};

/// The interning key that gives a face its stable identity.
///
/// Distinct namespaces keep an app face, a system fallback face, and an emoji
/// face from ever colliding even when their attributes coincide, mirroring the
/// namespace separation a deterministic scheme would use — but as an exact key
/// comparison, so two different faces never alias onto one id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum FaceKey {
    /// An application-shipped face, identified by its packaged asset and index.
    App { asset: AssetRef, face_index: u32 },
    /// A platform system face resolved for a role at given attributes.
    System {
        role: FontRole,
        weight: FontWeight,
        width: FontWidth,
        slant: FontSlant,
        lang: String,
    },
}

/// The owned bytes and index backing one registered face.
///
/// A system face arrives with its bytes already owned. An app face is interned
/// for identity when it is first resolved but its bytes are read lazily from
/// the packaged asset, so `bytes` is `None` until the asset is loaded.
#[derive(Debug)]
struct FaceEntry {
    /// Owned sfnt bytes once loaded; `None` for an app face not yet read from
    /// its asset. `ttf-parser` / `rustybuzz` faces are reconstructed from this
    /// on demand.
    bytes: Option<Vec<u8>>,
    /// Face index within `bytes` for a collection; 0 for a single face.
    index: u32,
}

/// The key a resolution outcome is cached under.
///
/// It carries the manifest revision and the system-font revision so that
/// changing either invalidates cached outcomes, per the resolve-cache contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ResolveKey {
    manifest_revision: u32,
    system_revision: u32,
    request: FontRequest,
    lang: String,
}

/// The outcome of resolving a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A concrete face, whether app-supplied or a system face.
    Face(FontFaceId),
    /// No app or system face could satisfy the request; the caller applies the
    /// missing-font policy.
    Missing,
}

/// The face registry and request resolver.
#[derive(Debug, Default)]
pub struct FontResolver {
    /// Dense registry: `faces[id.0 as usize]` is the face for `FontFaceId(id)`.
    faces: Vec<FaceEntry>,
    /// Intern table: a distinct [`FaceKey`] maps to the id it was assigned.
    interned: HashMap<FaceKey, FontFaceId>,
    /// Warm Resolve Cache, including negative (`Missing`) outcomes.
    resolve_cache: HashMap<ResolveKey, Resolved>,
    /// Monotonic system-font revision; bumping it invalidates cached system
    /// resolutions (for example after a system font set change).
    system_revision: u32,
}

impl FontResolver {
    /// A fresh resolver with no registered faces.
    pub fn new() -> Self {
        Self::default()
    }

    /// Intern `key` to a stable id, allocating a dense registry slot the first
    /// time the key is seen.
    ///
    /// The slot starts with `bytes` (owned bytes for a system face, `None` for
    /// an app face whose bytes load lazily). A repeated key returns the
    /// existing id and does not disturb its slot.
    fn intern(&mut self, key: FaceKey, bytes: Option<Vec<u8>>, index: u32) -> FontFaceId {
        if let Some(&id) = self.interned.get(&key) {
            // Fill lazily-loaded bytes into an existing empty slot.
            if let Some(bytes) = bytes {
                let slot = &mut self.faces[id.0 as usize];
                if slot.bytes.is_none() {
                    slot.bytes = Some(bytes);
                }
            }
            return id;
        }
        let id = FontFaceId(self.faces.len() as u32);
        self.faces.push(FaceEntry { bytes, index });
        self.interned.insert(key, id);
        id
    }

    /// Register an application face's owned sfnt bytes, returning its id.
    ///
    /// The `asset` and `face_index` give the face its identity, so this yields
    /// the same id the resolver assigned when it first resolved to this face,
    /// and fills that slot's lazily-loaded bytes.
    pub fn register_app_face(
        &mut self,
        asset: AssetRef,
        face_index: u32,
        sfnt: Vec<u8>,
    ) -> FontFaceId {
        self.intern(FaceKey::App { asset, face_index }, Some(sfnt), face_index)
    }

    /// The owned sfnt bytes and face index for a registered id, if its bytes
    /// are loaded.
    ///
    /// This is a load-time accessor for building a `ttf-parser` / `rustybuzz`
    /// face; it is not a steady-state path. Returns `None` for an app face
    /// whose asset has not been read yet.
    pub fn face_bytes(&self, id: FontFaceId) -> Option<(&[u8], u32)> {
        self.faces
            .get(id.0 as usize)
            .and_then(|f| f.bytes.as_ref().map(|b| (b.as_slice(), f.index)))
    }

    /// Bump the system-font revision, invalidating cached system resolutions.
    pub fn bump_system_revision(&mut self) {
        self.system_revision = self.system_revision.wrapping_add(1);
    }

    /// Resolve a request to a face, App-first then System-second.
    ///
    /// `manifest` is the app's declared families; `provider` is the platform
    /// system-font seam; `lang` is a BCP-47 hint used for CJK disambiguation.
    /// Outcomes — including `Missing` — are cached, so an unsatisfiable family
    /// is not re-queried against the OS.
    pub fn resolve(
        &mut self,
        request: &FontRequest,
        manifest: &FontManifest,
        provider: &dyn SystemFontProvider,
        lang: &str,
    ) -> Resolved {
        let cache_key = ResolveKey {
            manifest_revision: manifest.revision(),
            system_revision: self.system_revision,
            request: request.clone(),
            lang: lang.to_owned(),
        };
        if let Some(hit) = self.resolve_cache.get(&cache_key) {
            return hit.clone();
        }

        let outcome = self.resolve_uncached(request, manifest, provider, lang);
        self.resolve_cache.insert(cache_key, outcome.clone());
        outcome
    }

    /// The App-first / System-second resolution, run only on a cache miss.
    fn resolve_uncached(
        &mut self,
        request: &FontRequest,
        manifest: &FontManifest,
        provider: &dyn SystemFontProvider,
        lang: &str,
    ) -> Resolved {
        // Determine the family to look for and the role to fall through on.
        let (family, role): (Option<&str>, FontRole) = match &request.target {
            FontTarget::Family(name) => (Some(name.as_str()), FontRole::Ui),
            FontTarget::Role(role) => (manifest.family_for_role(*role), *role),
        };

        // App-first: a manifest family the application ships.
        if let Some(family) = family
            && let Some(entry) =
                manifest.select(family, request.weight, request.width, request.slant)
        {
            // The bytes are read lazily elsewhere; here we intern identity so a
            // repeated request returns the same id without re-reading.
            let id = self.intern(
                FaceKey::App {
                    asset: entry.asset,
                    face_index: entry.face_index,
                },
                None,
                entry.face_index,
            );
            return Resolved::Face(id);
        }

        // System-second: ask the platform for a face for the role.
        let query = SystemFontQuery {
            role,
            weight: request.weight,
            width: request.width,
            slant: request.slant,
            lang: lang.to_owned(),
            sample: String::new(),
        };
        if let Some(result) = provider.resolve_system_face(&query) {
            let id = self.intern(
                FaceKey::System {
                    role,
                    weight: request.weight,
                    width: request.width,
                    slant: request.slant,
                    lang: lang.to_owned(),
                },
                Some(result.bytes),
                result.index,
            );
            return Resolved::Face(id);
        }

        Resolved::Missing
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::font_manifest::{ManifestEntry, ScriptCoverageSummary};
    use crate::system_fonts::SystemFontResult;

    /// A provider that satisfies any query with fixed bytes, counting calls so a
    /// test can assert the OS is not re-queried.
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
                bytes: vec![0u8; 4],
                index: 0,
            })
        }
    }

    fn app_entry(family: &str, weight: FontWeight, asset: u32) -> ManifestEntry {
        ManifestEntry {
            family: family.to_owned(),
            weight,
            width: FontWidth::NORMAL,
            slant: FontSlant::Normal,
            face_index: 0,
            color: false,
            coverage: ScriptCoverageSummary::default(),
            asset: AssetRef(asset),
        }
    }

    #[test]
    fn app_first_when_manifest_ships_family() {
        let manifest = FontManifest::from_declared(
            vec![app_entry("Inter", FontWeight::REGULAR, 12)],
            vec![(FontRole::Ui, "Inter".to_owned())],
        );
        let provider = CountingProvider::new(true);
        let mut resolver = FontResolver::new();

        let got = resolver.resolve(&FontRequest::role(FontRole::Ui), &manifest, &provider, "");

        assert_eq!(got, Resolved::Face(FontFaceId(0)));
        // App face satisfied the request; the OS must not have been queried.
        assert_eq!(provider.calls.get(), 0);
    }

    #[test]
    fn system_second_when_no_app_family() {
        let manifest = FontManifest::default();
        let provider = CountingProvider::new(true);
        let mut resolver = FontResolver::new();

        let got = resolver.resolve(&FontRequest::role(FontRole::Ui), &manifest, &provider, "");

        assert!(matches!(got, Resolved::Face(_)));
        assert_eq!(provider.calls.get(), 1);
    }

    #[test]
    fn missing_when_neither_app_nor_system() {
        let manifest = FontManifest::default();
        let provider = CountingProvider::new(false);
        let mut resolver = FontResolver::new();

        let got = resolver.resolve(
            &FontRequest::family("NoSuchFamily"),
            &manifest,
            &provider,
            "",
        );

        assert_eq!(got, Resolved::Missing);
    }

    #[test]
    fn negative_outcome_is_cached_not_requeried() {
        let manifest = FontManifest::default();
        let provider = CountingProvider::new(false);
        let mut resolver = FontResolver::new();
        let request = FontRequest::family("NoSuchFamily");

        assert_eq!(
            resolver.resolve(&request, &manifest, &provider, ""),
            Resolved::Missing
        );
        assert_eq!(
            resolver.resolve(&request, &manifest, &provider, ""),
            Resolved::Missing
        );

        // The unsatisfiable family was queried against the OS exactly once.
        assert_eq!(provider.calls.get(), 1);
    }

    #[test]
    fn same_face_interns_to_one_stable_id() {
        let manifest = FontManifest::from_declared(
            vec![app_entry("Inter", FontWeight::REGULAR, 12)],
            vec![(FontRole::Ui, "Inter".to_owned())],
        );
        let provider = CountingProvider::new(false);
        let mut resolver = FontResolver::new();
        let request = FontRequest::role(FontRole::Ui);

        let first = resolver.resolve(&request, &manifest, &provider, "");
        let second = resolver.resolve(&request, &manifest, &provider, "");
        assert_eq!(first, second);

        // Registering the same app face's bytes yields that same id and fills
        // the lazily-loaded slot.
        let id = resolver.register_app_face(AssetRef(12), 0, vec![1, 2, 3, 4]);
        assert_eq!(Resolved::Face(id), first);
        assert_eq!(resolver.face_bytes(id).map(|(b, _)| b.len()), Some(4));
    }

    #[test]
    fn bumping_system_revision_reresolves_after_font_set_change() {
        let manifest = FontManifest::default();
        let provider = CountingProvider::new(true);
        let mut resolver = FontResolver::new();
        let request = FontRequest::role(FontRole::Ui);

        // First resolution queries the OS and caches the outcome.
        resolver.resolve(&request, &manifest, &provider, "");
        resolver.resolve(&request, &manifest, &provider, "");
        assert_eq!(provider.calls.get(), 1);

        // A system font-set change bumps the revision, changing the cache key so
        // the next request re-queries the platform instead of serving a stale
        // system resolution.
        resolver.bump_system_revision();
        resolver.resolve(&request, &manifest, &provider, "");
        assert_eq!(provider.calls.get(), 2);
    }

    #[test]
    fn lang_hint_is_part_of_the_resolve_key() {
        let manifest = FontManifest::default();
        let provider = CountingProvider::new(true);
        let mut resolver = FontResolver::new();
        let request = FontRequest::role(FontRole::Cjk);

        // Different language hints for the same request are distinct cache keys,
        // so each is resolved against the platform once (CJK disambiguation).
        resolver.resolve(&request, &manifest, &provider, "ja");
        resolver.resolve(&request, &manifest, &provider, "zh-Hans");
        resolver.resolve(&request, &manifest, &provider, "ja");
        assert_eq!(provider.calls.get(), 2);
    }
}
