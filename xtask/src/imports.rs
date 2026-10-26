//! The module-level import rules `check-deps` holds below the crate edges:
//! which modules of the platform, UI, text, render, GPU and DSL crates may
//! name what.
//!
//! Each source file is parsed and every path it names, in a `use` or inline,
//! is resolved against its module (`crate::`, `self::`, `super::`), test
//! code (`#[cfg(test)]` and `#[test]` items) aside. A path is forbidden in a
//! module when a [`RULES`] entry for the module names a prefix of it, or
//! when it names a crate its crate must never depend on (the `NEVER`
//! table), which a stray `extern crate` or a path through a re-export would
//! otherwise reach.

use std::path::{Path, PathBuf};

use syn::visit::Visit;

/// One rule: in the modules of the crate at `dir` whose paths start with one
/// of `modules` (`""` is every module), no path may start with one of
/// `forbidden`.
pub(crate) struct ModuleRule {
    pub(crate) dir: &'static str,
    pub(crate) modules: &'static [&'static str],
    pub(crate) forbidden: &'static [&'static str],
    pub(crate) why: &'static str,
}

/// What an OS gives that a microcontroller has none of: time comes in
/// through an injected clock, concurrency and wakeups through the runtime.
const OS: &[&str] = &["std::thread", "std::time", "std::fs", "std::sync::Mutex"];

/// The DSL compiler's later stages, which no earlier stage may reach.
const AFTER_AST: &[&str] = &[
    "crate::resolve",
    "crate::hir",
    "crate::ir",
    "crate::behavior",
    "crate::aot",
    "crate::export",
    "crate::hotreload",
    "crate::dump",
    "crate::frontend",
];

/// What consumes the DSL compiler's IR: the generated code, the exports,
/// hot reload's diffs and the dumps.
const AFTER_IR: &[&str] = &[
    "crate::aot",
    "crate::export",
    "crate::hotreload",
    "crate::dump",
    "crate::frontend",
];

/// The GPU backends, each the one backend of its build.
const BACKENDS: &[&str] = &[
    "crate::metal",
    "crate::vulkan",
    "crate::d3d12",
    "crate::gles",
    "crate::webgpu",
];

pub(crate) const RULES: &[ModuleRule] = &[
    ModuleRule {
        dir: "crates/ui",
        modules: &[
            "layout",
            "reactive",
            "binding",
            "flush",
            "dirty",
            "structure",
        ],
        forbidden: OS,
        why: "the arena, reactive and layout modules stay MCU-ready",
    },
    ModuleRule {
        dir: "crates/text",
        modules: &["shaping", "segment", "line_break", "bidi", "paragraph"],
        forbidden: OS,
        why: "shaping and segmentation stay MCU-ready",
    },
    ModuleRule {
        dir: "crates/ui",
        modules: &[
            "layout",
            "paint",
            "hit_test",
            "reactive",
            "flush",
            "transform",
        ],
        forbidden: &["viso_ende"],
        why: "the frame's hot path passes node, layout and paint data without encoding it",
    },
    ModuleRule {
        dir: "crates/render",
        modules: &["batch", "renderer", "frame", "damage", "cull", "clip"],
        forbidden: &["viso_ende"],
        why: "the frame's hot path passes node, layout and paint data without encoding it",
    },
    ModuleRule {
        dir: "crates/dsl",
        modules: &["syntax"],
        forbidden: &[
            "crate::ast",
            "crate::resolve",
            "crate::hir",
            "crate::ir",
            "crate::behavior",
            "crate::aot",
            "crate::export",
            "crate::hotreload",
            "crate::dump",
            "crate::frontend",
        ],
        why: "the lossless syntax tree knows no later stage",
    },
    ModuleRule {
        dir: "crates/dsl",
        modules: &["ast"],
        forbidden: AFTER_AST,
        why: "the AST knows no later stage",
    },
    // Resolution reads the HIR's types, and the HIR walk lowers behavior and
    // views to their IR as it checks them: neither reaches what consumes the
    // IR.
    ModuleRule {
        dir: "crates/dsl",
        modules: &["resolve", "hir"],
        forbidden: AFTER_IR,
        why: "resolution and the typed HIR know nothing that consumes the IR",
    },
    ModuleRule {
        dir: "crates/gpu",
        modules: &["backend", "retire", "slots", "cpu"],
        forbidden: BACKENDS,
        why: "the backend contract and the software raster name no native backend",
    },
];

/// The crates the scan covers, by directory and crate name.
pub(crate) const SCANNED: &[(&str, &str)] = &[
    ("crates/platform", "viso-platform"),
    ("crates/ui", "viso-ui"),
    ("crates/text", "viso-text"),
    ("crates/render", "viso-render"),
    ("crates/gpu", "viso-gpu"),
    ("crates/dsl", "viso-dsl"),
];

