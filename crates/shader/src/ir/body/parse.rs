//! Tokens → [`ast`](crate::ir::body::ast): syntax only.
//!
//! The parser allocates one [`Local`] per parameter and declaration (in source
//! order, so [`StmtKind::Decl::local`] indexes [`Function::locals`]) but resolves
//! nothing; scoping, types and stage rules are the checker's.

use crate::ir::body::BodyError;
use crate::ir::body::ast::{
    AssignOp, BinOp, Case, Else, Expr, ExprKind, Function, Item, Local, Res, Stmt, StmtKind, Ty,
    TypeName, UnOp,
};
use crate::ir::body::check::Stage;
use crate::ir::body::lex::{Span, Token, TokenKind, lex};

/// Words that can never be a variable, function or field name.
const KEYWORDS: &[&str] = &[
    "if", "else", "switch", "case", "default", "for", "while", "do", "return", "break", "continue",
    "static", "inline", "const", "constant", "device", "thread", "struct", "true", "false",
];

/// Parse a helper fragment: `static inline` functions and the comments between
/// them.
pub fn parse_items(src: &str) -> Result<Vec<Item>, BodyError> {
    let mut p = Parser::new(src)?;
    let mut items = Vec::new();
    loop {
        let tok = p.peek().clone();
        match &tok.kind {
            TokenKind::Eof => return Ok(items),
            TokenKind::Comment(text) => {
                p.bump();
                items.push(Item::Comment {
                    text: text.clone(),
                    blank_before: tok.blank_before,
                });
            }
            _ => {
                let func = p.function()?;
                items.push(Item::Function {
                    func,
                    blank_before: tok.blank_before,
                });
            }
        }
    }
}

/// Parse an entry-point body fragment as a parameterless function of `stage`.
pub fn parse_entry(src: &str, stage: Stage) -> Result<Function, BodyError> {
    let mut p = Parser::new(src)?;
    let span = p.peek().span;
    let mut body = Vec::new();
    while p.peek().kind != TokenKind::Eof {
        body.push(p.stmt()?);
    }
    Ok(Function {
        ret: stage.ret(),
        name: stage.entry_name().to_string(),
        params: 0,
        locals: std::mem::take(&mut p.locals),
        body,
        span,
    })
}

struct Parser {
    toks: Vec<Token>,
    pos: usize,
    last: Span,
    locals: Vec<Local>,
}

fn join(a: Span, b: Span) -> Span {
    Span {
        start: a.start,
        end: b.end,
        line: a.line,
        col: a.col,
    }
}

fn describe(kind: &TokenKind) -> String {
    match kind {
        TokenKind::Ident(s) | TokenKind::Float(s) | TokenKind::Int(s) | TokenKind::Uint(s) => {
            format!("`{s}`")
        }
        TokenKind::Punct(p) => format!("`{p}`"),
        TokenKind::Comment(_) => "a comment".into(),
        TokenKind::Eof => "end of input".into(),
    }
}

impl Parser {
    fn new(src: &str) -> Result<Parser, BodyError> {
        Ok(Parser {
            toks: lex(src)?,
            pos: 0,
            last: Span::default(),
            locals: Vec::new(),
        })
    }

    fn peek(&self) -> &Token {
        &self.toks[self.pos]
    }

