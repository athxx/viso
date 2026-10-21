//! One-way exports of a compiled view into source another ecosystem owns:
//! an HTML page, a SolidJS project.
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
mod solid;

use std::fmt::Write as _;
use std::rc::Rc;

use crate::frontend::Compiled;
use crate::syntax::TextRange;
use crate::view_behavior::{ViewBehavior, view_behavior};

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

/// How an export is made.
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
    let mut prepared = prepare(compiled, &options.lang)?;
    // A page with a script follows its reader's language; a static one is
    // written in the source's.
    if !options.static_only {
        prepared.pick_locale();
    }
    let layout = prepared.eval.as_ref().map(|eval| eval.layout);
    let mut lower = html::Lower::new(
        prepared.vb.as_ref(),
        prepared.eval.as_mut(),
        &compiled.assets,
        options.static_only,
    );
    lower.view(&compiled.tree);
    if let Some(layout) = layout
        && options.static_only
        && let Some(effect) = layout.effects.first()
    {
        let at = layout.handlers[effect.body as usize].0.at;
        let what = if effect.resource.is_some() {
            "loads a `resource`"
        } else {
            "runs an `effect`"
        };
        lower.findings.push(Finding {
            need: Need::Dynamic,
            at,
            message: format!("the view {what}, which only the page script runs"),
            notes: vec!["a page without JavaScript runs nothing as it opens".to_owned()],
        });
    }
    let effects = layout.is_some_and(|layout| !layout.effects.is_empty());
    let scripted = effects || lower.out.root.marks.iter().any(|m| !m.wires.is_empty());
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
    let script = match (&prepared.eval, scripted) {
        (Some(eval), true) => {
            let catalog = prepared.vb.as_ref().and_then(|vb| vb.module.catalog());
            Some(script::script(&page, eval, catalog, GENERATED, prepared.origin).map_err(sorted)?)
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

/// How a SolidJS export is made.
#[derive(Debug, Clone)]
pub struct SolidOptions {
    /// The npm package name of the project.
    pub package: String,
    /// Its version.
    pub version: String,
    /// The page's title.
    pub title: String,
    /// The language of its text, as a BCP 47 tag.
    pub lang: String,
}

/// A SolidJS export: a Vite project whose component is the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SolidExport {
    /// Each file by its path in the project, in path order.
    pub files: Vec<(String, String)>,
    /// The path under the project's `assets/` of each asset the component
    /// shows, written to the same path under the export's `src/assets/`.
    pub assets: Vec<String>,
}

/// The view `compiled` mounts as a SolidJS component in a project of its own.
///
/// # Errors
///
/// Every place the view needs what the component cannot have, in source
/// order.
pub fn solid(compiled: &Compiled, options: &SolidOptions) -> Result<SolidExport, Vec<Finding>> {
    let mut prepared = prepare(compiled, &options.lang)?;
    prepared.pick_locale();
    if let Some(eval) = prepared.eval.as_mut() {
        eval.props = true;
    }
    let mut lower = html::Lower::new(
        prepared.vb.as_ref(),
        prepared.eval.as_mut(),
        &compiled.assets,
        false,
    );
    lower.view(&compiled.tree);
    lower.all_assets();
    let html::Lower {
        findings,
        out: page,
        ..
    } = lower;
    if !findings.is_empty() {
        return Err(sorted(findings));
    }
    let catalog = prepared.vb.as_ref().and_then(|vb| vb.module.catalog());
    let files = solid::project(&solid::Input {
        page: &page,
        eval: prepared.eval.as_ref(),
        catalog,
        component: compiled.component.as_ref(),
        styles: &styles(&page),
        options,
        origin: prepared.origin,
    })
    .map_err(sorted)?;
    Ok(SolidExport {
        files,
        assets: page.assets.values().cloned().collect(),
    })
}

/// What every export reads of the view: its behavior table and the
/// interpreter over its component, once nothing it cannot run is found.
struct Prepared<'a> {
    vb: Option<ViewBehavior>,
    eval: Option<eval::Eval<'a>>,
    origin: TextRange,
}

impl Prepared<'_> {
    /// Has the page pick its locale as it loads, when the catalog has more
    /// than the source's.
    fn pick_locale(&mut self) {
        let locales = self
            .vb
            .as_ref()
            .and_then(|vb| vb.module.catalog())
            .map_or(0, |catalog| catalog.locales().count());
        if locales > 1
            && let Some(eval) = self.eval.as_mut()
        {
            eval.pick_locale();
        }
    }
}

fn prepare<'a>(compiled: &'a Compiled, lang: &str) -> Result<Prepared<'a>, Vec<Finding>> {
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
    if layout.is_some() && !compiled.behavior.systems.is_empty() {
        findings.push(Finding {
            need: Need::Unsupported,
            at: origin,
            message: "the view runs a game's systems, which only the Viso runtime schedules"
                .to_owned(),
            notes: vec!["build for the browser instead: `viso build web-gpu`".to_owned()],
        });
    }
    let eval = match (layout, &vb) {
        (Some(layout), Some(vb)) => {
            match eval::Eval::new(
                Rc::clone(&vb.module),
                &compiled.behavior,
                layout,
                lang,
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
    Ok(Prepared { vb, eval, origin })
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
    html::write_html(&page.root.nodes, &page.regions, &mut out);
    out.push_str("\n</body>\n</html>\n");
    out
}

/// The base rules every element class reads, then the theme, then the
/// page's own.
const BASE: &str = "\
*, *::before, *::after { box-sizing: border-box; }
html, body { margin: 0; width: 100%; height: 100%; }
body { font-family: var(--typography-family); font-size: var(--typography-base-size); line-height: normal; }
.vx-flex { display: flex; align-items: flex-start; }
.vx-grid { display: grid; grid-template-columns: minmax(0, 1fr); align-items: stretch; }
.vx-stack { display: grid; grid-template: minmax(0, 1fr) / minmax(0, 1fr); }
.vx-absolute { position: relative; }
.vx-scroll { display: flex; flex-direction: column; align-items: stretch; overflow: auto; }
.vx-list > * { content-visibility: auto; contain-intrinsic-block-size: auto var(--vx-row, 30px); }
.vx-list.vx-across > * { contain-intrinsic-block-size: none; contain-intrinsic-inline-size: auto var(--vx-row, 30px); }
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
    out.push_str(&css::theme_rule(&viso_behavior::theme::default_theme()));
    if page.device {
        out.push_str(":root { --viso-px: 1px; }\n");
        out.push_str("@media (resolution >= 2dppx) { :root { --viso-px: 0.5px; } }\n");
        out.push_str("@media (resolution >= 3dppx) { :root { --viso-px: 0.3333px; } }\n");
    }
    out.push_str(BASE);
    let _ = writeln!(out, "body {{ color: {}; }}", html::TEXT_COLOR);
    page.sheet.write(&mut out);
    out
}

/// The names `runtime`'s top level declares more than once, which a module
/// refuses to load.
#[cfg(test)]
fn redeclared(runtime: &str) -> Vec<&str> {
    let mut seen = std::collections::HashSet::new();
    runtime
        .lines()
        .filter_map(|line| {
            let line = line.strip_prefix("export ").unwrap_or(line);
            ["const ", "let ", "function ", "class "]
                .iter()
                .find_map(|keyword| line.strip_prefix(keyword))
        })
        .filter_map(|rest| {
            rest.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
                .next()
        })
        .filter(|name| !seen.insert(*name))
        .collect()
}
