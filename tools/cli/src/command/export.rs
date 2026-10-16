//! `viso export html`: a component, the one named or else the one
//! `src/app.vs` exports, as a page another web project owns: semantic HTML,
//! CSS with the theme as custom properties, and vanilla JavaScript for what
//! changes, written into `dist-html/` or the directory `--out` or
//! `export.html.out_dir` names.
//!
//! The view is checked and every node classified before a file is written:
//! what the page cannot show is an `EXPORT_*` diagnostic at its source. The
//! directory is replaced whole, and only while it holds nothing but what the
//! last export wrote, unchanged, unless `--force`.

use std::fs;
use std::path::{Path, PathBuf};

use viso_dsl::Diagnostic;
use viso_dsl::export::{self, HtmlOptions, Need};
use viso_dsl::frontend::{Origin, compile_file_for, compile_mount_for};

use super::build::relative_name;
use super::check;
use super::snapshot::component;
use crate::args::ExportHtmlArgs;
use crate::context::Context;
use crate::error::{
    CLI_USAGE, Code, DIAGNOSTICS, EXPORT_CONFLICT, EXPORT_HTML_DYNAMIC_REQUIRED,
    EXPORT_HTML_GPU_ONLY_NODE, EXPORT_NO_VIEW, EXPORT_UNSUPPORTED, EXPORT_WRITE, SUCCESS,
};
use crate::generated::{self, Refusal, Tree};
use crate::output::{Artifact, Output, Source, human, sayln};

/// Where an export goes when neither `--out` nor `Viso.toml` says.
const OUT_DIR: &str = "dist-html";

/// The module whose view an export starts from: `src/app.vs`.
const APP: &str = "app";

pub fn html(ctx: &Context<'_>, args: &ExportHtmlArgs, out: &mut Output) -> u8 {
    let checked = match check::load(ctx, out) {
        Ok(checked) => checked,
        Err(code) => return code,
    };
    if checked.code(out) != SUCCESS {
        return checked.code(out);
    }
    let root = checked.project.root.clone();
    let dir = match &args.out {
        Some(dir) => match ctx.cwd(out) {
            Ok(cwd) => cwd.join(dir),
            Err(code) => return code,
        },
        None => root.join(
            checked
                .project
                .manifest
                .export
                .html
                .out_dir
                .as_ref()
                .map_or(OUT_DIR, |dir| dir.value.as_str()),
        ),
    };
    if let Some(why) = unsafe_dir(&root, &dir) {
        return out.failure(
            CLI_USAGE,
            &format!("`{}` {why}", relative_name(&root, &dir, true)),
            &[
                "an export replaces its directory whole; name one of its own with `--out`"
                    .to_owned(),
            ],
        );
    }

    let natives = viso_dsl::schema::Natives::standard();
    let (path, text, compiled) = match &args.component {
        Some(wanted) => {
            let found = match component::find(&checked.package, wanted) {
                Ok(Some(found)) => found,
                Ok(None) => {
                    return out.failure(
                        EXPORT_NO_VIEW,
                        &format!("the project declares no component `{wanted}`"),
                        &["name it `Name` or `module::Name`".to_owned()],
                    );
                }
                Err(why) => return out.failure(CLI_USAGE, &why, &[]),
            };
            if !found.required.is_empty() {
                let names: Vec<String> = found.required.iter().map(|n| format!("`{n}`")).collect();
                return out.failure(
                    EXPORT_UNSUPPORTED,
                    &format!(
                        "`{}` needs {}, which has no default; an export mounts it alone",
                        found.path,
                        names.join(", ")
                    ),
                    &[
                        "export the component that gives it, or give the input a default"
                            .to_owned(),
                    ],
                );
            }
            let origin = Origin {
                package: checked.name.clone(),
                module: found.module.clone(),
                language: checked.language(),
            };
            let compiled = compile_mount_for(
                &found.source,
                &origin,
                natives,
                checked.view_profile(),
                &found.name,
            );
            (found.file, found.source, compiled)
        }
        None => {
            let Some(file) = checked
                .package
                .files
                .iter()
                .find(|file| file.module.len() == 1 && file.module[0] == APP)
            else {
                return out.failure(
                    EXPORT_NO_VIEW,
                    "the project has no `src/app.vs`, whose view an export starts from",
                    &["name the component to export: `viso export html <COMPONENT>`".to_owned()],
                );
            };
            let origin = Origin {
                package: checked.name.clone(),
                module: file.module.clone(),
                language: checked.language(),
            };
            let compiled = compile_file_for(&file.source, &origin, natives, checked.view_profile());
            (file.path.clone(), file.source.clone(), compiled)
        }
    };
    let source = Source::new(&path, &root, &text);
    if compiled.has_errors() {
        for diagnostic in compiled.errors() {
            out.source(Some(&source), &[], diagnostic);
        }
        return DIAGNOSTICS;
    }

    let manifest = &checked.project.manifest;
    let options = HtmlOptions {
        static_only: args.static_only,
        title: manifest
            .package
            .display_name
            .as_ref()
            .map_or(checked.name.clone(), |name| name.value.clone()),
        lang: manifest
            .i18n
            .source
            .as_ref()
            .map_or(viso_dsl::i18n::DEFAULT_SOURCE.to_owned(), |s| {
                s.value.clone()
            }),
    };
    out.progress("export", "lowering the view to HTML");
    let page = match export::html(&compiled, &options) {
        Ok(page) => page,
        Err(findings) => {
            for finding in &findings {
                let code = match finding.need {
                    Need::Dynamic => EXPORT_HTML_DYNAMIC_REQUIRED,
                    Need::GpuOnly => EXPORT_HTML_GPU_ONLY_NODE,
                    Need::Unsupported => EXPORT_UNSUPPORTED,
                };
                out.source(Some(&source), &[], &diagnostic(code, finding));
            }
            return DIAGNOSTICS;
        }
    };

    let mut tree = Tree::default();
    tree.add("index.html", page.index);
    tree.add("styles.css", page.styles);
    if let Some(script) = page.script {
        tree.add("app.js", script);
    }
    let assets = root.join(viso_dsl::assets::ASSET_DIR);
    for path in &page.assets {
        match fs::read(assets.join(path)) {
            Ok(bytes) => tree.add(format!("assets/{path}"), bytes),
            Err(e) => {
                return out.failure(
                    EXPORT_WRITE,
                    &format!("cannot read `assets/{path}`: {e}"),
                    &[],
                );
            }
        }
    }
    let files = tree.files().count();
    out.progress("export", "writing the export");
    match generated::write(&dir, tree, "viso export html", args.force) {
        Ok(()) => {}
        Err(Refusal::Conflict {
            foreign,
            paths,
            more,
        }) => {
            let shown = relative_name(&root, &dir, true);
            let message = if foreign {
                format!("`{shown}` holds files the export did not write")
            } else {
                format!("`{shown}` holds files edited or added since it was exported")
            };
            let mut notes: Vec<String> = paths
                .iter()
                .filter(|p| !p.is_empty())
                .map(|p| format!("`{p}`"))
                .collect();
            if more > 0 {
                notes.push(format!("and {more} more"));
            }
            notes.push("move them out, or pass `--force` to replace them".to_owned());
            return out.failure_next(
                EXPORT_CONFLICT,
                &message,
                &notes,
                &["viso export html --force".to_owned()],
            );
        }
        Err(Refusal::Io(why)) => return out.failure(EXPORT_WRITE, &why, &[]),
    }

    let (size, hash) = match crate::build::hash(&dir) {
        Ok(found) => found,
        Err(e) => return out.failure(EXPORT_WRITE, &format!("{}: {e}", dir.display()), &[]),
    };
    let build_id: String = hash
        .trim_start_matches("sha256:")
        .chars()
        .take(32)
        .collect();
    let path = relative_name(&root, &dir, true);
    if out.human() {
        sayln!("Exported html in {files} files");
    }
    out.artifact(
        &Artifact {
            kind: "export-dir",
            path: path.trim_end_matches('/'),
            target: "html",
            profile: None,
            build_id: &build_id,
            size,
            hash: &hash,
        },
        || format!("  export-dir {path}  {}\n", human::size(size)),
    );
    SUCCESS
}

