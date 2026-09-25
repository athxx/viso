//! Linux / BSD system fonts through fontconfig.
//!
//! `libfontconfig` is loaded at runtime rather than linked, so a binary starts
//! on a system without it and simply resolves no system faces there. The
//! library and its configuration load once, on the first query, off whichever
//! thread asks first; every later query only builds a pattern and matches it.
//!
//! A query becomes one pattern: the role's generic family alias
//! (`sans-serif` / `serif` / `monospace` / `emoji`), weight, slant and width,
//! the CJK region as a language, and the sample's scalars as a required
//! charset. fontconfig ranks charset coverage above family, so the match is the
//! configured face for the role when it covers the sample and the best
//! covering face otherwise. The match is kept only if it really covers the
//! sample's first scalar, and its file is returned as-is with its collection
//! index.

use std::ffi::{CStr, CString, OsStr, c_char, c_int, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::OnceLock;

use viso_text::{FontRole, FontSlant, SystemFontProvider, SystemFontQuery, SystemFontResult};

use super::sample::{self, CjkRegion};
use super::{LiveFontRegistry, dylib};

type FcBool = c_int;
type FcConfig = c_void;
type FcPattern = c_void;
type FcCharSet = c_void;
type FcLangSet = c_void;

const FC_RESULT_MATCH: c_int = 0;
const FC_MATCH_PATTERN: c_int = 0;
const FC_MONO: c_int = 100;

/// The low 16 bits of a face's `index` are the collection index; the high
/// bits select a named instance of a variable face.
const COLLECTION_INDEX_MASK: c_int = 0xFFFF;

/// The fontconfig entry points a query uses, resolved once from the library.
struct Fontconfig {
    config: *mut FcConfig,
    pattern_create: unsafe extern "C" fn() -> *mut FcPattern,
    pattern_destroy: unsafe extern "C" fn(*mut FcPattern),
    pattern_add_string: unsafe extern "C" fn(*mut FcPattern, *const c_char, *const u8) -> FcBool,
    pattern_add_integer: unsafe extern "C" fn(*mut FcPattern, *const c_char, c_int) -> FcBool,
    pattern_add_char_set:
        unsafe extern "C" fn(*mut FcPattern, *const c_char, *const FcCharSet) -> FcBool,
    pattern_add_lang_set:
        unsafe extern "C" fn(*mut FcPattern, *const c_char, *const FcLangSet) -> FcBool,
    pattern_get_string:
        unsafe extern "C" fn(*const FcPattern, *const c_char, c_int, *mut *mut u8) -> c_int,
    pattern_get_integer:
        unsafe extern "C" fn(*const FcPattern, *const c_char, c_int, *mut c_int) -> c_int,
    pattern_get_char_set:
        unsafe extern "C" fn(*const FcPattern, *const c_char, c_int, *mut *mut FcCharSet) -> c_int,
    char_set_create: unsafe extern "C" fn() -> *mut FcCharSet,
    char_set_destroy: unsafe extern "C" fn(*mut FcCharSet),
    char_set_add_char: unsafe extern "C" fn(*mut FcCharSet, u32) -> FcBool,
    char_set_has_char: unsafe extern "C" fn(*const FcCharSet, u32) -> FcBool,
    lang_set_create: unsafe extern "C" fn() -> *mut FcLangSet,
    lang_set_destroy: unsafe extern "C" fn(*mut FcLangSet),
    lang_set_add: unsafe extern "C" fn(*mut FcLangSet, *const u8) -> FcBool,
    config_substitute: unsafe extern "C" fn(*mut FcConfig, *mut FcPattern, c_int) -> FcBool,
    default_substitute: unsafe extern "C" fn(*mut FcPattern),
    font_match: unsafe extern "C" fn(*mut FcConfig, *mut FcPattern, *mut c_int) -> *mut FcPattern,
}

// SAFETY: `config` is a fontconfig configuration that is never destroyed or
// modified after load; fontconfig serializes its own internal state, so
// matching against one configuration from several threads is supported. The
// remaining fields are plain function pointers.
unsafe impl Send for Fontconfig {}
// SAFETY: as above — shared use only ever matches against the loaded config.
unsafe impl Sync for Fontconfig {}

impl Fontconfig {
    /// Load the library and its configuration, or `None` when either is
    /// unavailable.
    fn load() -> Option<Self> {
        // SAFETY: each symbol is read as the prototype fontconfig's public
        // header declares for it; `FcInitLoadConfigAndFonts` has no
        // preconditions.
        unsafe {
            let library = dylib::open(&[c"libfontconfig.so.1", c"libfontconfig.so"])?;
            let init: unsafe extern "C" fn() -> *mut FcConfig =
                dylib::symbol(library, c"FcInitLoadConfigAndFonts")?;
            let config = init();
            if config.is_null() {
                return None;
            }
            Some(Self {
                config,
                pattern_create: dylib::symbol(library, c"FcPatternCreate")?,
                pattern_destroy: dylib::symbol(library, c"FcPatternDestroy")?,
                pattern_add_string: dylib::symbol(library, c"FcPatternAddString")?,
                pattern_add_integer: dylib::symbol(library, c"FcPatternAddInteger")?,
                pattern_add_char_set: dylib::symbol(library, c"FcPatternAddCharSet")?,
                pattern_add_lang_set: dylib::symbol(library, c"FcPatternAddLangSet")?,
                pattern_get_string: dylib::symbol(library, c"FcPatternGetString")?,
                pattern_get_integer: dylib::symbol(library, c"FcPatternGetInteger")?,
                pattern_get_char_set: dylib::symbol(library, c"FcPatternGetCharSet")?,
                char_set_create: dylib::symbol(library, c"FcCharSetCreate")?,
                char_set_destroy: dylib::symbol(library, c"FcCharSetDestroy")?,
                char_set_add_char: dylib::symbol(library, c"FcCharSetAddChar")?,
                char_set_has_char: dylib::symbol(library, c"FcCharSetHasChar")?,
                lang_set_create: dylib::symbol(library, c"FcLangSetCreate")?,
                lang_set_destroy: dylib::symbol(library, c"FcLangSetDestroy")?,
                lang_set_add: dylib::symbol(library, c"FcLangSetAdd")?,
                config_substitute: dylib::symbol(library, c"FcConfigSubstitute")?,
                default_substitute: dylib::symbol(library, c"FcDefaultSubstitute")?,
                font_match: dylib::symbol(library, c"FcFontMatch")?,
            })
        }
    }

    /// Match `query` to a face file: `(path, collection index, PostScript name)`.
    fn match_face(&self, query: &SystemFontQuery) -> Option<(CString, u32, Option<String>)> {
        let role = sample::effective_role(query);
        let scalars = sample::distinct_scalars(&query.sample);
        // SAFETY: every object created here is owned by this call and
        // destroyed before it returns; `FcPatternAdd*` copy their arguments, so
        // destroying the charset and langset after adding them is sound. The
        // strings read from `matched` are copied out before it is destroyed.
        unsafe {
            let pattern = (self.pattern_create)();
            if pattern.is_null() {
                return None;
            }
            (self.pattern_add_string)(pattern, c"family".as_ptr(), family(role).as_ptr().cast());
            if role == FontRole::Mono {
                (self.pattern_add_integer)(pattern, c"spacing".as_ptr(), FC_MONO);
            }
            (self.pattern_add_integer)(pattern, c"weight".as_ptr(), weight(query.weight.0));
            (self.pattern_add_integer)(pattern, c"slant".as_ptr(), slant(query.slant));
            (self.pattern_add_integer)(pattern, c"width".as_ptr(), width(query.width.0));
            if role == FontRole::Cjk {
                let region = sample::cjk_region(&query.lang, &query.sample);
                let langs = (self.lang_set_create)();
                if !langs.is_null() {
                    (self.lang_set_add)(langs, lang(region).as_ptr().cast());
                    (self.pattern_add_lang_set)(pattern, c"lang".as_ptr(), langs);
                    (self.lang_set_destroy)(langs);
                }
            }
            if !scalars.is_empty() {
                let required = (self.char_set_create)();
                if !required.is_null() {
                    for &ch in &scalars {
                        (self.char_set_add_char)(required, ch as u32);
                    }
                    (self.pattern_add_char_set)(pattern, c"charset".as_ptr(), required);
                    (self.char_set_destroy)(required);
                }
            }
            (self.config_substitute)(self.config, pattern, FC_MATCH_PATTERN);
            (self.default_substitute)(pattern);
            let mut result = 0;
            let matched = (self.font_match)(self.config, pattern, &mut result);
            (self.pattern_destroy)(pattern);
            if matched.is_null() {
                return None;
            }
            let face = self.read_match(matched, scalars.first().copied());
            (self.pattern_destroy)(matched);
            face
        }
    }

    /// The file, index and PostScript name of a matched pattern, if it covers
    /// `first`.
    ///
    /// # Safety
    /// `matched` must be a live pattern returned by `FcFontMatch`.
    unsafe fn read_match(
        &self,
        matched: *mut FcPattern,
        first: Option<char>,
    ) -> Option<(CString, u32, Option<String>)> {
        // SAFETY: the caller keeps `matched` alive for this call; the out
        // pointers fontconfig writes point into it, and are only read (and
        // copied) before returning.
        unsafe {
            if let Some(ch) = first {
                let mut coverage = std::ptr::null_mut();
                let found =
                    (self.pattern_get_char_set)(matched, c"charset".as_ptr(), 0, &mut coverage);
                if found != FC_RESULT_MATCH || (self.char_set_has_char)(coverage, ch as u32) == 0 {
                    return None;
                }
            }
            let mut file = std::ptr::null_mut();
            if (self.pattern_get_string)(matched, c"file".as_ptr(), 0, &mut file) != FC_RESULT_MATCH
                || file.is_null()
            {
                return None;
            }
            let path = CStr::from_ptr(file.cast()).to_owned();
            let mut index = 0;
            if (self.pattern_get_integer)(matched, c"index".as_ptr(), 0, &mut index)
                != FC_RESULT_MATCH
            {
                index = 0;
            }
            let mut name = std::ptr::null_mut();
            let postscript_name =
                ((self.pattern_get_string)(matched, c"postscriptname".as_ptr(), 0, &mut name)
                    == FC_RESULT_MATCH
                    && !name.is_null())
                .then(|| CStr::from_ptr(name.cast()).to_string_lossy().into_owned());
            Some((
                path,
                (index & COLLECTION_INDEX_MASK) as u32,
                postscript_name,
            ))
        }
    }
}

/// The generic family alias fontconfig's configuration maps to the user's
/// chosen face for `role`.
fn family(role: FontRole) -> &'static CStr {
    match role {
        FontRole::Ui | FontRole::Cjk => c"sans-serif",
        FontRole::Serif => c"serif",
        FontRole::Mono => c"monospace",
        FontRole::Emoji => c"emoji",
    }
}

