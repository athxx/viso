# ADR 0018 — Formatter and LSP (viso-lsp: analysis engine + thin stdio frontend)

- Status: Accepted
- Date: 2026-09-04

## Context

`todo.md:336-338` (doc §71 item 1) is the last unmet Phase 6 exit criterion:
**"formatter / LSP / goto / rename / reference usable"** for `.vs`. The other four Phase 6
criteria (release needs no startup `.vs` parse; hot reload is compile → validate → atomic patch;
a failed compile keeps last-good UI; state/focus/scroll migration has explicit rules) landed with
Slices K–Q. Delivering this criterion closes Phase 6.

The `viso-dsl` frontend was already rust-analyzer-shaped before this slice: a lossless green/red
CST (`root.text()` is a byte-exact round-trip), a typed AST view over the red tree, a name
resolver that records use → def edges, a `LineIndex` for byte ↔ line/column ↔ UTF-16 conversion,
and a `Diagnostic` carrying spans. `crates/dsl/src/diag.rs:8` already names LSP as a consumer, and
`crates/dsl/src/syntax/red.rs:23` states the red tree's identity "is what rename/goto/reference
tooling keys on" — the frontend was designed with this slice in mind. Slice R is thin adaptation
over that frontend, **not a second compiler**.

Two gaps were confirmed against the source before implementation:

1. **No reverse def → use index.** `ResolvedModule.refs` records each use's `Resolution` target,
   but nothing inverts it: find-references and rename need `Resolution → all use spans`.
2. **No `SymbolId → declaration span` map.** `build_symbol_table` mints a `SymbolId` from a
   declaration's *canonical identity* (never its position) and used the name-token span only for
   collision diagnostics, discarding it. goto-definition and rename need to locate and rewrite the
   definition itself, so the id alone is insufficient.

These two are the only real net-new frontend work; everything else is a thin wrapper over existing
structure.

Two design decisions were locked with the user (AskUserQuestion, both recommended options):

- **LSP form = analysis engine + thin stdio frontend.** A pure-function analysis engine (source +
  position → spans/edits, zero protocol dependencies, fully headless-unit-testable) plus a thin
  synchronous stdio JSON-RPC bin. **No async runtime.**
- **Formatter form = normalizing re-layout + idempotence.** Re-layout per the DSL style rules
  (AGENTS 21.5.2), anchored by `format(format(x)) == format(x)` plus golden tests, comments
  preserved.

This slice fires the §68 ADR trigger for **crate dependency direction** and **Viso DSL
language/tool semantics**. It adds **one crate** (`viso-lsp`) and **one new leaf edge**
(`("viso-lsp", &["viso-dsl"])`); the workspace goes 16 → 17 crates.

## Decision

### 1. A new `viso-lsp` crate is justified (AGENTS 3.3); its one edge is a clean leaf

`.vs` tooling — formatter, LSP — is listed in §32 as a long-term standalone tool surface. It needs
an **independent executable** (a `viso-lsp` bin) and **dependencies independent of the runtime**
(JSON-RPC framing, a stdio loop). Folding it into `viso-dsl` would pollute the compiler library
with an executable entry point and protocol code. That satisfies multiple AGENTS 3.3 conditions
(independent reuse / target-specific deps / independent tool release), so it earns its own crate
rather than a module.

The sole internal edge is `("viso-lsp", &["viso-dsl"])` — a clean new leaf above the DAG (the tool
depends on the compiler frontend; nothing depends on the tool). It is not one of the AGENTS 3.5
forbidden edges, and `cargo xtask check-deps` confirms the DAG stays acyclic (17 crates, all edges
within the §10 DAG).

### 2. Two layers: a pure analysis engine, and a thin stdio frontend

The crate is deliberately split so all analysis logic is testable without a transport.

**Analysis engine (zero protocol deps, headless-unit-tested):**

- `index.rs` — `ReferenceIndex { def_to_uses: HashMap<Resolution, Vec<TextRange>>, decl_span:
  HashMap<SymbolId, TextRange> }`, built by a single pass over `refs` (net-new piece 1) plus the
  resolver's now-recorded decl-spans (net-new piece 2).