    fn peek_at(&self, n: usize) -> &TokenKind {
        &self.toks[(self.pos + n).min(self.toks.len() - 1)].kind
    }

    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if t.kind != TokenKind::Eof {
            self.pos += 1;
        }
        self.last = t.span;
        t
    }

    fn unexpected(&self, expected: &str) -> BodyError {
        let t = self.peek();
        let code = if matches!(t.kind, TokenKind::Comment(_)) {
            "S0204"
        } else {
            "S0201"
        };
        BodyError::new(
            code,
            t.span,
            format!("expected {expected}, found {}", describe(&t.kind)),
        )
    }

    fn is_punct(&self, p: &str) -> bool {
        matches!(self.peek().kind, TokenKind::Punct(q) if q == p)
    }

    fn is_word(&self, w: &str) -> bool {
        matches!(&self.peek().kind, TokenKind::Ident(s) if s == w)
    }

    fn eat_punct(&mut self, p: &str) -> bool {
        if self.is_punct(p) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expect_punct(&mut self, p: &str) -> Result<Span, BodyError> {
        if self.is_punct(p) {
            Ok(self.bump().span)
        } else {
            Err(self.unexpected(&format!("`{p}`")))
        }
    }

    fn expect_word(&mut self, w: &str) -> Result<Span, BodyError> {
        if self.is_word(w) {
            Ok(self.bump().span)
        } else {
            Err(self.unexpected(&format!("`{w}`")))
        }
    }

    /// A name: an identifier that is neither a keyword nor a type.
    fn name(&mut self) -> Result<(String, Span), BodyError> {
        match &self.peek().kind {
            TokenKind::Ident(s) => {
                if KEYWORDS.contains(&s.as_str()) || TypeName::from_keyword(s).is_some() {
                    return Err(BodyError::new(
                        "S0202",
                        self.peek().span,
                        format!("`{s}` is reserved and cannot be used as a name"),
                    ));
                }
                let s = s.clone();
                Ok((s, self.bump().span))
            }
            _ => Err(self.unexpected("a name")),
        }
    }

    fn type_name(&mut self) -> Result<TypeName, BodyError> {
        match &self.peek().kind {
            TokenKind::Ident(s) => match TypeName::from_keyword(s) {
                Some(t) => {
                    self.bump();
                    Ok(t)
                }
                None => Err(self.unexpected("a type")),
            },
            _ => Err(self.unexpected("a type")),
        }
    }

    fn at_type_decl(&self) -> bool {
        matches!(&self.peek().kind, TokenKind::Ident(s) if TypeName::from_keyword(s).is_some())
            && matches!(self.peek_at(1), TokenKind::Ident(_))
    }

    fn local(&mut self, name: &str, ty: TypeName, is_param: bool) -> u32 {
        self.locals.push(Local {
            name: name.to_string(),
            ty,
            is_param,
            mutated: false,
        });
        (self.locals.len() - 1) as u32
    }

    fn function(&mut self) -> Result<Function, BodyError> {
        let start = self.expect_word("static")?;
        self.expect_word("inline")?;
        let ret = self.type_name()?;
        let (name, _) = self.name()?;
        self.locals.clear();
        self.expect_punct("(")?;
        let mut params = 0;
        if !self.is_punct(")") {
            loop {
                let ty = self.type_name()?;
                let (pname, _) = self.name()?;
                self.local(&pname, ty, true);
                params += 1;
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        self.expect_punct(")")?;
        let body = self.block()?;
        Ok(Function {
            ret,
            name,
            params,
            locals: std::mem::take(&mut self.locals),
            body,
            span: join(start, self.last),
        })
    }

    fn block(&mut self) -> Result<Vec<Stmt>, BodyError> {
        self.expect_punct("{")?;
        let mut out = Vec::new();
        while !self.is_punct("}") {
            if self.peek().kind == TokenKind::Eof {
                return Err(self.unexpected("`}`"));
            }
            out.push(self.stmt()?);
        }
        self.bump();
        Ok(out)
    }

    fn stmt(&mut self) -> Result<Stmt, BodyError> {
        let first = self.peek().clone();
        let kind = match &first.kind {
            TokenKind::Comment(text) => {
                self.bump();
                StmtKind::Comment(text.clone())
            }
            TokenKind::Ident(w) if w == "if" => return self.if_stmt(),
            TokenKind::Ident(w) if w == "switch" => {
                self.bump();
                self.expect_punct("(")?;
                let selector = self.expr()?;
                self.expect_punct(")")?;
                self.expect_punct("{")?;
                let mut cases = Vec::new();
                while !self.is_punct("}") {
                    let label = if self.is_word("case") {
                        self.bump();
                        Some(self.expr()?)
                    } else if self.is_word("default") {
                        self.bump();
                        None
                    } else {
                        return Err(self.unexpected("`case`, `default` or `}`"));
                    };
                    self.expect_punct(":")?;
                    let mut body = Vec::new();
                    while !(self.is_word("case") || self.is_word("default") || self.is_punct("}")) {
                        if self.peek().kind == TokenKind::Eof {
                            return Err(self.unexpected("`}`"));
                        }
                        body.push(self.stmt()?);
                    }
                    cases.push(Case { label, body });
                }
                self.bump();
                StmtKind::Switch { selector, cases }
            }
            TokenKind::Ident(w) if w == "for" => {
                self.bump();
                self.expect_punct("(")?;
                let init = Box::new(self.simple_or_decl()?);
                self.expect_punct(";")?;
                let cond = self.expr()?;
                self.expect_punct(";")?;
                let step = Box::new(self.simple()?);
                self.expect_punct(")")?;
                let body = self.block()?;
                StmtKind::For {
                    init,
                    cond,
                    step,
                    body,
                }
            }
            TokenKind::Ident(w) if w == "return" => {
                self.bump();
                let value = if self.is_punct(";") {
                    None
                } else {
                    Some(self.expr()?)
                };
                self.expect_punct(";")?;
                StmtKind::Return(value)
            }
            TokenKind::Ident(w) if w == "break" => {
                self.bump();
                self.expect_punct(";")?;
                StmtKind::Break
            }
            _ => {
                let s = self.simple_or_decl()?;
                self.expect_punct(";")?;
                s.kind
            }
        };
        Ok(Stmt {
            kind,
            span: join(first.span, self.last),
            blank_before: first.blank_before,
        })
    }

    fn if_stmt(&mut self) -> Result<Stmt, BodyError> {
        let first = self.peek().clone();
        self.expect_word("if")?;
        self.expect_punct("(")?;
        let cond = self.expr()?;
        self.expect_punct(")")?;
        let then = self.block()?;
        let els = if self.is_word("else") {
            self.bump();
            if self.is_word("if") {
                Some(Else::If(Box::new(self.if_stmt()?)))
            } else {
                Some(Else::Block(self.block()?))
            }
        } else {
            None
        };
        Ok(Stmt {
            kind: StmtKind::If { cond, then, els },
            span: join(first.span, self.last),
            blank_before: first.blank_before,
        })
    }

    /// A declaration or a simple statement, without the terminating `;`.
    fn simple_or_decl(&mut self) -> Result<Stmt, BodyError> {
        if !self.at_type_decl() {
            return self.simple();
        }
        let first = self.peek().clone();
        let ty = self.type_name()?;
        let (name, _) = self.name()?;
        let init = if self.eat_punct("=") {
            Some(self.expr()?)
        } else {
            None
        };
        let local = self.local(&name, ty, false);
        Ok(Stmt {
            kind: StmtKind::Decl {
                ty,
                name,
                local,
                init,
            },
            span: join(first.span, self.last),
            blank_before: first.blank_before,
        })
    }

    /// An assignment or increment, without the terminating `;`.
    fn simple(&mut self) -> Result<Stmt, BodyError> {
        let first = self.peek().clone();
        let kind = if self.is_punct("++") || self.is_punct("--") {
            let decrement = self.bump().kind == TokenKind::Punct("--");
            let target = self.postfix()?;
            StmtKind::Step {
                target,
                decrement,
                prefix: true,
            }
        } else {
            let target = self.postfix()?;
            match self.peek().kind {
                TokenKind::Punct(p @ ("++" | "--")) => {
                    self.bump();
                    StmtKind::Step {
                        target,
                        decrement: p == "--",
                        prefix: false,
                    }
                }
                TokenKind::Punct(p) if AssignOp::from_punct(p).is_some() => {
                    self.bump();
                    let op = AssignOp::from_punct(p).expect("checked above");
                    let value = self.expr()?;
                    StmtKind::Assign { target, op, value }
                }
                _ => {
                    return Err(BodyError::new(
                        "S0203",
                        join(first.span, self.last),
                        "an expression is not a statement; expected an assignment or `++`/`--`",
                    ));
                }
            }
        };
        Ok(Stmt {
            kind,
            span: join(first.span, self.last),
            blank_before: first.blank_before,
        })
    }

    fn mk(&self, kind: ExprKind, start: Span) -> Expr {
        Expr {
            kind,
            span: join(start, self.last),
            ty: Ty::Unknown,
        }
    }

    fn expr(&mut self) -> Result<Expr, BodyError> {
        let cond = self.binary(1)?;
        if !self.eat_punct("?") {
            return Ok(cond);
        }
        let t = self.expr()?;
        self.expect_punct(":")?;
        let f = self.expr()?;
        let start = cond.span;
        Ok(self.mk(
            ExprKind::Ternary(Box::new(cond), Box::new(t), Box::new(f)),
            start,
        ))
    }

    fn binary(&mut self, min_prec: u8) -> Result<Expr, BodyError> {
        let mut lhs = self.unary()?;
        while let TokenKind::Punct(p) = self.peek().kind {
            let Some(op) = BinOp::from_punct(p) else {
                break;
            };
            if op.precedence() < min_prec {
                break;
            }
            self.bump();
            let rhs = self.binary(op.precedence() + 1)?;
            let start = lhs.span;
            lhs = self.mk(ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)), start);
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> Result<Expr, BodyError> {
        let op = match self.peek().kind {
            TokenKind::Punct("-") => UnOp::Neg,
            TokenKind::Punct("!") => UnOp::Not,
            TokenKind::Punct("~") => UnOp::BitNot,
            _ => return self.postfix(),
        };
        let start = self.bump().span;
        let e = self.unary()?;
        Ok(self.mk(ExprKind::Unary(op, Box::new(e)), start))
    }

    fn args(&mut self) -> Result<Vec<Expr>, BodyError> {
        self.expect_punct("(")?;
        let mut args = Vec::new();
        if !self.is_punct(")") {
            loop {
                args.push(self.expr()?);
                if !self.eat_punct(",") {
                    break;
                }
            }
        }
        self.expect_punct(")")?;
        Ok(args)
    }

    fn postfix(&mut self) -> Result<Expr, BodyError> {
        let mut e = self.primary()?;
        loop {
            let start = e.span;
            if self.eat_punct(".") {
                let (name, _) = self.field_name()?;
                if self.is_punct("(") {
                    let args = self.args()?;
                    e = self.mk(
                        ExprKind::Method {
                            base: Box::new(e),
                            method: name,
                            args,
                        },
                        start,
                    );
                } else {
                    e = self.mk(
                        ExprKind::Field {
                            base: Box::new(e),
                            name,
                        },
                        start,
                    );
                }
            } else if self.eat_punct("[") {
                let index = self.expr()?;
                self.expect_punct("]")?;
                e = self.mk(
                    ExprKind::Index {
                        base: Box::new(e),
                        index: Box::new(index),
                    },
                    start,
                );
            } else {
                return Ok(e);
            }
        }
    }

    fn field_name(&mut self) -> Result<(String, Span), BodyError> {
        match &self.peek().kind {
            TokenKind::Ident(s) => {
                let s = s.clone();
                Ok((s, self.bump().span))
            }
            _ => Err(self.unexpected("a member name")),
        }
    }

    fn primary(&mut self) -> Result<Expr, BodyError> {
        let tok = self.peek().clone();
        let kind = match &tok.kind {
            TokenKind::Float(s) => {
                self.bump();
                ExprKind::Float(s.clone())
            }
            TokenKind::Int(s) => {
                self.bump();
                ExprKind::Int(s.clone())
            }
            TokenKind::Uint(s) => {
                self.bump();
                ExprKind::Uint(s.clone())
            }
            TokenKind::Punct("(") => {
                self.bump();
                let e = self.expr()?;
                self.expect_punct(")")?;
                ExprKind::Paren(Box::new(e))
            }
            TokenKind::Ident(w) => {
                if let Some(ty) = TypeName::from_keyword(w) {
                    self.bump();
                    if !self.is_punct("(") {
                        return Err(self.unexpected("`(` after a type in an expression"));
                    }
                    let args = self.args()?;
                    ExprKind::Construct { ty, args }
                } else {
                    let (name, _) = self.name()?;
                    if self.is_punct("(") {
                        let args = self.args()?;
                        ExprKind::Call { name, args }
                    } else {
                        ExprKind::Var {
                            name,
                            res: Res::Unresolved,
                        }
                    }
                }
            }
            _ => return Err(self.unexpected("an expression")),
        };
        Ok(self.mk(kind, tok.span))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frag(src: &str) -> Result<Function, BodyError> {
        parse_entry(src, Stage::Fragment)
    }

    #[test]
    fn precedence_and_ternary_nest_like_c() {
        let f = frag("float r = a < b ? -x * y : c ? 1.0 : 2.0;\nreturn r;").unwrap();
        let StmtKind::Decl {
            init: Some(init), ..
        } = &f.body[0].kind
        else {
            panic!()
        };
        let ExprKind::Ternary(c, t, e) = &init.kind else {
            panic!()
        };
        assert!(matches!(c.kind, ExprKind::Binary(BinOp::Lt, ..)));
        let ExprKind::Binary(BinOp::Mul, l, _) = &t.kind else {
            panic!()
        };
        assert!(matches!(l.kind, ExprKind::Unary(UnOp::Neg, _)));
        assert!(matches!(e.kind, ExprKind::Ternary(..)));
        assert_eq!(f.locals.len(), 1);
    }

    #[test]
    fn statements_and_blank_lines() {
        let f = frag(
            "float4 acc = float4(0.0);\n\n// taps\nfor (int i = -R; i <= R; ++i) {\n    acc += w;\n}\nswitch (m) {\n    case 0u: return acc;\n    default: break;\n}\nreturn acc;",
        )
        .unwrap();
        assert!(matches!(f.body[1].kind, StmtKind::Comment(ref t) if t == "taps"));
        assert!(f.body[1].blank_before);
        assert!(!f.body[2].blank_before);
        let StmtKind::For { init, step, .. } = &f.body[2].kind else {
            panic!()
        };
        assert!(matches!(init.kind, StmtKind::Decl { .. }));
        assert!(matches!(step.kind, StmtKind::Step { prefix: true, .. }));
        let StmtKind::Switch { cases, .. } = &f.body[3].kind else {
            panic!()
        };
        assert_eq!(cases.len(), 2);
        assert!(cases[1].label.is_none());
        assert_eq!(f.locals.len(), 2);
    }

    #[test]
    fn helpers_are_items() {
        let items =
            parse_items("// a\nstatic inline float f(float2 p, float k) {\n    return k;\n}")
                .unwrap();
        assert_eq!(items.len(), 2);
        let Item::Function { func, .. } = &items[1] else {
            panic!()
        };
        assert_eq!(func.params, 2);
        assert!(func.locals.iter().all(|l| l.is_param));
    }

    #[test]
    fn syntax_errors_are_spanned() {
        let e = frag("float x = 1.0\nreturn x;").unwrap_err();
        assert_eq!(e.code, "S0201");
        assert_eq!((e.span.line, e.span.col), (2, 1));
        assert_eq!(frag("x + 1.0;").unwrap_err().code, "S0203");
        assert_eq!(frag("float if = 1.0;").unwrap_err().code, "S0202");
        assert_eq!(
            frag("float x = (1.0 +\n// c\n2.0);").unwrap_err().code,
            "S0204"
        );
        assert_eq!(frag("if (a) x = 1.0;").unwrap_err().code, "S0201");
        assert_eq!(frag("{").unwrap_err().code, "S0201");
    }
}
