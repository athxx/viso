//! What the packages an app links carry for its platform beside their code,
//! merged into what the app is packaged with, so linking a package is all an
//! app does to have what the package needs declared:
//!
//! - `android/manifest.xml`: a `<manifest>` whose elements (permissions,
//!   features, queries) join the app's manifest, and whose `<application>`
//!   holds components (services, receivers, providers, activities,
//!   meta-data) that join the app's `<application>`;
//! - `ios/Info.plist`, `macos/Info.plist`: a dictionary merged into the
//!   app's Info.plist;
//! - `ios/entitlements.plist`, `macos/entitlements.plist`: a dictionary of
//!   entitlements the app is signed with;
//! - `web/worker.js`: a script the app's service worker runs.
//!
//! The same declaration from two packages is made once; two different ones
//! of the same thing are a [`Problem`] naming both.

use std::ops::Range;
use std::path::PathBuf;

use crate::plist::{self, Dict};
use crate::xml::{self, Element};

/// A file a package carries for its platform.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Fragment {
    /// The package's name.
    pub package: String,
    pub path: PathBuf,
    pub text: String,
}

/// What is wrong with a fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// It does not read as what its file is.
    Invalid,
    /// It declares differently what another declares.
    Conflict,
}

/// Why a fragment does not merge: what, in which file and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub kind: Kind,
    pub path: PathBuf,
    /// The bytes of the fragment it is about.
    pub at: Range<usize>,
    pub message: String,
    pub note: Option<String>,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)?;
        if let Some(note) = &self.note {
            write!(f, " ({note})")?;
        }
        Ok(())
    }
}

impl Fragment {
    fn invalid(&self, error: xml::Error) -> Problem {
        Problem {
            kind: Kind::Invalid,
            path: self.path.clone(),
            at: error.at..error.at,
            message: error.message,
            note: None,
        }
    }

    /// Its file as a message names it: `` `name`'s `ios/Info.plist` ``.
    fn named(&self) -> String {
        let file = self
            .path
            .iter()
            .rev()
            .take(2)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<PathBuf>();
        format!("`{}`'s `{}`", self.package, file.display())
    }
}

/// Merges the plist `fragments` into `into`, whose own entries are what
/// `base` names (as "what Viso writes from `Viso.toml`").
pub(crate) fn merge_plist(
    into: &mut Dict,
    base: &str,
    fragments: &[Fragment],
) -> Result<(), Problem> {
    for (index, fragment) in fragments.iter().enumerate() {
        let dict = plist::read(&fragment.text, index + 1).map_err(|e| fragment.invalid(e))?;
        plist::merge(into, dict).map_err(|conflict| {
            let other = match conflict.other {
                0 => base.to_owned(),
                n => fragments[n - 1].named(),
            };
            Problem {
                kind: Kind::Conflict,
                path: fragment.path.clone(),
                at: conflict.at,
                message: format!(
                    "`{}` is {} in {} but {} in {other}",
                    conflict.key,
                    conflict.theirs,
                    fragment.named(),
                    conflict.ours
                ),
                note: Some(
                    "arrays and dictionaries merge, other values must agree: give the key in one \
                     place"
                        .to_owned(),
                ),
            }
        })?;
    }
    Ok(())
}

/// Who declares an element of an Android manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    /// The shell's template.
    Shell,
    /// The app's `[package.permissions]`.
    Permissions,
    /// The fragment at this index.
    Fragment(usize),
}

/// A manifest element and who declares it.
struct Declared {
    element: Element,
    source: Source,
}

