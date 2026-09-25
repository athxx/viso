//! Windows system fonts through DirectWrite.
//!
//! A query first tries the role's families in the system collection — the
//! faces Windows itself uses for UI, serif, monospace, emoji and each CJK
//! region — keeping the first whose best style match maps the sample's first
//! scalar. Otherwise DirectWrite's system fallback (`MapCharacters`) chooses
//! the face, with the same locale and script rules platform text applies.
//! The chosen face's file is returned as-is with its collection index.

use std::sync::OnceLock;

use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH, DWRITE_FONT_STYLE, DWRITE_FONT_STYLE_ITALIC,
    DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_STYLE_OBLIQUE, DWRITE_FONT_WEIGHT,
    DWRITE_INFORMATIONAL_STRING_POSTSCRIPT_NAME, DWRITE_READING_DIRECTION,
    DWRITE_READING_DIRECTION_LEFT_TO_RIGHT, DWriteCreateFactory, IDWriteFactory2, IDWriteFont,
    IDWriteFontCollection, IDWriteFontFallback, IDWriteFontFile, IDWriteLocalFontFileLoader,
    IDWriteLocalizedStrings, IDWriteNumberSubstitution, IDWriteTextAnalysisSource,
    IDWriteTextAnalysisSource_Impl,
};
use windows::core::{BOOL, HSTRING, Interface, OutRef, Result as WinResult, implement};

use viso_text::{FontRole, FontSlant, SystemFontProvider, SystemFontQuery, SystemFontResult};

use super::LiveFontRegistry;
use super::sample::{self, CjkRegion};

/// The shared factory, the system collection and its fallback, created once.
struct DirectWrite {
    collection: IDWriteFontCollection,
    fallback: Option<IDWriteFontFallback>,
}

impl DirectWrite {
    fn load() -> Option<Self> {
        // SAFETY: plain DirectWrite factory calls; a shared factory and the
        // objects it returns are free-threaded, and every out parameter is a
        // local owned by this call.
        unsafe {
            let factory: IDWriteFactory2 = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED).ok()?;
            let mut collection = None;
            factory
                .GetSystemFontCollection(&mut collection, false)
                .ok()?;
            Some(Self {
                collection: collection?,
                fallback: factory.GetSystemFontFallback().ok(),
            })
        }
    }

    /// The font DirectWrite draws `query`'s sample with.
    fn match_font(&self, query: &SystemFontQuery) -> Option<IDWriteFont> {
        let role = sample::effective_role(query);
        let region = sample::cjk_region(&query.lang, &query.sample);
        let families = role_families(role, region);
        let first = query.sample.chars().next();
        let weight = DWRITE_FONT_WEIGHT(i32::from(query.weight.0.clamp(1, 999)));
        let stretch = DWRITE_FONT_STRETCH(i32::from(query.width.0.clamp(1, 9)));
        let style = style(query.slant);
        for name in families {
            // SAFETY: DirectWrite calls on live interfaces; `index` and
            // `exists` are locals the call writes.
            let font = unsafe {
                let mut index = 0;
                let mut exists = BOOL(0);
                if self
                    .collection
                    .FindFamilyName(&HSTRING::from(*name), &mut index, &mut exists)
                    .is_err()
                    || !exists.as_bool()
                {
                    continue;
                }
                let Ok(family) = self.collection.GetFontFamily(index) else {
                    continue;
                };
                let Ok(font) = family.GetFirstMatchingFont(weight, stretch, style) else {
                    continue;
                };
                font
            };
            if first.is_none_or(|ch| has_character(&font, ch)) {
                return Some(font);
            }
        }
        let fallback = self.fallback.as_ref()?;
        let text: Vec<u16> = sample::distinct_scalars(&query.sample)
            .into_iter()
            .collect::<String>()
            .encode_utf16()
            .collect();
        if text.is_empty() {
            return None;
        }
        let locale = if query.lang.is_empty() {
            region_locale(role, region)
        } else {
            &query.lang
        };
        let length = text.len() as u32;
        let source: IDWriteTextAnalysisSource = SampleSource {
            text,
            locale: locale.encode_utf16().chain(Some(0)).collect(),
        }
        .into();
        let base = HSTRING::from(families.first().copied().unwrap_or("Segoe UI"));
        let mut mapped_length = 0;
        let mut mapped = None;
        let mut scale = 1.0;
        // SAFETY: `source` stays alive across the call and serves `length`
        // UTF-16 units from position 0; the out parameters are locals.
        unsafe {
            fallback
                .MapCharacters(
                    &source,
                    0,
                    length,
                    &self.collection,
                    &base,
                    weight,
                    style,
                    stretch,
                    &mut mapped_length,
                    &mut mapped,
                    &mut scale,
                )
                .ok()?;
        }
        let font = mapped?;
        (mapped_length > 0 && first.is_none_or(|ch| has_character(&font, ch))).then_some(font)
    }
}

