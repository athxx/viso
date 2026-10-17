//! `viso export html|solid`: a component, the one named or else the one
//! `src/app.vs` exports, as source another web project owns. `html` writes
//! a page (semantic HTML, CSS with the theme as custom properties, vanilla
//! JavaScript for what changes) into `dist-html/`; `solid` a Vite project
//! whose SolidJS component is the view into `web-solid/`; `--out` or
//! `export.<target>.out_dir` names another directory.
//!
//! The view is checked and every node classified before a file is written:
//! what the export cannot express is an `EXPORT_*` diagnostic at its source.
//! The directory is replaced whole, and only while it holds nothing but what
//! the last export wrote, unchanged, unless `--force`.

use std::fs;
use std::path::{Path, PathBuf};

use viso_dsl::Diagnostic;
use viso_dsl::export::{self, Finding, HtmlOptions, Need, SolidOptions};
use viso_dsl::frontend::{Compiled, Origin, compile_file_for, compile_mount_for};
use viso_project::Spanned;

use super::build::relative_name;
use super::check::{self, Checked};
use super::snapshot::component;
use crate::args::{ExportHtmlArgs, ExportSolidArgs};
use crate::context::Context;
use crate::error::{
    CLI_USAGE, Code, DIAGNOSTICS, EXPORT_CONFLICT, EXPORT_HTML_DYNAMIC_REQUIRED,
    EXPORT_HTML_GPU_ONLY_NODE, EXPORT_NO_VIEW, EXPORT_SOLID_GPU_ONLY_NODE, EXPORT_UNSUPPORTED,
    EXPORT_WRITE, SUCCESS,
};
use crate::generated::{self, Refusal, Tree};
use crate::output::{Artifact, Output, Source, human, sayln};

/// What in a SolidJS export's directory belongs to the tools the project
/// runs, which a new export keeps as it is.
const KEPT: &[&str] = &[
    "node_modules",
    "dist",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lock",
    "bun.lockb",
];

/// The module whose view an export starts from: `src/app.vs`.
const APP: &str = "app";

pub fn html(ctx: &Context<'_>, args: &ExportHtmlArgs, out: &mut Output) -> u8 {
    let view = match View::load(
        ctx,
        out,
        "html",
        args.component.as_deref(),
        args.out.as_deref(),
    ) {
        Ok(view) => view,
        Err(code) => return code,
    };
    let options = HtmlOptions {
        static_only: args.static_only,
        title: view.title(),
        lang: view.lang(),
    };
    out.progress("export", "lowering the view to HTML");
    let page = match export::html(&view.compiled, &options) {
        Ok(page) => page,
        Err(findings) => return view.refuse(out, &findings, EXPORT_HTML_GPU_ONLY_NODE),
    };
    let mut tree = Tree::default();
    tree.add("index.html", page.index);
    tree.add("styles.css", page.styles);
    if let Some(script) = page.script {
        tree.add("app.js", script);
    }
    if let Err(code) = view.assets(out, &page.assets, "assets", &mut tree) {
        return code;
    }
    view.write(out, tree, args.force)
}

pub fn solid(ctx: &Context<'_>, args: &ExportSolidArgs, out: &mut Output) -> u8 {
    let view = match View::load(
        ctx,
        out,
        "solid",
        args.component.as_deref(),
        args.out.as_deref(),
    ) {
        Ok(view) => view,
        Err(code) => return code,
    };
    let manifest = &view.checked.project.manifest;
    let options = SolidOptions {
        package: npm_name(&view.checked.name),
        version: manifest
            .package
            .version
            .as_ref()
            .map_or("0.1.0".to_owned(), |v| v.value.clone()),
        title: view.title(),
        lang: view.lang(),
    };
    out.progress("export", "lowering the view to a SolidJS component");
    let project = match export::solid(&view.compiled, &options) {
        Ok(project) => project,
        Err(findings) => return view.refuse(out, &findings, EXPORT_SOLID_GPU_ONLY_NODE),
    };
    let mut tree = Tree::default();
    for (path, text) in project.files {
        tree.add(path, text);
    }
    // What the package manager installs and the bundler builds stays.
    for kept in KEPT {
        tree.keep(kept);
    }
    if let Err(code) = view.assets(out, &project.assets, "src/assets", &mut tree) {
        return code;
    }
    let code = view.write(out, tree, args.force);
    if code == SUCCESS && out.human() {
        let manager = manifest
            .export
            .solid
            .package_manager
            .as_ref()
            .map_or("npm", |m| m.value.as_str());
        let dir = relative_name(&view.root, &view.dir, true);
        let run = if manager == "npm" {
            "npm run dev"
        } else {
            &format!("{manager} dev")
        };
        sayln!(
            "  next: cd {} && {manager} install && {run}",
            dir.trim_end_matches('/')
        );
    }
    code
}

/// An npm package name for the Viso package `name`: lower case, `-` for
/// what npm does not allow.
fn npm_name(name: &str) -> String {
    let name: String = name
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{}-solid", name.trim_matches(|c| c == '-' || c == '.'))
}

/// The view an export starts from, compiled, and where it goes.
struct View {
    checked: Checked,
    root: PathBuf,
    dir: PathBuf,
    target: &'static str,
    path: PathBuf,
    text: String,
    compiled: Compiled,
}

