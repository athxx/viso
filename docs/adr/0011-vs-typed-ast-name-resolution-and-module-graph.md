# ADR 0011 — `.vs` typed AST, name resolution, module graph, and unified diagnostics

- Status: Accepted
- Date: 2026-09-03

## Context

Slice K (ADR 0010) delivered the `.vs` lexical layer: a streaming tokenizer, a
lossless rowan-style green-tree CST, and a *coarse* parser that only groups tokens
at `Item`/`Block`/`{}` granularity. It explicitly deferred the typed grammar
(the `:` vs `=` context split, `node name: Type {}` identity, precedence-correct
expressions) plus the AST, name resolution, and the module graph to Slice L.

Slice L closes that gap. It upgrades the parser to a typed CST, projects a typed AST
over the green tree, and resolves names across a deterministic module graph — the
foundation every later slice (M Typed HIR, N UI/Binding IR, O hot reload, P AOT)
consumes. Governing spec is `Viso_DSL_1.0.md` Appendix A (the parser contract),
sections 21.4/21.5 (module + source-form semantics), section 10.4 (identity), section
40 (namespaces), and section 30 (diagnostics); AGENTS section 21.2 (pipeline), section
7.2/section 44 (cold-path abstractions), section 68 (ADR trigger: "Viso DSL
language/module semantics").

The reference framework's DSL path (`makepad/platform/script`) is a single-file
dynamic VM: it evaluates a script module in place, with no module graph and no
cross-module stable identity. Per the standing "take semantics, not coarse
architecture" rule, Viso keeps its slot-based local-scope idiom for `let`/param
resolution but invents the module graph and `SymbolId` itself.

This is a cold-path compiler frontend, so `Rc`/`HashMap`/`String` interning are
allowed (AGENTS section 7.2); the hot-path zero-allocation contract applies to what
this frontend *lowers to* (Slice N), not to the frontend. The dense-ID discipline of
section 10.4 (`NameId`/`SymbolId`, no universal `Id(u64)`, no `DefaultHasher`, no
process seed, no byte-offset as identity) still binds.

## Decision

### 1. Event-driven typed parser, layered over the coarse skeleton

The typed grammar is an event-driven recursive-descent + Pratt parser
(`syntax/grammar/`). It emits a flat `Vec<Event>` (`Start`/`Finish`/`Token`) that a
single final pass (`build_tree`) plays into the green-tree `GreenBuilder`,
interleaving trivia in their original stream positions so losslessness holds
(`root.text() == source`). An event buffer, rather than driving the builder
directly, is what lets a Pratt parser retroactively wrap an already-parsed left
operand in a `BinaryExpr` (via a `Marker`/`forward_parent`) — a stack-machine
builder cannot re-open a finished node. Recovery is total: `ErrorNode`/`MissingToken`
on error, synchronize to sync points, multiple diagnostics per pass, and no input
panics or stalls.

The Slice-K coarse parser (`syntax/parser.rs`) is retained: it still compiles and is
re-exported, and it holds the *shared parse types* (`Parse`, `ParseErrorKind`). The
typed parser supersedes it as the live entry (`syntax::grammar::parse`), producing the
grammar's real node kinds (Appendix A) instead of coarse `Item`/`Block` grouping.

### 2. The `:` vs `=` split is grammar-positional

View/style **property binding** is `PropertyPath ":" Expression ";"` — a colon — per
Appendix A / section 21.5.2. Imperative `let`/assignment/initializers use `=`. The
split is driven by grammar position, not by a token-level heuristic. All three source
forms (`ui!` ViewFragment, `component!` ComponentEntry, `view!`/`.vs` CompilationUnit)
route through one grammar so they share resolution downstream.

### 3. Typed AST as red-tree views, plus a full red tree

The AST (`ast/`) is rust-analyzer-style **typed views** over the green tree: an
`AstNode` trait with `cast(SyntaxNode) -> Option<Self>` + `syntax()`, and thin wrapper
structs with typed accessors. There is no separate owned AST allocation — the green
tree stays the single source of truth (needed by the formatter, LSP, and rename
later).

To support those views and the tooling that follows, Slice L lands a complete red-tree
layer (`syntax/red.rs`): `SyntaxNode`/`SyntaxToken`/`SyntaxElement` with cached,
`Rc`-shared red nodes carrying a parent pointer, absolute offset, and index-in-parent;
bidirectional navigation (`parent`/`children`/siblings/`ancestors`/`descendants`); and
stable per-node identity (green pointer + offset). This is more than the AST strictly
needs now, but the formatter/LSP/goto/rename (Phase 6 exit, Phase 9 Studio) consume it
directly, so building it whole here avoids a later AST re-architecture. It stays
cold-path (`Rc` sharing allowed, AGENTS section 7.2).

### 4. Identity: `NameId` interner + 128-bit versioned `SymbolId`

Per section 10.4:

- `NameId(u32)` + `NameInterner`: string→id with a reverse lookup for diagnostics.
  AST and resolution carry `NameId`, not repeated `String`.
- `SymbolId { lo: u64, hi: u64 }` (128-bit, `#[repr(C)]`, equality/hash/ord only — no
  arithmetic) is durable declaration identity. It is minted by a **fixed FNV-1a-128**
  we implement ourselves (constant offset-basis + prime, byte stream folded into two
  `u64` lanes — no table, no dependency). Chosen over an xxh3-style hash because
  identity needs determinism and low collision on a cold path, not throughput, so the
  simplest self-owned algorithm wins. **Never** `DefaultHasher`, never a process seed,
  never a byte offset as identity. Fingerprint inputs are canonical (package identity +
  module path + declaration kind + canonical declaration path); the algorithm version
  (`FINGERPRINT_VERSION`) is tagged for artifact metadata, and a known-answer test pins
  the constants so a future refactor cannot silently shift identities.

### 5. Deterministic module graph from an in-memory unit set

`ModulePath` is a `Vec<NameId>` (`::`-separated). Module identity is the module path
alone — there is no in-file `module` header (section 21.5.3). A `SourceUnit` pairs a
module path with an already-parsed `Parse`; the caller supplies the set in memory.
There is no filesystem package loader yet (no `Viso.toml` exists, and the slice must be
testable standalone).

`ModuleGraph::build` is **deterministic**: units are keyed and iterated in sorted
module-path *text* order, never registration order, so the same input set always yields
the same graph and diagnostics regardless of how the caller assembled it (section 21.4:
no registration-order API). It resolves each unit's `import` declarations to edges,
reporting a duplicate module path as ambiguity (E2002), an import of an unknown module
as unresolved (E2001), and an import cycle (iterative DFS, back-edge detection, cycle
members reported in sorted index order) as E2003.

### 6. Full cross-module resolution with slot-based local scopes

The resolver (`resolve/resolver.rs`) produces a `ResolvedModule`: per-module symbol
tables map top-level decls to `SymbolId` with `export` visibility; `import` alias/rename
(`as`) and selective import bring names into a module's environment; a `type_path`/`path`
referencing another module's exported symbol resolves to its `SymbolId` (unresolved =
E2001, ambiguous = E2002). Local scopes borrow the reference's slot idiom: component
members form one scope with separate Value/Type/Event namespaces (a collision within a
namespace is an error, cross-namespace is fine, section 40); `view` introduces
`node`/`for`-pattern bindings; `fn`/`action`/`on`-handler bodies introduce `let`/param
scopes. This layer answers *what each name refers to*, not *what type it has* — type,
effect, and capability checking are Slice M.

