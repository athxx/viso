//! Android system fonts.
//!
//! From API 29 the platform's own matcher, `AFontMatcher`, answers a query:
//! it applies the system's family, locale and fallback rules exactly as
//! platform text does. It is looked up in `libandroid` at runtime, so one
//! binary also runs on older releases, where the query is answered from
//! `/system/etc/fonts.xml` instead ([`super::android_config`]).
//!
//! Either way a candidate is kept only when its `cmap` covers the sample's
//! first scalar. The check maps the file rather than reading it, so a large
//! collection that turns out not to cover costs only the pages its tables
//! occupy; only the face that is returned is read in full.

use std::ffi::{CStr, CString, OsStr, c_char, c_void};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use viso_text::{
    FontRole, FontSlant, SystemFontProvider, SystemFontQuery, SystemFontResult, face_covers,
};

use super::android_config::{FontConfig, Want};
use super::sample::{self, CjkRegion};
use super::{LiveFontRegistry, dylib};

const FONTS_XML: &str = "/system/etc/fonts.xml";
const FONTS_DIR: &str = "/system/fonts";

type AFontMatcher = c_void;
type AFont = c_void;

/// The `AFontMatcher` entry points (API 29+).
struct Matcher {
    create: unsafe extern "C" fn() -> *mut AFontMatcher,
    destroy: unsafe extern "C" fn(*mut AFontMatcher),
    set_style: unsafe extern "C" fn(*mut AFontMatcher, u16, bool),
    set_locales: unsafe extern "C" fn(*mut AFontMatcher, *const c_char),
    match_text: unsafe extern "C" fn(
        *const AFontMatcher,
        *const c_char,
        *const u16,
        u32,
        *mut u32,
    ) -> *mut AFont,
    font_path: unsafe extern "C" fn(*const AFont) -> *const c_char,
    collection_index: unsafe extern "C" fn(*const AFont) -> usize,
    close: unsafe extern "C" fn(*mut AFont),
}

impl Matcher {
    fn load() -> Option<Self> {
        // SAFETY: each symbol is read as the prototype `<android/font_matcher.h>`
        // and `<android/font.h>` declare for it; a missing symbol (API < 29)
        // declines the matcher as a whole.
        unsafe {
            let library = dylib::open(&[c"libandroid.so"])?;
            Some(Self {
                create: dylib::symbol(library, c"AFontMatcher_create")?,
                destroy: dylib::symbol(library, c"AFontMatcher_destroy")?,
                set_style: dylib::symbol(library, c"AFontMatcher_setStyle")?,
                set_locales: dylib::symbol(library, c"AFontMatcher_setLocales")?,
                match_text: dylib::symbol(library, c"AFontMatcher_match")?,
                font_path: dylib::symbol(library, c"AFont_getFontFilePath")?,
                collection_index: dylib::symbol(library, c"AFont_getCollectionIndex")?,
                close: dylib::symbol(library, c"AFont_close")?,
            })
        }
    }

    /// The face the platform draws `query`'s sample with: `(path, index)`.
    fn match_face(&self, query: &SystemFontQuery) -> Option<(PathBuf, u32)> {
        let role = sample::effective_role(query);
        let family = match role {
            FontRole::Serif => c"serif",
            FontRole::Mono => c"monospace",
            FontRole::Ui | FontRole::Cjk | FontRole::Emoji => c"sans-serif",
        };
        let text: Vec<u16> = match (role, query.sample.is_empty()) {
            // An empty sample still needs a character to match on.
            (FontRole::Emoji, true) => "\u{1F600}".encode_utf16().collect(),
            (_, true) => "a".encode_utf16().collect(),
            (_, false) => sample::distinct_scalars(&query.sample)
                .into_iter()
                .collect::<String>()
                .encode_utf16()
                .collect(),
        };
        let locales = locales(query, role);
        let weight = query.weight.0.clamp(1, 1000);
        let italic = query.slant != FontSlant::Normal;
        // SAFETY: the matcher and the font it returns are owned by this call
        // and released before it returns; `text` and `locales` outlive the
        // calls that read them, and the font's path is copied out before the
        // font is closed.
        unsafe {
            let matcher = (self.create)();
            if matcher.is_null() {
                return None;
            }
            (self.set_style)(matcher, weight, italic);
            if let Some(locales) = &locales {
                (self.set_locales)(matcher, locales.as_ptr());
            }
            let mut run_length = 0;
            let font = (self.match_text)(
                matcher,
                family.as_ptr(),
                text.as_ptr(),
                text.len() as u32,
                &mut run_length,
            );
            (self.destroy)(matcher);
            if font.is_null() {
                return None;
            }
            let path = (self.font_path)(font);
            let face = (!path.is_null()).then(|| {
                let path = Path::new(OsStr::from_bytes(CStr::from_ptr(path).to_bytes()));
                (path.to_path_buf(), (self.collection_index)(font) as u32)
            });
            (self.close)(font);
            face
        }
    }
}