/// The manifest `filled` (the shell's template filled in for the app, with
/// `marker` where its permissions go) with the `permissions` the app asks
/// for and the `fragments` merged in, `{package}` in a fragment read as
/// `package`.
pub(crate) fn merge_manifest(
    filled: &str,
    marker: &str,
    permissions: &[&str],
    fragments: &[Fragment],
    package: &str,
) -> Result<String, Problem> {
    let shell_error = |error: xml::Error| Problem {
        kind: Kind::Invalid,
        path: PathBuf::from("AndroidManifest.xml"),
        at: error.at..error.at,
        message: format!(
            "the shell's manifest template does not read: {}",
            error.message
        ),
        note: None,
    };
    let root = xml::parse(filled).map_err(shell_error)?;
    let application = root
        .elements()
        .find(|e| e.name == "application")
        .ok_or_else(|| {
            shell_error(xml::Error {
                at: root.span.start,
                message: "it has no `<application>`".to_owned(),
            })
        })?;

    let mut top: Vec<Declared> = root
        .elements()
        .filter(|e| e.name != "application")
        .map(|e| Declared {
            element: e.clone(),
            source: Source::Shell,
        })
        .collect();
    let shell_top = top.len();
    let mut components: Vec<Declared> = application
        .elements()
        .map(|e| Declared {
            element: e.clone(),
            source: Source::Shell,
        })
        .collect();
    let shell_components = components.len();

    for line in permissions {
        let element = xml::parse(line).expect("a permission line is an element");
        add(&mut top, element, Source::Permissions, fragments)?;
    }
    for (index, fragment) in fragments.iter().enumerate() {
        let mut root = xml::parse(&fragment.text).map_err(|e| fragment.invalid(e))?;
        root.replace("{package}", package);
        let invalid = |at: &Range<usize>, message: String, note: Option<&str>| Problem {
            kind: Kind::Invalid,
            path: fragment.path.clone(),
            at: at.clone(),
            message,
            note: note.map(str::to_owned),
        };
        if root.name != "manifest" {
            return Err(invalid(
                &root.span,
                format!(
                    "a manifest fragment is a `<manifest>`, not a `<{}>`",
                    root.name
                ),
                None,
            ));
        }
        if let Some((name, _)) = root.attrs.iter().find(|(n, _)| !n.starts_with("xmlns")) {
            return Err(invalid(
                &root.span,
                format!("the fragment's `<manifest>` sets `{name}`"),
                Some(
                    "the app's id and version come from `Viso.toml`; a fragment's `<manifest>` names only its namespaces",
                ),
            ));
        }
        if let Some(text) = root.stray_text() {
            return Err(invalid(
                &root.span,
                format!("text in the `<manifest>`: `{text}`"),
                None,
            ));
        }
        for element in root.elements() {
            match element.name.as_str() {
                "application" => {
                    if let Some((name, _)) = element.attrs.first() {
                        return Err(invalid(
                            &element.span,
                            format!("the fragment's `<application>` sets `{name}`"),
                            Some(
                                "a fragment's `<application>` holds components; its attributes are the app's",
                            ),
                        ));
                    }
                    for component in element.elements() {
                        add(
                            &mut components,
                            component.clone(),
                            Source::Fragment(index),
                            fragments,
                        )?;
                    }
                }
                "uses-sdk" => {
                    return Err(invalid(
                        &element.span,
                        "a fragment does not set the API levels".to_owned(),
                        Some("the lowest API level is `[targets.android] min_sdk` in `Viso.toml`"),
                    ));
                }
                _ => add(
                    &mut top,
                    element.clone(),
                    Source::Fragment(index),
                    fragments,
                )?,
            }
        }
    }

    let mut written = String::new();
    for (n, declared) in top.iter().enumerate().skip(shell_top) {
        if n > shell_top {
            written.push('\n');
        }
        declared.element.write(4, &mut written);
    }
    let permissions = written.trim_start();
    let mut added = String::new();
    for declared in &components[shell_components..] {
        added.push('\n');
        declared.element.write(8, &mut added);
    }
    // The whitespace before `</application>` stays before it.
    let insert = filled[..application.content_end].trim_end().len();
    let mut out = String::with_capacity(filled.len() + permissions.len() + added.len());
    out.push_str(&filled[..insert]);
    out.push_str(&added);
    out.push_str(&filled[insert..]);
    Ok(out.replacen(marker, permissions, 1))
}

