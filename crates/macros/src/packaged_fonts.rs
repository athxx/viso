//! `packaged_fonts!`: scan `assets/fonts/` at build time and emit the face table.
//!
//! Every `.ttf` / `.otf` / `.ttc` / `.otc` file below the directory (recursively,
//! in path order) is parsed face by face. A file that does not parse is a
//! compile error naming it, as is a compressed web font the runtime cannot
//! read. For each face the macro extracts what the manifest keeps — family,
//! weight, width, slant, color and monospace flags, and a coarse script
//! summary — and embeds the file once with `include_bytes!`, so Cargo rebuilds
//! when a packaged file changes.
//!
//! When the build sets `VISO_FONT_CHARSET` or `VISO_FONT_TEXT` (an embedded
//! target, see [`font_subset`](crate::font_subset)), each file is embedded as
//! its subset instead, and a character of the static text no packaged face
//! maps is a compile error listing it. The metadata still comes from the
//! original face; the script summary keeps the scripts the subset still maps.
//! A face the subsetter keeps whole (CFF2, color glyph tables) is embedded as
//! it is, with a warning at the macro call.
//!
//! When the build sets `VISO_FONT_BAKE` (see [`font_bake`](crate::font_bake)),
//! each face's drawable glyphs are also rasterized at those sizes and embedded
//! as an aligned static of baked A8 pages beside the font. With
//! `VISO_FONT_RASTERIZER=off` every face must bake, the outlines leave the
//! embedded font, and the expansion asserts at compile time that the facade
//! links no rasterizer; a facade without one refuses an expansion that is not
//! fully baked.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use proc_macro2::{Literal, Span, TokenStream};
use quote::{format_ident, quote};
use syn::LitStr;
use ttf_parser::{Face, name_id};
use unicode_script::{Script, UnicodeScript};

use crate::font_bake::{BAKE_VAR, Bake, RASTERIZER_VAR};
use crate::font_subset::{self, CHARSET_VAR, Request, TEXT_VAR};

const DEFAULT_DIR: &str = "assets/fonts";
const SFNT_EXTENSIONS: [&str; 4] = ["ttf", "otf", "ttc", "otc"];
const WEB_FONT_EXTENSIONS: [&str; 2] = ["woff", "woff2"];

/// A script enters a face's summary once its `cmap` maps this many scalars of
/// it, so a stray symbol does not claim a whole script.
const MIN_SCRIPT_SCALARS: u32 = 8;

pub fn expand(dir: Option<LitStr>) -> TokenStream {
    let span = dir.as_ref().map_or_else(Span::call_site, LitStr::span);
    let env_error = |error| syn::Error::new(span, format!("packaged_fonts!: {error}"));
    let request = manifest_dir(span)
        .and_then(|manifest_dir| Request::from_env(&manifest_dir).map_err(env_error));
    let bake = Bake::from_env().map_err(env_error);
    match request.and_then(|request| expand_dir(dir, request.as_ref(), &bake?)) {
        Ok(tokens) => tokens,
        Err(error) => error.to_compile_error(),
    }
}

fn manifest_dir(span: Span) -> syn::Result<PathBuf> {
    std::env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .ok_or_else(|| syn::Error::new(span, "CARGO_MANIFEST_DIR is not set"))
}

