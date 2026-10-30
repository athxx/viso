# Viso DSL — concise surface (C0 → C9)

Goal: the authoring surface the 2026-10 evaluation asked for — no line-end semicolons,
string interpolation, primary properties, enum shorthand, fit sizing that matches the
spec, brushes that are only a `fragment`, one reuse mechanism (component + slot), a
readable spec, and a real app of several thousand lines that exercises all of it.

Order: docs first (C0), then one section per commit. Each section migrates every `.vs`
source, spec example, test and golden it touches in the same commit, so the tree stays
green. Rules of the design are in `Viso_DSL_Rationale.md` §1.2.

---

## C0 — Documents

- [x] Split `Viso_DSL_1.0.md` into the language spec, `Viso_DSL_UI.md`, `Viso_DSL_Game.md`
      and `Viso_DSL_Implementation.md`; section numbers global; doc-example tests read all.
- [x] Language implementation notes moved out of the language spec, filed by section.
- [x] Rationale §1.2: the concise-surface decisions and why.
- [x] Spec prose: §14 newlines and `;`, §21 `item_end`, §61/§62 tails and statements,
      §17 interpolation, §41/§50.1 primary property, §45/§53 slots and fills,
      §57/§58 removed, §100 brush builtins and default vertex; UI U1.3 enum shorthand,
      U2 slots, U3.1 root sizing, U10.3 interpolation and `E3705`; Implementation §140.

## C1 — Newline-terminated items (§14)

- [x] Lexer/token stream: each significant token knows whether a newline precedes it.
- [x] Parser: item ends at `;`, a newline after a complete item, `}` or end of input;
      newlines inside `( )`, `[ ]` and record-expression `{ }` are whitespace.
- [x] Continuation tokens at line start (`.` `?.` `?` `::` `,` `:` `=`, compound
      assignment, `->` `=>` `as` `else`, infix-only operators); `+ - ( [ {` start a new
      item; `return`/`break` before a newline have no value.
- [x] `match` arms separated by newline or `,`.
- [x] Tail rule: the last expression item is the block's value whatever follows it;
      a `Unit` expected type discards it (HIR).
- [x] Missing separator on one line reports `E1404` with a fix inserting `;`.
- [x] Coarse parser, incremental reparse and LSP completion work without `;`.
- [x] `ui!`/`component!` source text keeps the author's line breaks.
- [x] Formatter: one item per line in expanded blocks (`;` and `match`-arm `,`
      dropped), `;` kept between items of a one-line block, dropped before `}`;
      separators kept when the parse is broken; format fuzz/golden compare tokens
      modulo separators.
- [x] Migrate every `.vs`, inline macro, spec/rationale/design-doc example, golden;
      Appendix A EBNF (`ItemEnd`) and its golden. Test sources keep `;`, still valid.
      (The ```viso-pending blocks wait on C3/C5.)
- [x] Tests: positive/negative parse, continuation table, tail discard, recovery,
      incremental edits across newlines.

## C2 — String interpolation (§17)

- [x] Lexer: `StringStart`/`StringMiddle`/`StringEnd` with a delimiter stack; `{{` `}}`;
      raw strings untouched; highlighter kinds.
- [x] Parser/CST/AST: an interpolated string node with literal parts and hole expressions.
- [x] HIR: holes typed against `Display` (`E2108` for empty holes and non-`Display`);
      lowering to the same code `format` produced; constant strings unchanged.
- [x] Remove the `format` builtin; `E2108` meaning updated; `E3705` on interpolation.
- [x] Literal-only positions (import, attribute, `asset`, `preserve`, `MessageKey`) reject
      holes with their own codes (`E3301` for `preserve`, `E3713` for `asset`, ...).
- [x] Formatter (a string with holes kept as written), LSP (definition inside holes;
      `StringEnd` ends an item), code-editor highlighting of holes, hot reload,
      incremental re-lex restarting only outside strings.
- [x] Migrate every `format(` use; text-slot fix offers `"{x}"`.

## C3 — Primary property (§50.1)

- [x] Widget schema `primary` flag: `Text.text`, `Button.text`, `LinkLabel.text`,
      `Toggle.label`, `CheckBox.label`, `Image.source`, `Svg.source`, `Video.source`
      (native schema JSON carries it; there is no `Icon` widget).
- [x] `@primary input` on user components (one per component, an `input`, not
      `@bindable`; otherwise `E3109`).
- [x] Grammar: `Type(expr)` with an optional body; a node with neither omits `{}`;
      the argument and the body start on the type's line. Appendix A `PrimaryArgument`.
- [x] HIR: the argument lowers to the primary binding; no primary → `E3101`; primary
      written in the body → `E3109` with a machine-applicable fix (override bodies
      exempt until C5 removes them).
- [x] Macros, AOT, hot reload, LSP completion (`Type($0)` snippet, primary not offered
      in the body), structured edits (`SetPropertyBinding` writes the argument; a
      bodyless node gets a body), schema output (`primary`).
- [x] Migrate sources and examples; registry `E3109`, Appendix C, `docs/diagnostics.md`.

## C4 — Enum shorthand (U1.3)

- [x] Bare variant name where a property or record field expects an enum (through
      `Option` and `if`/`match` tails); `fill` without `{}`.
- [x] Ambiguity with a value in scope → `E2002` with both fixes.
- [x] LSP completion offers variants there; migrate sources.

## C5 — Slots and reuse (§45, §53)

- [x] Grammar: `slot` / `slot name` / `slot name { default }` as view items; `name { .. }`
      fill in a component node body; remove `slot_decl`, `fill`, `SlotOutlet`, `template`,
      `use`, `part`, `override part`, `replace part`; keyword tables.
- [x] HIR: slots read from the view; default content in the definer's scope; forwarding;
      `E3003`, `E3501`, `E3502` per §45/§53; drop `E3004`, `E3601`, part codes.
- [x] Schema `slots` output; LSP; hot reload identities.
- [x] Migrate tests (`templates.rs`, `view_controls.rs`, slot unit tests, goldens).

## C6 — Shader brush builtins (§100)

- [ ] `viewport_size`, `rect_pos`, `rect_size`, `time` implicit; declared ones → `E8107`.
- [ ] Default vertex when `vertex` is absent; `uv`/`local` varyings only when read.
- [ ] Layout: builtins first; `BrushLayout` and host record fields follow.
- [ ] Backends (MSL/WGSL/HLSL/CPU) unchanged output for a hand-written equivalent.
- [ ] Migrate every shader; update `shader_brush`, `shader_check`, CLI and LSP tests.

## C7 — Sizing defaults (U3.1)

- [ ] Containers default to `fit` as in U3.1; the view root fills the window.
- [ ] Headless layout checks for root fill and container fit.
- [ ] Remove hard-coded sizes from examples; screenshot each example.

## C8 — Real-scale app

- [ ] An example app of several thousand lines of `.vs` (navigation, lists, forms,
      async, resources, i18n, adaptive layout, custom brush, slots) built and run on
      the desktop host; notes on what was awkward fed back as fixes.

## C9 — Close

- [ ] Appendix C / `docs/diagnostics.md` / CLI goldens current; spec, UI and
      Implementation docs read through once for stale syntax.
- [ ] `viso-ide` tree-sitter grammar and highlight tests: newline items, string
      holes, primary arguments, slots.
