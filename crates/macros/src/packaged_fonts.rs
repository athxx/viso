//! `packaged_fonts!`: scan `assets/fonts/` at build time and emit the face table.
//!
//! Every `.ttf` / `.otf` / `.ttc` / `.otc` file below the directory (recursively,
//! in path order) is parsed face by face. A file that does not parse is a
//! compile error naming it, as is a compressed web font the runtime cannot
//! read. For each face the macro extracts what the manifest keeps — family,
//! weight, width, slant, color and monospace flags, and a coarse script
//! summary — and embeds the file once with `include_bytes!`, so Cargo rebuilds
//! when a packaged file changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::LitStr;
use ttf_parser::{Face, name_id};
use unicode_script::{Script, UnicodeScript};

const DEFAULT_DIR: &str = "assets/fonts";
const SFNT_EXTENSIONS: [&str; 4] = ["ttf", "otf", "ttc", "otc"];
const WEB_FONT_EXTENSIONS: [&str; 2] = ["woff", "woff2"];

/// A script enters a face's summary once its `cmap` maps this many scalars of
/// it, so a stray symbol does not claim a whole script.
const MIN_SCRIPT_SCALARS: u32 = 8;

pub fn expand(dir: Option<LitStr>) -> TokenStream {
    match expand_dir(dir) {
        Ok(tokens) => tokens,
        Err(error) => error.to_compile_error(),
    }
}

fn expand_dir(dir: Option<LitStr>) -> syn::Result<TokenStream> {
    let span = dir.as_ref().map_or_else(Span::call_site, LitStr::span);
    let relative = dir.map_or_else(|| DEFAULT_DIR.to_owned(), |dir| dir.value());
    let manifest_dir = std::env::var_os("CARGO_MANIFEST_DIR")
        .ok_or_else(|| syn::Error::new(span, "CARGO_MANIFEST_DIR is not set"))?;
    let root = Path::new(&manifest_dir).join(&relative);
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

    let mut statics = Vec::new();
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
        let bytes = format_ident!("FILE_{file_index}");
        let absolute = path
            .to_str()
            .ok_or_else(|| fail("the path is not UTF-8".into()))?;
        statics.push(quote! {
            static #bytes: &[u8] = include_bytes!(#absolute);
        });
        let file_index = file_index as u32;
        for face_index in 0..count {
            let face = Face::parse(&data, face_index)
                .map_err(|error| fail(format!("face {face_index} does not parse: {error}")))?;
            let family = family(&face).unwrap_or_else(|| file_stem(path));
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
            let scripts = scripts(&face);
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
                }
            });
        }
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
    let mut counts: BTreeMap<&'static str, u32> = BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    let Some(cmap) = face.tables().cmap else {
        return Vec::new();
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
        .into_iter()
        .filter(|&(_, count)| count >= MIN_SCRIPT_SCALARS)
        .map(|(code, _)| code)
        .collect()
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
            expand_dir(Some(LitStr::new(
                self.0.to_str().unwrap(),
                Span::call_site(),
            )))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    const DEJAVU: &[u8] = include_bytes!("../../text/tests/fixtures/DejaVuSans-subset.ttf");

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
        let error = expand_dir(Some(LitStr::new(
            "/nonexistent/viso/fonts",
            Span::call_site(),
        )))
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
}
