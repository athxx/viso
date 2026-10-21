//! Fonts packaged into the binary from the application's `assets/fonts/`.
//!
//! The `packaged_fonts!` macro scans that directory at build time, validates
//! every container, and emits one static [`PackagedFace`] per face: the
//! metadata the manifest keeps, next to the file's bytes embedded with
//! `include_bytes!`. This module turns that table into a [`FontManifest`] and
//! serves the bytes through [`AssetSource`]; building the manifest parses
//! nothing, and a face's bytes are only parsed when it is first used.
//!
//! Packaged faces take part in resolution twice. A role the application ships
//! a family for binds to it in the manifest, ahead of the system face for that
//! role. And [`PackagedFirst`] puts the packaged faces ahead of the platform in
//! fallback, so a run the primary face cannot draw is tried against the
//! application's own faces before the system is asked.

use std::sync::Arc;

use icu_properties::CodePointSetData;
use icu_properties::props::EmojiPresentation;
use unicode_script::Script;

use crate::app_fonts::AssetSource;
use crate::coverage::face_covers;
use crate::fallback::FontFallback;
use crate::font_manifest::{AssetRef, FontManifest, ManifestEntry, ScriptCoverageSummary};
use crate::font_request::{FontRole, FontSlant, FontWeight, FontWidth};
use crate::raster_a8::BakedFace;
use crate::system_fonts::{SystemFontProvider, SystemFontQuery, SystemFontResult};

/// One face found in `assets/fonts/` at build time.
#[derive(Debug)]
pub struct PackagedFace {
    /// The file's path below `assets/fonts/`, for diagnostics.
    pub file: &'static str,
    /// Which packaged file holds the face; faces of one collection share it.
    pub file_index: u32,
    /// The whole file.
    pub bytes: &'static [u8],
    /// The face's index within a collection file (0 for a single face).
    pub face_index: u32,
    /// Typographic family name, else the legacy family name.
    pub family: &'static str,
    /// `OS/2` weight class.
    pub weight: u16,
    /// `OS/2` width class (1–9).
    pub width: u8,
    pub slant: FontSlant,
    /// Whether the face carries a color glyph table (`COLR`, `CBDT`, `sbix` or
    /// `SVG `).
    pub color: bool,
    /// Whether the face is monospaced.
    pub mono: bool,
    /// ISO 15924 codes of the scripts the face's `cmap` substantially covers.
    pub scripts: &'static [&'static str],
    /// The face's glyphs rasterized at build time for the sizes the build
    /// declared ([`BakedFace`] pages, §3.6); empty when it baked none.
    pub baked: &'static [u8],
}

/// The application's packaged faces, as emitted by `packaged_fonts!`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PackagedFonts {
    faces: &'static [PackagedFace],
}

impl PackagedFonts {
    /// Packaged assets are numbered from here, clear of the assets an
    /// application registers at runtime.
    pub const ASSET_BASE: u32 = 1 << 31;

    /// The table `packaged_fonts!` emits; not for use by hand.
    #[doc(hidden)]
    pub const fn __new(faces: &'static [PackagedFace]) -> Self {
        Self { faces }
    }