fn expand_dir(
    dir: Option<LitStr>,
    request: Option<&Request>,
    bake: &Bake,
) -> syn::Result<TokenStream> {
    let span = dir.as_ref().map_or_else(Span::call_site, LitStr::span);
    let relative = dir.map_or_else(|| DEFAULT_DIR.to_owned(), |dir| dir.value());
    let root = manifest_dir(span)?.join(&relative);
    if !root.is_dir() {
        return Err(syn::Error::new(
            span,
            format!("packaged_fonts!: no font directory at `{}`", root.display()),
        ));
    }
    let mut files = Vec::new();
    collect(&root, &mut files).map_err(|error| {
        syn::Error::new(
            span,
            format!("packaged_fonts!: cannot read `{}`: {error}", root.display()),
        )
    })?;
    files.sort();

    // The variables are read through `option_env!` too, so rustc records them
    // and the invoking crate rebuilds when they change.
    let mut statics = vec![quote! {
        const _: ::core::option::Option<&str> = ::core::option_env!(#CHARSET_VAR);
        const _: ::core::option::Option<&str> = ::core::option_env!(#TEXT_VAR);
        const _: ::core::option::Option<&str> = ::core::option_env!(#BAKE_VAR);
        const _: ::core::option::Option<&str> = ::core::option_env!(#RASTERIZER_VAR);
    }];
    // Faces the build does not bake, with why; with no rasterizer each is an
    // error, so the list only ever reaches the rasterizer assertion empty.
    let mut unbaked: Vec<String> = Vec::new();
    if let Some((path, _)) = request.and_then(|request| request.text.as_ref()) {
        let path = path.to_str().ok_or_else(|| {
            syn::Error::new(
                span,
                format!("packaged_fonts!: the {TEXT_VAR} path is not UTF-8"),
            )
        })?;
        statics.push(quote! { const _: &[u8] = include_bytes!(#path); });
    }
    let mut missing: BTreeSet<char> = request
        .and_then(|request| request.text.as_ref())
        .map(|(_, needed)| needed.clone())
        .unwrap_or_default();
    let mut faces = Vec::new();
    for (file_index, path) in files.iter().enumerate() {
        let shown = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .display()
            .to_string();
        let fail = |reason: String| syn::Error::new(span, format!("`{shown}`: {reason}"));
        if has_extension(path, &WEB_FONT_EXTENSIONS) {
            return Err(fail(
                "compressed web fonts are not read; package the decompressed .ttf / .otf".into(),
            ));
        }
        let data = std::fs::read(path).map_err(|error| fail(error.to_string()))?;
        let count = ttf_parser::fonts_in_collection(&data).unwrap_or(1);
        if count == 0 {
            return Err(fail("the collection holds no faces".into()));
        }
        let originals = (0..count)
            .map(|face_index| {
                Face::parse(&data, face_index)
                    .map_err(|error| fail(format!("face {face_index} does not parse: {error}")))
            })
            .collect::<syn::Result<Vec<_>>>()?;
        let bytes = format_ident!("FILE_{file_index}");
        let absolute = path
            .to_str()
            .ok_or_else(|| fail("the path is not UTF-8".into()))?;
        let subset = match request {
            Some(request) => Some(
                font_subset::subset_file(&data, &request.kept)
                    .map_err(|error| fail(format!("cannot subset: {error}")))?,
            ),
            None => None,
        };
        let source: &[u8] = subset.as_ref().map_or(&data, |subset| &subset.bytes);
        let mut baked = Vec::with_capacity(count as usize);
        for (face_index, original) in (0..count).zip(&originals) {
            if !bake.is_on() {
                baked.push(None);
                continue;
            }
            let glyphs = match &subset {
                Some(subset) => subset.drawn[face_index as usize].clone().ok_or_else(|| {
                    subset
                        .whole
                        .iter()
                        .find(|(index, _)| *index == face_index)
                        .map_or_else(String::new, |(_, reason)| reason.clone())
                }),
                None => match font_subset::kept_whole(&data, face_index).map_err(&fail)? {
                    Some(reason) => Err(reason),
                    None => Ok((0..original.number_of_glyphs()).collect()),
                },
            };
            let blob = glyphs.and_then(|glyphs| {
                viso_glyph_a8::bake_face(source, face_index, &glyphs, &bake.sizes)
                    .map_err(|error| error.to_string())
            });
            match blob {
                Ok(blob) => baked.push(Some(blob)),
                Err(reason) if !bake.rasterizer => {
                    return Err(fail(format!(
                        "face {face_index} cannot be baked ({reason}), and this build links no \
                         rasterizer ({RASTERIZER_VAR}=off): package a face whose glyphs bake, \
                         or keep the target's rasterizer"
                    )));
                }
                Err(reason) => {
                    let note = format!(
                        "packaged_fonts!: `{shown}` face {face_index} is not baked ({reason}); \
                         its glyphs are rasterized at run time"
                    );
                    statics.push(quote! {
                        const _: () = {
                            #[deprecated(note = #note)]
                            struct NotBaked;
                            let _ = NotBaked;
                        };
                    });
                    unbaked.push(format!("`{shown}` face {face_index} ({reason})"));
                    baked.push(None);
                }
            }
        }
        let stripped = if bake.rasterizer {
            None
        } else {
            Some(
                font_subset::strip_outlines(source)
                    .map_err(|error| fail(format!("cannot drop the outlines: {error}")))?,
            )
        };
        let rewritten = stripped
            .as_deref()
            .or(subset.as_ref().map(|subset| &subset.bytes[..]));
        match rewritten {
            Some(rewritten) => {
                let embedded = Literal::byte_string(rewritten);
                statics.push(quote! {
                    const _: &[u8] = include_bytes!(#absolute);
                    static #bytes: &[u8] = #embedded;
                });
            }
            None => statics.push(quote! {
                static #bytes: &[u8] = include_bytes!(#absolute);
            }),
        }
        match &subset {
            Some(subset) => {
                for (face_index, reason) in &subset.whole {
                    let note = format!(
                        "packaged_fonts!: `{shown}` face {face_index} is embedded whole: {reason}"
                    );
                    statics.push(quote! {
                        const _: () = {
                            #[deprecated(note = #note)]
                            struct EmbeddedWhole;
                            let _ = EmbeddedWhole;
                        };
                    });
                }
            }
            None => {}
        }
        let file_index = file_index as u32;
        for ((face_index, face), baked) in (0..count).zip(&originals).zip(&baked) {
            missing.retain(|&ch| !font_subset::maps(face, ch));
            let family = family(face).unwrap_or_else(|| file_stem(path));
            let weight = face.weight().to_number();
            let width = face.width().to_number() as u8;
            let slant = if face.is_italic() {
                quote!(Italic)
            } else if face.is_oblique() {
                quote!(Oblique)
            } else {
                quote!(Normal)
            };
            let tables = face.tables();
            let color = tables.colr.is_some()
                || tables.cbdt.is_some()
                || tables.sbix.is_some()
                || tables.svg.is_some();
            let mono = face.is_monospaced();
            let scripts = match &subset {
                Some(subset) => {
                    let kept = Face::parse(&subset.bytes, face_index).map_err(|error| {
                        fail(format!(
                            "the subset of face {face_index} does not parse: {error}"
                        ))
                    })?;
                    subset_scripts(face, &kept)
                }
                None => scripts(face),
            };
            let baked = match baked {
                Some(blob) => {
                    let name = format_ident!("BAKED_{file_index}_{face_index}");
                    let len = blob.len();
                    let blob = Literal::byte_string(blob);
                    statics.push(quote! {
                        static #name: ::viso::__private::viso_ende::Aligned<[u8; #len]> =
                            ::viso::__private::viso_ende::Aligned(*#blob);
                    });
                    quote!(&#name.0)
                }
                None => quote!(&[]),
            };
            faces.push(quote! {
                ::viso::fonts::PackagedFace {
                    file: #shown,
                    file_index: #file_index,
                    bytes: #bytes,
                    face_index: #face_index,
                    family: #family,
                    weight: #weight,
                    width: #width,
                    slant: ::viso::fonts::FontSlant::#slant,
                    color: #color,
                    mono: #mono,
                    scripts: &[#(#scripts),*],
                    baked: #baked,
                }
            });
        }
    }
    if let Some((text, _)) = request.and_then(|request| request.text.as_ref())
        && !missing.is_empty()
    {
        return Err(syn::Error::new(span, missing_message(text, &missing)));
    }
    if let Some(why) = incomplete(bake, faces.is_empty(), &unbaked) {
        let message = format!(
            "packaged_fonts!: this build links no glyph rasterizer (viso's `rasterizer` \
             feature is off), but {why}"
        );
        statics.push(quote! {
            const _: () = ::core::assert!(::viso::fonts::__RASTERIZER, #message);
        });
    }
    if !bake.rasterizer {
        let message = format!(
            "packaged_fonts!: {RASTERIZER_VAR}=off dropped the packaged fonts' outlines, but \
             viso's `rasterizer` feature is on; build without it"
        );
        statics.push(quote! {
            const _: () = ::core::assert!(!::viso::fonts::__RASTERIZER, #message);
        });
    }
    let count = faces.len();
    Ok(quote! {
        {
            #(#statics)*
            static FACES: [::viso::fonts::PackagedFace; #count] = [#(#faces),*];
            ::viso::fonts::PackagedFonts::__new(&FACES)
        }
    })
}

/// Why the expansion does not draw every glyph from baked pages, which a
/// facade without a rasterizer needs; `None` when it does.
fn incomplete(bake: &Bake, no_faces: bool, unbaked: &[String]) -> Option<String> {
    if no_faces {
        return None;
    }
    if !bake.is_on() {
        return Some(format!(
            "no glyph is baked: declare the sizes the target draws at in its `fonts.bake` \
             ({BAKE_VAR})"
        ));
    }
    (!unbaked.is_empty()).then(|| format!("{} not baked", unbaked.join(", ")))
}

/// How many missing characters the diagnostic lists before summarizing.
const MISSING_SHOWN: usize = 64;

fn missing_message(text: &Path, missing: &BTreeSet<char>) -> String {
    let mut listed: Vec<String> = missing
        .iter()
        .take(MISSING_SHOWN)
        .map(|&ch| format!("U+{:04X} '{ch}'", ch as u32))
        .collect();
    if missing.len() > MISSING_SHOWN {
        listed.push(format!("and {} more", missing.len() - MISSING_SHOWN));
    }
    format!(
        "packaged_fonts!: no packaged font maps {} character(s) of the static text in `{}` \
         ({TEXT_VAR}): {}; package a font that covers them",
        missing.len(),
        text.display(),
        listed.join(", ")
    )
}

fn collect(dir: &Path, files: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, files)?;
        } else if has_extension(&path, &SFNT_EXTENSIONS)
            || has_extension(&path, &WEB_FONT_EXTENSIONS)
        {
            files.push(path);
        }
    }
    Ok(())
}

fn has_extension(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extensions
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The typographic family name, else the legacy family name.
fn family(face: &Face<'_>) -> Option<String> {
    let named = |id: u16| {
        face.names()
            .into_iter()
            .filter(|name| name.name_id == id)
            .find_map(|name| name.to_string())
            .filter(|name| !name.is_empty())
    };
    named(name_id::TYPOGRAPHIC_FAMILY).or_else(|| named(name_id::FAMILY))
}

/// ISO 15924 codes of the scripts the face maps at least
/// [`MIN_SCRIPT_SCALARS`] scalars of, in code order.
fn scripts(face: &Face<'_>) -> Vec<&'static str> {
    script_counts(face)
        .into_iter()
        .filter(|&(_, count)| count >= MIN_SCRIPT_SCALARS)
        .map(|(code, _)| code)
        .collect()
}

/// The scripts of `original`'s summary that its subset `kept` still maps a
/// scalar of: a subset holding a handful of hanzi still draws Han.
fn subset_scripts(original: &Face<'_>, kept: &Face<'_>) -> Vec<&'static str> {
    let counts = script_counts(kept);
    scripts(original)
        .into_iter()
        .filter(|code| counts.contains_key(code))
        .collect()
}

/// How many scalars of each script (but Common, Inherited and Unknown) the
/// face's Unicode `cmap` maps.
fn script_counts(face: &Face<'_>) -> BTreeMap<&'static str, u32> {
    let mut counts: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    let Some(cmap) = face.tables().cmap else {
        return counts;
    };
    for subtable in cmap.subtables {
        if !subtable.is_unicode() {
            continue;
        }
        subtable.codepoints(|codepoint| {
            let Some(ch) = char::from_u32(codepoint) else {
                return;
            };
            if !seen.insert(ch) || subtable.glyph_index(codepoint).is_none() {
                return;
            }
            let script = ch.script();
            if !matches!(script, Script::Common | Script::Inherited | Script::Unknown) {
                *counts.entry(script.short_name()).or_default() += 1;
            }
        });
    }
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch font directory, removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str, files: &[(&str, &[u8])]) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("viso_packaged_fonts_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            for (file, bytes) in files {
                std::fs::write(dir.join(file), bytes).unwrap();
            }
            Self(dir)
        }

        fn expand(&self) -> syn::Result<TokenStream> {
            self.expand_with(None)
        }

        fn expand_with(&self, request: Option<&Request>) -> syn::Result<TokenStream> {
            self.expand_baked(request, &Bake::new(None, None).unwrap())
        }

        fn expand_baked(&self, request: Option<&Request>, bake: &Bake) -> syn::Result<TokenStream> {
            expand_dir(
                Some(LitStr::new(self.0.to_str().unwrap(), Span::call_site())),
                request,
                bake,
            )
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const DEJAVU: &[u8] = include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf");
    const COLR: &[u8] = include_bytes!("../../text/tests/fixtures/ColrFixture.ttf");

    /// A scratch text file, removed on drop.
    struct TextFile(PathBuf);

    impl TextFile {
        fn new(name: &str, text: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("viso_font_text_{name}_{}.txt", std::process::id()));
            std::fs::write(&path, text).unwrap();
            Self(path)
        }
    }

    impl Drop for TextFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn a_file_that_does_not_parse_is_an_error_naming_it() {
        let scratch = Scratch::new("bad", &[("Broken.ttf", b"not a font")]);
        let error = scratch.expand().unwrap_err().to_string();
        assert!(error.contains("Broken.ttf"), "{error}");
    }

    #[test]
    fn a_web_font_is_an_error_naming_it() {
        let scratch = Scratch::new("woff", &[("Inter.woff2", b"wOF2")]);
        let error = scratch.expand().unwrap_err().to_string();
        assert!(
            error.contains("Inter.woff2") && error.contains("decompressed"),
            "{error}"
        );
    }

    #[test]
    fn a_missing_directory_is_an_error() {
        let error = expand_dir(
            Some(LitStr::new("/nonexistent/viso/fonts", Span::call_site())),
            None,
            &Bake::new(None, None).unwrap(),
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("no font directory"), "{error}");
    }

    #[test]
    fn other_files_are_ignored_and_fonts_embedded_in_path_order() {
        let scratch = Scratch::new(
            "order",
            &[
                ("b.ttf", DEJAVU),
                ("LICENSE.txt", b"OFL"),
                ("a.TTF", DEJAVU),
            ],
        );
        let tokens = scratch.expand().unwrap().to_string();
        let first = tokens.find("\"a.TTF\"").unwrap();
        let second = tokens.find("\"b.ttf\"").unwrap();
        assert!(first < second);
        assert!(!tokens.contains("LICENSE"));
        assert!(tokens.contains("\"DejaVu Sans\""));
    }

    #[test]
    fn without_a_request_files_are_included_whole_and_the_variables_tracked() {
        let scratch = Scratch::new("plain", &[("a.ttf", DEJAVU)]);
        let tokens = scratch.expand().unwrap().to_string();
        assert!(tokens.contains("\"VISO_FONT_CHARSET\""), "{tokens}");
        assert!(tokens.contains("\"VISO_FONT_TEXT\""), "{tokens}");
        assert!(
            tokens.contains("static FILE_0 : & [u8] = include_bytes !"),
            "{tokens}"
        );
    }

    #[test]
    fn a_request_embeds_the_subset_and_tracks_the_original_file() {
        let scratch = Scratch::new("subset", &[("a.ttf", DEJAVU)]);
        let request = Request::new(Some("U+41-42"), None).unwrap();
        let tokens = scratch.expand_with(request.as_ref()).unwrap().to_string();
        assert!(tokens.contains("static FILE_0 : & [u8] = b\""), "{tokens}");
        assert!(
            tokens.contains("const _ : & [u8] = include_bytes !"),
            "{tokens}"
        );
        assert!(tokens.contains("\"DejaVu Sans\""));
        // Two Latin letters are below the summary threshold, but the original
        // face covers Latin and the subset still maps some of it.
        assert!(tokens.contains("\"Latn\""), "{tokens}");
    }

    #[test]
    fn a_text_character_no_face_maps_is_an_error_listing_it() {
        let scratch = Scratch::new("missing", &[("a.ttf", DEJAVU)]);
        let text = TextFile::new("missing", "Hi 中文\n");
        let request = Request::new(Some("gb2312"), Some(text.0.clone())).unwrap();
        let error = scratch
            .expand_with(request.as_ref())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("2 character(s)")
                && error.contains("U+4E2D '中'")
                && error.contains("U+6587 '文'"),
            "{error}"
        );
        assert!(error.contains(&text.0.display().to_string()), "{error}");
        assert!(error.contains("VISO_FONT_TEXT"), "{error}");
    }

    #[test]
    fn covered_text_tracks_its_file_and_a_charset_beyond_every_face_is_skipped() {
        let scratch = Scratch::new("covered", &[("a.ttf", DEJAVU)]);
        let text = TextFile::new("covered", "OK\n");
        let request = Request::new(Some("ascii,gb2312"), Some(text.0.clone())).unwrap();
        let tokens = scratch.expand_with(request.as_ref()).unwrap().to_string();
        let tracked = format!("include_bytes ! ({:?})", text.0.display().to_string());
        assert!(tokens.contains(&tracked), "{tokens}");
    }

    fn bake(sizes: &str, rasterizer: Option<&str>) -> Bake {
        Bake::new(Some(sizes), rasterizer).unwrap()
    }

    #[test]
    fn without_a_bake_the_fonts_need_a_rasterizer() {
        let scratch = Scratch::new("unbaked", &[("a.ttf", DEJAVU)]);
        let tokens = scratch.expand().unwrap().to_string();
        assert!(tokens.contains("baked : & []"), "{tokens}");
        assert!(
            tokens.contains(":: core :: assert ! (:: viso :: fonts :: __RASTERIZER")
                && tokens.contains("no glyph is baked"),
            "{tokens}"
        );
        assert!(tokens.contains("\"VISO_FONT_BAKE\""), "tracked");
    }

    #[test]
    fn a_bake_embeds_aligned_pages_beside_each_face() {
        let scratch = Scratch::new("baked", &[("a.ttf", DEJAVU)]);
        let request = Request::new(Some("ascii"), None).unwrap();
        let tokens = scratch
            .expand_baked(request.as_ref(), &bake("16,24", None))
            .unwrap()
            .to_string();
        assert!(
            tokens.contains("static BAKED_0_0 : :: viso :: __private :: viso_ende :: Aligned"),
            "{tokens}"
        );
        assert!(tokens.contains("baked : & BAKED_0_0 . 0"), "{tokens}");
        assert!(
            !tokens.contains("__RASTERIZER"),
            "fully baked, outlines kept"
        );
    }

    #[test]
    fn no_rasterizer_drops_the_outlines_and_asserts_none_links() {
        let scratch = Scratch::new("bakedonly", &[("a.ttf", DEJAVU)]);
        let tokens = scratch
            .expand_baked(None, &bake("16", Some("off")))
            .unwrap()
            .to_string();
        assert!(tokens.contains("baked : & BAKED_0_0 . 0"), "{tokens}");
        assert!(
            tokens.contains("assert ! (! :: viso :: fonts :: __RASTERIZER"),
            "{tokens}"
        );
        assert!(
            !tokens.contains("assert ! (:: viso :: fonts :: __RASTERIZER"),
            "every face baked"
        );
        // The embedded font is the stripped one, not the file.
        assert!(tokens.contains("static FILE_0 : & [u8] = b\""), "{tokens}");
    }

    #[test]
    fn a_face_that_cannot_bake_is_an_error_without_a_rasterizer_and_a_warning_with_one() {
        let scratch = Scratch::new("colrbake", &[("Colr.ttf", COLR)]);
        let request = Request::new(Some("ascii"), None).unwrap();
        let error = scratch
            .expand_baked(request.as_ref(), &bake("16", Some("off")))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("`Colr.ttf`") && error.contains("cannot be baked"),
            "{error}"
        );
        let tokens = scratch
            .expand_baked(request.as_ref(), &bake("16", None))
            .unwrap()
            .to_string();
        assert!(tokens.contains("face 0 is not baked"), "{tokens}");
        assert!(
            tokens.contains("assert ! (:: viso :: fonts :: __RASTERIZER")
                && tokens
                    .contains("`Colr.ttf` face 0 (COLR color glyphs are not subset) not baked"),
            "{tokens}"
        );
    }

    #[test]
    fn a_face_kept_whole_warns_at_the_call() {
        let scratch = Scratch::new("whole", &[("Colr.ttf", COLR)]);
        let request = Request::new(Some("ascii"), None).unwrap();
        let tokens = scratch.expand_with(request.as_ref()).unwrap().to_string();
        assert!(
            tokens.contains("deprecated") && tokens.contains("`Colr.ttf` face 0 is embedded whole"),
            "{tokens}"
        );
    }
}