/// Adds `element` from `source` to `list`, once: a declaration of the same
/// thing (its tag and `android:name`) as one already there must be the
/// same, but for a permission, which is asked for at the widest any asks.
fn add(
    list: &mut Vec<Declared>,
    element: Element,
    source: Source,
    fragments: &[Fragment],
) -> Result<(), Problem> {
    let name = element.attr("android:name").map(str::to_owned);
    let Some(name) = name else {
        if !list.iter().any(|d| d.element.same(&element)) {
            list.push(Declared { element, source });
        }
        return Ok(());
    };
    let Some(have) = list
        .iter_mut()
        .find(|d| d.element.name == element.name && d.element.attr("android:name") == Some(&name))
    else {
        list.push(Declared { element, source });
        return Ok(());
    };
    if have.element.same(&element) {
        return Ok(());
    }
    if element.name == "uses-permission"
        && let Some(widest) = widest(&have.element, &element)
    {
        have.element = widest;
        return Ok(());
    }
    let named = |source: Source| match source {
        Source::Shell => "the Android shell".to_owned(),
        Source::Permissions => "`[package.permissions]`".to_owned(),
        Source::Fragment(n) => fragments[n].named(),
    };
    let what = match element.name.as_str() {
        "uses-permission" => "the permission",
        "uses-feature" => "the feature",
        _ => "",
    };
    let what = if what.is_empty() {
        format!("the `<{}>`", element.name)
    } else {
        what.to_owned()
    };
    Err(Problem {
        kind: Kind::Conflict,
        path: match source {
            Source::Fragment(n) => fragments[n].path.clone(),
            _ => PathBuf::from("AndroidManifest.xml"),
        },
        at: element.span.clone(),
        message: format!(
            "{} declares {what} `{name}` other than {} does",
            named(source),
            named(have.source)
        ),
        note: Some("a thing is declared once, by the package that needs it".to_owned()),
    })
}

/// The one of two requests of a permission that asks the most, when they
/// differ only in the highest API level they ask on.
fn widest(a: &Element, b: &Element) -> Option<Element> {
    const MAX: &str = "android:maxSdkVersion";
    fn rest(e: &Element) -> Vec<&(String, String)> {
        let mut attrs: Vec<&(String, String)> = e.attrs.iter().filter(|(n, _)| n != MAX).collect();
        attrs.sort();
        attrs
    }
    if rest(a) != rest(b) || a.elements().next().is_some() || b.elements().next().is_some() {
        return None;
    }
    let max = |e: &Element| e.attr(MAX).map(|v| v.parse::<u32>().unwrap_or(u32::MAX));
    let mut widest = a.clone();
    widest.attrs.retain(|(n, _)| n != MAX);
    if let (Some(x), Some(y)) = (max(a), max(b)) {
        widest.attrs.push((MAX.to_owned(), x.max(y).to_string()));
    }
    Some(widest)
}

