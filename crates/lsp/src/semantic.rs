//! Semantic tokens: what each name of a document is, from the resolver where it
//! resolved a name and from the syntax tree where a name is a widget, a property, an
//! event or a label the resolver does not bind.
//!
//! An editor's own grammar guesses from the text; these tokens are the compiler's
//! answer, so a contextual keyword, a state read inside a handler or a native call
//! reads the same in every editor.

use std::collections::HashMap;

use viso_dsl::LineIndex;
use viso_dsl::resolve::{Resolution, SymbolId};
use viso_dsl::syntax::{SyntaxNode, SyntaxToken};
use viso_dsl::{SyntaxKind, TextRange};

use crate::position;
use crate::source_map::OpenDoc;

/// The token types, in the order of the legend the server advertises.
pub const TOKEN_TYPES: &[&str] = &[
    "namespace",
    "type",
    "class",
    "enum",
    "enumMember",
    "struct",
    "function",
    "method",
    "property",
    "event",
    "parameter",
    "variable",
    "keyword",
    "typeParameter",
    "decorator",
];

/// The token modifiers, in the order of the legend's bits.
pub const TOKEN_MODIFIERS: &[&str] = &["declaration", "readonly", "defaultLibrary"];

/// What a name is: an index into [`TOKEN_TYPES`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TokenType {
    Namespace,
    Type,
    Class,
    Enum,
    EnumMember,
    Struct,
    Function,
    Method,
    Property,
    Event,
    Parameter,
    Variable,
    Keyword,
    TypeParameter,
    Decorator,
}

/// The `declaration` modifier bit.
pub const DECLARATION: u32 = 1;
/// The `readonly` modifier bit.
pub const READONLY: u32 = 1 << 1;
/// The `defaultLibrary` modifier bit: a framework widget, native or type.
pub const DEFAULT_LIBRARY: u32 = 1 << 2;

/// One classified token: its byte range, type and modifier bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticToken {
    pub range: TextRange,
    pub ty: TokenType,
    pub modifiers: u32,
}

/// Every keyword and classified name of `doc`, in source order.
pub fn semantic_tokens(doc: &OpenDoc) -> Vec<SemanticToken> {
    let root = SyntaxNode::new_root(doc.parse.root.clone());
    let refs: HashMap<TextRange, Resolution> =
        doc.resolved.refs.iter().map(|r| (r.range, r.to)).collect();
    let decl_at: HashMap<TextRange, SymbolId> = doc
        .resolved
        .decls
        .iter()
        .map(|d| (d.name_range, d.id))
        .collect();
    let mut kinds: HashMap<SymbolId, (TokenType, u32)> = HashMap::new();
    let tokens: Vec<SyntaxToken> = root
        .descendants_with_tokens()
        .into_iter()
        .filter_map(|element| element.as_token().cloned())
        .filter(|token| !token.kind().is_trivia())
        .collect();
    for token in &tokens {
        if let Some(&id) = decl_at.get(&token.text_range())
            && let Some(kind) = declared(token)
        {
            kinds.insert(id, kind);
        }
    }
    // A native path's id is recorded on its head; its last segment is what the
    // id names, so the class is carried there.
    let mut carried: HashMap<TextRange, (TokenType, u32)> = HashMap::new();
    let mut out = Vec::new();
    for (index, token) in tokens.iter().enumerate() {
        let kind = token.kind();
        if kind.is_keyword() {
            out.push(SemanticToken {
                range: token.text_range(),
                ty: TokenType::Keyword,
                modifiers: 0,
            });
            continue;
        }
        if !matches!(
            kind,
            SyntaxKind::Ident | SyntaxKind::RawIdent | SyntaxKind::ModuleQualifier
        ) {
            continue;
        }
        let next = tokens.get(index + 1).map(SyntaxToken::kind);
        let class = if kind == SyntaxKind::ModuleQualifier {
            Some((TokenType::Namespace, 0))
        } else if decl_at.contains_key(&token.text_range()) {
            declared(token).map(|(ty, modifiers)| (ty, modifiers | DECLARATION))
        } else if let Some(&class) = carried.get(&token.text_range()) {
            Some(class)
        } else if let Some(&to) = refs.get(&token.text_range()) {
            let class = resolved(doc, to, &kinds, token);
            if next == Some(SyntaxKind::ColonColon) && matches!(to, Resolution::Native(_)) {
                let segments = path_segments(token);
                if let Some((tail, middle)) = segments.split_last() {
                    carried.insert(tail.text_range(), class);
                    for segment in middle.iter().skip(1) {
                        let (ty, _) = by_case(segment);
                        let ty = if ty == TokenType::Type {
                            ty
                        } else {
                            TokenType::Namespace
                        };
                        carried.insert(segment.text_range(), (ty, DEFAULT_LIBRARY));
                    }
                }
                Some((TokenType::Namespace, DEFAULT_LIBRARY))
            } else {
                Some(class)
            }
        } else {
            contextual(doc, token, next)
        };
        if let Some((ty, modifiers)) = class {
            out.push(SemanticToken {
                range: token.text_range(),
                ty,
                modifiers,
            });
        }
    }
    out
}

