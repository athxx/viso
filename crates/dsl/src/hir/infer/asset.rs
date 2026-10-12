//! The `asset("…")` built-in: a string literal naming a file under the
//! package's `assets/`, checked when compiling, and typed by the file's format —
//! an `ImageAsset` for a PNG or JPEG, an `SvgAsset` for an SVG (`E3713` otherwise).

use super::InferCx;
use super::format::template_literal;
use super::pattern::unescape;
use crate::assets::{AssetIssue, AssetUse};
use crate::ast::{AstNode, Expr};
use crate::diag::{Applicability, Diagnostic, Fix, TextEdit};
use crate::hir::ty::Ty;
use crate::resolve::prelude::{image_record, svg_record};
use crate::syntax::SyntaxNode;
use viso_ui::resource::{AssetFormat, AssetKind, ResourceId, normalize_path};

/// The diagnostic code of every misuse of an asset.
pub(crate) const ASSET: &str = "E3713";

/// The path `asset(…)`'s argument spells, when it is a string literal.
pub(crate) fn asset_path(arg: &Expr) -> Option<String> {
    let (body, raw) = template_literal(arg)?;
    if raw { Some(body) } else { unescape(&body) }
}

/// The resource an `asset("…")` call with argument `arg` names, from its path
/// alone: what the behavior lowering makes the value of.
pub(crate) fn asset_resource(arg: &Expr) -> Option<ResourceId> {
    Some(ResourceId::of(&normalize_path(&asset_path(arg)?)?))
}

/// The prelude record of an asset of `format`.
pub(crate) fn asset_record(format: AssetFormat) -> crate::resolve::SymbolId {
    match format.kind() {
        AssetKind::Raster => image_record(),
        AssetKind::Vector => svg_record(),
    }
}

impl InferCx<'_> {
    /// Types `asset(path)`: an `ImageAsset` or an `SvgAsset` by the format of the file
    /// `path` names, which must be a string literal under `assets/`.
    pub(super) fn check_asset(&mut self, args: &[Expr], node: &SyntaxNode) -> Ty {
        let [arg] = args else {
            for arg in args {
                let _ = self.infer_expr(arg, None);
            }
            self.diagnostics.push(Diagnostic::error(
                ASSET,
                node.text_range(),
                "`asset` takes one path, a string literal: `asset(\"images/logo.png\")`",
            ));
            return Ty::Unknown;
        };
        let at = arg.syntax().text_range();
        let Some(path) = asset_path(arg) else {
            let _ = self.infer_expr(arg, None);
            self.diagnostics.push(Diagnostic::error(
                ASSET,
                at,
                "an asset is named by a string literal, so the package knows every asset \
                 it ships: to choose one, branch between `asset(\"…\")` values",
            ));
            return Ty::Unknown;
        };
        let Some(catalog) = self.env.assets() else {
            self.diagnostics.push(Diagnostic::error(
                ASSET,
                at,
                "this compile has no `assets/` directory to find the asset in",
            ));
            return Ty::Unknown;
        };
        match catalog.lookup(&path) {
            Ok((normal, id, format)) => {
                self.env.record_asset(AssetUse {
                    id,
                    path: normal,
                    format,
                });
                Ty::named(asset_record(format))
            }
            Err(issue) => {
                let mut diagnostic = Diagnostic::error(
                    ASSET,
                    at,
                    match &issue {
                        AssetIssue::Outside => format!(
                            "`{path}` is outside `assets/`: an asset path is relative to it \
                             and never climbs out"
                        ),
                        AssetIssue::Missing { .. } => format!("`assets/{path}` does not exist"),
                        AssetIssue::Format => {
                            format!("`assets/{path}` is not a PNG, JPEG or SVG file")
                        }
                    },
                );
                if let AssetIssue::Missing {
                    nearest: Some(nearest),
                } = issue
                {
                    diagnostic.fixes.push(Fix {
                        title: format!("use `{nearest}`"),
                        applicability: Applicability::MaybeIncorrect,
                        edits: vec![TextEdit {
                            module: None,
                            range: at,
                            replacement: format!("\"{nearest}\""),
                        }],
                    });
                }
                self.diagnostics.push(diagnostic);
                Ty::Unknown
            }
        }
    }
}