/// Why `dir` cannot be an export's directory: the project, an ancestor of
/// it, or where its sources live.
fn unsafe_dir(root: &Path, dir: &Path) -> Option<&'static str> {
    let clean = |p: &Path| -> PathBuf {
        let mut out = PathBuf::new();
        for part in p.components() {
            match part {
                std::path::Component::ParentDir => {
                    out.pop();
                }
                std::path::Component::CurDir => {}
                part => out.push(part),
            }
        }
        out
    };
    let (root, dir) = (clean(root), clean(dir));
    if root.starts_with(&dir) {
        return Some("holds the project");
    }
    for sources in [viso_dsl::package::SOURCE_ROOT, viso_dsl::assets::ASSET_DIR] {
        if dir.starts_with(root.join(sources)) {
            return Some("is among the project's sources");
        }
    }
    None
}

/// A finding as a diagnostic under `code`.
fn diagnostic(code: Code, finding: &export::Finding) -> Diagnostic {
    let mut diagnostic = Diagnostic::error(code.name, finding.at, finding.message.clone());
    diagnostic.notes = finding.notes.clone();
    diagnostic
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_export_never_replaces_the_project_or_its_sources() {
        let root = Path::new("/p/app");
        assert!(unsafe_dir(root, Path::new("/p/app")).is_some());
        assert!(unsafe_dir(root, Path::new("/p")).is_some());
        assert!(unsafe_dir(root, Path::new("/p/app/src/web")).is_some());
        assert!(unsafe_dir(root, Path::new("/p/app/assets")).is_some());
        assert!(unsafe_dir(root, Path::new("/p/app/x/..")).is_some());
        assert!(unsafe_dir(root, Path::new("/p/app/dist-html")).is_none());
        assert!(unsafe_dir(root, Path::new("/elsewhere/site")).is_none());
    }
}