impl View {
    fn load(
        ctx: &Context<'_>,
        out: &mut Output,
        target: &'static str,
        component: Option<&str>,
        flag: Option<&Path>,
    ) -> Result<View, u8> {
        let checked = check::load(ctx, out)?;
        if checked.code(out) != SUCCESS {
            return Err(checked.code(out));
        }
        let root = checked.project.root.clone();
        let export = &checked.project.manifest.export;
        let (configured, default): (Option<&Spanned<String>>, &str) = match target {
            "solid" => (export.solid.out_dir.as_ref(), "web-solid"),
            _ => (export.html.out_dir.as_ref(), "dist-html"),
        };
        let dir = match flag {
            Some(dir) => ctx.cwd(out)?.join(dir),
            None => root.join(configured.map_or(default, |dir| dir.value.as_str())),
        };
        if let Some(why) = unsafe_dir(&root, &dir) {
            return Err(out.failure(
                CLI_USAGE,
                &format!("`{}` {why}", relative_name(&root, &dir, true)),
                &[
                    "an export replaces its directory whole; name one of its own with `--out`"
                        .to_owned(),
                ],
            ));
        }

        let natives = viso_dsl::schema::Natives::standard();
        let (path, text, compiled) = match component {
            Some(wanted) => {
                let found = match component::find(&checked.package, wanted) {
                    Ok(Some(found)) => found,
                    Ok(None) => {
                        return Err(out.failure(
                            EXPORT_NO_VIEW,
                            &format!("the project declares no component `{wanted}`"),
                            &["name it `Name` or `module::Name`".to_owned()],
                        ));
                    }
                    Err(why) => return Err(out.failure(CLI_USAGE, &why, &[])),
                };
                if !found.required.is_empty() {
                    let names: Vec<String> =
                        found.required.iter().map(|n| format!("`{n}`")).collect();
                    return Err(out.failure(
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
                    ));
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
                    return Err(out.failure(
                        EXPORT_NO_VIEW,
                        "the project has no `src/app.vs`, whose view an export starts from",
                        &[format!(
                            "name the component to export: `viso export {target} <COMPONENT>`"
                        )],
                    ));
                };
                let origin = Origin {
                    package: checked.name.clone(),
                    module: file.module.clone(),
                    language: checked.language(),
                };
                let compiled =
                    compile_file_for(&file.source, &origin, natives, checked.view_profile());
                (file.path.clone(), file.source.clone(), compiled)
            }
        };
        if compiled.has_errors() {
            let source = Source::new(&path, &root, &text);
            for diagnostic in compiled.errors() {
                out.source(Some(&source), &[], diagnostic);
            }
            return Err(DIAGNOSTICS);
        }
        Ok(View {
            checked,
            root,
            dir,
            target,
            path,
            text,
            compiled,
        })
    }

    /// The page's title: the package's display name, else its name.
    fn title(&self) -> String {
        let manifest = &self.checked.project.manifest;
        manifest
            .package
            .display_name
            .as_ref()
            .map_or(self.checked.name.clone(), |name| name.value.clone())
    }

    /// The language of the view's text: the package's source locale.
    fn lang(&self) -> String {
        let manifest = &self.checked.project.manifest;
        manifest
            .i18n
            .source
            .as_ref()
            .map_or(viso_dsl::i18n::DEFAULT_SOURCE.to_owned(), |s| {
                s.value.clone()
            })
    }

    /// Reports each finding at its source; a node only the GPU draws is
    /// `gpu_only`.
    fn refuse(&self, out: &mut Output, findings: &[Finding], gpu_only: Code) -> u8 {
        let source = Source::new(&self.path, &self.root, &self.text);
        for finding in findings {
            let code = match finding.need {
                Need::Dynamic => EXPORT_HTML_DYNAMIC_REQUIRED,
                Need::GpuOnly => gpu_only,
                Need::Unsupported => EXPORT_UNSUPPORTED,
            };
            out.source(Some(&source), &[], &diagnostic(code, finding));
        }
        DIAGNOSTICS
    }

    /// Adds each of `paths` under the project's `assets/` to `tree` under
    /// `under`.
    fn assets(
        &self,
        out: &mut Output,
        paths: &[String],
        under: &str,
        tree: &mut Tree,
    ) -> Result<(), u8> {
        let assets = self.root.join(viso_dsl::assets::ASSET_DIR);
        for path in paths {
            match fs::read(assets.join(path)) {
                Ok(bytes) => tree.add(format!("{under}/{path}"), bytes),
                Err(e) => {
                    return Err(out.failure(
                        EXPORT_WRITE,
                        &format!("cannot read `assets/{path}`: {e}"),
                        &[],
                    ));
                }
            }
        }
        Ok(())
    }

    /// Replaces the export's directory with `tree` and reports it.
    fn write(&self, out: &mut Output, tree: Tree, force: bool) -> u8 {
        let (root, dir, target) = (&self.root, &self.dir, self.target);
        let files = tree.files().count();
        let (size, hash) = tree.hash();
        out.progress("export", "writing the export");
        let generator = format!("viso export {target}");
        match generated::write(dir, tree, &generator, force) {
            Ok(()) => {}
            Err(Refusal::Conflict {
                foreign,
                paths,
                more,
            }) => {
                let shown = relative_name(root, dir, true);
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
                    &[format!("viso export {target} --force")],
                );
            }
            Err(Refusal::Io(why)) => return out.failure(EXPORT_WRITE, &why, &[]),
        }

        let build_id: String = hash
            .trim_start_matches("sha256:")
            .chars()
            .take(32)
            .collect();
        let path = relative_name(root, dir, true);
        if out.human() {
            sayln!("Exported {target} in {files} files");
        }
        out.artifact(
            &Artifact {
                kind: "export-dir",
                path: path.trim_end_matches('/'),
                target,
                profile: None,
                build_id: &build_id,
                size,
                hash: &hash,
                cached: false,
            },
            || format!("  export-dir {path}  {}\n", human::size(size)),
        );
        SUCCESS
    }
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