/// The matcher's comma-separated locale list: the query's language, with the
/// CJK region inferred from the sample when the language does not name one.
fn locales(query: &SystemFontQuery, role: FontRole) -> Option<CString> {
    let mut tags = Vec::new();
    if !query.lang.is_empty() {
        tags.push(query.lang.clone());
    }
    if role == FontRole::Cjk {
        tags.push(region_tag(sample::cjk_region(&query.lang, &query.sample)).to_owned());
    }
    (!tags.is_empty())
        .then(|| CString::new(tags.join(",")).ok())
        .flatten()
}

fn region_tag(region: CjkRegion) -> &'static str {
    match region {
        CjkRegion::Japanese => "ja",
        CjkRegion::Korean => "ko",
        CjkRegion::TraditionalChinese => "zh-Hant",
        CjkRegion::SimplifiedChinese => "zh-Hans",
    }
}

/// Whether the face at `path` has a glyph for `ch`, reading only the pages its
/// tables occupy.
fn maps_scalar(path: &Path, index: u32, ch: char) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(metadata) = file.metadata() else {
        return false;
    };
    let len = metadata.len() as usize;
    if len == 0 {
        return false;
    }
    // SAFETY: a private read-only mapping of an open file, `len` bytes long;
    // the slice built over it is only read while the mapping is live and is
    // dropped before `munmap`. System font files are not rewritten while
    // mapped (the partition is read-only).
    unsafe {
        let address = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        );
        if address == libc::MAP_FAILED {
            return false;
        }
        let bytes = std::slice::from_raw_parts(address.cast::<u8>(), len);
        let mut utf8 = [0; 4];
        let covered = face_covers(bytes, index, ch.encode_utf8(&mut utf8));
        libc::munmap(address, len);
        covered
    }
}

/// The fonts.xml resolver, for releases without `AFontMatcher`.
struct Configured {
    config: FontConfig,
}

impl Configured {
    fn load() -> Option<Self> {
        let xml = std::fs::read_to_string(FONTS_XML).ok()?;
        let config = FontConfig::parse(&xml);
        (!config.families.is_empty()).then_some(Self { config })
    }

    fn match_face(
        &self,
        query: &SystemFontQuery,
        first: char,
    ) -> Option<(PathBuf, u32, Option<String>)> {
        let role = sample::effective_role(query);
        let want = Want {
            // A serif or monospace query keeps its family ahead of the CJK
            // chain, so the chain's serif fallbacks are chosen for it.
            role: match (role, query.role) {
                (FontRole::Cjk, FontRole::Serif | FontRole::Mono) => query.role,
                _ => role,
            },
            region: (role == FontRole::Cjk).then(|| sample::cjk_region(&query.lang, &query.sample)),
            weight: query.weight.0,
            italic: query.slant != FontSlant::Normal,
            sample: &query.sample,
        };
        self.config.candidates(&want).into_iter().find_map(|font| {
            let path = Path::new(FONTS_DIR).join(&font.file);
            maps_scalar(&path, font.index, first)
                .then(|| (path, font.index, font.postscript_name.clone()))
        })
    }
}

enum Source {
    Matcher(Matcher),
    Configured(Configured),
}

fn source() -> Option<&'static Source> {
    static SOURCE: OnceLock<Option<Source>> = OnceLock::new();
    SOURCE
        .get_or_init(|| {
            Matcher::load()
                .map(Source::Matcher)
                .or_else(|| Configured::load().map(Source::Configured))
        })
        .as_ref()
}

/// System faces from the platform matcher or `fonts.xml`.
pub struct AndroidFontProvider {
    _live: LiveFontRegistry,
}

impl AndroidFontProvider {
    pub fn new(live: LiveFontRegistry) -> Self {
        Self { _live: live }
    }
}

impl SystemFontProvider for AndroidFontProvider {
    fn resolve_system_face(&self, query: &SystemFontQuery) -> Option<SystemFontResult> {
        let first = query.sample.chars().next();
        let (path, index, postscript_name) = match source()? {
            Source::Matcher(matcher) => {
                let (path, index) = matcher.match_face(query)?;
                if first.is_some_and(|ch| !maps_scalar(&path, index, ch)) {
                    return None;
                }
                (path, index, None)
            }
            Source::Configured(configured) => configured.match_face(query, first.unwrap_or('a'))?,
        };
        let bytes = std::fs::read(path).ok()?;
        Some(SystemFontResult {
            bytes,
            index,
            postscript_name,
        })
    }
}