    pub fn faces(&self) -> &'static [PackagedFace] {
        self.faces
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }

    /// The asset a face's file is served under.
    pub fn asset(face: &PackagedFace) -> AssetRef {
        AssetRef(Self::ASSET_BASE | face.file_index)
    }

    /// The manifest the packaged faces declare, with each role bound to the
    /// family [`role_bindings`](Self::role_bindings) picks.
    pub fn manifest(&self) -> FontManifest {
        let entries = self
            .faces
            .iter()
            .map(|face| ManifestEntry {
                family: face.family.to_owned(),
                weight: FontWeight(face.weight),
                width: FontWidth(face.width),
                slant: face.slant,
                face_index: face.face_index,
                color: face.color,
                coverage: ScriptCoverageSummary {
                    scripts: face
                        .scripts
                        .iter()
                        .filter_map(|code| Script::from_short_name(code))
                        .collect(),
                },
                asset: Self::asset(face),
            })
            .collect();
        FontManifest::from_discovered(entries, self.role_bindings())
    }

    /// The family each role binds to, taking the first packaged face (in file
    /// order) that suits it:
    ///
    /// - [`FontRole::Ui`]: a plain text face, one covering Latin if any does;
    /// - [`FontRole::Mono`]: a monospaced face;
    /// - [`FontRole::Emoji`]: a color face;
    /// - [`FontRole::Cjk`]: a plain face covering Han, other than the UI family.
    ///
    /// A role nothing suits stays unbound and resolves through the system.
    /// Serif is never inferred.
    pub fn role_bindings(&self) -> Vec<(FontRole, String)> {
        let text = || self.faces.iter().filter(|face| !face.color && !face.mono);
        let covers = |face: &PackagedFace, code: &str| face.scripts.contains(&code);
        let ui = text()
            .find(|face| covers(face, "Latn"))
            .or_else(|| text().next())
            .map(|face| face.family);
        let mono = self.faces.iter().find(|face| face.mono && !face.color);
        let emoji = self.faces.iter().find(|face| face.color);
        let cjk = text().find(|face| covers(face, "Hani") && Some(face.family) != ui);
        [
            (FontRole::Ui, ui),
            (FontRole::Mono, mono.map(|face| face.family)),
            (FontRole::Emoji, emoji.map(|face| face.family)),
            (FontRole::Cjk, cjk.map(|face| face.family)),
        ]
        .into_iter()
        .filter_map(|(role, family)| Some((role, family?.to_owned())))
        .collect()
    }

    /// The baked glyphs of the packaged face whose embedded bytes are
    /// `bytes` and index is `index`: a face the runtime loaded, from either
    /// resolution or fallback, recognized by its bytes. A file a development
    /// session replaced reads other bytes, so its stale pages are never used.
    /// Cold: once per face the runtime registers.
    pub fn baked_face(&self, bytes: &[u8], index: u32) -> Option<BakedFace<'static>> {
        self.faces
            .iter()
            .filter(|face| !face.baked.is_empty() && face.face_index == index)
            .find(|face| face.bytes.len() == bytes.len() && face.bytes == bytes)
            .and_then(|face| BakedFace::parse(face.baked))
    }

    /// The packaged file at `path`, a path under the package's asset
    /// directory: the file whose path below the fonts directory ends it, the
    /// longest such.
    pub fn file_for(&self, path: &str) -> Option<u32> {
        let ends = |file: &str| {
            path == file
                || path
                    .strip_suffix(file)
                    .is_some_and(|dir| dir.ends_with('/'))
        };
        self.faces
            .iter()
            .filter(|face| ends(face.file))
            .max_by_key(|face| face.file.len())
            .map(|face| face.file_index)
    }

    /// The packaged face that best draws `query`'s sample: one whose script
    /// summary admits the sample's script and whose `cmap` maps its first
    /// scalar, preferring a color face for emoji and a plain one otherwise,
    /// then the closest style.
    pub fn face_for(&self, query: &SystemFontQuery) -> Option<&'static PackagedFace> {
        self.face_in(query, &ReplacedFiles::default())
    }

    /// [`face_for`](Self::face_for), reading each face from `replaced` where
    /// its file was replaced.
    fn face_in(
        &self,
        query: &SystemFontQuery,
        replaced: &ReplacedFiles,
    ) -> Option<&'static PackagedFace> {
        let first = query.sample.chars().next()?;
        let script = FontFallback::run_script(&query.sample);
        let wants_color = CodePointSetData::new::<EmojiPresentation>().contains(first);
        let mut utf8 = [0; 4];
        let first = &*first.encode_utf8(&mut utf8);
        self.faces
            .iter()
            .filter(|face| {
                script == Script::Common
                    || face.scripts.is_empty()
                    || face.scripts.contains(&script.short_name())
            })
            .filter(|face| face_covers(replaced.bytes_of(face), face.face_index, first))
            .min_by_key(|face| {
                let slant = u32::from(face.slant != query.slant) * 1_000_000;
                let width = (i32::from(face.width) - i32::from(query.width.0)).unsigned_abs();
                let weight = (i32::from(face.weight) - i32::from(query.weight.0)).unsigned_abs();
                (face.color != wants_color, slant + width * 10_000 + weight)
            })
    }
}

impl AssetSource for PackagedFonts {
    fn read(&self, asset: AssetRef) -> Option<Arc<[u8]>> {
        PackagedSource {
            packaged: *self,
            replaced: &ReplacedFiles::default(),
        }
        .read(asset)
    }
}

/// Packaged files whose bytes changed after the build, which a development
/// session replaced in the running app: served instead of the embedded ones.
#[derive(Debug, Clone, Default)]
pub struct ReplacedFiles {
    files: Vec<(u32, Arc<[u8]>)>,
}

impl ReplacedFiles {
    /// Serve `bytes` for the packaged file `file_index` from now on.
    pub fn replace(&mut self, file_index: u32, bytes: Arc<[u8]>) {
        match self.files.iter_mut().find(|(file, _)| *file == file_index) {
            Some((_, held)) => *held = bytes,
            None => self.files.push((file_index, bytes)),
        }
    }

