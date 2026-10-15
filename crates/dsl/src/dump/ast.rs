//! The syntax tree of a file: each node with its kind and span, each token
//! with its kind, text and span, trivia left out.

use std::fmt::Write as _;

use viso_ende::JsonWriter;

use super::{Dump, FileCx};
use crate::syntax::{SyntaxElement, SyntaxNode, TextRange};

/// A node or a token of the tree.
enum Element {
    Node {
        kind: &'static str,
        range: TextRange,
        at: (u32, u32),
        children: Vec<Element>,
    },
    Token {
        kind: &'static str,
        text: String,
        range: TextRange,
        at: (u32, u32),
    },
}

/// A file's tree, or the subtree of the declaration the selection names.
pub(super) struct Tree {
    root: Element,
}

pub(super) fn tree(cx: &FileCx<'_>) -> Tree {
    let root = cx
        .focus
        .as_ref()
        .and_then(|focus| {
            cx.root
                .descendants()
                .into_iter()
                .find(|node| node.text_range() == focus.range)
        })
        .unwrap_or_else(|| cx.root.clone());
    Tree {
        root: node(cx, &root),
    }
}

fn node(cx: &FileCx<'_>, node: &SyntaxNode) -> Element {
    let children = node
        .children_with_tokens()
        .into_iter()
        .filter(|e| !e.kind().is_trivia())
        .map(|e| match e {
            SyntaxElement::Node(child) => self::node(cx, &child),
            SyntaxElement::Token(token) => Element::Token {
                kind: token.kind().name(),
                text: token.text(),
                range: token.text_range(),
                at: cx.position(token.text_range()),
            },
        })
        .collect();
    Element::Node {
        kind: node.kind().name(),
        range: node.text_range(),
        at: cx.position(node.text_range()),
        children,
    }
}

impl Dump for Tree {
    fn write_text(&self, out: &mut String) {
        text(&self.root, 0, out);
    }

    fn write_json(&self, w: &mut JsonWriter) {
        json(&self.root, w);
    }
}

fn text(element: &Element, depth: usize, out: &mut String) {
    let indent = "  ".repeat(depth);
    match element {
        Element::Node {
            kind,
            range,
            at,
            children,
        } => {
            let _ = writeln!(
                out,
                "{indent}{kind} {}..{} @{}:{}",
                range.start().to_u32(),
                range.end().to_u32(),
                at.0,
                at.1
            );
            for child in children {
                self::text(child, depth + 1, out);
            }
        }
        Element::Token {
            kind,
            text,
            range,
            at,
        } => {
            let _ = writeln!(
                out,
                "{indent}{kind} {} {}..{} @{}:{}",
                super::quote(text),
                range.start().to_u32(),
                range.end().to_u32(),
                at.0,
                at.1
            );
        }
    }
}

fn span(w: &mut JsonWriter, range: TextRange, at: (u32, u32)) {
    w.name("span");
    w.begin_object();
    w.name("start");
    w.uint(u64::from(range.start().to_u32()));
    w.name("end");
    w.uint(u64::from(range.end().to_u32()));
    w.name("line");
    w.uint(u64::from(at.0));
    w.name("column_utf16");
    w.uint(u64::from(at.1));
    w.end_object();
}

fn json(element: &Element, w: &mut JsonWriter) {
    w.begin_object();
    match element {
        Element::Node {
            kind,
            range,
            at,
            children,
        } => {
            w.name("node");
            w.string(kind);
            span(w, *range, *at);
            w.name("children");
            w.begin_array();
            for child in children {
                json(child, w);
            }
            w.end_array();
        }
        Element::Token {
            kind,
            text,
            range,
            at,
        } => {
            w.name("token");
            w.string(kind);
            w.name("text");
            w.string(text);
            span(w, *range, *at);
        }
    }
    w.end_object();
}
