//! The Behavior IR a file compiles to: each component's slots, the native
//! imports and every function, its instructions with the span of each.

use viso_ende::JsonWriter;

use super::{Dump, FileCx, strings};
use crate::behavior::inst_text;
use crate::behavior::ir::{ComponentLayout, Function, Program};
use crate::frontend::Compiled;

/// The program, and the part of it the selection keeps.
pub(super) struct BehaviorFile {
    program: Program,
    /// The component the selection names, and its member.
    focus: Option<(String, Option<String>)>,
    /// Each function's position, by function and instruction.
    spans: Vec<Vec<(u32, u32)>>,
    unsupported: Vec<Option<(u32, u32)>>,
}

pub(super) fn program(cx: &FileCx<'_>, compiled: &Compiled) -> BehaviorFile {
    let program = compiled.behavior.clone();
    let spans = program
        .functions
        .iter()
        .map(|f| match &f.body {
            Ok(body) => body.spans.iter().map(|s| cx.position(*s)).collect(),
            Err(_) => Vec::new(),
        })
        .collect();
    let unsupported = program
        .functions
        .iter()
        .map(|f| f.body.as_ref().err().map(|u| cx.position(u.at)))
        .collect();
    BehaviorFile {
        program,
        focus: cx
            .focus
            .as_ref()
            .map(|focus| (focus.name.clone(), focus.member.clone())),
        spans,
        unsupported,
    }
}

impl BehaviorFile {
    fn keeps_component(&self, layout: &ComponentLayout) -> bool {
        self.focus
            .as_ref()
            .is_none_or(|(name, _)| &layout.name == name)
    }

    fn keeps_function(&self, function: &Function) -> bool {
        let Some((name, member)) = &self.focus else {
            return true;
        };
        let Some(rest) = function.name.strip_prefix(name.as_str()) else {
            return false;
        };
        match member {
            Some(member) => rest.strip_prefix('.') == Some(member.as_str()),
            None => rest.is_empty() || rest.starts_with('.'),
        }
    }
}

impl Dump for BehaviorFile {
    fn write_text(&self, out: &mut String) {
        self.program
            .write_dump(out, |c| self.keeps_component(c), |f| self.keeps_function(f));
    }

    fn write_json(&self, w: &mut JsonWriter) {
        let program = &self.program;
        w.begin_object();
        w.name("components");
        w.begin_array();
        for layout in program
            .components
            .iter()
            .filter(|c| self.keeps_component(c))
        {
            w.begin_object();
            w.name("name");
            w.string(&layout.name);
            w.name("symbol_id");
            w.string(&layout.symbol.to_string());
            w.name("states");
            strings(w, layout.states.iter().map(String::as_str));
            w.name("inputs");
            strings(w, layout.inputs.iter().map(String::as_str));
            w.name("events");
            strings(w, layout.events.iter().map(String::as_str));
            w.name("members");
            w.begin_array();
            for (name, func) in &layout.members {
                w.begin_object();
                w.name("name");
                w.string(name);
                w.name("function");
                w.uint(u64::from(func.0));
                w.end_object();
            }
            w.end_array();
            w.end_object();
        }
        w.end_array();
        w.name("natives");
        w.begin_array();
        for native in &program.natives {
            w.begin_object();
            w.name("path");
            w.string(&native.path);
            w.name("params");
            w.uint(u64::from(native.params));
            w.name("signature");
            w.string(&format!("{:016x}", native.signature));
            w.end_object();
        }
        w.end_array();
        w.name("functions");
        w.begin_array();
        for (i, function) in program.functions.iter().enumerate() {
            if !self.keeps_function(function) {
                continue;
            }
            w.begin_object();
            w.name("index");
            w.uint(i as u64);
            w.name("name");
            w.string(&function.name);
            w.name("kind");
            w.string(function.kind.name());
            w.name("symbol_id");
            match function.symbol {
                Some(symbol) => w.string(&symbol.to_string()),
                None => w.null(),
            }
            w.name("params");
            w.uint(u64::from(function.params));
            w.name("captures");
            w.begin_array();
            for capture in &function.captures {
                w.uint(u64::from(capture.0));
            }
            w.end_array();
            match &function.body {
                Ok(body) => {
                    w.name("regs");
                    w.uint(u64::from(body.regs));
                    w.name("insts");
                    w.begin_array();
                    for (j, inst) in body.insts.iter().enumerate() {
                        w.begin_object();
                        w.name("text");
                        w.string(&inst_text(inst));
                        if let (Some(range), Some(at)) = (body.spans.get(j), self.spans[i].get(j)) {
                            super::hir::span(w, *range, *at);
                        }
                        w.end_object();
                    }
                    w.end_array();
                }
                Err(unsupported) => {
                    w.name("unsupported");
                    w.begin_object();
                    w.name("reason");
                    w.string(&unsupported.reason);
                    if let Some(at) = self.unsupported[i] {
                        super::hir::span(w, unsupported.at, at);
                    }
                    w.end_object();
                }
            }
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
}