    /// Each replaced file and the bytes it reads now.
    pub fn files(&self) -> impl Iterator<Item = (u32, &Arc<[u8]>)> {
        self.files.iter().map(|(file, bytes)| (*file, bytes))
    }

    /// The bytes `face` reads now: its file's replacement, else the embedded
    /// file.
    pub fn bytes_of<'a>(&'a self, face: &'a PackagedFace) -> &'a [u8] {
        self.files
            .iter()
            .find(|(file, _)| *file == face.file_index)
            .map_or(face.bytes, |(_, bytes)| bytes)
    }
}

/// The packaged faces as they read now, `replaced` served over the embedded
/// files.
pub struct PackagedSource<'a> {
    pub packaged: PackagedFonts,
    pub replaced: &'a ReplacedFiles,
}

impl AssetSource for PackagedSource<'_> {
    fn read(&self, asset: AssetRef) -> Option<Arc<[u8]>> {
        let file_index = asset.0.checked_sub(PackagedFonts::ASSET_BASE)?;
        let face = self
            .packaged
            .faces
            .iter()
            .find(|face| face.file_index == file_index)?;
        // A replacement is shared as it arrived; an embedded file is copied
        // once, into the one allocation every reader of it shares.
        Some(
            self.replaced
                .files
                .iter()
                .find(|(file, _)| *file == file_index)
                .map_or_else(|| Arc::from(face.bytes), |(_, bytes)| Arc::clone(bytes)),
        )
    }
}

/// Fallback that tries the packaged faces before the platform.
pub struct PackagedFirst<'a> {
    pub packaged: PackagedFonts,
    /// Packaged files replaced since the build.
    pub replaced: &'a ReplacedFiles,
    pub system: &'a dyn SystemFontProvider,
}