fn has_character(font: &IDWriteFont, ch: char) -> bool {
    // SAFETY: a query on a live font.
    unsafe { font.HasCharacter(ch as u32) }.is_ok_and(|found| found.as_bool())
}

fn style(slant: FontSlant) -> DWRITE_FONT_STYLE {
    match slant {
        FontSlant::Normal => DWRITE_FONT_STYLE_NORMAL,
        FontSlant::Italic => DWRITE_FONT_STYLE_ITALIC,
        FontSlant::Oblique => DWRITE_FONT_STYLE_OBLIQUE,
    }
}

/// The families Windows ships for a role, preferred first; later entries
/// cover older releases and trimmed installs.
fn role_families(role: FontRole, region: CjkRegion) -> &'static [&'static str] {
    match role {
        FontRole::Ui => &["Segoe UI", "Tahoma", "Arial"],
        FontRole::Serif => &["Times New Roman", "Georgia"],
        FontRole::Mono => &["Consolas", "Courier New"],
        FontRole::Emoji => &["Segoe UI Emoji", "Segoe UI Symbol"],
        FontRole::Cjk => match region {
            CjkRegion::Japanese => &["Yu Gothic UI", "Meiryo UI", "MS Gothic"],
            CjkRegion::Korean => &["Malgun Gothic", "Gulim"],
            CjkRegion::TraditionalChinese => {
                &["Microsoft JhengHei UI", "Microsoft JhengHei", "PMingLiU"]
            }
            CjkRegion::SimplifiedChinese => &["Microsoft YaHei UI", "Microsoft YaHei", "SimSun"],
        },
    }
}

/// The locale system fallback resolves in when the query names none.
fn region_locale(role: FontRole, region: CjkRegion) -> &'static str {
    match (role, region) {
        (FontRole::Cjk, CjkRegion::Japanese) => "ja-JP",
        (FontRole::Cjk, CjkRegion::Korean) => "ko-KR",
        (FontRole::Cjk, CjkRegion::TraditionalChinese) => "zh-TW",
        (FontRole::Cjk, CjkRegion::SimplifiedChinese) => "zh-CN",
        _ => "en-US",
    }
}

/// The sample as a one-paragraph analysis source for system fallback.
#[implement(IDWriteTextAnalysisSource)]
struct SampleSource {
    text: Vec<u16>,
    /// NUL-terminated.
    locale: Vec<u16>,
}

impl IDWriteTextAnalysisSource_Impl for SampleSource_Impl {
    fn GetTextAtPosition(
        &self,
        position: u32,
        text: *mut *mut u16,
        length: *mut u32,
    ) -> WinResult<()> {
        let position = position as usize;
        // SAFETY: DirectWrite passes valid out pointers; the text they point
        // into is owned by this source, which outlives the analysis.
        unsafe {
            if position >= self.text.len() {
                *text = std::ptr::null_mut();
                *length = 0;
            } else {
                *text = self.text.as_ptr().add(position).cast_mut();
                *length = (self.text.len() - position) as u32;
            }
        }
        Ok(())
    }

    fn GetTextBeforePosition(
        &self,
        position: u32,
        text: *mut *mut u16,
        length: *mut u32,
    ) -> WinResult<()> {
        let position = position as usize;
        // SAFETY: as in `GetTextAtPosition`.
        unsafe {
            if position == 0 || position > self.text.len() {
                *text = std::ptr::null_mut();
                *length = 0;
            } else {
                *text = self.text.as_ptr().cast_mut();
                *length = position as u32;
            }
        }
        Ok(())
    }

    fn GetParagraphReadingDirection(&self) -> DWRITE_READING_DIRECTION {
        DWRITE_READING_DIRECTION_LEFT_TO_RIGHT
    }

    fn GetLocaleName(
        &self,
        position: u32,
        length: *mut u32,
        locale: *mut *mut u16,
    ) -> WinResult<()> {
        // SAFETY: valid out pointers from DirectWrite; the locale buffer is
        // NUL-terminated and owned by this source.
        unsafe {
            *length = (self.text.len() as u32).saturating_sub(position);
            *locale = self.locale.as_ptr().cast_mut();
        }
        Ok(())
    }