### 7. One shared `Diagnostic`; the `*Kind` enums stay the emit vocabulary

All diagnostics converge on a single `Diagnostic { severity, code: &'static str,
primary: TextRange, related: Vec<(TextRange, String)>, notes: Vec<String>, message:
String }` (`diag.rs`), with a `Severity { Note, Warning, Error }` ordered
least-to-most-severe. This is the single type every downstream consumer (Slice M/N,
LSP, Studio, CLI) sees, matching section 30 and the section 138 schema.

Full-replacement consolidation was chosen over an adapter layer (better-designed at
equal cold-path cost):

- **Kept**: the three `*Kind` enums — `LexError` (`syntax/token.rs`), `ParseErrorKind`
  (`syntax/parser.rs`), `ResolveErrorKind` (`resolve/module.rs`). They are the *single
  source* of the `code()`/`message()` string tables; deleting them would duplicate those
  tables at every construction site. `*Kind` stays the emit vocabulary.
- **Deleted**: the per-family wrapper structs `ParseError` and `ResolveError`. Each family
  now exposes a uniform `to_diagnostic()` that lifts a `*Kind` into a `Diagnostic`
  (`LexError` picks its severity from `is_warning`; `ResolveErrorKind` takes an
  `Option<TextRange>` — substituting a zero-width span at source start for whole-graph
  facts like a duplicate path or a cycle member — and folds the offending subject into
  the message).