fn lang(region: CjkRegion) -> &'static CStr {
    match region {
        CjkRegion::Japanese => c"ja",
        CjkRegion::Korean => c"ko",
        CjkRegion::TraditionalChinese => c"zh-tw",
        CjkRegion::SimplifiedChinese => c"zh-cn",
    }
}

/// CSS weight (1–1000) to fontconfig's weight scale, interpolating between
/// the named stops fontconfig defines.
fn weight(css: u16) -> c_int {
    const STOPS: [(u16, c_int); 10] = [
        (100, 0),
        (200, 40),
        (300, 50),
        (350, 55),
        (400, 80),
        (500, 100),
        (600, 180),
        (700, 200),
        (800, 205),
        (900, 210),
    ];
    let css = css.clamp(STOPS[0].0, STOPS[STOPS.len() - 1].0);
    let upper = STOPS
        .iter()
        .position(|&(stop, _)| stop >= css)
        .unwrap_or(STOPS.len() - 1);
    if upper == 0 {
        return STOPS[0].1;
    }
    let (low_css, low_fc) = STOPS[upper - 1];
    let (high_css, high_fc) = STOPS[upper];
    let span = c_int::from(high_css - low_css);
    low_fc + (high_fc - low_fc) * c_int::from(css - low_css) / span
}