/// What the declaration whose name is `token` declares.
fn declared(token: &SyntaxToken) -> Option<(TokenType, u32)> {
    Some(match token.parent().kind() {
        SyntaxKind::ComponentDecl | SyntaxKind::SystemDecl => (TokenType::Class, 0),
        SyntaxKind::RecordDecl => (TokenType::Struct, 0),
        SyntaxKind::EnumDecl => (TokenType::Enum, 0),
        SyntaxKind::EnumVariant => (TokenType::EnumMember, 0),
        SyntaxKind::FnDecl
        | SyntaxKind::ActionDecl
        | SyntaxKind::TaskDecl
        | SyntaxKind::ShaderFn
        | SyntaxKind::NativeDecl => (TokenType::Function, 0),
        SyntaxKind::StateDecl | SyntaxKind::SlotDecl | SyntaxKind::ResourceDecl => {
            (TokenType::Property, 0)
        }
        SyntaxKind::InputDecl | SyntaxKind::ComputedDecl => (TokenType::Property, READONLY),
        SyntaxKind::EventDecl => (TokenType::Event, 0),
        SyntaxKind::ConstDecl => (TokenType::Variable, READONLY),
        SyntaxKind::GenericParam => (TokenType::TypeParameter, 0),
        SyntaxKind::NamedNode | SyntaxKind::PartNode => (TokenType::Variable, READONLY),
        SyntaxKind::TypeAliasDecl
        | SyntaxKind::TraitDecl
        | SyntaxKind::AssocTypeDecl
        | SyntaxKind::TemplateDecl
        | SyntaxKind::StyleDecl
        | SyntaxKind::ThemeDecl
        | SyntaxKind::ShaderDecl => (TokenType::Type, 0),
        _ => return None,
    })
}

/// What a name the resolver bound to `to` is.
fn resolved(
    doc: &OpenDoc,
    to: Resolution,
    kinds: &HashMap<SymbolId, (TokenType, u32)>,
    token: &SyntaxToken,
) -> (TokenType, u32) {
    match to {
        Resolution::Symbol(id) => kinds.get(&id).copied().unwrap_or_else(|| by_case(token)),
        Resolution::Local(_) => (TokenType::Variable, 0),
        Resolution::Native(id) => {
            if let Some(function) = doc.natives.function_by_id(id) {
                let ty = if function.is_method(&doc.natives) {
                    TokenType::Method
                } else {
                    TokenType::Function
                };
                (ty, DEFAULT_LIBRARY)
            } else if doc.natives.ty_by_id(id).is_some() {
                (TokenType::Type, DEFAULT_LIBRARY)
            } else {
                (TokenType::Namespace, DEFAULT_LIBRARY)
            }
        }
        Resolution::Env | Resolution::Theme | Resolution::Game => {
            (TokenType::Variable, READONLY | DEFAULT_LIBRARY)
        }
    }
}

/// A symbol of another module: a type when it is capitalized, a value else.
fn by_case(token: &SyntaxToken) -> (TokenType, u32) {
    if token.text().starts_with(|c: char| c.is_uppercase()) {
        (TokenType::Type, 0)
    } else {
        (TokenType::Variable, 0)
    }
}

/// The name tokens of the path `head` begins, `head` first.
fn path_segments(head: &SyntaxToken) -> Vec<SyntaxToken> {
    let parent = head.parent();
    let names = |node: &SyntaxNode| -> Vec<SyntaxToken> {
        node.children_with_tokens()
            .into_iter()
            .filter_map(|e| e.as_token().cloned())
            .filter(|t| matches!(t.kind(), SyntaxKind::Ident | SyntaxKind::RawIdent))
            .collect()
    };
    match parent.kind() {
        SyntaxKind::TypePathSegment => parent
            .parent()
            .map(|path| path.children().iter().flat_map(names).collect())
            .unwrap_or_default(),
        _ => names(&parent),
    }
}

