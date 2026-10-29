# ADR 0034 — DSL keyword tiers: strict vs contextual keywords, and label positions

- Status: Accepted (2026-10-29)
- Date: 2026-09-26

## Context

Before this ADR, `Viso_DSL_1.0.md` §12 listed a single flat set of reserved words.
Three problems followed:

- Common UI and domain names could not be used as identifiers. Examples include
  `state`, `event`, `style`, `theme`, `key`, `input`, `start`, `success`, `error`,
  `empty`, `node`, `fill` and `scope`. That forced `r#` escapes into ordinary code.
- The grammar's `member_suffix` accepted only `IDENT`, so field access such as
  `theme.style`, `env.input`, `item.key` or `result.error` was a syntax error, even
  though the spec's own examples used it.
- `theme` and `env` were treated partly as keywords and partly as values, and the text
  did not say which.

The design is AI-first: identifiers should be predictable, and escapes should be rare.
A language-semantics change needs an ADR.

## Decision

### 1. Two keyword tiers

The words split into two tiers.

**Strict keywords** can never be an identifier in a binding or declaration position:

```text
import export as component system record enum trait impl type implements where
for fn action task const native shader let mut return break continue if else
match while loop in true false None self Self dyn await
```

Reserved-forbidden words are strict as well, with no meaning yet:
`child store merge extend inherit class macro unsafe extern static yield try`.

**Contextual keywords** lex as `IDENT`. They are special only at their fixed grammar
position, and the parser decides which applies with at most 2 tokens of lookahead:

```text
input state computed event slot effect resource view style theme template part
requires capability on capture bubble emit transaction start move when run cleanup
success error cancelled node fill bind using use override replace preserve key load
policy scope vertex fragment compute uniform instance varying texture sampler empty
```

A contextual keyword is special only in these positions:

- statement and expression heads: `emit IDENT`, `start IDENT`, `transaction {` and
  `move |`;
- component and system member heads;
- view-item heads: `node IDENT :`, `on IDENT`, `bind IDENT`, `fill IDENT`,
  `preserve STRING`, and `key` inside a `for` head;
- resource items, start-handler items, effect clauses and shader items.

### 2. Label positions accept any keyword

A label position accepts every keyword, strict or contextual, with no `r#`:

- after `.` or `?.`;
- after `::` in a path;
- record field declarations and record-literal field labels;
- named-argument labels and attribute argument labels;
- property-path segments;
- enum variant names.

The grammar adds `label = IDENT | keyword`. Binding positions keep
`binding_identifier = IDENT`. These are `let`, parameters, patterns, declaration names
and closure parameters, so contextual keywords are allowed there and strict ones are
not.

### 3. `theme` and `env` are context bindings

`theme` and `env` are ordinary identifiers. Name resolution binds them to implicitly
injected typed context values:

| Binding | Available in                     |
| ------- | -------------------------------- |
| `theme` | View, Style and Theme expressions |
| `env`   | View only                        |

They are not grammar productions. A local binding with the same name shadows them
under ordinary lexical scoping. Reading them outside the scopes above is an
unresolved symbol, `E2001`.

### 4. Diagnostics

`E1301` now means "strict keyword used in a binding or declaration position". The
diagnostic's fix-it suggests `r#name`.

## Consequences

The spec needs these updates:

- §11 and §12 are rewritten.
- The member, path, record, argument and property-path productions in the body and in
  Appendix A are updated.
- `E1301` is redefined.

`viso-dsl` needs these updates:

- The lexer keeps a strict-keyword table only.
- The parser matches contextual keywords as `IDENT` text at the listed positions.
- A `label` rule is added, which accepts any keyword token as text.
- The formatter and LSP semantic tokens colour contextual keywords by syntactic
  position, not by lexeme.

Adding a new contextual keyword later is backward-compatible for existing identifiers.
Adding a strict keyword stays a breaking change and needs an ADR.

## As built

- Reading `env` or `theme` outside its scopes is `E2111`, not `E2001`.
- A theme reading `theme` gets its base's value; a theme without a base is `E2111`.
- A third injected binding, `game`, reads game state in a HUD (ADR 0045).
