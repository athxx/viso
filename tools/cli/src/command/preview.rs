//! `viso preview <file> [<component>]`: generated code shown alone in a
//! sandboxed headless run.
//!
//! The file compiles with the package, as the module named after it, and its
//! component mounts alone with its inputs' defaults, as `viso snapshot
//! capture` shows one; but its view runs with the preview grant alone —
//! `ui.basic`, `gpu.draw.sandboxed` and `asset.read.package`, and each
//! `--grant` — whatever the package's manifest grants. A native it calls
//! outside the grant faults `E6103` and its transaction rolls back, and the
//! view runs on; each fault is reported at its source. The run's snapshot is
//! written as `<component>.png` and `.tree` to `target/viso/preview/`, or
//! `--output`.

use std::fs;
use std::path::Path;

use sha2::{Digest, Sha256};
use viso_dev_protocol::headless::HeadlessSurface;
use viso_dsl::{Diagnostic, TextRange, TextSize};

use super::check;
use super::scenario::{self, Case, Ended};
use super::snapshot::component;
use super::test::{invalid, line_range};
use crate::args::PreviewArgs;
use crate::context::Context;
use crate::error::{
    DIAGNOSTICS, ENV_SNAPSHOT_WRITE, ENV_SOURCE_UNREADABLE, RUN_SNAPSHOT_FAILED, RUN_TEST_CRASHED,
    RUNTIME, SUCCESS,
};
use crate::output::{Artifact, Output, Source};

/// The target a preview runs on.
const TARGET: &str = "headless";

pub fn run(ctx: &Context<'_>, args: &PreviewArgs, out: &mut Output) -> u8 {
    let checked = match check::load(ctx, out) {
        Ok(checked) => checked,
        Err(code) => return code,
    };
    let code = checked.code(out);
    if code != SUCCESS {
        return code;
    }
    let cwd = match ctx.cwd(out) {
        Ok(cwd) => cwd,
        Err(code) => return code,
    };
    let path = cwd.join(&args.file);
    let source = match fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => {
            return out.failure(
                ENV_SOURCE_UNREADABLE,
                &format!("cannot read `{}`: {error}", args.file.display()),
                &[],
            );
        }
    };
    let root = checked.project.root.clone();
    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
    // A file of the package keeps its module path; any other is the module
    // its name gives.
    let module = checked
        .package
        .files
        .iter()
        .find(|file| file.path.canonicalize().is_ok_and(|p| p == canonical))
        .map_or_else(
            || {
                let stem = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("preview");
                vec![stem.to_owned()]
            },
            |file| file.module.clone(),
        );
    let shown = Source::new(&path, &root, &source);
    let Some(found) = component::in_source(
        path.clone(),
        source.clone(),
        module,
        args.component.as_deref(),
    ) else {
        let why = match &args.component {
            Some(name) => format!("`{}` declares no component `{name}`", args.file.display()),
            None => format!("`{}` declares no component", args.file.display()),
        };
        invalid(out, Some(&shown), None, &why);
        return DIAGNOSTICS;
    };
    let view = match component::package(
        &found,
        &checked.name,
        checked.language(),
        checked.package.siblings(),
        checked.view_profile(),
    ) {
        Ok(view) => view,
        Err(component::Unbuilt::Source(diagnostics)) => {
            for diagnostic in &diagnostics {
                out.source(Some(&shown), &[], diagnostic);
            }
            return DIAGNOSTICS;
        }
        Err(component::Unbuilt::Alone(why)) => {
            invalid(
                out,
                Some(&shown),
                Some(line_range(&source, found.line)),
                &format!(
                    "the component `{}` cannot be previewed alone: {}",
                    found.name,
                    why.join("; ")
                ),
            );
            return DIAGNOSTICS;
        }
    };
    let (width, height, scale) = checked.project.manifest.test.surface.resolved();
    let surface = HeadlessSurface {
        width,
        height,
        scale: scale.get(),
    };
    let name = found.name.clone();
    let mut case = Case::component(
        name.clone(),
        found.path.clone(),
        (found.file.clone(), found.source.clone(), found.line),
        view,
        surface,
    );
    case.grant = Some(args.grants.clone());

    let build = match scenario::build(ctx, None, out) {
        Ok(build) => build,
        Err(code) => return code,
    };
    let dir = match &args.output {
        Some(dir) => cwd.join(dir),
        None => ctx.layout(&root).viso_dir().join("preview"),
    };
    scenario::play(
        ctx,
        &build,
        vec![case],
        out,
        |_| false,
        |out, case, played| {
            let mut code = SUCCESS;
            for fault in &played.faults {
                let at = fault.span.map_or_else(
                    || line_range(&source, found.line),
                    |(start, end)| {
                        let clamp = |at: u32| TextSize::from(at.min(source.len() as u32));
                        TextRange::new(clamp(start), clamp(end.max(start)))
                    },
                );
                let code_name =
                    viso_dsl::diag::codes::lookup(&fault.code).map_or("E7105", |info| info.code);
                out.source(
                    Some(&shown),
                    &[],
                    &Diagnostic::error(code_name, at, fault.message.clone()),
                );
                code = RUNTIME;
            }
            match &played.ended {
                Ended::Passed | Ended::Stopped => {}
                Ended::Failed { message, .. } => {
                    out.failure(RUN_SNAPSHOT_FAILED, message, &[]);
                    out.case_output(&case.name, &played.output);
                    return RUNTIME;
                }
                Ended::Crashed(why) => {
                    out.failure(RUN_TEST_CRASHED, why, &[]);
                    out.case_output(&case.name, &played.output);
                    return RUNTIME;
                }
            }
            let written = ["png", "tree"].map(|ext| {
                let from = played.snapshots().join(format!("{name}.{ext}"));
                let to = dir.join(format!("{name}.{ext}"));
                fs::create_dir_all(&dir)
                    .and_then(|()| fs::read(&from))
                    .and_then(|bytes| fs::write(&to, &bytes).map(|()| bytes))
                    .map(|bytes| (ext, to, bytes))
            });
            for file in written {
                match file {
                    Ok((ext, to, bytes)) => {
                        let hex: String = Sha256::digest(&bytes)
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect();
                        let shown = shown_path(&root, &to);
                        out.artifact(
                            &Artifact {
                                kind: if ext == "png" { "snapshot" } else { "manifest" },
                                path: &shown,
                                target: TARGET,
                                profile: Some(build.profile),
                                build_id: &build.build_id,
                                size: bytes.len() as u64,
                                hash: &format!("sha256:{hex}"),
                                cached: false,
                            },
                            || format!("  {ext:<10} {shown}\n"),
                        );
                    }
                    Err(error) => {
                        out.failure(
                            ENV_SNAPSHOT_WRITE,
                            &format!("cannot write the preview into `{}`: {error}", dir.display()),
                            &[],
                        );
                        return RUNTIME.max(code);
                    }
                }
            }
            played.discard();
            code
        },
    )
}

/// `path` relative to `root` when below it, with `/` separators.
fn shown_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_file_is_shown_below_the_project() {
        let root = Path::new("/p");
        assert_eq!(
            shown_path(root, Path::new("/p/target/viso/preview/Card.png")),
            "target/viso/preview/Card.png"
        );
        assert_eq!(shown_path(root, Path::new("/q/Card.png")), "/q/Card.png");
    }
}