/// What a name the resolver does not bind is, from where it stands.
fn contextual(
    doc: &OpenDoc,
    token: &SyntaxToken,
    next: Option<SyntaxKind>,
) -> Option<(TokenType, u32)> {
    let parent = token.parent();
    Some(match parent.kind() {
        SyntaxKind::TypePathSegment => {
            let in_node = parent
                .parent()
                .and_then(|path| path.parent())
                .is_some_and(|n| {
                    matches!(
                        n.kind(),
                        SyntaxKind::AnonymousNode | SyntaxKind::NamedNode | SyntaxKind::PartNode
                    )
                });
            let text = token.text();
            if in_node {
                let framework = doc.natives.widget(&text).is_some();
                (
                    TokenType::Class,
                    if framework { DEFAULT_LIBRARY } else { 0 },
                )
            } else {
                (TokenType::Type, DEFAULT_LIBRARY)
            }
        }
        SyntaxKind::PropertyPath
        | SyntaxKind::ThemeItem
        | SyntaxKind::RecordField
        | SyntaxKind::RecordExprField
        | SyntaxKind::RecordPatternField
        | SyntaxKind::ShaderUniform
        | SyntaxKind::ShaderInstance
        | SyntaxKind::ShaderVarying
        | SyntaxKind::ShaderTexture
        | SyntaxKind::ShaderSampler => (TokenType::Property, 0),
        SyntaxKind::FieldExpr | SyntaxKind::OptionalFieldExpr => {
            let called = parent
                .parent()
                .is_some_and(|call| call.kind() == SyntaxKind::CallExpr)
                && next == Some(SyntaxKind::LParen);
            if called {
                (TokenType::Method, 0)
            } else {
                (TokenType::Property, 0)
            }
        }
        SyntaxKind::EventHandler | SyntaxKind::EmitStmt => (TokenType::Event, 0),
        SyntaxKind::Argument | SyntaxKind::Param | SyntaxKind::EventParam => {
            (TokenType::Parameter, 0)
        }
        SyntaxKind::Attribute => (TokenType::Decorator, 0),
        SyntaxKind::ModulePath => (TokenType::Namespace, 0),
        SyntaxKind::ImportItem | SyntaxKind::RenameClause => match by_case(token) {
            (TokenType::Type, _) => (TokenType::Type, 0),
            _ => (TokenType::Namespace, 0),
        },
        // A path's tail the resolver left alone: a variant after its enum, a
        // function after its module.
        SyntaxKind::PathExpr => {
            let head = path_segments(token).first().map(SyntaxToken::text_range)
                == Some(token.text_range());
            match (head, next == Some(SyntaxKind::ColonColon)) {
                (true, false) => return None,
                (_, true) => (TokenType::Namespace, 0),
                (false, false) => match by_case(token) {
                    (TokenType::Type, _) => (TokenType::EnumMember, 0),
                    _ => (TokenType::Function, 0),
                },
            }
        }
        SyntaxKind::IdentPattern => (TokenType::Variable, 0),
        _ => return None,
    })
}

