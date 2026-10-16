//! One-way exports of a compiled view into source another ecosystem owns.
//!
//! An exporter reads what the compiler made of the view — the UI IR, the
//! behavior table, the Behavior IR — and never the `.vs` text. Before it
//! writes anything it classifies every node: what HTML says as it is, what
//! needs the page script, what only the Viso renderer draws. Whatever it
//! cannot say is a [`Finding`] at its source, never dropped.

mod css;
mod eval;
mod html;
mod script;

use std::fmt::Write as _;
use std::rc::Rc;

use crate::frontend::Compiled;
use crate::syntax::TextRange;
use crate::view_behavior::view_behavior;

/// The line every generated file starts with, in its comment syntax.
pub const GENERATED: &str = "Generated from Viso source. This directory is an export artifact.";

/// What the view needs that an export lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// The page script: a value that changes, a handler, a region. Only a
    /// static export refuses it.
    Dynamic,
    /// The Viso renderer: a node a shader paints.
    GpuOnly,
    /// A construct the exporter has no form for.
    Unsupported,
}

/// Something in the view the export cannot write, where it is written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub need: Need,
    pub at: TextRange,
    pub message: String,
    pub notes: Vec<String>,
}

/// How an HTML export is made.
#[derive(Debug, Clone)]
pub struct HtmlOptions {
    /// Write no script: what needs one is a [`Need::Dynamic`] finding.
    pub static_only: bool,
    /// The page's title.
    pub title: String,
    /// The language of its text, as a BCP 47 tag.
    pub lang: String,
}

/// An HTML export: the page, its stylesheet, its script when it has one,
/// and the assets it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlExport {
    /// `index.html`.
    pub index: String,
    /// `styles.css`.
    pub styles: String,
    /// `app.js`, when anything on the page changes.
    pub script: Option<String>,
    /// The path under the project's `assets/` of each asset the page shows,
    /// written to the same path under the export's `assets/`.
    pub assets: Vec<String>,
}

/// The view `compiled` mounts as an HTML page.
///
/// # Errors
///
/// Every place the view needs what the page cannot have, in source order.
pub fn html(compiled: &Compiled, options: &HtmlOptions) -> Result<HtmlExport, Vec<Finding>> {
    let origin = compiled
        .component
        .as_ref()
        .map_or_else(|| TextRange::new(0.into(), 0.into()), |c| c.source_origin);
    let vb = match view_behavior(compiled) {
        Ok(vb) => vb,
        Err(errors) => {
            return Err(errors
                .into_iter()
                .map(|e| Finding {
                    need: Need::Unsupported,
                    at: e.at.unwrap_or(origin),
                    message: e.message,
                    notes: Vec::new(),
                })
                .collect());
        }
    };
    let mut findings = Vec::new();
    let layout = compiled
        .component
        .as_ref()
        .and_then(|c| compiled.behavior.component(&c.schema.name));
    if let Some(layout) = layout {
        if !compiled.behavior.systems.is_empty() {
            findings.push(Finding {
                need: Need::Unsupported,
                at: origin,
                message: "the view runs a game's systems, which only the Viso runtime schedules"
                    .to_owned(),
                notes: vec!["build for the browser instead: `viso build web-gpu`".to_owned()],
            });
        }
        if !layout.effects.is_empty() {
            findings.push(Finding {
                need: Need::Unsupported,
                at: origin,
                message: "the view runs an `effect` or loads a `resource`, which an HTML export does not run".to_owned(),
                notes: Vec::new(),
            });
        }
        if !layout.regional.is_empty() {
            let at = compiled
                .tree
                .instance(layout.regional[0].instance)
                .map_or(origin, |i| i.origin);
            findings.push(Finding {
                need: Need::Unsupported,
                at,
                message: "a component a region mounts keeps states of its own, which an HTML export does not hold".to_owned(),
                notes: Vec::new(),
            });
        }
    }
    let mut eval = match (layout, &vb) {
        (Some(layout), Some(vb)) => {
            match eval::Eval::new(
                Rc::clone(&vb.module),
                &compiled.behavior,
                layout,
                &options.lang,
                origin,
            ) {
                Ok(eval) => Some(eval),
                Err(finding) => {
                    findings.push(finding);
                    None
                }
            }
        }
        _ => None,
    };
    if !findings.is_empty() {
        return Err(sorted(findings));
    }

    let mut lower = html::Lower::new(
        vb.as_ref(),
        eval.as_mut(),
        &compiled.assets,
        options.static_only,
    );
    lower.view(&compiled.tree.items);
    let scripted = lower.out.root.marks.iter().any(|m| !m.wires.is_empty());
    if scripted {
        lower.all_assets();
    }
    let html::Lower {
        findings,
        out: page,
        ..
    } = lower;
    if !findings.is_empty() {
        return Err(sorted(findings));
    }
    let script = match (&eval, scripted) {
        (Some(eval), true) => {
            let catalog = vb.as_ref().and_then(|vb| vb.module.catalog());
            Some(script::script(&page, eval, catalog, GENERATED, origin).map_err(sorted)?)
        }
        _ => None,
    };
    Ok(HtmlExport {
        index: index(&page, options, script.is_some()),
        styles: styles(&page),
        script,
        assets: page.assets.values().cloned().collect(),
    })
}

