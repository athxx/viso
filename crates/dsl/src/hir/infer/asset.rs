//! The `asset("…")` built-in: a string literal naming a file under the
//! package's `assets/`, checked when compiling, and typed by the file's format —
//! an `ImageAsset` for a raster, an `SvgAsset` for an SVG, a `VideoAsset` for a
//! video (`E3713` otherwise).

use super::InferCx;
use super::interpolation::string_literal;
use super::pattern::unescape;
use crate::assets::{AssetIssue, AssetUse};
use crate::ast::{AstNode, Expr};
use crate::diag::{Applicability, Diagnostic, Fix, TextEdit};
use crate::hir::ty::Ty;
use crate::resolve::prelude::{image_record, svg_record, video_record};
use crate::syntax::SyntaxNode;
use viso_schema::resource::{AssetFormat, AssetKind, ResourceId, normalize_path};

pub(crate) use crate::assets::ASSET_ERROR as ASSET;

/// The path `asset(…)`'s argument spells, when it is a string literal.
pub(crate) fn asset_path(arg: &Expr) -> Option<String> {
    let (body, raw) = string_literal(arg)?;
    if raw { Some(body) } else { unescape(&body) }
}

/// The resource an `asset("…")` call with argument `arg` names, from its path
/// alone: what the behavior lowering makes the value of.
pub(crate) fn asset_resource(arg: &Expr) -> Option<ResourceId> {
    Some(ResourceId::of(&normalize_path(&asset_path(arg)?)?))
}

/// The prelude record of an asset of `format`; `None` for a model, which a
/// view does not show.
pub(crate) fn asset_record(format: AssetFormat) -> Option<crate::resolve::SymbolId> {
    match format.kind() {
        AssetKind::Raster => Some(image_record()),
        AssetKind::Vector => Some(svg_record()),
        AssetKind::Video => Some(video_record()),
        AssetKind::Model => None,
    }
}

impl InferCx<'_> {
    /// Types `asset(path)`: an `ImageAsset`, `SvgAsset` or `VideoAsset` by the format of the file
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
            Ok((_, _, format)) if asset_record(format).is_none() => {
                self.diagnostics.push(Diagnostic::error(
                    ASSET,
                    at,
                    format!(
                        "`assets/{path}` is a {} model: a view shows images, SVGs and \
                         videos, and a scene draws a model from Rust",
                        format.name()
                    ),
                ));
                Ty::Unknown
            }
            Ok((normal, id, format)) => {
                self.env.record_asset(AssetUse {
                    id,
                    path: normal,
                    format,
                });
                asset_record(format).map_or(Ty::Unknown, Ty::named)
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
                            format!("`assets/{path}` is not an image, SVG or video file")
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