/// `tokens` in the protocol's relative encoding: five numbers a token, its
/// line and start each from the previous token's, in UTF-16 units.
pub fn encode(tokens: &[SemanticToken], line_index: &LineIndex) -> Vec<u32> {
    let mut data = Vec::with_capacity(tokens.len() * 5);
    let (mut line, mut start) = (0u32, 0u32);
    for token in tokens {
        let range = position::to_lsp_range(line_index, token.range);
        // A name never spans lines; one that did would be dropped, not split.
        if range.start.line != range.end.line {
            continue;
        }
        let delta_line = range.start.line - line;
        let delta_start = if delta_line == 0 {
            range.start.character - start
        } else {
            range.start.character
        };
        data.extend([
            delta_line,
            delta_start,
            range.end.character - range.start.character,
            token.ty as u32,
            token.modifiers,
        ]);
        line = range.start.line;
        start = range.start.character;
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source_map::SourceMap;

    /// Each classified token of `source` as `(text, type, modifiers)`.
    fn classify(source: &str) -> Vec<(String, TokenType, u32)> {
        let mut map = SourceMap::new();
        let id = map.open(source.to_string(), &["m"], None);
        let doc = map.get(id).unwrap();
        semantic_tokens(doc)
            .into_iter()
            .map(|t| {
                let text = source[t.range.start().to_usize()..t.range.end().to_usize()].to_string();
                (text, t.ty, t.modifiers)
            })
            .collect()
    }

    fn find(tokens: &[(String, TokenType, u32)], text: &str, nth: usize) -> (TokenType, u32) {
        let (_, ty, modifiers) = tokens
            .iter()
            .filter(|(t, _, _)| t == text)
            .nth(nth)
            .unwrap_or_else(|| panic!("no token `{text}` #{nth} in {tokens:?}"));
        (*ty, *modifiers)
    }

    const COUNTER: &str = "import viso::text;\n\
        component Counter {\n\
        \x20   input step: I32 = 1;\n\
        \x20   state count = 0;\n\
        \x20   event changed(to: I32);\n\
        \x20   action bump() { count += step; emit changed(to: count); }\n\
        \x20   view {\n\
        \x20       Column {\n\
        \x20           gap: 8dp;\n\
        \x20           Text(text::upper(\"hi\"))\n\
        \x20           Button(\"Add\") { on click { bump(); } }\n\
        \x20       }\n\
        \x20   }\n\
        }\n";

    #[test]
    fn declarations_carry_what_they_declare() {
        let tokens = classify(COUNTER);
        assert_eq!(find(&tokens, "Counter", 0), (TokenType::Class, DECLARATION));
        assert_eq!(
            find(&tokens, "step", 0),
            (TokenType::Property, DECLARATION | READONLY)
        );
        assert_eq!(
            find(&tokens, "count", 0),
            (TokenType::Property, DECLARATION)
        );
        assert_eq!(find(&tokens, "changed", 0), (TokenType::Event, DECLARATION));
        assert_eq!(find(&tokens, "bump", 0), (TokenType::Function, DECLARATION));
    }

    #[test]
    fn a_use_reads_as_its_declaration() {
        let tokens = classify(COUNTER);
        assert_eq!(find(&tokens, "count", 1), (TokenType::Property, 0));
        assert_eq!(find(&tokens, "step", 1), (TokenType::Property, READONLY));
        assert_eq!(find(&tokens, "bump", 1), (TokenType::Function, 0));
        assert_eq!(find(&tokens, "changed", 1).0, TokenType::Event);
    }

    #[test]
    fn widgets_properties_events_and_natives_are_told_apart() {
        let tokens = classify(COUNTER);
        assert_eq!(
            find(&tokens, "Column", 0),
            (TokenType::Class, DEFAULT_LIBRARY)
        );
        assert_eq!(find(&tokens, "gap", 0), (TokenType::Property, 0));
        assert_eq!(find(&tokens, "click", 0), (TokenType::Event, 0));
        assert_eq!(
            find(&tokens, "upper", 0),
            (TokenType::Function, DEFAULT_LIBRARY)
        );
        assert_eq!(find(&tokens, "text", 1).0, TokenType::Namespace);
        assert_eq!(find(&tokens, "to", 0).0, TokenType::Parameter);
        assert_eq!(find(&tokens, "I32", 0), (TokenType::Type, DEFAULT_LIBRARY));
    }

    #[test]
    fn contextual_keywords_are_keywords_where_the_parser_says() {
        let tokens = classify(COUNTER);
        for word in ["input", "state", "event", "action", "view", "on", "emit"] {
            assert_eq!(find(&tokens, word, 0).0, TokenType::Keyword, "{word}");
        }
    }

    #[test]
    fn the_encoding_is_relative_and_in_utf16() {
        let source = "component É {\n    state n = 0;\n}\n";
        let mut map = SourceMap::new();
        let id = map.open(source.to_string(), &["m"], None);
        let doc = map.get(id).unwrap();
        let data = encode(&semantic_tokens(doc), &doc.line_index);
        // `component` at 0:0, `É` (one UTF-16 unit) at 0:10, `state` at 1:4,
        // `n` at 1:10.
        assert_eq!(
            data,
            [
                0,
                0,
                9,
                TokenType::Keyword as u32,
                0,
                0,
                10,
                1,
                TokenType::Class as u32,
                DECLARATION,
                1,
                4,
                5,
                TokenType::Keyword as u32,
                0,
                0,
                6,
                1,
                TokenType::Property as u32,
                DECLARATION,
            ]
        );
    }
}
