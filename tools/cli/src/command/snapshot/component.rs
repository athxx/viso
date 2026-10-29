//! A component captured alone: found by name among the package's sources,
//! compiled into a view package that mounts it with its inputs' defaults,
//! for a headless run to show in place of the app's view.
//!
//! The package mounts a harness appended to the component's source, whose
//! view is the component alone with no properties given, so each input takes
//! its default as it would where the app uses the component; one that has
//! none cannot be captured alone.

use std::path::PathBuf;

use viso_dsl::Diagnostic;
use viso_dsl::frontend::{DeclaredComponent, Origin, Siblings, declared_components};
use viso_dsl::hir::TargetProfile;
use viso_dsl::package::LoadedPackage;

/// A component the package declares.
pub(in crate::command) struct Found {
    pub name: String,
    /// Its module path and name, `::`-separated: what its golden records.
    pub path: String,
    pub module: Vec<String>,
    pub file: PathBuf,
    pub source: String,
    /// The line its declaration starts on.
    pub line: u32,
    /// The inputs it declares without a default.
    pub required: Vec<String>,
}

/// The component `wanted` names, `Name` or `module::Name`, among the
/// package's sources: the one declaring it, an exported one first when
/// several do. `Ok(None)` when none does.
///
/// # Errors
///
/// Why the name is ambiguous.
pub(in crate::command) fn find(
    package: &LoadedPackage,
    wanted: &str,
) -> Result<Option<Found>, String> {
    let (module, name) = match wanted.rsplit_once("::") {
        Some((module, name)) => (Some(module), name),
        None => (None, wanted),
    };
    let mut found: Vec<(bool, Found)> = Vec::new();
    for file in &package.files {
        let joined = file.module.join("::");
        if module.is_some_and(|module| module != joined) {
            continue;
        }
        let declared = declared_components(&file.source);
        let Some(DeclaredComponent {
            exported,
            range,
            required,
            ..
        }) = declared.into_iter().find(|c| c.name == name)
        else {
            continue;
        };
        let start = range.start().to_usize().min(file.source.len());
        let line = file.source[..start].matches('\n').count() as u32 + 1;
        found.push((
            exported,
            Found {
                name: name.to_owned(),
                path: if joined.is_empty() {
                    name.to_owned()
                } else {
                    format!("{joined}::{name}")
                },
                module: file.module.clone(),
                file: file.path.clone(),
                source: file.source.clone(),
                line,
                required,
            },
        ));
    }
    if found.iter().any(|(exported, _)| *exported) {
        found.retain(|(exported, _)| *exported);
    }
    match found.len() {
        0 => Ok(None),
        1 => Ok(found.pop().map(|(_, found)| found)),
        _ => {
            let paths: Vec<String> = found
                .iter()
                .map(|(_, found)| format!("`{}`", found.path))
                .collect();
            Err(format!(
                "several components are named `{name}`: {}; name one by its module path",
                paths.join(", ")
            ))
        }
    }
}

/// The component of `source`, the text of `file` as module `module`, that
/// `wanted` names, else the one it exports, else the first it declares.
pub(in crate::command) fn in_source(
    file: PathBuf,
    source: String,
    module: Vec<String>,
    wanted: Option<&str>,
) -> Option<Found> {
    let declared = declared_components(&source);
    let chosen = match wanted {
        Some(name) => declared.into_iter().find(|c| c.name == name)?,
        None => {
            let first_exported = declared.iter().position(|c| c.exported).unwrap_or(0);
            declared.into_iter().nth(first_exported)?
        }
    };
    let start = chosen.range.start().to_usize().min(source.len());
    let line = source[..start].matches('\n').count() as u32 + 1;
    let joined = module.join("::");
    Some(Found {
        path: if joined.is_empty() {
            chosen.name.clone()
        } else {
            format!("{joined}::{}", chosen.name)
        },
        name: chosen.name,
        module,
        file,
        source,
        line,
        required: chosen.required,
    })
}

/// Why a component cannot be compiled to capture alone.
pub(in crate::command) enum Unbuilt {
    /// Diagnostics of the component's own source.
    Source(Vec<Diagnostic>),
    /// Why mounting it alone fails.
    Alone(Vec<String>),
}

/// The view package mounting `found` alone, compiled with `profile`, the
/// one the app's views are compiled with.
pub(in crate::command) fn package(
    found: &Found,
    package: &str,
    language: Option<String>,
    siblings: Siblings,
    profile: TargetProfile,
) -> Result<Vec<u8>, Unbuilt> {
    if !found.required.is_empty() {
        let names: Vec<String> = found.required.iter().map(|n| format!("`{n}`")).collect();
        return Err(Unbuilt::Alone(vec![format!(
            "it needs {}, which has no default",
            names.join(", ")
        )]));
    }
    let harness = harness_name(&found.source);
    let mut text = found.source.clone();
    if !text.ends_with('\n') {
        text.push('\n');
    }
    let appended = text.len();
    text.push_str(&format!(
        "\ncomponent {harness} {{\n    view {{\n        {} {{}}\n    }}\n}}\n",
        found.name
    ));
    let origin = Origin {
        package: package.to_owned(),
        module: found.module.clone(),
        language,
        siblings,
    };
    viso_dsl::aot::build_mount_package_for(&text, &origin, profile, &harness).map_err(|errors| {
        let (alone, own): (Vec<Diagnostic>, Vec<Diagnostic>) = errors
            .into_iter()
            .partition(|d| d.primary.start().to_usize() >= appended);
        if own.is_empty() {
            Unbuilt::Alone(alone.into_iter().map(|d| d.message).collect())
        } else {
            Unbuilt::Source(own)
        }
    })
}

/// A component name `source` does not use.
fn harness_name(source: &str) -> String {
    (0..)
        .map(|n| {
            if n == 0 {
                "SnapshotHarness".to_owned()
            } else {
                format!("SnapshotHarness{n}")
            }
        })
        .find(|name| !source.contains(name.as_str()))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_harness_takes_a_name_the_source_does_not_use() {
        assert_eq!(harness_name("component A {}"), "SnapshotHarness");
        assert_eq!(
            harness_name("component SnapshotHarness {}"),
            "SnapshotHarness1"
        );
    }

    #[test]
    fn a_component_alone_takes_its_inputs_defaults() {
        let found = Found {
            name: "Badge".into(),
            path: "ui::Badge".into(),
            module: vec!["ui".into()],
            file: PathBuf::from("src/ui.vs"),
            source: "export component Badge {\n    input label: String = \"new\";\n    view { Text { text: label; } }\n}\n".into(),
            line: 1,
            required: Vec::new(),
        };
        assert!(
            package(
                &found,
                "app",
                None,
                Siblings::None,
                TargetProfile::default()
            )
            .is_ok()
        );
        let needy = Found {
            required: vec!["label".into()],
            ..found
        };
        match package(
            &needy,
            "app",
            None,
            Siblings::None,
            TargetProfile::default(),
        ) {
            Err(Unbuilt::Alone(why)) => {
                assert!(why.iter().any(|m| m.contains("label")), "{why:?}");
            }
            Err(Unbuilt::Source(d)) => panic!("{d:?}"),
            Ok(_) => panic!("a required input without a default was given one"),
        }
    }
}
