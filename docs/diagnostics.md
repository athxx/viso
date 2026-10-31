# Diagnostic codes

Generated from the compiler's diagnostics registry (`crates/dsl/src/diag/codes.rs`) by its tests; `viso explain <code>` prints the same text. Do not edit by hand.

| Code | Title | Severity | Stage |
| ---- | ----- | -------- | ----- |
| [E1001](#e1001) | Unsupported Language Version | error | manifest |
| [E1101](#e1101) | Identifier Normalization Conflict | error | resolve |
| [E1102](#e1102) | Confusable Identifier | warning | lex |
| [E1201](#e1201) | Unterminated String | error | lex |
| [E1202](#e1202) | Unterminated Block Comment | error | lex |
| [E1203](#e1203) | Invalid Numeric Literal | error | lex |
| [E1204](#e1204) | Unknown Unit Suffix | error | lex |
| [E1205](#e1205) | Invalid Escape Sequence | error | lex |
| [E1206](#e1206) | Misplaced Numeric Separator | error | lex |
| [E1207](#e1207) | Invalid Color Literal | error | lex |
| [E1208](#e1208) | Character Literal Not One Character | error | lex |
| [E1209](#e1209) | Invalid Source Character | error | lex |
| [E1210](#e1210) | Too Many Raw String Hashes | error | lex |
| [E1301](#e1301) | Strict Keyword Used As A Name | error | parse |
| [E1401](#e1401) | Unclosed Delimiter | error | parse |
| [E1402](#e1402) | Unmatched Closing Delimiter | error | parse |
| [E1403](#e1403) | Unexpected Tokens | error | parse |
| [E1404](#e1404) | Missing Token | error | parse |
| [E1405](#e1405) | Expected Expression | error | parse |
| [E1406](#e1406) | Nesting Too Deep | error | parse |
| [E1407](#e1407) | Source Too Large | error | parse |
| [E2001](#e2001) | Unresolved Symbol | error | resolve |
| [E2002](#e2002) | Ambiguous Name | error | resolve |
| [E2003](#e2003) | Initialization Cycle | error | resolve |
| [E2004](#e2004) | Missing Turbofish Or Const Marker | error | parse |
| [E2005](#e2005) | No Component To Mount | error | resolve |
| [E2006](#e2006) | Several Components To Mount | error | resolve |
| [E2101](#e2101) | Float Is Not A Type | error | resolve |
| [E2102](#e2102) | Illegal Implicit Numeric Conversion | error | type |
| [E2103](#e2103) | Type Mismatch | error | type |
| [E2104](#e2104) | State Initializer Forward Reference | error | type |
| [E2105](#e2105) | Computed Dependency Cycle | error | type |
| [E2106](#e2106) | MixedLength Cannot Narrow Before Layout | error | type |
| [E2107](#e2107) | Illegal Dimensional Operation | error | type |
| [E2108](#e2108) | Value Not Shown In A String | error | type |
| [E2109](#e2109) | Length Divided By Constant Zero | error | type |
| [E2110](#e2110) | Assignment Target Is Not Writable | error | type |
| [E2111](#e2111) | Context Read Outside Its Scope | error | type |
| [E2112](#e2112) | Compile-Time Argument Rejected | error | type |
| [E2201](#e2201) | Trait Bound Not Satisfied | error | type |
| [E2202](#e2202) | Overlapping Or Ambiguous Impl | error | type |
| [E2203](#e2203) | Unbounded Generic Instantiation | error | type |
| [E2301](#e2301) | Non-Exhaustive Match | error | type |
| [E2302](#e2302) | Unreachable Pattern | warning | type |
| [E2303](#e2303) | Refutable Pattern In Binding Position | error | type |
| [E2401](#e2401) | Closure Parameter Cannot Be Inferred | error | type |
| [E2501](#e2501) | Effect Kind Violation | error | effect |
| [E2502](#e2502) | Side Effect In View Or Computed | error | effect |
| [E2601](#e2601) | Missing Capability | error | effect |
| [E2701](#e2701) | Type Cannot Be A StableKey | error | type |
| [E2702](#e2702) | Duplicate Runtime Key (reserved) | error | runtime |
| [E2801](#e2801) | Record Expression In Control Head Needs Parentheses | error | parse |
| [E2802](#e2802) | Chained Non-Associative Operator | error | parse |
| [E2803](#e2803) | Misplaced Control Transfer | error | type |
| [E3001](#e3001) | Reserved Word Child | error | view |
| [E3002](#e3002) | View Cardinality Not Satisfied (reserved) | error | view |
| [E3003](#e3003) | No Default Slot | error | view |
| [E3101](#e3101) | Unknown Property | error | view |
| [E3102](#e3102) | Property Bound Twice | error | view |
| [E3103](#e3103) | Property Is Not Two-Way | error | view |
| [E3104](#e3104) | Percent Without Percent Basis | error | view |
| [E3105](#e3105) | Indefinite Percent Basis | warning | view |
| [E3106](#e3106) | Non-Finite Length | warning | view |
| [E3107](#e3107) | Bind Source Is Not A State Lens | error | view |
| [E3108](#e3108) | Invalid Grid Area Or Cell | error | view |
| [E3109](#e3109) | Misplaced Primary Property | error | view |
| [E3201](#e3201) | Arrow Event Handler | error | view |
| [E3202](#e3202) | Unknown Event Or Wrong Payload | error | view |
| [E3301](#e3301) | Invalid Preserve Identity | error | view |
| [E3401](#e3401) | View For Without Key | error | view |
| [E3402](#e3402) | Unstable Key Expression | warning | view |
| [E3501](#e3501) | Unknown Slot | error | view |
| [E3502](#e3502) | Slot Conflict | error | view |
| [E3701](#e3701) | Invalid Bindable Input | error | view |
| [E3702](#e3702) | Misplaced Parent-Provided Property | error | view |
| [E3703](#e3703) | Invalid Transition | error | view |
| [E3704](#e3704) | Interactive Node Without Role Or Name | warning | view |
| [E3705](#e3705) | Concatenated Localizable Text | warning | view |
| [E3706](#e3706) | Message Catalog Mismatch | error | view |
| [E3707](#e3707) | Invalid VirtualList Item Template | error | view |
| [E3708](#e3708) | Interactive Node Without Keyboard Path | warning | view |
| [E3709](#e3709) | Property Not Yet Supported By Runtime (reserved) | error | view |
| [E3710](#e3710) | Invalid Styleable Or Selector Member | error | view |
| [E3711](#e3711) | View Cannot Be Mounted | error | view |
| [E3712](#e3712) | Invalid Migrate Function | error | view |
| [E3713](#e3713) | Invalid Asset | error | view |
| [E4101](#e4101) | Await Outside a Task | error | async |
| [E4102](#e4102) | Task Reads State After Suspending | error | async |
| [E4201](#e4201) | Effect Reads an Undeclared Dependency | error | effect |
| [E4202](#e4202) | Reactive Cycle | error | runtime |
| [E4203](#e4203) | Effect Run Policy Does Not Fit Its Dependencies | error | effect |
| [E4204](#e4204) | Adaptive Cycle | error | runtime |
| [E4301](#e4301) | Resource Missing Load or Key | error | resource |
| [E4302](#e4302) | Invalid Resource or Task Policy | error | resource |
| [E4401](#e4401) | Start Target Is Not a Task | error | async |
| [E4501](#e4501) | Detached Task | error | async |
| [E5101](#e5101) | State Reset on Hot Reload | warning | hot_reload |
| [E5102](#e5102) | Stable Identity Collision on Reload | error | hot_reload |
| [E5103](#e5103) | Game Reload Tier | note | hot_reload |
| [E5104](#e5104) | Duplicate Stable Id | error | resolve |
| [E5105](#e5105) | Misplaced Sensitive Attribute | error | resolve |
| [E6101](#e6101) | Native Declaration Does Not Match the Schema | error | native |
| [E6102](#e6102) | Native Ownership or Thread Domain Violation | error | native |
| [E6103](#e6103) | Capability Denied | error | runtime |
| [E6104](#e6104) | Native Unavailable on the Target | error | native |
| [E7101](#e7101) | Execution Budget Exceeded | error | runtime |
| [E7102](#e7102) | Memory Budget Exceeded | error | runtime |
| [E7103](#e7103) | Arithmetic Fault | error | runtime |
| [E7104](#e7104) | Index Out of Bounds | error | runtime |
| [E7105](#e7105) | Function Cannot Run | error | runtime |
| [E7106](#e7106) | Native Function Failed | error | runtime |
| [E8101](#e8101) | Host-Only Type in a Shader | error | shader |
| [E8102](#e8102) | F64 in a Shader | error | shader |
| [E8103](#e8103) | Shader Loop Without a Static Bound | error | shader |
| [E8104](#e8104) | Shader ABI Mismatch | error | shader |
| [E8105](#e8105) | Construct Outside the Shader Subset | error | shader |
| [E8106](#e8106) | Render Profile Stage Rule Violated | error | shader |
| [E8107](#e8107) | Brush Profile Violated | error | shader |
| [E9101](#e9101) | System Order Cycle | error | game |
| [E9102](#e9102) | Fixed Tick Budget Exceeded | error | runtime |
| [E9103](#e9103) | Simulation Reaches Local State | error | game |
| [E9104](#e9104) | Non-Deterministic Source in the Simulation | error | game |
| [E9105](#e9105) | Simulation State Without Snapshot | error | game |
| [E9106](#e9106) | Invalid Persisted State | error | game |
| [E9107](#e9107) | Input Action Missing a Device Binding | warning | game |
| [E9108](#e9108) | Audio Realtime Rule Violated | error | game |
| [E9109](#e9109) | UI Member in a System | error | game |
| [E9110](#e9110) | Misplaced Probe | error | game |
| [E9111](#e9111) | Persisted State Failed to Load or Store | error | runtime |
| [E9112](#e9112) | Invalid Game Query | error | game |

## E1001

**Unsupported Language Version** — error, manifest

A package pins the language version it is written in with the `language` key of its `Viso.toml` (or its lockfile); individual `.vs` files never restate it. The compiler only accepts the exact version it implements, so that a package is never silently compiled under rules it was not written for. This error is raised when the pinned version is unknown or differs from the compiler's version, and its primary location is the version value in the manifest.

```text
# Viso.toml
[package]
name = "app"
language = "2.0"    # this compiler implements "1.0"
```

Fix: Set `language` in `Viso.toml` to the version the compiler implements, or build the package with a compiler that implements the pinned version.

```text
[package]
name = "app"
language = "1.0"
```

See also: `viso check`

## E1101

**Identifier Normalization Conflict** — error, resolve

Identifiers are normalized to Unicode NFC before they enter the symbol table, so two spellings that are NFC-equal name the same symbol. Declaring a name twice in one namespace with spellings that differ only in normalization (for example a precomposed `é` and `e` followed by a combining accent) is rejected, because the two declarations look identical but are written with different code points. The related location points at the earlier declaration; two declarations with the exact same spelling are an ordinary duplicate instead.

```vs
const café = 1;
const café = 2;

component Menu {
    view { Text("menu") }
}
```

Fix: Give the second declaration a distinct name, or remove it if it was meant to be the same symbol. Write identifiers in one consistent normalization form (NFC); most editors do this when the file is saved as NFC.

```vs
const cafe_price = 1;
const cafe_count = 2;
```

## E1102

**Confusable Identifier** — warning, lex

An identifier mixes Unicode scripts in a way that is easy to confuse visually, such as a Latin word containing a Cyrillic `а`. Such names look like an existing ASCII name but are a different symbol, which hides bugs and can be used to disguise code. Detection follows the UTS #39 mixed-script rules at the Highly Restrictive level; natural mixes such as `café`, `名前かな` or `ユーザーID` are not reported. This is a warning by default.

```vs
component Checkout {
    state pаy = 0;
    view { Text("checkout") }
}
```

Fix: Retype the identifier using characters from a single script (here, replace the Cyrillic `а` with the Latin `a`).

```vs
state pay = 0;
```

## E1201

**Unterminated String** — error, lex

A string, raw string or character literal was opened but never closed. Ordinary string and character literals cannot span physical lines, so a missing closing quote is detected at the end of the line; a raw string must end with a quote followed by exactly as many `#` as it opened with. The usual cause is a forgotten closing quote or a line break inside a string.

```vs
component Greeting {
    state message = "hello;
    view { Text(message) }
}
```

Fix: Add the missing closing quote. Write a line break inside a string as `\n`, and close a raw string with the same number of `#` it was opened with.

```vs
state message = "hello";
```

## E1202

**Unterminated Block Comment** — error, lex

A block comment opened with `/*` reaches the end of the file without a matching `*/`. Everything after the opener would be swallowed by the comment, so the lexer reports the opener instead of guessing where the comment was meant to end.

```vs
component Notes {
    /* draft notes
    state count = 0;
    view { Text("notes") }
}
```

Fix: Close the comment with `*/` where it is meant to end, or use `//` line comments.

```vs
/* draft notes */
state count = 0;
```

## E1203

**Invalid Numeric Literal** — error, lex

A numeric literal is malformed: a radix-prefixed integer (`0x`, `0o`, `0b`) has no digits or a digit outside its radix, or a suffix does not fit the body it follows. A unit literal takes no exponent (`1e2dp`), an integer suffix only follows an integer body, and a float suffix only follows a float body. The whole run is kept as one bad token so the rest of the file still parses.

```vs
component Spacer {
    state gap = 1e2dp;
    view { Text("spacer") }
}
```

Fix: Pair each suffix with a body it fits: write unit literals without an exponent, put integer suffixes only on integers and float suffixes only on floats, and use only digits valid for a literal's radix.

```vs
state gap = 100dp;
```

## E1204

**Unknown Unit Suffix** — error, lex

A number is immediately followed by a suffix outside the closed set of units and numeric types. The unit suffixes are `dp`, `px`, `sp`, `em`, `%`, `ns`, `us`, `ms`, `s`, `min`, `deg`, `rad`, `turn`, `hz` and `khz`; the type suffixes are `i8`–`i64`, `u8`–`u64`, `f32` and `f64`. The set is fixed so that every literal has one known type and libraries cannot introduce new lexical forms; custom quantities use ordinary constructors or newtypes.

```vs
component Title {
    view {
        Text("title") { font_size: 12pt }
    }
}
```

Fix: Use one of the supported unit suffixes (for font sizes usually `sp`), or construct a custom quantity with a function or newtype instead of a suffix.

```vs
Text("title") { font_size: 12sp }
```

## E1205

**Invalid Escape Sequence** — error, lex

A string or character literal contains an escape the language does not define, or a malformed one. The supported escapes are `\\`, `\"`, `\'`, `\n`, `\r`, `\t`, `\0`, `\xNN` (exactly two hex digits, at most U+007F) and `\u{H...H}` (one to six hex digits, at most U+10FFFF and not a surrogate). Anything else, such as `\q` or `\u{110000}`, is rejected.

```vs
component Path {
    state label = "C:\qtemp";
    view { Text(label) }
}
```

Fix: Use one of the supported escapes: write a literal backslash as `\\`, or use a raw string, which takes no escapes.

```vs
state label = r"C:\qtemp";
```

## E1206

**Misplaced Numeric Separator** — error, lex

A digit separator `_` in a numeric literal is not between two digits. Separators may not lead, trail or repeat, and may not touch a radix prefix (`0x_1`), the decimal point, the exponent or a suffix. The rule keeps the boundary between a number and its suffix unambiguous.

```vs
component Budget {
    state limit = 1__000;
    view { Text("budget") }
}
```

Fix: Put each `_` between two digits, with at most one in a row.

```vs
state limit = 1_000;
```

## E1207

**Invalid Color Literal** — error, lex

A color literal must be `#` followed by exactly 3, 4, 6 or 8 hexadecimal digits (`#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`), read as sRGB with alpha `FF` when omitted. Any other digit count, or a non-hexadecimal character, is rejected immediately rather than guessed at.

```vs
component Badge {
    view {
        Text("new") { color: #12345 }
    }
}
```

Fix: Write the color with 3, 4, 6 or 8 hex digits.

```vs
Text("new") { color: #123456 }
```

## E1208

**Character Literal Not One Character** — error, lex

A character literal must contain exactly one Unicode scalar value after escapes are decoded. Writing several characters (or none) between single quotes is rejected; text of any other length is a `String` and uses double quotes.

```vs
component Initial {
    state initial = 'ab';
    view { Text("initial") }
}
```

Fix: Keep exactly one character between the single quotes, or use a double-quoted string for longer text.

```vs
state initial = 'a';
```

## E1209

**Invalid Source Character** — error, lex

The source contains a character that cannot appear in Viso text: a NUL byte, a carriage return that is not part of a `\r\n` line ending, or a character that cannot begin any token (such as `¡` outside a string). These usually come from copy-pasted text, binary data or a stray key press.

```vs
component Shop {
    state price = 0;
    view { Text("shop") }
    ¡
}
```

Fix: Delete the stray character, or move it inside a string literal if it is meant as text. Save the file with `\n` or `\r\n` line endings and no NUL bytes.

## E1210

**Too Many Raw String Hashes** — error, lex

A raw string is delimited by `r`, zero to 255 `#` characters and a quote, and must close with a quote followed by the same number of `#`. An opener with more than 255 `#` exceeds the format's limit and is rejected. In practice one or two `#` are always enough to enclose any text.

```vs
component Banner {
    state art = r################################################################################################################################################################################################################################################################"x"################################################################################################################################################################################################################################################################;
    view { Text("banner") }
}
```

Fix: Use the smallest number of `#` that does not appear after a quote inside the text, usually one.

```vs
state art = r#"x"#;
```

## E1301

**Strict Keyword Used As A Name** — error, parse

A strict keyword (such as `fn`, `type`, `match`, `in` or `let`) appears where a name is bound or declared: a `let`, a parameter, a pattern binding, a closure parameter, an import alias, or any declaration name. Strict keywords are always lexed as keywords, so they can never be names there. Contextual keywords such as `state` or `view` are allowed as names, and label positions (member names after `.`, path segments after `::`, record field names, named-argument labels) accept any keyword.

```vs
component Search {
    action run_query() {
        let match = 1;
    }
    view { Text("search") }
}
```

Fix: Rename the binding. If the name must match an external schema symbol spelled like a keyword, escape it with `r#`.

```vs
let found = 1;
```

## E1401

**Unclosed Delimiter** — error, parse

An opening `(`, `[` or `{` has no matching closing delimiter before the enclosing construct or the file ends. The parser keeps the partial construct and continues, but everything after the opener is read as part of it until the error is fixed.

```vs
component Total {
    state total = 0;
    view { Text("total") }
```

Fix: Add the missing closing delimiter where the construct ends (here, the `}` that closes the component).

See also: `viso fmt`

## E1402

**Unmatched Closing Delimiter** — error, parse

A closing `)`, `]` or `}` has no opening delimiter to match. It is usually left over after deleting or moving a block, or an extra brace at the end of a component.

```vs
component Panel {
    view { Text("panel") }
}
}
```

Fix: Remove the extra closing delimiter, or add the opener it was meant to close.

See also: `viso fmt`

## E1403

**Unexpected Tokens** — error, parse

The parser found tokens that cannot begin any declaration, member or statement at this point. It collects them into an error node and resumes at the next construct, so a single stray fragment yields one diagnostic instead of a cascade. Typical causes are stray operators, misplaced punctuation or code from another language.

```vs
component Card {
    %
    view { Text("card") }
}
```

Fix: Delete the stray tokens or turn them into a valid member or statement (for example a `state`, `action` or `view` member inside a component).

See also: `viso fmt`

## E1404

**Missing Token** — error, parse

The grammar requires a specific token here, such as `=`, `:` or a closing keyword, and it is absent; two items written on one line need a `;` between them. The parser inserts a placeholder for the missing token and continues, so the rest of the declaration is still checked.

```vs
type Id I64

component Profile {
    view { Text("profile") }
}
```

Fix: Insert the token the message names at the reported position.

```vs
type Id = I64;
```

See also: `viso fmt`

## E1405

**Expected Expression** — error, parse

The parser needs an expression here but found something else: an operator with no right operand, an empty initializer, or a closing delimiter. It is usually an unfinished edit.

```vs
component Sum {
    state total = 1 +;
    view { Text("sum") }
}
```

Fix: Complete the expression, or remove the dangling operator.

```vs
state total = 1 + 2;
```

## E1406

**Nesting Too Deep** — error, parse

Expressions, types, patterns, blocks and view bodies may nest at most 256 levels deep. The innermost construct past the budget is kept as an error node and reported once, which bounds the stack depth of the parser and of every later pass over the syntax tree. This normally only happens with generated code.

```text
// A source whose nesting exceeds 256 levels, for example an expression
// written with 257 nested parentheses:
component Deep {
    state n = ((((((((/* ...257 levels in total... */ 1))))))));
    view { Text("deep") }
}
```

Fix: Flatten the construct: bind intermediate results with `let`, move parts into a function, or split a deep view into child components.

## E1407

**Source Too Large** — error, parse

A single source file may contain at most 2^20 (1,048,576) significant tokens. Everything past the budget is kept as one error node and reported once, which bounds parser time and memory. This normally only happens with generated code or embedded data.

```text
// A single .vs file with more than 1,048,576 significant tokens, for example
// a generated table:
const TABLE: List<I64> = [0, 1, 2, 3, /* ...over a million more... */];
```

Fix: Split the source into several modules, or load large data at run time through a resource instead of writing it as literals.

## E2001

**Unresolved Symbol** — error, resolve

A name does not refer to anything in scope: no local, member, declaration, import or prelude item has that name in the namespace where it is used. The same code covers imports of modules or names that do not exist or are not exported, unknown native functions, methods, fields and enum variants. When close candidates exist, the diagnostic lists the nearest ones by edit distance as fixes.

```vs
component Counter {
    state count = 0;
    action increment() {
        count = cuont + 1;
    }
    view { Text("counter") }
}
```

Fix: Correct the spelling to an existing name, declare the symbol, or import it from the module that exports it (adding `export` to its declaration if it is private).

```vs
count = count + 1;
```

See also: `viso check`

## E2002

**Ambiguous Name** — error, resolve

One name refers to more than one declaration, so the resolver cannot pick one. This covers a name declared twice in the same namespace and scope (including a member that repeats the name of a `fn`, `action`, `task` or `const`), and two source files that derive the same module path, and a bare name written where an enum is expected (`align: center`) that is both a variant of that enum and a value in scope, and a type declared under the name of a built-in type (`Unit`, `String`, `List`), which every annotation already means. Names must be unique within a namespace so that every use resolves to exactly one symbol.

```vs
component Counter {
    state count = 0;
    state count = 1;
    view { Text("counter") }
}
```

Fix: Rename or remove one of the declarations so each name is declared once; for colliding module paths, rename or move one of the files; for a bare variant, write `Enum::variant` for the variant or `(name)` for the value; give a type its own name rather than a built-in one.

```vs
state count = 0;
state total = 1;
```

## E2003

**Initialization Cycle** — error, resolve

Values whose initialization depends on each other in a cycle cannot be computed in any order. This covers themes that reach each other through their base or item expressions, styles whose base chain loops, and modules that import each other. Each declaration on the cycle is reported.

```vs
theme Dark: Night {}
theme Night: Dark {}

component Screen {
    view { Text("screen") }
}
```

Fix: Break the cycle: make one declaration stand on its own (for a theme, give it its own `colors` and `elevation` items or base it on a theme outside the cycle), so the dependencies form a chain instead of a loop.

## E2004

**Missing Turbofish Or Const Marker** — error, parse

Explicit generic arguments in an expression must be written with a turbofish, `name::<T>(..)` or `Pair::<A, B> { .. }`; the form `name<T>(..)` is read as comparisons and rejected. A const generic argument must be marked `const`, as in `Matrix<F32, const 4, const 4>`. Both rules let the parser tell comparisons from generics and types from constants without consulting the symbol table.

```vs
component Loader {
    action load() {
        let items = decode<I64>("1");
    }
    view { Text("loader") }
}
```

Fix: Insert `::` before the generic arguments in an expression, and mark const arguments with `const`.

```vs
let items = decode::<I64>("1");
```

## E2005

**No Component To Mount** — error, resolve

A source compiled as a mountable unit must declare a component for the host to mount. A file that declares only records, functions, constants or other non-component items has nothing to mount. A file that declares only systems runs under a scheduler and is exempt.

```vs
record Point {
    x: F32;
    y: F32;
}
```

Fix: Declare the component to mount in this file, or import this file's declarations from the file that declares it.

```vs
component App {
    view { Text("app") }
}
```

## E2006

**Several Components To Mount** — error, resolve

A source that declares more than one component must say which one is mounted. When no component, or more than one, is marked `export`, the choice is ambiguous and the file is rejected. Exactly one exported component, or exactly one component in total, makes the entry point unambiguous.

```vs
component Header {
    view { Text("header") }
}

component Page {
    view { Header {} }
}
```

Fix: Mark the component to mount with `export`, or move the other components into their own files.

```vs
export component Page {
    view { Header {} }
}
```

## E2101

**Float Is Not A Type** — error, resolve

The language has no `Float` type; floating-point values are `F32` or `F64`. Writing `Float` in any type position (a parameter, return type, field, variant payload, `let`, `const`, member, generic argument, function type or `as` cast) is reported once by name resolution and does not cascade into further type errors. It usually comes from habits carried over from other languages.

```vs
component Gauge {
    state level: Float = 0.5;
    view { Text("gauge") }
}
```

Fix: Write `F64` (or `F32` when single precision is enough) instead of `Float`.

```vs
state level: F64 = 0.5;
```

## E2102

**Illegal Implicit Numeric Conversion** — error, type

Only lossless widening happens implicitly: `I8` to `I64` along the signed ladder, `U8` to `U64` along the unsigned ladder, and `F32` to `F64`. Narrowing, changing signedness, and moving between integers and floats can lose information, so they must be written as an explicit cast. This typically fires when an integer value is returned or assigned where a float or a smaller integer is expected.

```vs
component Counter {
    state n: I64 = 3;
    fn half() -> F64 { n }
    view { Text("counter") }
}
```

Fix: Convert the value with an explicit `as` cast to the expected numeric type.

```vs
fn half() -> F64 { n as F64 }
```

## E2103

**Type Mismatch** — error, type

An expression's type does not match the type its position expects, such as a declared state type, a return type, a function argument or a view property. Types are checked nominally and there are no implicit conversions between unrelated types such as `String` and `I64` or `Dp` and `Px`. The message names the expected and the found type.

```vs
component A {
    state n: I64 = "x";
    view { Text("a") }
}
```

Fix: Change the expression so it produces the expected type, or change the declared type to match the value. For text, show a displayable value in a string, `"{value}"`.

```vs
state n: I64 = 0;
```

See also: `viso schema Text`

## E2104

**State Initializer Forward Reference** — error, type

A `state` initializer may read only the states declared before it in source order. States are initialized once, top to bottom, so reading a later state would observe a value that does not exist yet. Computed values are evaluated on demand and may refer forward; states may not.

```vs
component A {
    state total = count * 2;
    state count = 1;
    view { Text("a") }
}
```

Fix: Move the state being read above the one that reads it, or turn the dependent value into a `computed`.

```vs
state count = 1;
state total = count * 2;
```

## E2105

**Computed Dependency Cycle** — error, type

The computed values of a component must form an acyclic dependency graph, because each one is derived from the values it reads. When a computed depends on itself, directly or through other computeds, it has no well-defined value. The message lists the cycle path, such as `a` -> `b` -> `a`.

```vs
component A {
    computed a: I64 = b + 1;
    computed b: I64 = a;
    view { Text("a") }
}
```

Fix: Break the cycle: make one of the values on the path depend on a `state`, an `input` or a constant instead of another computed in the loop.

```vs
state base: I64 = 0;
computed a: I64 = b + 1;
computed b: I64 = base;
```

## E2106

**MixedLength Cannot Narrow Before Layout** — error, type

Adding lengths of different units, such as `10dp + 5px` or `100% - 8dp`, produces a `MixedLength`, whose concrete size is known only during layout. Such a value cannot be typed as a single unit like `Dp` or `Px` before layout. This fires when a mixed sum is assigned, returned or passed where a concrete unit is declared.

```vs
component A {
    fn gap() -> Dp { 10dp + 5px }
    view { Text("a") }
}
```

Fix: Declare the target as `MixedLength`, or keep the arithmetic within a single unit.

```vs
fn gap() -> MixedLength { 10dp + 5px }
```

## E2107

**Illegal Dimensional Operation** — error, type

Some operations on lengths have no meaning before layout: a `MixedLength` cannot be compared, multiplied by or divided by another length, a length cannot be multiplied by a length or used with `%`, a scalar cannot be divided by a length, and a `Percent` cannot be added to a plain scalar. These rules keep length arithmetic dimensionally consistent. Scaling a length by a number remains allowed.

```vs
component A {
    fn narrow() -> Bool { (100% - 8dp) < 200dp }
    view { Text("a") }
}
```

Fix: Rewrite the expression so it only adds lengths or scales them by numbers, and compare only values of one concrete unit.

```vs
fn narrow(width: Dp) -> Bool { width < 200dp }
```

## E2108

**Value Not Shown In A String** — error, type

A string's hole (`"{count} items"`) shows its value as text, which takes a type with `Display`: the scalars, `String`, `Char` and the single-unit lengths, durations, angles and frequencies have it; records, enums, lists, options, tuples, colors and similar types do not and must be mapped to text first, for example with a `match`. A hole with nothing in it is this error too; a literal brace in a string is written `{{` or `}}`.

```vs
record Point { x: I64, y: I64 }

component A {
    state at = Point { x: 1, y: 2 }
    view { Text("at {at} {}") }
}
```

Fix: Show a displayable value in each hole, such as a field of the record, and put something in every hole.

```vs
    view { Text("at {at.x}, {at.y}") }
```

## E2109

**Length Divided By Constant Zero** — error, type

Dividing a length-family value (`Dp`, `Px`, `Sp`, `Em`, `Percent` or `MixedLength`) by an expression that folds to the constant `0` is rejected during constant folding. The result would be an infinite or undefined size, which layout cannot use.

```vs
component A {
    fn cell() -> Dp { 8dp / (1 - 1) }
    view { Text("a") }
}
```

Fix: Divide by a non-zero constant, or remove the division.

```vs
fn cell() -> Dp { 8dp / 2 }
```

## E2110

**Assignment Target Is Not Writable** — error, type

Only a `state`, a `let mut` local, a `mut` parameter, or a field or index path rooted in one of them can be assigned or mutated in place. Inputs, computed values, constants, resource values, plain `let` bindings and ordinary parameters are read-only. The same rule applies to mutating list methods such as `push` and `remove`.

```vs
component A {
    input limit: I64;
    action reset() { limit = 0; }
    view { Text("a") }
}
```

Fix: Assign to a writable place instead: declare the value as `state`, bind the local with `let mut`, or mark the parameter `mut`. To change an input, emit an event and let the parent update it.

```vs
state limit: I64 = 10;
action reset() { limit = 0; }
```

## E2111

**Context Read Outside Its Scope** — error, type

`env` and `theme` are context bindings of the view execution scope: they are read in view items, property bindings, view conditions, view handlers and the arguments of pure calls made there, and in a `computed`, which reads them per instance and again as they change. `tr` reads `env.locale` and so is used only within a component instance (its view, styles, computeds, actions and effects). State and input initializers, member functions, tasks, constants, themes and module-level declarations cannot read them implicitly; pass the needed value in as a parameter.

```vs
component A {
    state scale = env.text_scale;
    view { Text("a") }
}
```

Fix: Read the context value inside the view or a `computed`, or pass it as an argument to the function that needs it.

```vs
fn scaled(scale: F32) -> F32 { scale * 2.0 }
view { Text("{scaled(env.text_scale)}") }
```

## E2112

**Compile-Time Argument Rejected** — error, type

Some native calls are evaluated by the compiler: `@const` natives that build compile-time constants such as an `InputMap`, and native parameters declared as tick durations, which are converted to whole ticks of the fixed step. When such an argument is a constant but out of range, for example a dead-zone radius above 1 or a negative duration, the compiler rejects it. A non-constant argument in these positions is reported separately as an effect violation.

```vs
import viso::game::{InputAction, InputMap};

@derive(InputAction) enum Act { Jump; }

const CONTROLS: InputMap<Act> = InputMap::new().dead_zone(1.5);

system Host {}
```

Fix: Pass a value inside the accepted range: a dead zone of at least 0 and below 1, or a duration of zero or more ticks that fits `I64`.

```vs
const CONTROLS: InputMap<Act> = InputMap::new().dead_zone(0.15);
```

See also: `viso schema InputMap`

## E2201

**Trait Bound Not Satisfied** — error, type

A generic argument must implement every trait its parameter is bounded by, and an `impl` must match its trait exactly: provide every required member, add no foreign members, and keep each signature. Generic bodies are checked against their bounds, so an unbounded `T` cannot use `==` or call trait methods. This code also covers a bound that is not a trait and a derive that does not apply to its type.

```vs
trait Shape {
    fn area(self) -> I64;
}

fn total<T: Shape>(x: T) -> I64 { return x.area(); }

component A {
    state n = 0;
    action go() { n = total(1); }
    view { Text("a") }
}
```

Fix: Pass a type that implements the trait, add the missing `impl`, or add the bound the generic body needs.

```vs
record Square { side: I64; }
impl Shape for Square { fn area(self) -> I64 { return self.side * self.side; } }
action go() { n = total(Square { side: 2 }); }
```

## E2202

**Overlapping Or Ambiguous Impl** — error, type

Each trait may have at most one impl that can apply to a given type, so method dispatch always has a single answer. Two impls of the same trait for the same type overlap. A method call is ambiguous when several traits implemented for the receiver provide a method of that name.

```vs
trait Shape {
    fn area(self) -> I64;
}

record Square { side: I64; }

impl Shape for Square { fn area(self) -> I64 { return self.side * self.side; } }
impl Shape for Square { fn area(self) -> I64 { return 0; } }

component A {
    view { Text("a") }
}
```

Fix: Remove or merge the duplicate impl. For an ambiguous method call, name the trait explicitly, as in `Shape::area(value)`.

## E2203

**Unbounded Generic Instantiation** — error, type

Generic callables are monomorphized for each set of type arguments. When instantiation recurses with ever-growing types, or exceeds the limits on chain depth (32), total instances (4096) or type-argument size (256 nodes), the compiler stops and reports it. This usually comes from a generic function that calls itself with a type built from its own parameter.

```vs
record Pair<A, B> { a: A; b: B; }

fn grow<T>(x: T, n: I64) -> I64 {
    if n == 0 { return 0; }
    return grow(Pair { a: x, b: x }, n - 1);
}

component A {
    state k = 0;
    action go() { k = grow(1, 3); }
    view { Text("a") }
}
```

Fix: Make the recursive call use the same type arguments it was called with, or replace the type-level recursion with a list or an enum that holds the growing structure at runtime.

## E2301

**Non-Exhaustive Match** — error, type

A `match`, in expressions and in views, must cover every possible value of its scrutinee. Missing a variant would leave some values without a result. The message names the cases that are not covered.

```vs
enum Tab { Home; Settings; }

component A {
    state tab = Tab::Home;
    view {
        match tab {
            Tab::Home => { Text("home") },
        }
    }
}
```

Fix: Add arms for the missing cases, or a final `_` arm when they should share one result.

```vs
match tab {
    Tab::Home => { Text("home") },
    Tab::Settings => { Text("settings") },
}
```

## E2302

**Unreachable Pattern** — warning, type

Match arms are tried in order, so an arm whose pattern is fully covered by the arms before it can never run. This usually means a catch-all `_` or binding arm was placed too early, or a case was written twice.

```vs
enum Shape { Dot; Circle(I64); }

fn size(s: Shape) -> I64 {
    match s {
        _ => 0,
        Shape::Dot => 1,
    }
}

component A {
    view { Text("a") }
}
```

Fix: Remove the unreachable arm, or move the more specific arms before the catch-all.

```vs
match s {
    Shape::Dot => 1,
    _ => 0,
}
```

## E2303

**Refutable Pattern In Binding Position** — error, type

The patterns of `let`, `for` and closure parameters must match every value of their type, because there is no other branch to take when they fail. A pattern that can fail, such as `Some(x)`, an enum variant of a multi-variant enum or a literal, is refutable and is only allowed in `match`.

```vs
component A {
    fn first(o: Option<I64>) -> I64 {
        let Some(x) = o;
        x
    }
    view { Text("a") }
}
```

Fix: Use a `match` with an arm for the values the pattern does not cover.

```vs
fn first(o: Option<I64>) -> I64 {
    match o {
        Some(x) => x,
        None => 0,
    }
}
```

## E2401

**Closure Parameter Cannot Be Inferred** — error, type

A closure takes its parameter and result types from the function type it is checked against, such as the parameter type of the function it is passed to or a `let` annotation. When no such expected type exists and a parameter carries no annotation, its type cannot be determined.

```vs
component A {
    fn make() {
        let same = |x| x;
    }
    view { Text("a") }
}
```

Fix: Annotate the parameter, or give the binding a function type.

```vs
let same = |x: I64| x;
let same: Fn(I64) -> I64 = |x| x;
```

## E2501

**Effect Kind Violation** — error, effect

Each kind of body allows a fixed set of effects. Only actions and event handlers may write state, emit events or call actions; `fn` bodies, tasks, state, record and constant initializers, and theme items must be pure. Positions that are evaluated at compile time also reject non-constant expressions and non-`@const` calls.

```vs
component A {
    state n: I64 = 0;
    fn bump() { n += 1; }
    view { Text("a") }
}
```

Fix: Move the mutation into an `action` (or a view handler), or make the function return the new value and let an action store it.

```vs
action bump() { n += 1; }
```

## E2502

**Side Effect In View Or Computed** — error, effect

Views and computed values are re-evaluated whenever what they read changes, possibly many times and in any order, so they must be pure. Writing state, emitting events or calling an action inside a computed body or a view property binding is rejected. Side effects belong in actions and event handlers.

```vs
component A {
    state n: I64 = 0;
    computed doubled: I64 = { n = 2; n * 2 };
    view { Text("a") }
}
```

Fix: Keep the computed a pure expression of what it reads, and perform the write in an action or a handler such as `on click`.

```vs
computed doubled: I64 = n * 2;
```

## E2601

**Missing Capability** — error, effect

Native functions declare the capabilities they need, such as `network` or `timer`, with a `requires` clause, and a call needs the capabilities of its callee. A private callable without a clause infers its set from the call graph. An explicit `requires` clause is a contract and an upper bound, so a callable whose call graph needs a capability the clause does not list is a static error.

```vs
native fn fetch(url: String) -> String requires { network };

fn load() -> String requires { timer } { fetch("https://example.com") }

component A {
    view { Text("a") }
}
```

Fix: Add the missing capability to the `requires` clause of the callable (and of every caller with a clause), or remove the call.

```vs
fn load() -> String requires { network } { fetch("https://example.com") }
```

## E2701

**Type Cannot Be A StableKey** — error, type

The `key` of a resource and the `key` of a keyed view `for` must have a `StableKey` type, so that equal keys stay equal across runs and reloads. Integers, `Bool`, `Char`, `String`, `Unit`, and tuples, records, options and enums built only from them qualify. Floats, lists and other collections do not; the message names the offending member, such as `K.b`.

```vs
component A {
    state xs = [1.5, 2.5];
    view {
        Column {
            for x in xs key x { Text("row") }
        }
    }
}
```

Fix: Key by a stable value such as an integer id or a string; for rows, give each item an id field.

```vs
record Row { id: I64; value: F64; }
state xs = [Row { id: 1, value: 1.5 }];
for x in xs key x.id { Text("row") }
```

## E2702

**Duplicate Runtime Key** — error, runtime, reserved: specified, not raised yet

The keys of a keyed `for` identify its rows, so that each row keeps its node, state and animation when the list changes. Two items producing the same key value at runtime make that identity ambiguous. The key type is checked at compile time, but duplicates depend on the data and are detected while the app runs.

```text
A keyed list whose data contains two items with the same id:

    record Row { id: I64; name: String; }

    component List {
        state rows = [Row { id: 1, name: "a" }, Row { id: 1, name: "b" }];
        view {
            Column {
                for row in rows key row.id { Text(row.name) }
            }
        }
    }

Rendering this list yields two rows with key `1`.
```

Fix: Make every key unique within the list: key by a unique id, or deduplicate the data before rendering it.

## E2801

**Record Expression In Control Head Needs Parentheses** — error, parse

In the head of `if`, `while`, `for` and `match`, a `{` after a path is read as the start of the body block. A record expression written bare there would be ambiguous with the body, so it must be parenthesized.

```vs
record P { x: I64; }

fn check() -> Bool {
    if P { x: 1 }.x > 0 { return true; }
    return false;
}

component A {
    view { Text("a") }
}
```

Fix: Wrap the record expression in parentheses.

```vs
if (P { x: 1 }).x > 0 { return true; }
```

See also: `viso fmt`

## E2802

**Chained Non-Associative Operator** — error, parse

Comparison, equality and range operators are non-associative: `a < b < c`, `a == b != c` and `a .. b .. c` have no single obvious meaning, so chaining them at the same level is an error rather than a silently chosen grouping.

```vs
fn between(a: I64, b: I64, c: I64) -> Bool { a < b < c }

component A {
    view { Text("a") }
}
```

Fix: Split the chain into separate comparisons joined by `&&`, or add parentheses that state the intended grouping.

```vs
fn between(a: I64, b: I64, c: I64) -> Bool { a < b && b < c }
```

## E2803

**Misplaced Control Transfer** — error, type

`break` and `continue` are valid only inside a loop body, and not across a closure boundary; `break` with a value is allowed only in `loop`, not in `while` or `for`. `return` is valid only inside a callable body. A control transfer outside these positions has no target to jump to.

```vs
component A {
    fn stop() { break; }
    view { Text("a") }
}
```

Fix: Move the `break` or `continue` into a loop, use `return` to leave a function, and use `loop` when the break carries a value.

```vs
fn stop() { return; }
```

## E3001

**Reserved Word Child** — error, view

`child` is a reserved word in a view and cannot name a node, a member or a node prefix. A bare `Type { }` inside a node body is already an anonymous child node, and `node id: Type { }` is a named one, so there is no `child` keyword for placing children. The word is kept reserved so that code written in the style of other UI languages gets a clear diagnostic instead of being read as a component name.

```vs
component A {
    view {
        Column {
            child Text("a")
        }
    }
}
```

Fix: Delete `child`: write the child node directly in the parent's body, or give it a name with `node id: Type { }`. The diagnostic carries a machine-applicable fix that removes the word.

```vs
Column {
    Text("a")
}
```

## E3002

**View Cardinality Not Satisfied** — error, view, reserved: specified, not raised yet

A view block that must produce exactly one root node produces zero or several. A component's `view` root and a `ui! { ... }` fragment embedded in Rust each have to generate exactly one root node, because the runtime mounts them as a single node with a single identity. Writing two sibling nodes at the top of such a block, or a block that can produce no node at all, violates that cardinality.

```text
// Rust: a `ui!` fragment must generate exactly one root node.
let fragment = ui! {
    Text("Title")
    Text("Subtitle")
};
```

Fix: Wrap the sibling nodes in one container (or a `Fragment` placed inside a container) so the block has a single root.

```text
let fragment = ui! {
    Column {
        Text("Title")
        Text("Subtitle")
    }
};
```

## E3003

**No Default Slot** — error, view

A node body contains bare child items, but the node's type has no default slot to receive them. Bare structure items in a node body go into the type's default slot `children`, the one a component's view places with a bare `slot`; leaf widgets such as `Text`, `Button` and `TextInput` have no slots at all, and a component whose view places only named slots accepts children only through `name { ... }`. This usually happens when a node is nested inside a leaf widget.

```vs
component A {
    view {
        Column {
            Text("Title") {
                Text("Subtitle")
            }
        }
    }
}
```

Fix: Move the child out to a container that has a default slot, or fill one of the component's named slots with `name { ... }`; if it is your own component, place a bare `slot` in its view.

```vs
Column {
    Text("Title")
    Text("Subtitle")
}
```

See also: `viso schema <Component>`

## E3101

**Unknown Property** — error, view

A node binds a property that its type's schema does not declare. Widgets expose a fixed schema, user components expose their `input`s, parent-provided properties such as `grid.row` come from the parent container; any other name is rejected. The usual cause is a misspelling, and the diagnostic suggests the nearest known name. In a `style`, a property that is not styleable is reported the same way.

```vs
component A {
    view {
        Text("a") { opacty: 0.5 }
    }
}
```

Fix: Use a property the schema declares (often the suggested spelling); query the schema to see every property of the type.

```vs
Text("a") { opacity: 0.5 }
```

See also: `viso schema <Component>`, `viso schema <Component> --json`

## E3102

**Property Bound Twice** — error, view

The same property is bound more than once in one node body (with `:` or `bind`), or one style block. Each property of a node has exactly one binding, so a second one would silently override the first. The diagnostic points at the repeated binding and at the first one.

```vs
component A {
    view {
        Text("a") { opacity: 0.5; opacity: 1.0 }
    }
}
```

Fix: Keep a single binding for the property; to choose between values, put the choice in the expression.

```vs
Text("a") { opacity: if dim { 0.5 } else { 1.0 } }
```

## E3103

**Property Is Not Two-Way** — error, view

`bind property <=> lens` is written on a property that does not support two-way binding. Only properties the schema marks two-way can be bound: widget values such as `Toggle.checked`, `Slider.value` or `TextInput.text`, and component inputs marked `@bindable(event)`. Other properties are one-way and take a plain `property: value;` binding.

```vs
component A {
    state label: String = "a";

    view {
        Text { bind text <=> label; }
    }
}
```

Fix: Bind the property one way with `:`; for a component input, mark it `@bindable(event)` if the component should write it back.

```vs
Text(label)
```

See also: `viso schema <Component>`

## E3104

**Percent Without Percent Basis** — error, view

A `Percent` value, or a `MixedLength` with a percent component, reaches a length property whose schema declares no percent basis. A percentage only means something relative to a known reference length, such as the parent's size for `width`; properties like `translate` (`Offset`), `inset` or `estimated_extent` have none. The value is followed through `state`, `computed`, locals and field access, and a user component input that forwards to such a property has no basis either.

```vs
component A {
    view {
        Text("a") {
            translate: Offset { x: 50%, y: 0dp };
        }
    }
}
```

Fix: Use an absolute length (such as `dp`) for this property, or move the percentage to a property that has a percent basis, like `width`.

```vs
Text("a") {
    translate: Offset { x: 24dp, y: 0dp };
}
```

See also: `viso schema <Component>.<property>`

## E3105

**Indefinite Percent Basis** — warning, view

A debug-runtime warning: during layout a length with a percent component had no definite reference length to resolve against. This happens when the parent is `Fit` on that axis, along the scrolling axis of a `Scroll`, or under an unbounded constraint; `fill` on an unbounded axis also degrades to `fit`. A value that is only a percentage falls back to the property's default sizing, and the percent part of a mixed length counts as zero. The runtime reports it once per node and the release build stays silent.

```text
// Inside a Scroll the column axis is unbounded, so `50%` has no basis.
Scroll {
    Column {
        height: 50%;
        Text("a")
    }
}
// debug runtime: E3105 warning: the percent basis of `height` is indefinite
```

Fix: Give the parent a definite size on that axis (a fixed length or `fill` under a bounded parent), or use an absolute length instead of a percentage where the axis is unbounded.

## E3106

**Non-Finite Length** — warning, view

A debug-runtime warning: a length resolved at run time to a non-finite value (infinity or NaN), for example through a dynamic division by zero. Non-finite values never enter layout or rendering; the property takes its schema default instead, and the `length_nonfinite` counter is incremented. The release build does not panic and reports nothing.

```text
component A {
    state parts: I64 = 0;

    view {
        Row {
            // `parts` is 0 at run time, so the width is infinite.
            width: 320dp / parts;
            Text("a")
        }
    }
}
// debug runtime: E3106 warning: `width` resolved to a non-finite length
```

Fix: Guard the computation so it always yields a finite length, for example by branching on the divisor.

```text
width: if parts > 0 { 320dp / parts } else { 320dp };
```

## E3107

**Bind Source Is Not A State Lens** — error, view

The right-hand side of `bind property <=> lens` is not a writable state lens. A two-way binding writes the new value back, so the lens must be a `state` of the component or a field or index path into one (`settings.volume`, `items[0]`). An `input`, `computed`, `const`, local binding, loop variable or function result cannot be written and is rejected.

```vs
component A {
    state on: Bool = true;
    computed off: Bool = !on;

    view {
        Toggle { bind checked <=> off; }
    }
}
```

Fix: Bind to the `state` itself (or a path into it), or bind one way with `:` and handle the widget's change event yourself.

```vs
Toggle { checked: off; on changed(e) { on = !e.value; } }
```

## E3108

**Invalid Grid Area Or Cell** — error, view

A grid's `areas` or a child's `grid.*` names no cells. Each string of `areas` is a row of whitespace-separated area names, `.` an empty cell: every row names the same number of cells, and each area fills a rectangle. A child's `grid.area` names an area of its grid's `areas` and places the child by itself, so it is not combined with `grid.column`, `grid.row` or a span; a `grid.column_span` or `grid.row_span` spans at least one track. The names resolve at compile time, so none reaches the runtime.

```vs
component A {
    view {
        Grid {
            areas: ["head head", "side main"];
            Text("a") { grid.area: "foot" }
        }
    }
}
```

Fix: Name an area of the grid's `areas`, or add the area to the template.

```vs
Text("a") { grid.area: "main" }
```

## E3109

**Misplaced Primary Property** — error, view

A node type has at most one primary property, written in parentheses after the type: `Text("Hello")`, `Button("Save") { on click { .. } }`. Binding it by name in the node's body is rejected, so each property has one spelling; the fix moves the value into the parentheses. A user component names its primary input with `@primary`: only an `input` takes it, only one input per component, and never a two-way (`@bindable`) one.

```vs
component A {
    view {
        Text { text: "Hello" }
    }
}
```

Fix: Write the primary property as `Type(value)`, and mark one one-way `input` of a component `@primary`.

```vs
Text("Hello")
```

## E3201

**Arrow Event Handler** — error, view

An event handler is written with an arrow, `on event => statement`. Handlers are blocks: `on click { ... }`, or `on click(event) { ... }` to name the payload. The arrow form is rejected so that a handler is always a block of statements with one shape.

```vs
component A {
    state n: I64 = 0;

    view {
        Button("Add") { on click => n += 1 }
    }
}
```

Fix: Wrap the statement in a block; the diagnostic carries a machine-applicable fix that does this.

```vs
Button("Add") { on click { n += 1; } }
```

## E3202

**Unknown Event Or Wrong Payload** — error, view

A handler names an event the node does not accept, or an `emit` does not match its event. A user component node accepts the standard events and the events the component declares; a built-in widget accepts the events its schema declares plus the standard ones, and `FocusScope`, `KeyShortcut` and `Fragment` take no standard events. `emit name(...)` must name an event of its own component and give each parameter exactly once, by position or as `name: value`. A misspelled event name is the usual cause, and the diagnostic suggests the nearest one.

```vs
component A {
    state n: I64 = 0;

    view {
        Button("Add") { on clik { n += 1; } }
    }
}
```

Fix: Use an event the node declares (often the suggested spelling), and give an `emit` exactly the parameters its event declares.

```vs
Button("Add") { on click { n += 1; } }
```

See also: `viso schema <Component>`

## E3301

**Invalid Preserve Identity** — error, view

`if condition preserve "name"` keeps a branch's nodes and state alive while it is switched away. The name after `preserve` must be a compile-time string literal, and it must be unique among the preserved branches of the component, because it is the identity under which the branch instance is cached. A computed name, or the same name used twice, is rejected; the diagnostic for a duplicate points at the first use.

```vs
component A {
    state open: Bool = false;

    view {
        Column {
            if open preserve "panel" { Text("a") }
            if !open preserve "panel" { Text("b") }
        }
    }
}
```

Fix: Give each preserved branch its own string literal. For a dynamic collection, use a keyed `for` instead of `preserve`.

```vs
if open preserve "open-panel" { Text("a") }
if !open preserve "closed-panel" { Text("b") }
```

## E3401

**View For Without Key** — error, view

A `for` in a view has no `key` clause. Every view `for` must be written `for item in items key expression { ... }`: the key gives each item a stable identity, so reordering, inserting or removing items moves the retained nodes, child state, focus and tasks with their item instead of rebuilding them by position. The key must implement `StableKey` and should be a value of the item, such as an id.

```vs
component A {
    state names: List<String> = ["a", "b"];

    view {
        Column {
            for name in names {
                Text(name)
            }
        }
    }
}
```

Fix: Add `key` followed by an expression that identifies the item independent of its position.

```vs
for name in names key name {
    Text(name)
}
```

## E3402

**Unstable Key Expression** — warning, view

Repeated content that carries per-item state (an event handler, a reactive binding or a nested list) has no stable key. Without a key that follows the item, reordering the list rebuilds each item's state against a different item instead of moving it. The key expression may only read the loop pattern, immutable outer values and pure functions, and must be unique per item. The compiler reports this alongside the missing-key error when the `for` body is stateful.

```vs
component A {
    state names: List<String> = ["a", "b"];
    state picked: String = "";

    view {
        Column {
            for name in names {
                Button(name) { on click { picked = name; } }
            }
        }
    }
}
```

Fix: Key the `for` by a stable value of the item, such as its id, not its position.

```vs
for name in names key name {
    Button(name) { on click { picked = name; } }
}
```

## E3501

**Unknown Slot** — error, view

A fill `name { ... }` names a slot the placed component does not have. A component's slots are the `slot name` items of its view, plus `children` when it places a bare `slot`; a native container's slots are those of its schema. The usual cause is a misspelling, and the diagnostic suggests the nearest name.

```vs
component Card {
    view {
        Column { slot footer }
    }
}

export component App {
    view {
        Card {
            foter { Text("Done") }
        }
    }
}
```

Fix: Name a slot the component places; query its schema to list them.

```vs
Card {
    footer { Text("Done") }
}
```

See also: `viso schema <Component>`

## E3502

**Slot Conflict** — error, view

A slot is placed or filled in a way that has no single meaning. A component's view places each slot once and never inside a `for`, which would put its caller's nodes once per item; a caller fills each slot once and does not mix bare children with a `children { ... }` fill. A native container's slot also has a cardinality, checked over the number of nodes each item can produce: `Scroll` takes exactly one child, a node counts one, `if` without `else` can produce zero, and `for` any number.

```vs
component A {
    view {
        Scroll {
            Text("a")
            Text("b")
        }
    }
}
```

Fix: Place and fill each slot once; give a native slot as many nodes as it takes, for example by wrapping several nodes in one container.

```vs
Scroll {
    Column {
        Text("a")
        Text("b")
    }
}
```

See also: `viso schema <Component>`

## E3701

**Invalid Bindable Input** — error, view

`@bindable(event)` makes a component input two-way, so callers can write `bind input <=> state;`. It marks only an `input`, takes exactly one event name, and that event must be declared in the same component with a first parameter of the input's type. The component never assigns the input; it emits the event with the requested new value, and the binding writes it back to the caller's state.

```vs
component Stepper {
    @bindable(moved)
    input value: I64;

    event changed(value: I64);

    view { Text("step") }
}
```

Fix: Name an event of the component whose first parameter has the input's type, and put the attribute on an `input`.

```vs
@bindable(changed)
input value: I64;

event changed(value: I64);
```

## E3702

**Misplaced Parent-Provided Property** — error, view

A parent-provided property (`grid.*`, `stack.*`, `absolute.*`) is written on a node whose direct parent does not provide it, or whose parent cannot be determined statically. These properties are declared by the parent container's schema and only mean something there, such as a grid cell. `Fragment`, `if`, `match` and `for` do not count as parents, but a user component's default slot does hide the real container, so the parent is unknown.

```vs
component A {
    view {
        Row {
            Text("a") { grid.row: 1 }
        }
    }
}
```

Fix: Place the node directly in the container that provides the prefix, or drop the property.

```vs
Grid {
    Text("a") { grid.row: 1 }
}
```

See also: `viso schema <Component>`

## E3703

**Invalid Transition** — error, view

`transition.p` animates changes of property `p` on the same node. `p` must be marked animatable in the schema (such as `opacity`, `background` or `translate`), and the value must be a `Transition` record. A transition on a non-animatable property like `text`, or a bare duration instead of a `Transition`, is rejected.

```vs
component A {
    view {
        Text("a") {
            transition.opacity: 200ms;
        }
    }
}
```

Fix: Give an animatable property a `Transition` value.

```vs
Text("a") {
    transition.opacity: Transition { duration: 200ms };
}
```

See also: `viso schema <Component>`

## E3704

**Interactive Node Without Role Or Name** — warning, view

A node that is not a standard interactive widget (for example a `Row`, or the root of a component's view) handles `click`, `tap`, `long_press`, `drag_*` or `key_down`, but assistive technology cannot describe it. Such a node needs a `semantics.role` other than `Role::group` and an accessible name, from `semantics.label` or text in its subtree. This is a warning by default; `viso check --a11y strict` makes it an error.

```vs
component A {
    state n: I64 = 0;

    view {
        Row {
            focusable: true;
            semantics.label: "Add";
            on click { n += 1; }
        }
    }
}
```

Fix: Give the node a role and a name, or use a standard interactive widget such as `Button`.

```vs
Row {
    focusable: true;
    semantics.role: button;
    semantics.label: "Add";
    on click { n += 1; }
}
```

See also: `viso check --a11y strict`

## E3705

**Concatenated Localizable Text** — warning, view

A localizable property (`Text.text`, `Button.text`, `Toggle.label`, `TextInput.placeholder`, `semantics.label`, `semantics.hint` and similar) is built by string concatenation or by a string with holes that holds literal words or more than one hole. Word order differs between languages, so such text cannot be translated; a message with named arguments through `tr` can. `"{n}"` with a single hole is allowed. This is a warning by default; `viso check --i18n strict` makes it an error and also reports plain literals.

```vs
component A {
    state name: String = "Ada"

    view {
        Text("Hello {name}")
    }
}
```

Fix: Move the sentence into the message catalog and pass the variable parts as named arguments of `tr`.

```vs
Text(tr("greeting", name: name))
```

See also: `viso check --i18n strict`

## E3706

**Message Catalog Mismatch** — error, view

A `tr` call or `MessageKey` literal does not match the package's message catalog, or the catalog itself has an error. A key written as a string literal must be a message of the source-locale catalog (`i18n/en.toml` by default), each message argument must be passed exactly once by name with a compatible kind, and a `MessageKey` literal must name a message without arguments. Using `tr` in a package with no catalog is also reported, as are catalog problems: invalid TOML or message syntax, invalid or duplicate locales, a missing source catalog, or translations using arguments the source does not declare.

```vs
import viso::i18n::tr;

component A {
    view {
        Text(tr("inbox.title"))
    }
}
```

Fix: Add the message to the source catalog (`i18n/en.toml` beside `Viso.toml`), or use a key the catalog has (the diagnostic suggests the nearest), and pass exactly the arguments the message declares.

```vs
# i18n/en.toml
[inbox]
title = "Inbox"
```

See also: `viso check`

## E3707

**Invalid VirtualList Item Template** — error, view

A `VirtualList` mounts only the items inside its viewport and overscan, so its body is an item template rather than ordinary children. The body must contain exactly one keyed `for`, which the compiler lowers to the virtualized template. A `for` without `key`, several `for`s, or any other child node in the body is rejected.

```text
VirtualList {
    Text("Header")            // not allowed: a child beside the template
    for message in messages key message.id {
        MessageRow { message: message; }
    }
}
```

Fix: Keep a single keyed `for` as the body of the `VirtualList`, and put headers or other nodes outside it.

```text
Column {
    Text("Header")
    VirtualList {
        for message in messages key message.id {
            MessageRow { message: message; }
        }
    }
}
```

See also: `viso schema VirtualList`

## E3708

**Interactive Node Without Keyboard Path** — warning, view

A node that is not a standard interactive widget handles a pointer or key gesture but cannot be operated from the keyboard. An equivalent keyboard path means the node is `focusable: true` and handles `click`, which keyboard activation also triggers; handling only `tap` or `pointer_*` does not count. This is a warning by default; `viso check --a11y strict` makes it an error.

```vs
component A {
    state n: I64 = 0;

    view {
        Row {
            semantics.role: button;
            Text("Add")
            on click { n += 1; }
        }
    }
}
```

Fix: Make the node focusable and handle `click`, or use a standard interactive widget such as `Button`.

```vs
Row {
    focusable: true;
    semantics.role: button;
    Text("Add")
    on click { n += 1; }
}
```

See also: `viso check --a11y strict`

## E3709

**Property Not Yet Supported By Runtime** — error, view, reserved: specified, not raised yet

A property whose schema entry is marked as not yet implemented by the runtime is given a non-default value. The schema and type checking of such entries are fixed, but the runtime does not apply them, so a non-default value would silently do nothing. The debug runtime reports it and the release build rejects the value.

```text
// Suppose the schema marks `Text.max_lines` as pending in the runtime.
Text("A long paragraph") {
    max_lines: 2;          // non-default value: E3709
}
```

Fix: Leave the property at its default until the runtime supports it, or reach the effect through a supported property; `viso schema` shows which entries are pending.

See also: `viso schema <Component>`

## E3710

**Invalid Styleable Or Selector Member** — error, view

`@styleable` and `@selector` expose a component member to styles. `@styleable` marks only an `input`, which a `style` for the component may then bind. `@selector` marks a `Bool` `input`, `state` or `computed`, published as a selector named after the member that a style's `when` can test. A selector may not take the name of a standard selector the runtime decides: `hover`, `pressed`, `focused`, `focus_visible`, `disabled` or `dragging`.

```vs
component Chip {
    @selector
    input count: I64 = 0;

    view { Text("chip") }
}
```

Fix: Put `@selector` on a `Bool` member with a name of its own, and `@styleable` on an `input`.

```vs
@selector
input selected: Bool = false;
```

## E3711

**View Cannot Be Mounted** — error, view

Part of the view cannot be lowered to something the runtime mounts. Common causes: a control-flow region (`if`, `match`, `for`) at the root of a view, where the root must be one node; a component that mounts itself directly or indirectly; a property or handler the runtime has no path for on that widget; `padding` on a node without a content box; a grid's `areas` or a child's `grid.area` that reads a value; a layout property a style binds to a value; a `bind` to a component input with `using`; properties forwarded to a component whose view does not mount exactly one node; and styles switching a user component's input by an interaction selector, or switching under `when` a property the runtime does not deliver or a layout property they give no base. The message names the specific reason.

```vs
component A {
    view {
        Column {
            Text("a") { padding: 8dp }
        }
    }
}
```

Fix: Address the reason the message names. For `padding` on a node without a content box, pad a container around it; for a region at the view root, wrap it in a container so the root is one node.

```vs
Column {
    padding: 8dp;
    Text("a")
}
```

See also: `viso check`, `viso schema <Component>`

## E3712

**Invalid Migrate Function** — error, view

`@migrate(from: "Type")` declares a function that converts persisted state of an older type into the current one when the declared type of a `state` changes. It marks only a `fn`, `from` must be given as a string spelling a type, the function takes exactly one parameter of that type, and it must declare its return type. Two `@migrate` functions with the same `from` and return type are ambiguous and rejected.

```vs
@migrate(from: "I64")
fn widen(old: I64) { }

component A {
    view { Text("a") }
}
```

Fix: Mark a `fn` with one parameter of the `from` type and declare the type it returns.

```vs
@migrate(from: "I64")
fn widen(old: I64) -> F64 { old as F64 }
```

## E3713

**Invalid Asset** — error, view

`asset("path")` names a file under the package's `assets/` directory and is checked at compile time, so the package knows every asset it ships. It takes exactly one argument, a string literal without interpolation; the path is relative to `assets/` and may not climb out of it; the file must exist and be an image, an SVG or a video. A compile without an assets directory, and a record expression building an `ImageAsset`, `SvgAsset` or `VideoAsset` directly, are also rejected. To choose between assets at run time, branch between `asset("...")` values.

```vs
component A {
    state dark: Bool = false;
    state logo: String = "images/logo.png";

    view {
        Image(asset(logo)) { width: 48dp }
    }
}
```

Fix: Pass a string literal naming an existing file under `assets/`, and branch between literal assets when the choice is dynamic; for a missing file, use the path the diagnostic suggests.

```vs
Image(if dark { asset("images/logo_dark.png") } else { asset("images/logo.png") }) {
    width: 48dp;
}
```

## E4101

**Await Outside a Task** — error, async

`await` marks a suspension point, and only a `task` body may suspend. Actions, event handlers, effects, computed values and functions run to completion inside one transaction on the UI thread, so they cannot wait for a future. This is raised when `await` appears in any body that is not a `task`.

```vs
import viso::time;

export component Timer {
    state done = false;

    task wait() -> I64 {
        await time::sleep(1s);
        3
    }

    action go() {
        let n = await wait();
        done = true;
    }

    view { Text("timer") { on click { go(); } } }
}
```

Fix: Move the waiting into a `task` and start it from the action; handle its result in a `success` handler.

```vs
action go() {
    start wait() { success(n) { done = true; } };
}
```

See also: `viso check`

## E4102

**Task Reads State After Suspending** — error, async

A task sees the state and inputs as they were when it started, but only until its first suspension point (an `await` or a call to another task). After that the component may have changed, so reading state, inputs, computed values, or a `fn` that reads them is rejected, as is any such read inside a loop that suspends. A read that slips past the check, for example through a closure created before the suspension, ends the task with this code at run time.

```vs
import viso::time;

export component Poller {
    state count = 0;
    state seen = 0;

    task look() -> I64 {
        await time::sleep(1s);
        count
    }

    view {
        Text("poll") {
            on click { start look() { success(v) { seen = v; } }; }
        }
    }
}
```

Fix: Read what the task needs before it first suspends (or pass it as an argument), and apply the result to current state in the `success` handler.

```vs
task look(start: I64) -> I64 {
    await time::sleep(1s);
    start
}
```

See also: `viso check`

## E4201

**Effect Reads an Undeclared Dependency** — error, effect

An effect's `when (...)` list is its complete dependency list: the effect runs again only when those values change. Reading a state, input or computed in the body that the list does not cover would make the effect silently stale, so it is rejected. A computed in the list does not cover the values it is derived from, because it may stay equal while they change.

```vs
export component Search {
    state query = "";
    state page = 0;

    effect log when (query) {
        let at = page;
    }

    view { Text(query) }
}
```

Fix: Add the value to the `when` list, or wrap a read that should not retrigger the effect in `untracked(..)`.

```vs
effect log when (query, page) {
    let at = page;
}
```

See also: `viso check`

## E4202

**Reactive Cycle** — error, runtime

A frame's state flush keeps scheduling computed values, bindings and effects while new writes appear. If writes are still pending after a bounded number of rounds, the reactive graph is feeding itself: typically an effect writes one of its own dependencies, or a computed's evaluation reaches itself. The runtime stops that frame's flush, reports the dropped cells and the effects they would wake, and keeps the values already written without running their reactions. An effect that runs 16 frames in a row only because effects wrote what it reads is cut the same way. Under `viso run`, the report names each state at its declaration and each effect at its body, rendered like a compiler diagnostic.

```text
Running this component and clicking the text raises E4202: the effect
depends on `count` and writes `count`, so every run schedules another.

    export component Loop {
        state count = 0;
        view { Text("tap") { on click { count += 1; } } }
        effect bump when (count) run EffectRun::change {
            transaction { count += 1; }
        }
    }
```

Fix: Break the loop: do not write an effect's own dependencies from its body. Derive the value with a `computed`, or move the write into the action that causes the change.

```text
computed shown = count + 1;
```

## E4203

**Effect Run Policy Does Not Fit Its Dependencies** — error, effect

The `run` policy of an effect must agree with its `when` list. Without `when`, an effect can only run on mount (`EffectRun::mount`); `EffectRun::change` and `EffectRun::mount_and_change` need a non-empty `when (...)`; and `EffectRun::mount` cannot have one. An unknown policy name is also rejected.

```vs
export component Theme {
    state name = "light";

    effect save run EffectRun::change {
    }

    view { Text(name) }
}
```

Fix: Give a change-driven effect the values it watches, or drop the policy to run once on mount.

```vs
effect save when (name) run EffectRun::change {
}
```

See also: `viso check`

## E4204

**Adaptive Cycle** — error, runtime

Within a frame, layout settles the adaptive environment (constraints and size class), the structure it selects is updated, and layout runs again until nothing changes. If the structure keeps moving the very constraints that select it, the loop does not settle within its bound. The runtime then keeps the new environment values, drops the reactions to them and keeps the last structure, instead of measuring forever. It usually means a structural branch depends on a size the subtree itself determines.

```text
A view whose `if env.size_class == SizeClass::Compact` branch mounts a
wide subtree, which pushes the enclosing adaptive scope out of compact,
which unmounts the subtree, which makes it compact again. The size class
flips on every layout and the frame stops with E4204.
```

Fix: Let structural branches depend only on values fixed before the node is measured: `env.constraints` and the size class of an adaptive scope whose basis comes from its parent. Drive measured sizes into properties (width, spacing, color) rather than into `if` / `match` / `for` structure.

## E4301

**Resource Missing Load or Key** — error, resource

A `resource` block must say how to load its value (`load = ...`) and what identifies a load (`key = ...`), each exactly once. Every other item (`policy`, `scope`) may also appear at most once. This is raised when `load` or `key` is missing or any item is repeated.

```vs
import viso::time;

export component Profile {
    state user = 1;

    task fetch(id: I64) -> Result<I64, String> {
        await time::sleep(1s);
        Ok(id)
    }

    resource score: Resource<I64, String> {
        load = fetch(user);
    }

    view { Text("profile") }
}
```

Fix: Give the resource both a loader and a key, once each.

```vs
resource score: Resource<I64, String> {
    load = fetch(user);
    key = user;
}
```

See also: `viso check`

## E4302

**Invalid Resource or Task Policy** — error, resource

A resource `policy` is a list of `ResourcePolicy::keep_latest`, `debounce(d)`, `cache_for(d)` and `cache_errors`, each at most once, with `d` a constant duration above zero; `cache_errors` needs `cache_for`, and `scope` is `ResourceScope::component` or `ResourceScope::shared`. A `start` block takes exactly one `TaskPolicy` (`keep_latest`, `drop_new`, `queue` or `parallel(n)`), only on a named slot (`start task() as slot { ... }`), and each handler at most once. Anything else is rejected.

```vs
import viso::time;

export component Uploader {
    state sent = 0;

    task upload() -> I64 {
        await time::sleep(1s);
        1
    }

    action go() {
        start upload() {
            policy = [TaskPolicy::queue];
            success(n) { sent += n; }
        };
    }

    view { Text("upload") { on click { go(); } } }
}
```

Fix: Name the slot the policy governs, and give one valid policy.

```vs
start upload() as uploads {
    policy = [TaskPolicy::queue];
    success(n) { sent += n; }
};
```

See also: `viso check`

## E4401

**Start Target Is Not a Task** — error, async

`start` launches a task on the UI task protocol, and a resource's `load` runs one; both need a call to a `task` (or a native task). A plain function, an action or any other expression has nothing to run asynchronously, so it is rejected. Call an action directly instead of starting it.

```vs
export component Counter {
    state count = 0;

    action reset() {
        count = 0;
    }

    action go() {
        start reset();
    }

    view { Text("reset") { on click { go(); } } }
}
```

Fix: Call an action directly; use `start` only with a task call.

```vs
action go() {
    reset();
}
```

See also: `viso check`

## E4501

**Detached Task** — error, async

Every started task belongs to a component instance: it is cancelled when that instance unmounts or reloads, and its handlers run against that instance. A module-level `action` has no instance, so a `start` in it would create a task nobody owns. This is raised for a `start` in an action declared outside any component.

```vs
task wait() -> I64 { 3 }

action go() {
    start wait();
}

export component Panel {
    view { Text("panel") }
}
```

Fix: Start the task from an action, event handler or effect of the component that owns it.

```vs
export component Panel {
    action go() { start wait(); }
    view { Text("panel") { on click { go(); } } }
}
```

See also: `viso check`

## E5101

**State Reset on Hot Reload** — warning, hot_reload

When a reload changes a state's type, the live value is carried into the new type when the new type can hold it, or through a `@migrate` function from the old type. If neither works, because the value does not fit or the migrate function faults, the state restarts from its new initializer and the reload reports this warning. The rest of the reload still commits.

```text
Before the edit, the running view holds `count == 300`:

    state count = 0;

After the edit the state is an `I8`, which cannot hold 300, and there is
no `@migrate` function, so `count` resets to 1 with E5101:

    state count: I8 = 1;
```

Fix: If the value matters, add a `@migrate` function from the old type that always produces a valid new value; otherwise the reset is expected and the warning can be ignored.

```text
@migrate(from: "I64")
fn clamp(old: I64) -> I8 { if old > 127 { 127 } else { 0 } }
```

See also: `viso run`

## E5102

**Stable Identity Collision on Reload** — error, hot_reload

A reload migrates each live state into the cell of the new build with the same stable identity. If two states of the candidate build end up with the same identity, the reload would migrate two states into one cell, so the whole reload is rejected and the running build keeps its last good state. It typically follows from a `@stable` pin that spells the identity another state already has.

```text
A view running with `state count = 0;` is edited to add a second state
pinned with `@stable(...)` to the identity that `count` already mints from
its name. The candidate has two states with one stable identity, the
reload is rejected with E5102, and the old build keeps running.
```

Fix: Give every state its own identity: remove the conflicting `@stable` pin, or choose an id no other declaration of the component uses.

See also: `viso run`

## E5103

**Game Reload Tier** — note, hot_reload

On a game reload the compiler compares the running build with the candidate by stable identity and picks the least disruptive layer every change allows: for example presentation-only, logic-only, or a world rebuild. This note reports the chosen tier, its primary span at the first change that decided it and every other change as a related span. It is informational and does not block the reload.

```text
Editing a running game's simulation hook from `steps += 1;` to
`steps += 2;` and a presentation helper from `x * 0.5f32` to `x * 0.25f32`
reports:

    game reload: logic-only (simulation code changed: `Mover.fixed_update`)

with the presentation change as a related span.
```

Fix: No fix is needed. If the tier is more disruptive than expected, check the changes it lists: keep simulation code, state layout and the input map unchanged when only presentation should reload.

See also: `viso run`

## E5104

**Duplicate Stable Id** — error, resolve

`@stable("id")` pins a declaration's identity so that it survives renames across reloads. Two declarations or members of the same kind in one module that claim the same id would mint the same symbol identity, so the second claim is rejected. This usually comes from copying a pinned member and forgetting to change its id.

```vs
export component Counter {
    @stable("count")
    state clicks = 0;
    @stable("count")
    state taps = 0;

    view { Text("count") }
}
```

Fix: Give each pinned declaration a distinct id.

```vs
@stable("count")
state clicks = 0;
@stable("taps")
state taps = 0;
```

See also: `viso check`

## E5105

**Misplaced Sensitive Attribute** — error, resolve

`@sensitive` marks a component `state` whose value a dev snapshot written to disk leaves out: `viso run --keep-state` keeps the app's state for the next session, but never a token, a password or what else the state holds that must not reach the disk. Only a component's `state` holds such a value; a system's states belong to its game, and an input, a computed or a function holds none of its own.

```vs
export component Login {
    @sensitive
    input token: String;

    view { Text(token) }
}
```

Fix: Mark the `state` the value is kept in.

```vs
@sensitive
state token = "";
```

See also: `viso check`

## E6101

**Native Declaration Does Not Match the Schema** — error, native

Native APIs come from the generated native schema registry. A handwritten `native` declaration must match the registry entry for its library exactly (same kind, signature and capabilities) and be written only in native schema types: no records or other DSL-only types, no generics, defaults or bounds. The code is also raised when a module needs a native library version the host does not provide.

```vs
record Point { x: I64 = 0; }

native fn measure(p: Point) -> I64;

export component Ruler {
    view { Text("ruler") }
}
```

Fix: Write the declaration in schema types (scalars, strings, lists, options and native types), or better, import the native from its generated schema instead of declaring it by hand.

```vs
native fn measure(x: I64) -> I64;
```

See also: `viso schema --list natives`, `viso check`

## E6102

**Native Ownership or Thread Domain Violation** — error, native

Native values carry an ownership and a thread domain. A borrowed handle, such as a game frame or world passed to a hook, is valid only for the call that receives it, so no place that outlives the call (state, input, computed, event payload, record or enum field, return value, const) may hold it. A native that runs on a worker thread can only be called from a `task`, because other bodies run on the UI thread.

```vs
import viso::game::{FixedUpdate, FixedFrame};

system Recorder implements FixedUpdate {
    @local state last: Option<FixedFrame> = Option::None;

    action fixed_update(frame: FixedFrame) {
    }
}
```

Fix: Use a borrowed handle only inside the call, passing it as a parameter or keeping it in a local; store the plain values you need from it instead.

```vs
state last_tick = 0;

action fixed_update(frame: FixedFrame) {
    last_tick = frame.tick();
}
```

See also: `viso schema --list natives`

## E6103

**Capability Denied** — error, runtime

A native that needs a capability (clipboard, network, persistent storage and so on) runs only when the package or host grants it. When a handler calls such a native outside the grant, the call faults with this code, the handler's writes roll back and the view keeps handling input. Persisted state without the `storage.persist` grant likewise starts from its initializer and is not stored.

```text
A package whose `Viso.toml` does not grant `clipboard.write` runs:

    import viso::clipboard;

    export component Notes {
        state note = "hi";
        view { Text(note) { on click { clipboard::write_text(note); } } }
    }

Clicking the text faults with E6103 and the click's writes are rolled back.
```

Fix: Grant the capability in the package manifest, or avoid calling the native in builds that should not have it.

```text
[package]
capabilities = ["clipboard.write"]
```

See also: `viso config show`

## E6104

**Native Unavailable on the Target** — error, native

A native package may limit a native to the platforms it has an implementation on (`NativeFunction::only_on`: macos, windows, linux, ios, android, web). When the package is checked or built for a target, every call to a native that does not exist on that target's platform is this error, at the call, naming the platforms it does exist on. Every web target is `web`, an embedded board `linux`; a headless run and a check without a target check no platform.

```text
A dependency's native exists on phones alone:

    crate::native!(action "vibrate" |cx, ms: i64| -> () { ... }).only_on(&["ios", "android"])

and a view calls it:

    import haptics;
    export component Bell {
        view { Text("ring") { on click { haptics::vibrate(30); } } }
    }

`viso check web-gpu` reports E6104 at the call: `haptics::vibrate` does not exist on web.
```

Fix: Keep the call to the targets that have the native, or offer what it does another way where it is missing (a web API, another native).

See also: `viso check <target>`, `viso schema --list natives`

## E7101

**Execution Budget Exceeded** — error, runtime

Every behavior call runs under an execution budget: an instruction count, a call depth limit and a native call quota. A body that exceeds one of them, typically an unbounded loop or deep recursion, faults with this code instead of freezing the frame; its writes roll back. In a game's fixed tick the same exhaustion is reported as the tick budget fault.

```text
An action whose loop never ends exhausts the instruction budget when it runs:

    action spin() {
        let mut n = 0;
        while true { n += 1; }
    }

Calling `spin()` faults with E7101 and its writes are rolled back.
```

Fix: Bound the work: give loops a reachable exit, replace deep recursion with iteration, and move long computations into a `task` or a native.

## E7102

**Memory Budget Exceeded** — error, runtime

Behavior code runs under a memory budget that counts the values it allocates, such as lists, strings and records. A body that grows data past the budget faults with this code and its writes roll back, instead of exhausting the host's memory. It usually comes from building a collection in an unbounded or very long loop.

```text
An action that keeps doubling a list runs out of its memory budget:

    action grow() {
        let mut xs = [1];
        loop { xs = xs + xs; }
    }

Calling `grow()` faults with E7102 and its writes are rolled back.
```

Fix: Limit how much a single call builds: bound the loop, page or stream large data, or keep it in a native-owned structure.

## E7103

**Arithmetic Fault** — error, runtime

Integer arithmetic is checked. An overflow, a division or remainder by zero, or a shift by an amount outside the type's bit width faults with this code at run time instead of wrapping or producing an undefined value. The faulting body's writes roll back.

```text
An action dividing by a state that is still zero:

    state total = 10;
    state parts = 0;
    action split() { total = total / parts; }

Calling `split()` faults with E7103 (integer division by zero).
```

Fix: Guard the operation: check divisors and shift amounts before using them, and pick an integer type wide enough for the values.

```text
action split() { if parts != 0 { total = total / parts; } }
```

## E7104

**Index Out of Bounds** — error, runtime

Indexing a list with `xs[i]` requires `i` to be inside the list. An index past the end or below zero faults with this code at run time and the body's writes roll back; in a game the faulting hook is rolled back and the next system still runs.

```text
A fixed-step hook indexing a one-element list with a growing tick:

    action fixed_update(frame: FixedFrame) {
        let xs = [1];
        n = xs[frame.tick() + 5];
    }

The hook faults with E7104 every tick and its writes are rolled back.
```

Fix: Check the index against `len()` first, or use `xs.get(i)`, which returns an `Option`, and handle the missing case.

```text
let n = match xs.get(i) { Option::Some(v) => v, Option::None => 0 };
```

See also: `viso test game`

## E7105

**Function Cannot Run** — error, runtime

A call could not run at all: the function has no body in this build, a native it calls is not linked or needs a thread the interpreter does not provide, an instance was created without a required input, or the code broke an invariant the compiler guarantees. These are build or host integration problems rather than mistakes in the logic of the code itself.

```text
A host mounts a component that declares `input title: String;` without
giving it a `title`; creating the instance faults with E7105
(a required input has no value).
```

Fix: Provide every required input when mounting, link the native libraries the module imports, and rebuild so the host runs the module it was compiled with. Report an invariant fault as a compiler bug.

See also: `viso doctor`

## E7106

**Native Function Failed** — error, runtime

A native function returned an error or panicked. The fault is caught at the native boundary and reported with this code, the calling body's writes roll back, and the host keeps running. The message carries the native's own error.

```text
A handler calls a host native that cannot complete, for example a native
whose service is not installed by the host or whose Rust implementation
returns `Err`; the call faults with E7106 and the click's writes roll back.
```

Fix: Read the native's error in the fault message and fix its cause: install the service it needs, pass valid arguments, or handle the failure on the Rust side and return a `Result` the DSL can match on.

## E8101

**Host-Only Type in a Shader** — error, shader

A shader may only use the closed set of GPU value types (scalars, vectors, matrices, `ColorLinear`, textures, samplers) and `@shader_value` records made of them. Host types such as `String`, `I64`, `List`, tuples, enums, unmarked records and `Color` literals have no GPU representation, so using them in a shader member, local or record field is rejected.

```vs
shader Label {
    uniform name: String;
    vertex(vertex_id: U32) -> VertexOutput {
        return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
    }
    fragment() -> Vec4F32 {
        return Vec4F32(0.0);
    }
}
```

Fix: Use a shader value type, or mark a record of shader value types with `@shader_value`.

```vs
uniform tint: Vec4F32;
```

See also: `viso check`

## E8102

**F64 in a Shader** — error, shader

Shaders compute in 32-bit types. `F64` members, locals and record fields, and `f64` literals, are not portable across GPU backends, so they are rejected. Unsuffixed float literals in a shader are already `F32` where the context needs one.

```vs
shader Fade {
    uniform amount: F64;
    vertex(vertex_id: U32) -> VertexOutput {
        return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
    }
    fragment() -> Vec4F32 {
        return Vec4F32(0.0);
    }
}
```

Fix: Use `F32` (and `F32` literals) inside the shader; convert on the host side.

```vs
uniform amount: F32;
```

See also: `viso check`

## E8103

**Shader Loop Without a Static Bound** — error, shader

GPU code needs a known upper bound on every loop. A `for` over an integer range is bounded when both ends are literals; otherwise it must carry `@max_iterations(N)`, which stops it after N iterations. `while` and `loop` have no bound and are always rejected in a shader.

```vs
shader Rings {
    uniform count: I32;
    vertex(vertex_id: U32) -> VertexOutput {
        return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
    }
    fragment() -> Vec4F32 {
        let mut sum = 0.0;
        for i in 0..count { sum += 0.1; }
        return Vec4F32(sum);
    }
}
```

Fix: Use literal range ends, or bound the loop with `@max_iterations`.

```vs
@max_iterations(16)
for i in 0..count { sum += 0.1; }
```

See also: `viso check`

## E8104

**Shader ABI Mismatch** — error, shader

Uniform and instance members are laid out in GPU buffers, so each must have a buffer layout. `Bool` has none, so a `Bool` uniform or instance member is rejected. The same code is raised when a Rust `#[derive(GpuPod)]` instance struct does not match the shader's instance layout field by field (name, format, offset or stride).

```vs
shader Toggle {
    instance on: Bool;
    vertex(vertex_id: U32) -> VertexOutput {
        return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
    }
    fragment() -> Vec4F32 {
        return Vec4F32(0.0);
    }
}
```

Fix: Pass flags as a number (for example `U32` with 0 or 1) and keep any Rust instance struct in the same order and formats as the shader's instance members.

```vs
instance on: U32;
```

See also: `viso check`

## E8105

**Construct Outside the Shader Subset** — error, shader

Shaders accept only a subset of the language that every GPU backend can run. Closures, recursion between shader functions, `?`, `??`, named arguments, turbofish, `emit`, `transaction`, `await` and generic shaders are rejected, as are `@max_iterations`, `@shader_value` and `@sampler` placed where they do not apply or with bad settings.

```vs
shader Glow {
    vertex(vertex_id: U32) -> VertexOutput {
        return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
    }
    fragment() -> Vec4F32 {
        let half = |x: F32| x * 0.5;
        return Vec4F32(half(1.0));
    }
}
```

Fix: Write the helper as a shader `fn` (non-recursive, reading only its parameters) and call it positionally.

```vs
fn half(x: F32) -> F32 { return x * 0.5; }
```

See also: `viso check`

## E8106

**Render Profile Stage Rule Violated** — error, shader

A render shader has at most one `vertex` entry, exactly one `fragment` entry and no `compute`; without `vertex` it takes the default one, which spans the node's box. `vertex` takes only the builtins `vertex_id: U32` and `instance_id: U32` and returns `VertexOutput`; `fragment` takes only `frag_coord: Vec4F32`. Only `vertex` writes varyings, fragment-only intrinsics such as `dpdx` and `sample` stay in `fragment`, and a shader `fn` may not read members.

```vs
shader Tint {
    varying shade: F32;
    vertex(vertex_id: U32) -> VertexOutput {
        return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
    }
    fragment() -> Vec4F32 {
        shade = 1.0;
        return Vec4F32(shade);
    }
}
```

Fix: Write varyings in `vertex` and only read them in `fragment`.

```vs
vertex(vertex_id: U32) -> VertexOutput {
    shade = 1.0;
    return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
}
```

See also: `viso check`

## E8107

**Brush Profile Violated** — error, shader

A shader paints a node's box through a `ShaderView`, so it is a brush. Every shader reads the members the renderer writes without declaring them: `viewport_size: Vec2F32` and `time: F32`, the box's `rect_pos: Vec2F32` and `rect_size: Vec2F32` and, without a `vertex` entry, the varyings `uv` and `local`; declaring one of them is rejected. A brush declares no uniform, since nothing on the host would write it. Its parameters are its instance members, limited to `F32 I32 U32 Vec2F32 Vec3F32 ColorLinear`, and image textures within the texture and sampler limits; these rules are checked only for a shader a view paints with.

```vs
shader Glow {
    instance rect_size: Vec2F32
    instance strength: F32
    fragment() -> Vec4F32 {
        return Vec4F32(strength * uv.x, 0.0, 0.0, 1.0)
    }
}

export component Card {
    view {
        ShaderView { shader: Glow { strength: 0.5 }; width: 120dp; height: 80dp }
    }
}
```

Fix: Drop the declaration and read the builtin as it is; keep parameters to the host-convertible types.

```vs
shader Glow {
    instance strength: F32
    fragment() -> Vec4F32 {
        return Vec4F32(strength * uv.x, 0.0, 0.0, 1.0)
    }
}
```

See also: `viso check`

## E9101

**System Order Cycle** — error, game

`@before(...)` and `@after(...)` order systems within a schedule. If the constraints form a cycle, including a system ordered after itself, no run order satisfies them, so the cycle is rejected and the message names the systems on it.

```vs
import viso::game::{FixedUpdate, FixedFrame};

@after(Physics)
system Input implements FixedUpdate {
    action fixed_update(frame: FixedFrame) {}
}

@after(Input)
system Physics implements FixedUpdate {
    action fixed_update(frame: FixedFrame) {}
}
```

Fix: Remove one of the constraints so the order is a chain.

```vs
@after(Input)
system Physics implements FixedUpdate {
    action fixed_update(frame: FixedFrame) {}
}
```

See also: `viso check`

## E9102

**Fixed Tick Budget Exceeded** — error, runtime

All hooks of one fixed tick share one instruction and native call budget. When a hook exhausts it, the hook is rolled back, the rest of that tick's hooks are skipped, and the tick still counts, so the simulation keeps its fixed rate. The same fault rejects a world rebuild or logic reload whose smoke tick runs out of budget.

```text
A game whose `fixed_update` hooks together do more work per tick than the
tick budget allows, for example looping over every entity many times,
faults with E9102 in the first such hook; the systems after it skip that
tick.
```

Fix: Do less work per tick: spread it over several ticks, move heavy work into natives, or lower the per-entity cost. Use `viso test game` to find which system exhausts the budget.

See also: `viso test game`

## E9103

**Simulation Reaches Local State** — error, game

Simulation code (fixed-step hooks and everything they call) must be reproducible from the snapshot and the inputs alone. It may not read or write `@local` state, which only presentation owns, nor use the return value of a presentation method. `@local` itself marks only a `state` of a `system`.

```vs
import viso::game::{FixedUpdate, FrameUpdate, FixedFrame, RenderFrame};

system Hud implements FixedUpdate + FrameUpdate {
    state ticks = 0;
    @local state frames = 0;

    action fixed_update(frame: FixedFrame) {
        ticks += 1;
        frames = 0;
    }

    action frame_update(frame: RenderFrame) {
        frames += 1;
    }
}
```

Fix: Keep `@local` state to presentation hooks; let the simulation write simulation state, and have the frame hook read it.

```vs
action frame_update(frame: RenderFrame) {
    frames = ticks;
}
```

See also: `viso check`

## E9104

**Non-Deterministic Source in the Simulation** — error, game

The simulation must give the same result for the same inputs, so replays, rollback and snapshots agree. Wall clocks, tasks and `await`, unseeded randomness, unordered hash iteration, UI state and the adaptive environment are non-deterministic sources, as is any native or physics engine whose reproducibility is below the game's `determinism` level.

```vs
import viso::game::{FixedUpdate, FixedFrame};
import viso::time::Stopwatch;

system Clock implements FixedUpdate {
    state ticks = 0;

    action fixed_update(frame: FixedFrame) {
        let w = Stopwatch::start();
        ticks += 1;
    }
}
```

Fix: Derive time from the tick (`frame.tick()` and the tick rate) and take randomness from the game's seeded source; measure wall time only in presentation hooks.

```vs
action fixed_update(frame: FixedFrame) {
    ticks = frame.tick();
}
```

See also: `viso check`

## E9105

**Simulation State Without Snapshot** — error, game

Simulation state is captured in snapshots for rollback, replay and reload, so its type must be snapshottable. A native handle lives outside the interpreter and cannot be captured, and neither can a record, list or option that contains one.

```vs
import viso::game::{FixedUpdate, FixedFrame};
import viso::time::Stopwatch;

system Timer implements FixedUpdate {
    state held: Option<Stopwatch> = Option::None;

    action fixed_update(frame: FixedFrame) {
    }
}
```

Fix: Keep plain values in simulation state, or mark state holding a handle `@local` so it belongs to presentation.

```vs
@local state held: Option<Stopwatch> = Option::None;
```

See also: `viso check`

## E9106

**Invalid Persisted State** — error, game

`@persist("key")` stores a `state` of a system or component across runs. Each key must be unique, the state's type must be persistable, the package must grant the `storage.persist` capability, and the attribute applies only to `state`. A component persisting state must be a view's own component, not one mounted inside another component's view, because each key holds one value.

```vs
import viso::game::{FixedUpdate, FixedFrame};

system Score implements FixedUpdate {
    @persist("best")
    state best = 0;

    action fixed_update(frame: FixedFrame) {
    }
}
```

Fix: Grant the capability in `Viso.toml`, and give each persisted state its own key.

```vs
[package]
capabilities = ["storage.persist"]
```

See also: `viso check`

## E9107

**Input Action Missing a Device Binding** — warning, game

When the target profile declares gamepad or touch devices, every input action in an `InputMap` should be reachable from each of them. An action with no binding for a target device is warned about, since players on that device could not trigger it. Without such devices in the profile the check is silent.

```text
With a target profile that declares gamepad and touch input, this map warns
twice: `A::Jump` has no touch binding and `A::Fire` has no gamepad binding.

    @derive(InputAction) enum A { Jump; Fire; }
    const C: InputMap<A> = InputMap::new()
        .key(Key::Space, A::Jump).pad(PadButton::South, A::Jump)
        .key(Key::J, A::Fire).touch(TouchButton::Primary, A::Fire);
```

Fix: Bind every action for each target device.

```text
.touch(TouchButton::Secondary, A::Jump).pad(PadButton::West, A::Fire)
```

See also: `viso check`

## E9108

**Audio Realtime Rule Violated** — error, game

An `AudioProcess` hook runs on the realtime audio thread, which must never block or allocate. The hook and everything it calls may not build lists, strings, records, tuples or closures, call natives that are not realtime-safe, recurse, or loop without a constant bound. An audio system also implements no trait that runs on another thread.

```vs
import viso::game::{AudioProcess, AudioBlock};

export system Synth implements AudioProcess {
    @local state gain: F32 = 0.5f32;

    action audio_process(block: AudioBlock) {
        let taps = [1, 2];
    }
}
```

Fix: Fill the block in place with constant-bounded loops over its channels and frames, and keep scratch data in `@local` state prepared outside the hook.

```vs
action audio_process(block: AudioBlock) {
    for c in 0..block.channels() {
        for f in 0..block.frames() {
            block.write(c, f, block.input(c, f) * gain);
        }
    }
}
```

See also: `viso check`

## E9109

**UI Member in a System** — error, game

A `system` is game logic driven by the scheduler, not UI. It may not declare `view`, `event`, `slot`, `effect` or `resource` members, run `start`, or call `tr` for translated text. Those belong to components.

```vs
system Score {
    event scored();
}
```

Fix: Move the UI-facing member into a component, and let the system expose its results through state that the component reads.

See also: `viso check`

## E9110

**Misplaced Probe** — error, game

`@probe` exposes a simulation value to game tests and scenarios, which assert on it tick by tick. It therefore marks only a simulation `state` of a `system`: not a `@local` state, not an action or other member, and not a component's state.

```vs
export component Hud {
    @probe state lives = 3;

    view { Text("hud") }
}
```

Fix: Put the probe on a simulation state of the system that owns the value.

```vs
system Player implements FixedUpdate {
    @probe state lives = 3;
    action fixed_update(frame: FixedFrame) {}
}
```

See also: `viso test game`

## E9111

**Persisted State Failed to Load or Store** — error, runtime

A persisted state is loaded before the view mounts or the game starts, and written back when it changes and on suspend. If the stored value cannot be read or converted into the state's current type, or the store fails to write, the runtime reports this code; a state that does not load starts from its initializer. The application keeps running.

```text
A game persisted `@persist("best") state best = 0;` as an integer. A later
build declares `@persist("best") state best = "none";` with no `@migrate`
function from `I64`; at start the stored value does not convert, `best`
starts as `"none"`, and the runtime reports E9111.
```

Fix: When changing a persisted state's type, add a `@migrate` function from the old type, or change the key so the new state starts fresh. Check the host's storage when writes fail.

## E9112

**Invalid Game Query** — error, game

A view reads the game its root runs through the `@query` functions of the game's systems, as `game.name()`; the session runs each query a view reads once a frame and the bindings reading it update when its value changes. A query therefore is a `fn` of a `system`, takes no parameters, returns a value type the view can hold (no closure or native handle), and its name is unique among the package's queries.

```vs
system Score implements FixedUpdate {
    state points = 0;
    @query fn points_after(bonus: I64) -> I64 { points + bonus }
    action fixed_update(frame: FixedFrame) { points += 1; }
}
```

Fix: Keep what the view needs in a state the system's hooks update, and return it from a query with no parameters.

```vs
system Score implements FixedUpdate {
    state points = 0;
    @query fn points() -> I64 { points }
    action fixed_update(frame: FixedFrame) { points += 1; }
}
```