/// Every forbidden path of the scanned crates under `root`, each as
/// `file:line: path in module (why)`.
pub(crate) fn scan(root: &Path, never: &[(&str, &str)]) -> Result<Vec<String>, String> {
    let mut found = Vec::new();
    for &(dir, name) in SCANNED {
        let crates: Vec<String> = never
            .iter()
            .filter(|(from, _)| *from == name)
            .map(|(_, to)| to.replace('-', "_"))
            .collect();
        let src = root.join(dir).join("src");
        for file in sources(&src)? {
            let module = module_of(&src, &file);
            let text = std::fs::read_to_string(&file)
                .map_err(|e| format!("cannot read {}: {e}", file.display()))?;
            let parsed = syn::parse_file(&text)
                .map_err(|e| format!("cannot parse {}: {e}", file.display()))?;
            let mut paths = Paths {
                module: module.clone(),
                found: Vec::new(),
            };
            paths.visit_file(&parsed);
            let shown = file
                .strip_prefix(root)
                .unwrap_or(&file)
                .display()
                .to_string();
            for (path, in_module, line) in paths.found {
                if let Some(why) = forbidden(dir, &in_module, &path, &crates) {
                    found.push(format!(
                        "{shown}:{line}: `{path}` in `{}` ({why})",
                        shown_module(&in_module)
                    ));
                }
            }
        }
    }
    Ok(found)
}

/// Why `path` is forbidden in `module` of the crate at `dir`, whose `NEVER`
/// targets are `crates`; `None` when it is not.
fn forbidden(dir: &str, module: &[String], path: &str, crates: &[String]) -> Option<String> {
    if crates.iter().any(|c| starts(path, c)) {
        return Some("a crate it must never depend on".to_owned());
    }
    let module = module.join("::");
    RULES
        .iter()
        .filter(|rule| rule.dir == dir)
        .filter(|rule| {
            rule.modules
                .iter()
                .any(|m| m.is_empty() || starts(&module, m))
        })
        .find(|rule| rule.forbidden.iter().any(|f| starts(path, f)))
        .map(|rule| rule.why.to_owned())
}

/// Whether `path` is `prefix` or lies under it.
fn starts(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
}

fn shown_module(module: &[String]) -> String {
    if module.is_empty() {
        "crate".to_owned()
    } else {
        module.join("::")
    }
}

/// The `.rs` files under `dir`, in order.
fn sources(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    let mut dirs = vec![dir.to_owned()];
    while let Some(dir) = dirs.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// The module a file under `src` is: `lib.rs` the crate's, `a/mod.rs` and
/// `a.rs` the module `a`, `a/b.rs` the module `a::b`.
fn module_of(src: &Path, file: &Path) -> Vec<String> {
    let relative = file.strip_prefix(src).unwrap_or(file).with_extension("");
    let mut module: Vec<String> = relative
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if module
        .last()
        .is_some_and(|last| last == "mod" || last == "lib" || last == "main")
    {
        module.pop();
    }
    module
}

/// The paths a file names, each resolved against its module, with the
/// module it is named in and its line.
struct Paths {
    module: Vec<String>,
    found: Vec<(String, Vec<String>, usize)>,
}

impl Paths {
    /// `segments` named in the current module, resolved.
    fn add(&mut self, segments: &[String], line: usize) {
        let Some(path) = resolve(&self.module, segments) else {
            return;
        };
        self.found.push((path, self.module.clone(), line));
    }

    fn use_tree(&mut self, prefix: &mut Vec<String>, tree: &syn::UseTree, line: usize) {
        match tree {
            syn::UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                self.use_tree(prefix, &p.tree, line);
                prefix.pop();
            }
            syn::UseTree::Name(n) => {
                let name = n.ident.to_string();
                if name == "self" {
                    self.add(prefix, line);
                } else {
                    prefix.push(name);
                    self.add(prefix, line);
                    prefix.pop();
                }
            }
            syn::UseTree::Rename(r) => {
                prefix.push(r.ident.to_string());
                self.add(prefix, line);
                prefix.pop();
            }
            syn::UseTree::Glob(_) => self.add(prefix, line),
            syn::UseTree::Group(g) => {
                for tree in &g.items {
                    self.use_tree(prefix, tree, line);
                }
            }
        }
    }
}

/// `segments` named in `module` as a path from its crate (`crate::…`) or
/// from another crate; `None` for a single local name.
fn resolve(module: &[String], segments: &[String]) -> Option<String> {
    let (first, rest) = segments.split_first()?;
    let mut base: Vec<String> = match first.as_str() {
        "crate" => Vec::new(),
        "self" => module.to_vec(),
        "super" => {
            let mut up = module.to_vec();
            up.pop();
            let mut rest = rest;
            while rest.first().is_some_and(|s| s == "super") {
                up.pop();
                rest = &rest[1..];
            }
            let mut path = vec!["crate".to_owned()];
            path.extend(up);
            path.extend(rest.iter().cloned());
            return Some(path.join("::"));
        }
        _ if rest.is_empty() => return None,
        _ => return Some(segments.join("::")),
    };
    base.extend(rest.iter().cloned());
    let mut path = vec!["crate".to_owned()];
    path.extend(base);
    Some(path.join("::"))
}

/// Whether `attrs` mark test code: `#[cfg(test)]` or `#[test]`.
fn is_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if attr.path().is_ident("test") {
            return true;
        }
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Meta>()
                .is_ok_and(|meta| meta.path().is_ident("test"))
    })
}