fn sorted(mut findings: Vec<Finding>) -> Vec<Finding> {
    findings.sort_by_key(|f| (f.at.start(), f.at.end()));
    findings.dedup();
    findings
}

fn index(page: &html::Lowered, options: &HtmlOptions, scripted: bool) -> String {
    let mut out = String::new();
    out.push_str("<!DOCTYPE html>\n");
    let _ = writeln!(out, "<!-- {GENERATED} -->");
    out.push_str("<html lang=\"");
    html::escape_attr(&options.lang, &mut out);
    out.push_str("\">\n<head>\n<meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1, viewport-fit=cover\">\n");
    out.push_str("<title>");
    html::escape_text(&options.title, &mut out);
    out.push_str("</title>\n<link rel=\"stylesheet\" href=\"styles.css\">\n");
    if scripted {
        out.push_str("<script src=\"app.js\" defer></script>\n");
    }
    out.push_str("</head>\n<body>\n");
    out.push_str(&page.root.html);
    out.push_str("\n</body>\n</html>\n");
    out
}

/// The base rules every element class reads, then the theme, then the
/// page's own.
const BASE: &str = "\
*, *::before, *::after { box-sizing: border-box; }
html, body { margin: 0; width: 100%; height: 100%; }
body { font-family: system-ui, -apple-system, \"Segoe UI\", Roboto, sans-serif; line-height: normal; }
.vx-flex { display: flex; align-items: flex-start; }
.vx-grid { display: grid; grid-template-columns: minmax(0, 1fr); align-items: stretch; }
.vx-scroll { display: flex; flex-direction: column; align-items: stretch; overflow: auto; }
.vx-text { white-space: pre; }
.vx-button { font: inherit; color: inherit; background: none; border: 0; border-radius: 0; padding: 0; margin: 0; text-align: start; white-space: pre; cursor: pointer; }
.vx-input { font: inherit; color: inherit; background: transparent; border: 0; padding: 0; margin: 0; }
.vx-check, .vx-range { margin: 0; accent-color: var(--colors-primary); }
.vx-image { display: block; object-fit: contain; }
.vx-mask { display: block; mask: center / contain no-repeat; -webkit-mask: center / contain no-repeat; }
";

fn styles(page: &html::Lowered) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "/* {GENERATED} */");
    out.push_str(&css::theme_rule(&viso_view::default_theme()));
    if page.device {
        out.push_str(":root { --viso-px: 1px; }\n");
        out.push_str("@media (resolution >= 2dppx) { :root { --viso-px: 0.5px; } }\n");
        out.push_str("@media (resolution >= 3dppx) { :root { --viso-px: 0.3333px; } }\n");
    }
    out.push_str(BASE);
    let _ = writeln!(
        out,
        "body {{ font-size: {}; color: {}; }}",
        html::FONT_SIZE,
        html::TEXT_COLOR
    );
    page.sheet.write(&mut out);
    out
}