impl SystemFontProvider for PackagedFirst<'_> {
    fn resolve_system_face(&self, query: &SystemFontQuery) -> Option<SystemFontResult> {
        match self.packaged.face_in(query, self.replaced) {
            Some(face) => Some(SystemFontResult {
                bytes: self.replaced.bytes_of(face).to_vec(),
                index: face.face_index,
                postscript_name: None,
                variations: None,
            }),
            None => self.system.resolve_system_face(query),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEJAVU: &[u8] = include_bytes!("../tests/fixtures/DejaVuSans-subset.ttf");
    const COLR: &[u8] = include_bytes!("../tests/fixtures/ColrFixture.ttf");

    const fn face(
        file_index: u32,
        bytes: &'static [u8],
        family: &'static str,
        weight: u16,
        color: bool,
        scripts: &'static [&'static str],
    ) -> PackagedFace {
        PackagedFace {
            file: "fixture.ttf",
            file_index,
            bytes,
            face_index: 0,
            family,
            weight,
            width: 5,
            slant: FontSlant::Normal,
            color,
            mono: false,
            scripts,
            baked: &[],
        }
    }

    static FACES: [PackagedFace; 4] = [
        face(0, COLR, "Colr", 400, true, &["Latn"]),
        face(1, DEJAVU, "Han Only", 400, false, &["Hani"]),
        face(2, DEJAVU, "Sans", 400, false, &["Latn"]),
        face(3, DEJAVU, "Sans", 700, false, &["Latn"]),
    ];

    fn packaged() -> PackagedFonts {
        PackagedFonts::__new(&FACES)
    }

    fn query(sample: &str, weight: u16) -> SystemFontQuery {
        SystemFontQuery {
            family: None,
            role: FontRole::Ui,
            weight: FontWeight(weight),
            width: FontWidth::NORMAL,
            slant: FontSlant::Normal,
            lang: String::new(),
            sample: sample.to_owned(),
        }
    }

    #[test]
    fn roles_bind_to_the_first_face_that_suits_them() {
        let manifest = packaged().manifest();
        assert_eq!(manifest.family_for_role(FontRole::Ui), Some("Sans"));
        assert_eq!(manifest.family_for_role(FontRole::Emoji), Some("Colr"));
        assert_eq!(manifest.family_for_role(FontRole::Cjk), Some("Han Only"));
        assert_eq!(manifest.family_for_role(FontRole::Mono), None);
        assert_eq!(manifest.family_for_role(FontRole::Serif), None);
    }

    #[test]
    fn the_manifest_keeps_each_face_and_its_script_summary() {
        let manifest = packaged().manifest();
        assert_eq!(manifest.entries().len(), 4);
        let bold = manifest
            .select(
                "Sans",
                FontWeight::BOLD,
                FontWidth::NORMAL,
                FontSlant::Normal,
            )
            .unwrap();
        assert_eq!(bold.asset, AssetRef(PackagedFonts::ASSET_BASE | 3));
        assert_eq!(bold.coverage.scripts, [Script::Latin]);
    }

    #[test]
    fn assets_serve_their_file_and_nothing_else() {
        let fonts = packaged();
        let asset = PackagedFonts::asset(&FACES[2]);
        assert_eq!(fonts.read(asset).as_deref(), Some(DEJAVU));
        assert!(fonts.read(AssetRef(0)).is_none(), "a runtime asset");
        assert!(
            fonts
                .read(AssetRef(PackagedFonts::ASSET_BASE | 9))
                .is_none()
        );
    }

    #[test]
    fn fallback_prefers_a_plain_covering_face_of_the_nearest_weight() {
        let fonts = packaged();
        let chosen = fonts.face_for(&query("Ab", 650)).unwrap();
        assert_eq!((chosen.family, chosen.weight), ("Sans", 700));
        // The Han-only summary rules its face out for Latin even though its
        // bytes cover it: a regular Latin query lands on the regular Sans.
        let regular = fonts.face_for(&query("A", 400)).unwrap();
        assert_eq!((regular.family, regular.weight), ("Sans", 400));
        assert!(fonts.face_for(&query("\u{4F60}", 400)).is_none());
    }

    #[test]
    fn packaged_faces_come_before_the_system() {
        struct Marker;
        impl SystemFontProvider for Marker {
            fn resolve_system_face(&self, _: &SystemFontQuery) -> Option<SystemFontResult> {
                Some(SystemFontResult {
                    bytes: vec![7],
                    index: 0,
                    postscript_name: None,
                    variations: None,
                })
            }
        }
        let provider = PackagedFirst {
            packaged: packaged(),
            replaced: &ReplacedFiles::default(),
            system: &Marker,
        };
        let packaged = provider.resolve_system_face(&query("A", 400)).unwrap();
        assert_eq!(packaged.bytes, DEJAVU);
        let system = provider
            .resolve_system_face(&query("\u{4F60}", 400))
            .unwrap();
        assert_eq!(system.bytes, [7]);
    }

    #[test]
    fn a_loaded_face_finds_its_baked_glyphs_by_its_bytes() {
        use viso_glyph_a8::{BakedStrike, CoverageBitmap};
        static BAKED: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        let baked = BAKED.get_or_init(|| {
            BakedStrike::encode(&[BakedStrike {
                ppem: 16,
                glyphs: vec![(
                    34,
                    CoverageBitmap {
                        width: 1,
                        height: 1,
                        left: 0.0,
                        top: 1.0,
                        coverage: vec![200],
                    },
                )],
            }])
            .unwrap()
        });
        let faces: &'static [PackagedFace] = Box::leak(Box::new([
            face(0, COLR, "Colr", 400, true, &["Latn"]),
            PackagedFace {
                baked,
                ..face(1, DEJAVU, "Sans", 400, false, &["Latn"])
            },
        ]));
        let fonts = PackagedFonts::__new(faces);
        let copy = DEJAVU.to_vec();
        let found = fonts.baked_face(&copy, 0).expect("the same bytes");
        assert_eq!(found.coverage(34, 16).unwrap().coverage, [200]);
        assert!(fonts.baked_face(&copy, 1).is_none(), "another face index");
        assert!(
            fonts.baked_face(COLR, 0).is_none(),
            "a face that baked none"
        );
        let mut edited = copy;
        edited[100] ^= 1;
        assert!(fonts.baked_face(&edited, 0).is_none(), "replaced bytes");
    }

    #[test]
    fn a_replaced_file_is_served_in_place_of_the_embedded_one() {
        static NAMED: [PackagedFace; 2] = [
            PackagedFace {
                file: "Sans.ttf",
                ..face(0, DEJAVU, "Sans", 400, false, &["Latn"])
            },
            PackagedFace {
                file: "display/Sans.ttf",
                ..face(1, DEJAVU, "Display", 400, false, &["Latn"])
            },
        ];
        let fonts = PackagedFonts::__new(&NAMED);
        assert_eq!(fonts.file_for("fonts/Sans.ttf"), Some(0));
        assert_eq!(fonts.file_for("fonts/display/Sans.ttf"), Some(1));
        assert_eq!(fonts.file_for("fonts/OtherSans.ttf"), None, "a whole name");

        let mut replaced = ReplacedFiles::default();
        replaced.replace(1, Arc::from(COLR));
        let source = PackagedSource {
            packaged: fonts,
            replaced: &replaced,
        };
        assert_eq!(
            source.read(PackagedFonts::asset(&NAMED[1])).as_deref(),
            Some(COLR)
        );
        assert_eq!(
            source.read(PackagedFonts::asset(&NAMED[0])).as_deref(),
            Some(DEJAVU),
            "another file keeps its bytes"
        );
    }
}