- `source_map.rs` — a `FileId`/`OpenDoc` boundary that feeds opened documents through the existing
  `parse` + `resolve`, holding `(source, LineIndex, Parse, ResolvedModule, ReferenceIndex)`.
- `position.rs` — byte ↔ UTF-16 line/column conversion (LSP positions are UTF-16), a thin wrapper
  over `LineIndex` with a crate-local `LspRange`/`LspPosition` carrying no protocol dependency.
- `engine.rs` — `goto_definition` / `find_references` / `rename` / `format` as plain functions from
  source text and a position to spans/edits. `rename` refuses names that are not identifiers.

**Thin stdio frontend (no async runtime):** `rpc/` is a self-contained JSON value + recursive-
descent parser + serializer and `Content-Length` framing; the workspace pulls in no
serde/serde_json and the protocol surface is small and fixed, so a tiny cold-path JSON adapter is
correct here (AGENTS 25 — a frontend adapts a protocol, it does not host a runtime). `server.rs`
dispatches LSP requests to engine calls; `src/bin/viso-lsp.rs` is a synchronous framed read → handle
→ write loop over stdin/stdout. The bin owns no analysis logic.

**Why tower-lsp + tokio was rejected.** It would drag a general async executor into a frontend tool
— the "adapters, not owner" violation AGENTS 25 forbids — for a synchronous request/response
protocol that gains nothing from it. The engine's purity (no `tokio`, no `lsp` types) is also what
makes every operation headless-unit-testable; a transport library would entangle the analysis in a
runtime with no test benefit.

### 3. The formatter is a CST-driven normalizing re-layout, anchored by idempotence

`format/` walks the red/green CST and re-lays-out per the DSL style rules (AGENTS 21.5.2): unified
indentation (one level per block), property binding `name: value;` (`:` binding + `;` terminator),
block-brace placement, and folded surplus blank lines. Comments are trivia carried in stream order
and are **preserved**, not dropped or reordered across semantic boundaries. Only trivia and layout
are normalized; no semantic token changes.

Correctness is anchored by `format(format(x)) == format(x)` idempotence plus golden `.vs`
input/output tests. Formatting is a pure `format(source) -> String` function in the engine, so it
is testable and re-usable without the transport.

### 4. The resolver records decl-spans at the mint site; `Resolution` gains `Hash`/`Ord`

Net-new piece 2 is a `SymbolDecl { id, name_range }` emitted at the single `SymbolId`-mint site in
`build_symbol_table` — no second tree walk. Net-new piece 1 needs `Resolution` as a map key, so it
gains `Hash`/`Ord` (both payloads, `SymbolId` and `LocalSlot`, are already hashable/ordered). Both
are cold-path tooling aids (AGENTS 7.2); the compiler proper still compares `Resolution` by
equality only. (The `too_many_arguments` this introduced in `define_members` was resolved by a
`SymbolTableBuild` sink struct grouping the `(table, decls, errors)` trio, not by suppressing the
lint.)

All of `viso-lsp` is cold-path tooling (AGENTS 7.2): `HashMap`, owned `String`, and `Vec` are the
right tools; there is no steady-state frame cost to guard.

## Consequences

- Phase 6's last exit criterion is met: goto-definition, find-references, rename, formatting, and
  publishDiagnostics are usable for `.vs` over stdio LSP. **Phase 6's five criteria are all green.**
- The frontend now durably records decl-spans and an index-friendly `Resolution`, reusable by any
  future tool (Studio, migration) without a second resolve pass.
- The engine is fully headless-tested (goto/references/rename), the formatter by idempotence +
  golden, and the transport by in-memory buffer round-trips — no editor required in CI.
- **Scope boundary (recorded, not swallowed):** this slice ships the minimal usable set only. It
  does **not** include the Viso CLI (Phase 9 Studio/Inspector/CLI), `viso migrate` (Phase 10),
  extended LSP methods (hover / completion / signature help / semantic tokens / code actions /
  folding), or cross-package reference indexing. The only executable is the narrow `.vs`
  formatter/language-server bin, not a full `viso` CLI.

## Alternatives considered