impl<'ast> Visit<'ast> for Paths {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let attrs = match item {
            syn::Item::Fn(f) => &f.attrs,
            syn::Item::Mod(m) => &m.attrs,
            syn::Item::Impl(i) => &i.attrs,
            syn::Item::Use(u) => &u.attrs,
            syn::Item::Const(c) => &c.attrs,
            syn::Item::Static(s) => &s.attrs,
            syn::Item::Struct(s) => &s.attrs,
            syn::Item::Enum(e) => &e.attrs,
            syn::Item::Trait(t) => &t.attrs,
            _ => return syn::visit::visit_item(self, item),
        };
        if !is_test(attrs) {
            syn::visit::visit_item(self, item);
        }
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        // An out-of-line module is its own file.
        if item.content.is_some() {
            self.module.push(item.ident.to_string());
            syn::visit::visit_item_mod(self, item);
            self.module.pop();
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        let line = item.use_token.span.start().line;
        self.use_tree(&mut Vec::new(), &item.tree, line);
    }

    fn visit_path(&mut self, path: &'ast syn::Path) {
        let segments: Vec<String> = path.segments.iter().map(|s| s.ident.to_string()).collect();
        let line = path
            .segments
            .first()
            .map_or(0, |s| s.ident.span().start().line);
        self.add(&segments, line);
        syn::visit::visit_path(self, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(module: &[&str], source: &str) -> Vec<String> {
        let mut paths = Paths {
            module: module.iter().map(|s| (*s).to_owned()).collect(),
            found: Vec::new(),
        };
        paths.visit_file(&syn::parse_file(source).unwrap());
        paths.found.into_iter().map(|(path, _, _)| path).collect()
    }

    #[test]
    fn paths_resolve_against_their_module_and_tests_are_left_out() {
        let found = named(
            &["layout", "flex"],
            "use std::{thread, time::Instant};\n\
             use super::grid::Track;\n\
             use self::inner::*;\n\
             fn f() { let _ = viso_ende::Encoder::new(); std::fs::read(\"x\"); }\n\
             #[cfg(test)] mod tests { use std::sync::Mutex; }\n\
             #[test] fn t() { std::thread::sleep(); }\n",
        );
        assert!(found.contains(&"std::thread".to_owned()), "{found:?}");
        assert!(
            found.contains(&"std::time::Instant".to_owned()),
            "{found:?}"
        );
        assert!(
            found.contains(&"crate::layout::grid::Track".to_owned()),
            "{found:?}"
        );
        assert!(
            found.contains(&"crate::layout::flex::inner".to_owned()),
            "{found:?}"
        );
        assert!(
            found.contains(&"viso_ende::Encoder::new".to_owned()),
            "{found:?}"
        );
        assert!(found.contains(&"std::fs::read".to_owned()), "{found:?}");
        assert!(!found.iter().any(|p| p.contains("Mutex")), "{found:?}");
        assert_eq!(
            found
                .iter()
                .filter(|p| p.starts_with("std::thread"))
                .count(),
            1
        );
    }

    #[test]
    fn a_module_rule_and_a_never_crate_are_forbidden() {
        let module = |m: &str| m.split("::").map(str::to_owned).collect::<Vec<_>>();
        let never = ["viso_widgets".to_owned()];
        assert!(
            forbidden(
                "crates/ui",
                &module("layout::flex"),
                "std::time::Instant",
                &never
            )
            .is_some()
        );
        assert!(forbidden("crates/ui", &module("window"), "std::time::Instant", &never).is_none());
        assert!(
            forbidden(
                "crates/ui",
                &module("window"),
                "viso_widgets::Button",
                &never
            )
            .is_some()
        );
        assert!(forbidden("crates/ui", &module("window"), "viso_widgets_extra", &never).is_none());
        assert!(forbidden("crates/dsl", &module("syntax::cst"), "crate::hir::Ty", &[]).is_some());
        assert!(forbidden("crates/dsl", &module("hir"), "crate::syntax::Node", &[]).is_none());
        assert!(
            forbidden(
                "crates/dsl",
                &module("hir::view"),
                "crate::behavior::Site",
                &[]
            )
            .is_none()
        );
        assert!(
            forbidden(
                "crates/dsl",
                &module("hir::view"),
                "crate::hotreload::Plan",
                &[]
            )
            .is_some()
        );
        assert!(
            forbidden(
                "crates/gpu",
                &module("backend"),
                "crate::metal::Device",
                &[]
            )
            .is_some()
        );
    }

    #[test]
    fn a_file_is_its_module() {
        let src = Path::new("/w/src");
        assert!(module_of(src, Path::new("/w/src/lib.rs")).is_empty());
        assert_eq!(module_of(src, Path::new("/w/src/layout.rs")), ["layout"]);
        assert_eq!(module_of(src, Path::new("/w/src/paint/mod.rs")), ["paint"]);
        assert_eq!(
            module_of(src, Path::new("/w/src/paint/glyph.rs")),
            ["paint", "glyph"]
        );
    }
}