/// The service worker the `fragments` make: each in turn, after a line
/// naming its package.
pub fn worker(fragments: &[Fragment]) -> Option<String> {
    if fragments.is_empty() {
        return None;
    }
    let mut out =
        String::from("// The app's service worker: what each package it links runs here.\n");
    for fragment in fragments {
        out.push_str(&format!("\n// --- {} ---\n", fragment.package));
        out.push_str(fragment.text.trim_end());
        out.push('\n');
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARK: &str = "<!--permissions-->";

    const SHELL: &str = "<?xml version=\"1.0\"?>\n<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\" package=\"dev.viso.a\">\n    \
        <uses-sdk android:minSdkVersion=\"26\" />\n    <!--permissions-->\n\n    <application android:label=\"a\">\n        \
        <activity android:name=\"dev.viso.VisoActivity\" />\n    </application>\n</manifest>\n";

    fn fragment(package: &str, text: &str) -> Fragment {
        Fragment {
            package: package.into(),
            path: PathBuf::from(format!("/p/{package}/android/manifest.xml")),
            text: text.into(),
        }
    }

    const PUSH: &str = "<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\">\n  \
        <uses-permission android:name=\"android.permission.INTERNET\" />\n  \
        <uses-permission android:name=\"android.permission.POST_NOTIFICATIONS\"/>\n  \
        <application>\n    <receiver android:name=\"dev.viso.push.VisoPushReceiver\" android:exported=\"true\">\n      \
        <intent-filter><action android:name=\"com.google.android.c2dm.intent.RECEIVE\" /></intent-filter>\n    \
        </receiver>\n  </application>\n</manifest>\n";

    const JOBS: &str = "<manifest xmlns:android=\"http://schemas.android.com/apk/res/android\">\n  \
        <uses-permission android:name=\"android.permission.INTERNET\" />\n  \
        <uses-permission android:name=\"android.permission.ACCESS_FINE_LOCATION\" android:maxSdkVersion=\"30\" />\n  \
        <application><service android:name=\"dev.viso.background.VisoJobService\" android:exported=\"false\" />\
        <meta-data android:name=\"k\" android:value=\"{package}.v\" /></application>\n</manifest>\n";

    #[test]
    fn fragments_join_the_manifest_each_declaration_once() {
        let permissions = [
            r#"<uses-permission android:name="android.permission.POST_NOTIFICATIONS" />"#,
            r#"<uses-permission android:name="android.permission.ACCESS_FINE_LOCATION" />"#,
        ];
        let merged = merge_manifest(
            SHELL,
            MARK,
            &permissions,
            &[
                fragment("viso-push", PUSH),
                fragment("viso-background", JOBS),
            ],
            "com.example.a",
        )
        .unwrap();
        let expected = "    <uses-sdk android:minSdkVersion=\"26\" />\n    \
            <uses-permission android:name=\"android.permission.POST_NOTIFICATIONS\" />\n    \
            <uses-permission android:name=\"android.permission.ACCESS_FINE_LOCATION\" />\n    \
            <uses-permission android:name=\"android.permission.INTERNET\" />\n\n    \
            <application android:label=\"a\">\n        \
            <activity android:name=\"dev.viso.VisoActivity\" />\n        \
            <receiver android:name=\"dev.viso.push.VisoPushReceiver\" android:exported=\"true\">\n            \
            <intent-filter>\n                <action android:name=\"com.google.android.c2dm.intent.RECEIVE\" />\n            \
            </intent-filter>\n        </receiver>\n        \
            <service android:name=\"dev.viso.background.VisoJobService\" android:exported=\"false\" />\n        \
            <meta-data android:name=\"k\" android:value=\"com.example.a.v\" />\n    </application>\n";
        assert!(merged.contains(expected), "{merged}");
        assert_eq!(merged.matches("ACCESS_FINE_LOCATION").count(), 1);
        assert!(xml::parse(&merged).is_ok());
        // Nothing to add leaves the template as it was filled.
        let plain = merge_manifest(SHELL, MARK, &[], &[], "a").unwrap();
        assert_eq!(plain, SHELL.replace(MARK, ""));
    }

    #[test]
    fn a_permission_is_asked_for_at_its_widest() {
        let low = fragment(
            "a",
            "<manifest><uses-permission android:name=\"P\" android:maxSdkVersion=\"28\"/></manifest>",
        );
        let high = fragment(
            "b",
            "<manifest><uses-permission android:name=\"P\" android:maxSdkVersion=\"30\"/></manifest>",
        );
        let merged = merge_manifest(SHELL, MARK, &[], &[low.clone(), high], "a").unwrap();
        assert!(
            merged.contains("<uses-permission android:name=\"P\" android:maxSdkVersion=\"30\" />")
        );
        let flagged = fragment(
            "c",
            "<manifest><uses-permission android:name=\"P\" android:usesPermissionFlags=\"neverForLocation\"/></manifest>",
        );
        let problem = merge_manifest(SHELL, MARK, &[], &[low, flagged], "a").unwrap_err();
        assert_eq!(problem.kind, Kind::Conflict);
        assert_eq!(
            problem.message,
            "`c`'s `android/manifest.xml` declares the permission `P` other than `a`'s `android/manifest.xml` does"
        );
    }

    #[test]
    fn two_declarations_of_one_component_conflict_naming_both() {
        let other = fragment(
            "viso-other",
            "<manifest><application><service android:name=\"dev.viso.background.VisoJobService\" \
             android:exported=\"true\"/></application></manifest>",
        );
        let problem = merge_manifest(
            SHELL,
            MARK,
            &[],
            &[fragment("viso-background", JOBS), other.clone()],
            "a",
        )
        .unwrap_err();
        assert_eq!(problem.kind, Kind::Conflict);
        assert_eq!(problem.path, other.path);
        assert!(problem.message.starts_with("`viso-other`'s `android/manifest.xml` declares the `<service>` `dev.viso.background.VisoJobService` other than `viso-background`'s"), "{}", problem.message);
        assert_eq!(&other.text[problem.at.clone()][..8], "<service");
        let shell = fragment(
            "x",
            "<manifest><application><activity android:name=\"dev.viso.VisoActivity\" android:exported=\"true\"/></application></manifest>",
        );
        let problem = merge_manifest(SHELL, MARK, &[], &[shell], "a").unwrap_err();
        assert!(
            problem
                .message
                .ends_with("other than the Android shell does"),
            "{}",
            problem.message
        );
    }

    #[test]
    fn a_fragment_that_is_no_manifest_fragment_says_why() {
        let fails =
            |text: &str| merge_manifest(SHELL, MARK, &[], &[fragment("a", text)], "a").unwrap_err();
        assert_eq!(fails("<manifest><application>").kind, Kind::Invalid);
        assert!(
            fails("<application/>")
                .message
                .contains("is a `<manifest>`")
        );
        assert!(
            fails("<manifest package=\"x\"/>")
                .message
                .contains("sets `package`")
        );
        assert!(
            fails("<manifest><application android:label=\"x\"/></manifest>")
                .message
                .contains("`<application>` sets `android:label`")
        );
        assert!(
            fails("<manifest><uses-sdk android:minSdkVersion=\"30\"/></manifest>")
                .message
                .contains("API levels")
        );
    }

    #[test]
    fn plists_merge_and_a_conflict_names_both_files() {
        let mut info = Dict::of(vec![("UIRequiresFullScreen", plist::Value::Bool(false))]);
        let modes = |package: &str, mode: &str| Fragment {
            package: package.into(),
            path: PathBuf::from(format!("/p/{package}/ios/Info.plist")),
            text: format!(
                "<plist><dict><key>UIBackgroundModes</key><array><string>{mode}</string></array></dict></plist>"
            ),
        };
        merge_plist(
            &mut info,
            "Viso",
            &[
                modes("a", "fetch"),
                modes("b", "fetch"),
                modes("c", "processing"),
            ],
        )
        .unwrap();
        assert_eq!(
            info.get("UIBackgroundModes"),
            Some(&plist::Value::Array(vec![
                plist::Value::string("fetch"),
                plist::Value::string("processing")
            ]))
        );
        let screen = Fragment {
            package: "d".into(),
            path: PathBuf::from("/p/d/ios/Info.plist"),
            text: "<plist><dict><key>UIRequiresFullScreen</key><true/></dict></plist>".into(),
        };
        let problem = merge_plist(&mut info, "what Viso writes", &[screen]).unwrap_err();
        assert_eq!(problem.kind, Kind::Conflict);
        assert_eq!(
            problem.message,
            "`UIRequiresFullScreen` is true in `d`'s `ios/Info.plist` but false in what Viso writes"
        );
        let mut info = Dict::default();
        let problem = merge_plist(
            &mut info,
            "Viso",
            &[
                modes("a", "fetch"),
                Fragment {
                    text:
                        "<plist><dict><key>UIBackgroundModes</key><string>x</string></dict></plist>"
                            .into(),
                    ..modes("b", "")
                },
            ],
        )
        .unwrap_err();
        assert!(
            problem
                .message
                .ends_with("but an array in `a`'s `ios/Info.plist`"),
            "{}",
            problem.message
        );
    }

    #[test]
    fn the_worker_runs_each_package_in_turn() {
        assert_eq!(worker(&[]), None);
        let script = |package: &str| Fragment {
            package: package.into(),
            path: PathBuf::from("w.js"),
            text: format!("self.{package} = 1;\n\n"),
        };
        let worker = worker(&[script("a"), script("b")]).unwrap();
        assert!(
            worker.ends_with("\n// --- a ---\nself.a = 1;\n\n// --- b ---\nself.b = 1;\n"),
            "{worker}"
        );
    }
}