fn slant(slant: FontSlant) -> c_int {
    match slant {
        FontSlant::Normal => 0,
        FontSlant::Italic => 100,
        FontSlant::Oblique => 110,
    }
}

/// `usWidthClass` (1–9) to fontconfig's percentage width.
fn width(class: u8) -> c_int {
    const WIDTHS: [c_int; 9] = [50, 63, 75, 87, 100, 113, 125, 150, 200];
    WIDTHS[usize::from(class.clamp(1, 9) - 1)]
}

/// System faces from fontconfig.
pub struct FontconfigProvider {
    _live: LiveFontRegistry,
}

impl FontconfigProvider {
    pub fn new(live: LiveFontRegistry) -> Self {
        Self { _live: live }
    }
}

fn fontconfig() -> Option<&'static Fontconfig> {
    static FONTCONFIG: OnceLock<Option<Fontconfig>> = OnceLock::new();
    FONTCONFIG.get_or_init(Fontconfig::load).as_ref()
}

impl SystemFontProvider for FontconfigProvider {
    fn resolve_system_face(&self, query: &SystemFontQuery) -> Option<SystemFontResult> {
        let (path, index, postscript_name) = fontconfig()?.match_face(query)?;
        let bytes = std::fs::read(Path::new(OsStr::from_bytes(path.as_bytes()))).ok()?;
        Some(SystemFontResult {
            bytes,
            index,
            postscript_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weights_hit_fontconfig_stops_and_interpolate() {
        assert_eq!(weight(400), 80);
        assert_eq!(weight(700), 200);
        assert_eq!(weight(1), 0);
        assert_eq!(weight(1000), 210);
        assert_eq!(weight(450), 90);
    }

    #[test]
    fn widths_cover_every_class() {
        assert_eq!(width(5), 100);
        assert_eq!(width(1), 50);
        assert_eq!(width(9), 200);
        assert_eq!(width(0), 50);
    }
}