- **Accumulators**: `Parse.errors`, both parsers' `Parser.errors`, `ModuleGraph`'s
  errors, and the resolver's error lists are all `Vec<Diagnostic>`.
- **Re-exports**: the deleted wrapper structs are dropped from `syntax/mod.rs`,
  `syntax/grammar/mod.rs`, `resolve/mod.rs`, and `lib.rs`; `ParseErrorKind`,
  `ResolveErrorKind`, and `LexError` stay exported (still the code/message vocabulary,
  used by tests); `diag::{Diagnostic, Severity}` are re-exported at the crate root.

No new diagnostic codes were minted: the spec codes already present are reused
(E1301, E2001–E2003, E2701/2, E2801, E3001, E3201, E3401, Lex0001–0016,
Parse0001–0005).

### 8. Two spec bugs flagged for the owner

While implementing against Appendix A, two `Viso_DSL_1.0.md` body-prose bugs were found
and are recorded here for the spec owner (Appendix A wins where they disagree):

1. **`:` vs `=` in property binding.** The examples in sections 54/56/65/E.1 show view
   property binding with `=`, contradicting Appendix A / A.8 / Appendix B, which specify
   `PropertyPath ":" Expression ";"`. The colon form is authoritative and is what the
   parser implements; imperative `=` assignment is a statement, not a property binding.
2. **`theme` used inside a property value.** Section 21.5.2's own example
   (`color: theme.colors.foreground;`) uses `theme` as an ordinary path head inside an
   expression, while `theme` is otherwise treated as a reserved declaration keyword.
   A reserved keyword cannot also be a value-namespace path root without an explicit
   carve-out; the spec must either reserve `theme` only in declaration position or name
   the value-side accessor differently.

## Consequences

- Slices M–P consume Slice L's typed AST + resolved `SymbolId`s directly; the module
  graph's determinism is a precondition for reproducible AOT artifacts and stable
  hot-reload identity.
- The green tree remains the single tree; the AST adds zero duplication, and the full
  red tree means the formatter/LSP/rename work does not force an AST rewrite later.
- `SymbolId` collisions within one artifact are a build error, and the pinned
  known-answer vector guards the fingerprint constants against silent drift.
- Advanced productions (`trait`/`impl`, general/const generics, `template`/`part`,
  `style`/`theme`, `shader`, `native` schema) parse into placeholder AST nodes but get
  no resolution this slice; they resolve when their consumer lands (recorded in todo).
- `viso-ende` byte-encoding stays deferred; nothing in Slice L needs it. The workspace
  remains **13 crates** and `viso-dsl` gains **zero** new dependencies;
  `cargo xtask check-deps` stays green.
- Verification for this slice: `cargo build/clippy -D warnings/fmt/test -p viso-dsl`
  clean — 93 tests (23 unit incl. red-tree navigation, fingerprint determinism +
  known-answer, module-graph determinism/cycle/ambiguity, resolver import/alias/
  cross-module/namespace/local-scope, and `Severity`/`Diagnostic` consolidation; plus
  7 AST, 11 decl-grammar, 15 expr-grammar, 11 view-grammar, 18 lexer, 8 coarse-parser
  integration tests). `cargo xtask check-deps` green (13 crates, 0 new deps). No
  shaders/UI/GPU touched, so no Metal/headless run was required.
