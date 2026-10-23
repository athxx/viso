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
.vx-snap:not(.vx-list) > * > *, .vx-list.vx-snap > * { scroll-snap-align: var(--vx-snap); scroll-snap-stop: var(--vx-snap-stop, normal); }
.vx-list.vx-pages > * { flex: none; block-size: 100%; }
.vx-list.vx-pages.vx-across > * { block-size: auto; inline-size: 100%; }
.vx-lanes > [style*=\"sticky\"] { grid-column: 1 / -1; }
.vx-lanes.vx-across > [style*=\"sticky\"] { grid-column: auto; grid-row: 1 / -1; }
[data-vx-away] { display: none !important; }
summary { list-style: none; }
summary::-webkit-details-marker { display: none; }
.vx-chevron::before { content: \"\"; display: inline-block; width: 0.42em; height: 0.42em; margin-inline: 0.3em 0.5em; vertical-align: 0.12em; border: solid currentColor; border-width: 0.12em 0.12em 0 0; transform: rotate(45deg); transition: transform 150ms cubic-bezier(0.33, 1, 0.68, 1); }
details[open] > .vx-chevron::before { transform: rotate(135deg); }
.vx-chevron:dir(rtl)::before { transform: rotate(-135deg); }
details[open] > .vx-chevron:dir(rtl)::before { transform: rotate(-225deg); }
.vx-radio::before { content: \"\"; display: inline-block; box-sizing: border-box; width: 1em; height: 1em; margin-inline-end: 0.5em; vertical-align: -0.15em; border-radius: 50%; border: 0.09em solid color-mix(in srgb, currentColor 60%, transparent); box-shadow: inset 0 0 0 0 currentColor; transition: border-color 150ms cubic-bezier(0.33, 1, 0.68, 1), box-shadow 150ms cubic-bezier(0.33, 1, 0.68, 1); }
.vx-radio[aria-checked=\"true\"]::before { border-color: currentColor; box-shadow: inset 0 0 0 0.21em currentColor; }
@media (prefers-reduced-motion: reduce) { .vx-chevron::before, .vx-radio::before { transition: none; } }
.vx-text { white-space: pre; user-select: none; -webkit-user-select: none; }
.vx-text.vx-selectable { user-select: text; -webkit-user-select: text; cursor: text; }
.vx-button { font: inherit; color: inherit; background: none; border: 0; border-radius: 0; padding: 0; margin: 0; text-align: start; white-space: pre; cursor: pointer; }
.vx-input { font: inherit; color: inherit; background: transparent; border: 0; padding: 0; margin: 0; }
.vx-check, .vx-range { margin: 0; accent-color: var(--vx-ink, var(--colors-primary)); }
.vx-vertical { writing-mode: vertical-lr; direction: rtl; }
.vx-knob { appearance: none; -webkit-appearance: none; width: 40px; height: 40px; border-radius: 50%; cursor: grab; background: radial-gradient(circle, #fff 0 52%, transparent 53%), conic-gradient(from 225deg, var(--vx-ink, var(--colors-primary)) calc(var(--vx-at, 0) * 270deg), color-mix(in srgb, currentColor 25%, transparent) 0 270deg, transparent 0); }
.vx-knob::-webkit-slider-thumb { -webkit-appearance: none; width: 0; height: 0; }
.vx-knob::-moz-range-thumb { width: 0; height: 0; border: 0; }
.vx-image { display: block; object-fit: contain; }
.vx-layer { position: fixed; inset: 0; width: auto; height: auto; max-width: none; max-height: none; margin: 0; padding: 0; border: 0; color: inherit; z-index: 1; }
:where(dialog.vx-layer) { background: rgba(0, 0, 0, 0.32); }
.vx-layer::backdrop { background: transparent; }
dialog.vx-layer:not([open]), .vx-layer[hidden] { display: none; }
.vx-toast { pointer-events: none; }
.vx-toast > * { pointer-events: auto; }
.vx-mask { display: block; mask: center / contain no-repeat; -webkit-mask: center / contain no-repeat; }
.vx-link { color: #0066cc; text-decoration: underline; cursor: pointer; }
.vx-chip { display: inline-flex; align-items: center; box-sizing: border-box; height: 32px; padding: 0 12px; border: 1px solid color-mix(in srgb, currentColor 40%, transparent); border-radius: 8px; background-color: transparent; color: inherit; font: inherit; font-weight: 500; line-height: 1; white-space: nowrap; cursor: pointer; transition: background-color 150ms cubic-bezier(0.33, 1, 0.68, 1), border-color 150ms cubic-bezier(0.33, 1, 0.68, 1); }
.vx-chip[aria-pressed]::before { content: \"\\2713\"; flex: none; width: 0; margin-inline: 0; overflow: hidden; text-align: center; transition: width 150ms cubic-bezier(0.33, 1, 0.68, 1), margin 150ms cubic-bezier(0.33, 1, 0.68, 1); }
.vx-chip[aria-pressed=\"true\"] { border-color: transparent; background-color: color-mix(in srgb, currentColor 16%, transparent); }
.vx-chip[aria-pressed=\"true\"]::before { width: 18px; margin-inline: -4px 8px; }
.vx-chip[data-removable=\"true\"]::after { content: \"\\00d7\"; flex: none; width: 18px; margin-inline: 8px -4px; font-size: 18px; text-align: center; opacity: 0.8; }
@media (prefers-reduced-motion: reduce) { .vx-chip, .vx-chip::before { transition: none; } }
.vx-stepper { display: inline-flex; box-sizing: border-box; width: 96px; height: 32px; border-radius: 8px; background-color: color-mix(in srgb, currentColor 8%, transparent); user-select: none; -webkit-user-select: none; touch-action: manipulation; }
.vx-step { flex: 1; display: flex; align-items: center; justify-content: center; margin: 2px; padding: 0; border: 0; border-radius: 6px; background-color: transparent; color: inherit; font: inherit; font-size: 20px; line-height: 1; cursor: pointer; }
.vx-step + .vx-step { position: relative; }
.vx-step + .vx-step::before { content: \"\"; position: absolute; inset-inline-start: -2.5px; top: 25%; height: 50%; width: 1px; background-color: color-mix(in srgb, currentColor 25%, transparent); }
.vx-step:active:not(.vx-spent) { background-color: color-mix(in srgb, currentColor 14%, transparent); }
.vx-step.vx-spent { opacity: 0.3; cursor: default; }
.vx-segmented { position: relative; isolation: isolate; border-radius: 8px; background-color: color-mix(in srgb, var(--vx-ink, currentColor) 10%, transparent); user-select: none; -webkit-user-select: none; }
.vx-segmented::before { content: \"\"; position: absolute; z-index: -1; left: 0; top: 2px; bottom: 2px; width: var(--vx-thumb-w, 0px); transform: translateX(var(--vx-thumb-x, 0px)); border-radius: 6px; background-color: color-mix(in srgb, var(--vx-ink, currentColor) 24%, transparent); box-shadow: 0 1px 4px rgb(0 0 0 / 0.2); transition: transform 300ms cubic-bezier(0.25, 1, 0.5, 1), width 300ms cubic-bezier(0.25, 1, 0.5, 1); }
.vx-segment { display: flex; align-items: center; justify-content: center; box-sizing: border-box; min-height: 28px; padding: 5px 12px; text-align: center; cursor: pointer; background: linear-gradient(color-mix(in srgb, var(--vx-ink, currentColor) 20%, transparent), color-mix(in srgb, var(--vx-ink, currentColor) 20%, transparent)) left center / 1px 50% no-repeat; transition: background-size 150ms cubic-bezier(0.33, 1, 0.68, 1); }
.vx-segment:dir(rtl) { background-position: right center; }
.vx-segment:first-child, .vx-segment[aria-checked=\"true\"], .vx-segment[aria-checked=\"true\"] + .vx-segment { background-size: 0 50%; }
@media (prefers-reduced-motion: reduce) { .vx-segmented::before, .vx-segment { transition: none; } }
.vx-badge { display: inline-flex; align-items: center; justify-content: center; box-sizing: border-box; min-width: 1.4545em; height: 1.4545em; padding: 0 0.3636em; border-radius: 999px; background-color: #b3261e; color: #fff; font-weight: 500; line-height: 1; white-space: nowrap; }
.vx-badge.vx-dot { min-width: 8px; width: 8px; height: 8px; padding: 0; }
.vx-avatar { display: inline-flex; align-items: center; justify-content: center; overflow: hidden; border-radius: 50%; color: #fff; font-weight: 500; line-height: 1; background-size: cover; background-position: center; user-select: none; }
.vx-progress, .vx-ring, .vx-spinner { color: var(--colors-primary); }
.vx-progress { width: 160px; height: 4px; }
.vx-ring { width: 32px; height: 32px; }
.vx-spinner { width: 20px; height: 20px; }
.vx-progress { position: relative; overflow: hidden; border-radius: 9999px; background: color-mix(in srgb, currentColor 20%, transparent); }
.vx-progress::before { content: \"\"; position: absolute; inset-block: 0; inset-inline-start: 0; width: calc(var(--vx-at, 0) * 100%); border-radius: inherit; background: currentColor; transition: width 250ms cubic-bezier(0.33, 1, 0.68, 1); }
.vx-progress:not([aria-valuenow])::before { width: 40%; animation: vx-sweep 1.6s cubic-bezier(0.4, 0, 0.2, 1) infinite; }
.vx-progress:not([aria-valuenow]):dir(rtl)::before { animation-direction: reverse; }
.vx-ring, .vx-spinner { border-radius: 50%; mask: radial-gradient(farthest-side, transparent calc(80% - 0.5px), #000 80%); -webkit-mask: radial-gradient(farthest-side, transparent calc(80% - 0.5px), #000 80%); }
.vx-ring { background: conic-gradient(currentColor calc(var(--vx-at, 0) * 1turn), color-mix(in srgb, currentColor 20%, transparent) 0); transition: --vx-at 250ms cubic-bezier(0.33, 1, 0.68, 1); }
.vx-ring:not([aria-valuenow]) { background: conic-gradient(currentColor 0.27turn, color-mix(in srgb, currentColor 20%, transparent) 0); animation: vx-turn 1.2s linear infinite; }
.vx-spinner { background: conic-gradient(transparent 0.1turn, currentColor); animation: vx-turn 0.9s linear infinite; }
@property --vx-at { syntax: \"<number>\"; inherits: false; initial-value: 0; }
@keyframes vx-sweep { from { inset-inline-start: -40%; } to { inset-inline-start: 100%; } }
@keyframes vx-turn { to { transform: rotate(1turn); } }
@media (prefers-reduced-motion: reduce) { .vx-progress::before, .vx-ring { transition: none; } }
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