    fn GetNumberSubstitution(
        &self,
        position: u32,
        length: *mut u32,
        substitution: OutRef<IDWriteNumberSubstitution>,
    ) -> WinResult<()> {
        // SAFETY: a valid out pointer from DirectWrite.
        unsafe {
            *length = (self.text.len() as u32).saturating_sub(position);
        }
        substitution.write(None)
    }
}

/// The file bytes, collection index and PostScript name behind `font`.
fn face_of(font: &IDWriteFont) -> Option<SystemFontResult> {
    // SAFETY: DirectWrite calls on live interfaces. `files` is sized from the
    // count the first `GetFiles` reports; the reference key is owned by `file`
    // and only read while it is alive.
    unsafe {
        let face = font.CreateFontFace().ok()?;
        let mut count = 0;
        face.GetFiles(&mut count, None).ok()?;
        if count == 0 {
            return None;
        }
        let mut files: Vec<Option<IDWriteFontFile>> = vec![None; count as usize];
        face.GetFiles(&mut count, Some(files.as_mut_ptr())).ok()?;
        let file = files.into_iter().next().flatten()?;
        let bytes = file_bytes(&file)?;
        Some(SystemFontResult {
            bytes,
            index: face.GetIndex(),
            postscript_name: postscript_name(font),
        })
    }
}

/// A font file's contents: read from disk for a local file, else streamed
/// from its loader.
fn file_bytes(file: &IDWriteFontFile) -> Option<Vec<u8>> {
    // SAFETY: the key pointer and size come from `file` and stay valid while
    // it lives; the path buffer is sized from the loader's reported length plus
    // the terminator; a stream fragment is copied out before it is released.
    unsafe {
        let mut key = std::ptr::null_mut();
        let mut key_size = 0;
        file.GetReferenceKey(&mut key, &mut key_size).ok()?;
        let loader = file.GetLoader().ok()?;
        if let Ok(local) = loader.cast::<IDWriteLocalFontFileLoader>() {
            let length = local.GetFilePathLengthFromKey(key, key_size).ok()?;
            let mut path = vec![0u16; length as usize + 1];
            local.GetFilePathFromKey(key, key_size, &mut path).ok()?;
            path.truncate(length as usize);
            return std::fs::read(String::from_utf16_lossy(&path)).ok();
        }
        let stream = loader.CreateStreamFromKey(key, key_size).ok()?;
        let size = stream.GetFileSize().ok()?;
        let mut fragment = std::ptr::null_mut();
        let mut context = std::ptr::null_mut();
        stream
            .ReadFileFragment(&mut fragment, 0, size, &mut context)
            .ok()?;
        let bytes = std::slice::from_raw_parts(fragment.cast::<u8>(), size as usize).to_vec();
        stream.ReleaseFileFragment(context);
        Some(bytes)
    }
}

fn postscript_name(font: &IDWriteFont) -> Option<String> {
    // SAFETY: DirectWrite calls on live interfaces; the string buffer holds
    // the reported length plus the terminator.
    unsafe {
        let mut strings: Option<IDWriteLocalizedStrings> = None;
        let mut exists = BOOL(0);
        font.GetInformationalStrings(
            DWRITE_INFORMATIONAL_STRING_POSTSCRIPT_NAME,
            &mut strings,
            &mut exists,
        )
        .ok()?;
        let strings = strings.filter(|_| exists.as_bool())?;
        let length = strings.GetStringLength(0).ok()?;
        let mut name = vec![0u16; length as usize + 1];
        strings.GetString(0, &mut name).ok()?;
        name.truncate(length as usize);
        Some(String::from_utf16_lossy(&name))
    }
}

fn direct_write() -> Option<&'static DirectWrite> {
    static DIRECT_WRITE: OnceLock<Option<DirectWrite>> = OnceLock::new();
    DIRECT_WRITE.get_or_init(DirectWrite::load).as_ref()
}

/// System faces from DirectWrite.
pub struct DirectWriteProvider {
    _live: LiveFontRegistry,
}

impl DirectWriteProvider {
    pub fn new(live: LiveFontRegistry) -> Self {
        Self { _live: live }
    }
}

impl SystemFontProvider for DirectWriteProvider {
    fn resolve_system_face(&self, query: &SystemFontQuery) -> Option<SystemFontResult> {
        face_of(&direct_write()?.match_font(query)?)
    }
}