- **Fold formatter/LSP into `viso-dsl` as a module.** Rejected: it forces an executable entry point
  and protocol/transport dependencies into the compiler library that every `viso-dsl` consumer
  would then pull in. §32 lists these as standalone tools; AGENTS 3.3 favors a crate.
- **tower-lsp + tokio async server.** Rejected: an async executor in a frontend tool violates
  AGENTS 25 and gives nothing for a synchronous protocol, while also blocking headless engine
  testing. A synchronous `Content-Length` loop over a pure engine is smaller and fully testable.
- **A serde/serde_json dependency for JSON.** Rejected for this slice: the protocol surface is
  small and fixed, the workspace has no existing serde dependency, and a ~200-line self-contained
  JSON value keeps the transport unit-testable with no new dependency. (AGENTS 25 would permit a
  mature JSON adapter; it was simply not needed here.)
- **A second decl-span tree walk in `viso-lsp`.** Rejected: the resolver already visits every
  declaration to mint its `SymbolId`; recording the name-token span there is free, whereas a second
  walk in the tool would duplicate traversal and risk drift.

## Amendment — 2026-10-15: the formatter lives in `viso-dsl`

`viso fmt` must format with the very function the language server uses. Reaching it in `viso-lsp`
would add a `viso-cli → viso-lsp` edge and pull the server, its JSON value and its transport into
the CLI for one pure function. The formatter itself depends only on the DSL's lexer, parser and
CST, so it moves to `viso_dsl::format`; `viso-lsp` re-exports it from its engine and both callers
share one implementation and one test corpus (`crates/dsl/tests/format_*.rs`).

The rejected alternative above — folding the formatter *and the LSP* into `viso-dsl` — stays
rejected for the reason it gives: the executable, protocol and transport remain in `viso-lsp`, and
no `viso-dsl` consumer gains a dependency.

`viso_dsl::format::try_format` is the CLI's entry: it formats a source that parses and returns the
syntax errors of one that does not — lexical ones included — so a file that does not parse is
reported and left as it is, while `format` stays the permissive editor operation.

## Amendment — 2026-10-15: hover on diagnostic codes, the exit code, `viso lsp`

The compiler's diagnostics registry (`viso_dsl::diag::codes`) is the one table of what each code
means and how to fix it; `viso explain` renders it. `textDocument/hover` over a diagnostic's range
renders the same entry — title, explanation, fix and the `viso explain` command — as markdown, so
an editor and the CLI cannot disagree about a code. This is the only extended method added; the
others listed as out of scope above stay so.

The binary now exits as the protocol specifies: `0` when `exit` follows `shutdown`, `1` for an
`exit` without it, an end of input before `exit`, or a broken transport. It exited `0` always.

`viso lsp` is the CLI's entry to the server and stays outside it: it finds `viso-lsp` beside the
`viso` executable, then on `PATH`, and replaces its own process with it, so no `viso-cli →
viso-lsp` edge exists and stdout carries the protocol alone.

## Amendment — 2026-10-29: semantic tokens and completion

The editor extensions (VSCode, Zed) are thin shells over this server, so the two methods an
editor's own grammar cannot answer are added here rather than in either shell.

- `textDocument/semanticTokens/full` classifies every keyword and name from the resolver's
  bindings and declarations (a state read in a handler reads as the state it binds; an
  imported native's path carries its function or type to its last segment) and, for names the
  resolver does not bind, from their CST position (a node's widget, a property, an event, a
  field, an argument label). The legend is standard LSP token types plus `declaration`,
  `readonly` and `defaultLibrary`; positions are UTF-16 as everywhere else in the server.
- `textDocument/completion` reads the position from the token before the word being typed, so a
  half-written word never decides it: a node body offers its widget's properties (from the
  widget schema `viso schema` reads) or a same-file component's inputs, the nodes and the
  node-member keywords; `on` offers events; an expression its component's members, locals,
  imports and value keywords; `lib::` the native library's functions, types and sub-libraries
  from the registry; a type position the builtin, structural and declared types; a declaration
  start the keywords of its level.

Both are cold-path and re-derive from the open document; signature help and folding stay out of
scope.
