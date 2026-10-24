//! The diagnostics registry: every code the compiler and the runtime it
//! targets raise, with its title, an explanation, an example that raises it
//! and its fix. `viso explain`, the language server's hover and the generated
//! code table all read this one table; a test holds it to the codes the
//! sources raise and to the specification's list.

use super::Severity;
use crate::resolve::suggest::{Candidate, nearest as nearest_names};

/// The stage that raises a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Reading the package manifest the compiler is handed.
    Manifest,
    /// Splitting source text into tokens.
    Lex,
    /// Building the syntax tree.
    Parse,
    /// Resolving names and modules.
    Resolve,
    /// Type checking and inference.
    Type,
    /// Effect, side-effect and capability checking.
    Effect,
    /// Checking and lowering views, styles and their bindings.
    View,
    /// Checking tasks and suspension.
    Async,
    /// Checking effects, resources and starts.
    Resource,
    /// Comparing a reloaded program with the running one.
    HotReload,
    /// Checking native declarations against the native registry.
    Native,
    /// Running the program.
    Runtime,
    /// Checking shaders.
    Shader,
    /// Checking systems and the game scheduler.
    Game,
}

impl Stage {
    /// The name tools report.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Manifest => "manifest",
            Stage::Lex => "lex",
            Stage::Parse => "parse",
            Stage::Resolve => "resolve",
            Stage::Type => "type",
            Stage::Effect => "effect",
            Stage::View => "view",
            Stage::Async => "async",
            Stage::Resource => "resource",
            Stage::HotReload => "hot_reload",
            Stage::Native => "native",
            Stage::Runtime => "runtime",
            Stage::Shader => "shader",
            Stage::Game => "game",
        }
    }
}

/// What shows a code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Example {
    /// A whole `.vs` file whose compilation raises the code.
    Source(&'static str),
    /// A sketch of what one file's compilation does not reach: a manifest, a
    /// reload, a running app.
    Text(&'static str),
}

impl Example {
    /// Its text.
    pub fn text(self) -> &'static str {
        match self {
            Example::Source(text) | Example::Text(text) => text,
        }
    }
}

/// One diagnostic code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CodeInfo {
    /// The stable code, `E` and four digits.
    pub code: &'static str,
    /// The severity it is raised with unless configured otherwise.
    pub severity: Severity,
    /// Which stage raises it.
    pub stage: Stage,
    /// A short title.
    pub title: &'static str,
    /// What the rule is and why it holds.
    pub explanation: &'static str,
    /// What raises it.
    pub example: Example,
    /// How to resolve it.
    pub fix: &'static str,
    /// The example's lines as the fix leaves them, when the fix is a change
    /// of source.
    pub fixed: Option<&'static str>,
    /// Commands that help.
    pub commands: &'static [&'static str],
    /// Specified, and not raised yet.
    pub reserved: bool,
}

/// The code `code`, if the registry has it.
pub fn lookup(code: &str) -> Option<&'static CodeInfo> {
    CODES
        .binary_search_by(|info| info.code.cmp(code))
        .ok()
        .map(|index| &CODES[index])
}

/// The names among `candidates` nearest to `name`, nearest first: what a
/// misspelled code suggests.
pub fn nearest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    nearest_names(
        name,
        candidates.into_iter().map(|name| Candidate {
            name,
            declared_at: None,
        }),
    )
    .into_iter()
    .map(|candidate| candidate.name)
    .collect()
}

/// Every code, in code order.
pub static CODES: &[CodeInfo] = &[
    CodeInfo {
        code: "E1001",
        severity: Severity::Error,
        stage: Stage::Manifest,
        title: "Unsupported Language Version",
        explanation: "A package pins the language version it is written in with the `language` key of \
            its `Viso.toml` (or its lockfile); individual `.vs` files never restate it. The \
            compiler only accepts the exact version it implements, so that a package is never \
            silently compiled under rules it was not written for. This error is raised when \
            the pinned version is unknown or differs from the compiler's version, and its \
            primary location is the version value in the manifest.",
        example: Example::Text(
            r#"# Viso.toml
[package]
name = "app"
language = "2.0"    # this compiler implements "1.0"
"#,
        ),
        fix: "Set `language` in `Viso.toml` to the version the compiler implements, or build the \
            package with a compiler that implements the pinned version.",
        fixed: Some(
            r#"[package]
name = "app"
language = "1.0"
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E1101",
        severity: Severity::Error,
        stage: Stage::Resolve,
        title: "Identifier Normalization Conflict",
        explanation: "Identifiers are normalized to Unicode NFC before they enter the symbol table, so \
            two spellings that are NFC-equal name the same symbol. Declaring a name twice in \
            one namespace with spellings that differ only in normalization (for example a \
            precomposed `é` and `e` followed by a combining accent) is rejected, because the \
            two declarations look identical but are written with different code points. The \
            related location points at the earlier declaration; two declarations with the \
            exact same spelling are an ordinary duplicate instead.",
        example: Example::Source(
            r#"const café = 1;
const café = 2;

component Menu {
    view { Text { text: "menu"; } }
}
"#,
        ),
        fix: "Give the second declaration a distinct name, or remove it if it was meant to be \
            the same symbol. Write identifiers in one consistent normalization form (NFC); \
            most editors do this when the file is saved as NFC.",
        fixed: Some(
            r#"const cafe_price = 1;
const cafe_count = 2;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1102",
        severity: Severity::Warning,
        stage: Stage::Lex,
        title: "Confusable Identifier",
        explanation: "An identifier mixes Unicode scripts in a way that is easy to confuse visually, \
            such as a Latin word containing a Cyrillic `а`. Such names look like an existing \
            ASCII name but are a different symbol, which hides bugs and can be used to \
            disguise code. Detection follows the UTS #39 mixed-script rules at the Highly \
            Restrictive level; natural mixes such as `café`, `名前かな` or `ユーザーID` are not \
            reported. This is a warning by default.",
        example: Example::Source(
            r#"component Checkout {
    state pаy = 0;
    view { Text { text: "checkout"; } }
}
"#,
        ),
        fix: "Retype the identifier using characters from a single script (here, replace the \
            Cyrillic `а` with the Latin `a`).",
        fixed: Some(
            r#"state pay = 0;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1201",
        severity: Severity::Error,
        stage: Stage::Lex,
        title: "Unterminated String",
        explanation: "A string, raw string or character literal was opened but never closed. Ordinary \
            string and character literals cannot span physical lines, so a missing closing \
            quote is detected at the end of the line; a raw string must end with a quote \
            followed by exactly as many `#` as it opened with. The usual cause is a forgotten \
            closing quote or a line break inside a string.",
        example: Example::Source(
            r#"component Greeting {
    state message = "hello;
    view { Text { text: message; } }
}
"#,
        ),
        fix: "Add the missing closing quote. Write a line break inside a string as `\\n`, and \
            close a raw string with the same number of `#` it was opened with.",
        fixed: Some(
            r#"state message = "hello";
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1202",
        severity: Severity::Error,
        stage: Stage::Lex,
        title: "Unterminated Block Comment",
        explanation: "A block comment opened with `/*` reaches the end of the file without a matching \
            `*/`. Everything after the opener would be swallowed by the comment, so the lexer \
            reports the opener instead of guessing where the comment was meant to end.",
        example: Example::Source(
            r#"component Notes {
    /* draft notes
    state count = 0;
    view { Text { text: "notes"; } }
}
"#,
        ),
        fix: "Close the comment with `*/` where it is meant to end, or use `//` line comments.",
        fixed: Some(
            r#"/* draft notes */
state count = 0;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1203",
        severity: Severity::Error,
        stage: Stage::Lex,
        title: "Invalid Numeric Literal",
        explanation: "A numeric literal is malformed: a radix-prefixed integer (`0x`, `0o`, `0b`) has no \
            digits or a digit outside its radix, or a suffix does not fit the body it follows. \
            A unit literal takes no exponent (`1e2dp`), an integer suffix only follows an \
            integer body, and a float suffix only follows a float body. The whole run is kept \
            as one bad token so the rest of the file still parses.",
        example: Example::Source(
            r#"component Spacer {
    state gap = 1e2dp;
    view { Text { text: "spacer"; } }
}
"#,
        ),
        fix: "Pair each suffix with a body it fits: write unit literals without an exponent, put \
            integer suffixes only on integers and float suffixes only on floats, and use only \
            digits valid for a literal's radix.",
        fixed: Some(
            r#"state gap = 100dp;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1204",
        severity: Severity::Error,
        stage: Stage::Lex,
        title: "Unknown Unit Suffix",
        explanation: "A number is immediately followed by a suffix outside the closed set of units and \
            numeric types. The unit suffixes are `dp`, `px`, `sp`, `em`, `%`, `ns`, `us`, \
            `ms`, `s`, `min`, `deg`, `rad`, `turn`, `hz` and `khz`; the type suffixes are \
            `i8`–`i64`, `u8`–`u64`, `f32` and `f64`. The set is fixed so that every literal \
            has one known type and libraries cannot introduce new lexical forms; custom \
            quantities use ordinary constructors or newtypes.",
        example: Example::Source(
            r#"component Title {
    view {
        Text { text: "title"; font_size: 12pt; }
    }
}
"#,
        ),
        fix: "Use one of the supported unit suffixes (for font sizes usually `sp`), or construct \
            a custom quantity with a function or newtype instead of a suffix.",
        fixed: Some(
            r#"Text { text: "title"; font_size: 12sp; }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1205",
        severity: Severity::Error,
        stage: Stage::Lex,
        title: "Invalid Escape Sequence",
        explanation: "A string or character literal contains an escape the language does not define, or \
            a malformed one. The supported escapes are `\\\\`, `\\\"`, `\\'`, `\\n`, `\\r`, \
            `\\t`, `\\0`, `\\xNN` (exactly two hex digits, at most U+007F) and `\\u{H...H}` \
            (one to six hex digits, at most U+10FFFF and not a surrogate). Anything else, such \
            as `\\q` or `\\u{110000}`, is rejected.",
        example: Example::Source(
            r#"component Path {
    state label = "C:\qtemp";
    view { Text { text: label; } }
}
"#,
        ),
        fix: "Use one of the supported escapes: write a literal backslash as `\\\\`, or use a \
            raw string, which takes no escapes.",
        fixed: Some(
            r#"state label = r"C:\qtemp";
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1206",
        severity: Severity::Error,
        stage: Stage::Lex,
        title: "Misplaced Numeric Separator",
        explanation: "A digit separator `_` in a numeric literal is not between two digits. Separators \
            may not lead, trail or repeat, and may not touch a radix prefix (`0x_1`), the \
            decimal point, the exponent or a suffix. The rule keeps the boundary between a \
            number and its suffix unambiguous.",
        example: Example::Source(
            r#"component Budget {
    state limit = 1__000;
    view { Text { text: "budget"; } }
}
"#,
        ),
        fix: "Put each `_` between two digits, with at most one in a row.",
        fixed: Some(
            r#"state limit = 1_000;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1207",
        severity: Severity::Error,
        stage: Stage::Lex,
        title: "Invalid Color Literal",
        explanation: "A color literal must be `#` followed by exactly 3, 4, 6 or 8 hexadecimal digits \
            (`#RGB`, `#RGBA`, `#RRGGBB`, `#RRGGBBAA`), read as sRGB with alpha `FF` when \
            omitted. Any other digit count, or a non-hexadecimal character, is rejected \
            immediately rather than guessed at.",
        example: Example::Source(
            r#"component Badge {
    view {
        Text { text: "new"; color: #12345; }
    }
}
"#,
        ),
        fix: "Write the color with 3, 4, 6 or 8 hex digits.",
        fixed: Some(
            r#"Text { text: "new"; color: #123456; }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1208",
        severity: Severity::Error,
        stage: Stage::Lex,
        title: "Character Literal Not One Character",
        explanation: "A character literal must contain exactly one Unicode scalar value after escapes \
            are decoded. Writing several characters (or none) between single quotes is \
            rejected; text of any other length is a `String` and uses double quotes.",
        example: Example::Source(
            r#"component Initial {
    state initial = 'ab';
    view { Text { text: "initial"; } }
}
"#,
        ),
        fix: "Keep exactly one character between the single quotes, or use a double-quoted \
            string for longer text.",
        fixed: Some(
            r#"state initial = 'a';
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1209",
        severity: Severity::Error,
        stage: Stage::Lex,
        title: "Invalid Source Character",
        explanation: "The source contains a character that cannot appear in Viso text: a NUL byte, a \
            carriage return that is not part of a `\\r\\n` line ending, or a character that \
            cannot begin any token (such as `¡` outside a string). These usually come from \
            copy-pasted text, binary data or a stray key press.",
        example: Example::Source(
            r#"component Shop {
    state price = 0;
    view { Text { text: "shop"; } }
    ¡
}
"#,
        ),
        fix: "Delete the stray character, or move it inside a string literal if it is meant as \
            text. Save the file with `\\n` or `\\r\\n` line endings and no NUL bytes.",
        fixed: None,
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1210",
        severity: Severity::Error,
        stage: Stage::Lex,
        title: "Too Many Raw String Hashes",
        explanation: "A raw string is delimited by `r`, zero to 255 `#` characters and a quote, and must \
            close with a quote followed by the same number of `#`. An opener with more than \
            255 `#` exceeds the format's limit and is rejected. In practice one or two `#` are \
            always enough to enclose any text.",
        example: Example::Source(
            "component Banner {\n    state art = r################################################################################################################################################################################################################################################################\"x\"################################################################################################################################################################################################################################################################;\n    view { Text { text: \"banner\"; } }\n}\n",
        ),
        fix: "Use the smallest number of `#` that does not appear after a quote inside the text, \
            usually one.",
        fixed: Some(
            r##"state art = r#"x"#;
"##,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1301",
        severity: Severity::Error,
        stage: Stage::Parse,
        title: "Strict Keyword Used As A Name",
        explanation: "A strict keyword (such as `fn`, `type`, `match`, `in` or `let`) appears where a \
            name is bound or declared: a `let`, a parameter, a pattern binding, a closure \
            parameter, an import alias, or any declaration name. Strict keywords are always \
            lexed as keywords, so they can never be names there. Contextual keywords such as \
            `state` or `view` are allowed as names, and label positions (member names after \
            `.`, path segments after `::`, record field names, named-argument labels) accept \
            any keyword.",
        example: Example::Source(
            r#"component Search {
    action run_query() {
        let match = 1;
    }
    view { Text { text: "search"; } }
}
"#,
        ),
        fix: "Rename the binding. If the name must match an external schema symbol spelled like \
            a keyword, escape it with `r#`.",
        fixed: Some(
            r#"let found = 1;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1401",
        severity: Severity::Error,
        stage: Stage::Parse,
        title: "Unclosed Delimiter",
        explanation: "An opening `(`, `[` or `{` has no matching closing delimiter before the enclosing \
            construct or the file ends. The parser keeps the partial construct and continues, \
            but everything after the opener is read as part of it until the error is fixed.",
        example: Example::Source(
            r#"component Total {
    state total = 0;
    view { Text { text: "total"; } }
"#,
        ),
        fix: "Add the missing closing delimiter where the construct ends (here, the `}` that \
            closes the component).",
        fixed: None,
        commands: &["viso fmt"],
        reserved: false,
    },
    CodeInfo {
        code: "E1402",
        severity: Severity::Error,
        stage: Stage::Parse,
        title: "Unmatched Closing Delimiter",
        explanation: "A closing `)`, `]` or `}` has no opening delimiter to match. It is usually left \
            over after deleting or moving a block, or an extra brace at the end of a \
            component.",
        example: Example::Source(
            r#"component Panel {
    view { Text { text: "panel"; } }
}
}
"#,
        ),
        fix: "Remove the extra closing delimiter, or add the opener it was meant to close.",
        fixed: None,
        commands: &["viso fmt"],
        reserved: false,
    },
    CodeInfo {
        code: "E1403",
        severity: Severity::Error,
        stage: Stage::Parse,
        title: "Unexpected Tokens",
        explanation: "The parser found tokens that cannot begin any declaration, member or statement at \
            this point. It collects them into an error node and resumes at the next construct, \
            so a single stray fragment yields one diagnostic instead of a cascade. Typical \
            causes are stray operators, misplaced punctuation or code from another language.",
        example: Example::Source(
            r#"component Card {
    %
    view { Text { text: "card"; } }
}
"#,
        ),
        fix: "Delete the stray tokens or turn them into a valid member or statement (for example \
            a `state`, `action` or `view` member inside a component).",
        fixed: None,
        commands: &["viso fmt"],
        reserved: false,
    },
    CodeInfo {
        code: "E1404",
        severity: Severity::Error,
        stage: Stage::Parse,
        title: "Missing Token",
        explanation: "The grammar requires a specific token here, such as `=`, `:`, `;` or a closing \
            keyword, and it is absent. The parser inserts a placeholder for the missing token \
            and continues, so the rest of the declaration is still checked.",
        example: Example::Source(
            r#"type Id I64;

component Profile {
    view { Text { text: "profile"; } }
}
"#,
        ),
        fix: "Insert the token the message names at the reported position.",
        fixed: Some(
            r#"type Id = I64;
"#,
        ),
        commands: &["viso fmt"],
        reserved: false,
    },
    CodeInfo {
        code: "E1405",
        severity: Severity::Error,
        stage: Stage::Parse,
        title: "Expected Expression",
        explanation: "The parser needs an expression here but found something else: an operator with no \
            right operand, an empty initializer, or a closing delimiter. It is usually an \
            unfinished edit.",
        example: Example::Source(
            r#"component Sum {
    state total = 1 +;
    view { Text { text: "sum"; } }
}
"#,
        ),
        fix: "Complete the expression, or remove the dangling operator.",
        fixed: Some(
            r#"state total = 1 + 2;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1406",
        severity: Severity::Error,
        stage: Stage::Parse,
        title: "Nesting Too Deep",
        explanation: "Expressions, types, patterns, blocks and view bodies may nest at most 256 levels \
            deep. The innermost construct past the budget is kept as an error node and \
            reported once, which bounds the stack depth of the parser and of every later pass \
            over the syntax tree. This normally only happens with generated code.",
        example: Example::Text(
            r#"// A source whose nesting exceeds 256 levels, for example an expression
// written with 257 nested parentheses:
component Deep {
    state n = ((((((((/* ...257 levels in total... */ 1))))))));
    view { Text { text: "deep"; } }
}
"#,
        ),
        fix: "Flatten the construct: bind intermediate results with `let`, move parts into a \
            function, or split a deep view into child components.",
        fixed: None,
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E1407",
        severity: Severity::Error,
        stage: Stage::Parse,
        title: "Source Too Large",
        explanation: "A single source file may contain at most 2^20 (1,048,576) significant tokens. \
            Everything past the budget is kept as one error node and reported once, which \
            bounds parser time and memory. This normally only happens with generated code or \
            embedded data.",
        example: Example::Text(
            r#"// A single .vs file with more than 1,048,576 significant tokens, for example
// a generated table:
const TABLE: List<I64> = [0, 1, 2, 3, /* ...over a million more... */];
"#,
        ),
        fix: "Split the source into several modules, or load large data at run time through a \
            resource instead of writing it as literals.",
        fixed: None,
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2001",
        severity: Severity::Error,
        stage: Stage::Resolve,
        title: "Unresolved Symbol",
        explanation: "A name does not refer to anything in scope: no local, member, declaration, import \
            or prelude item has that name in the namespace where it is used. The same code \
            covers imports of modules or names that do not exist or are not exported, unknown \
            native functions, methods, fields and enum variants. When close candidates exist, \
            the diagnostic lists the nearest ones by edit distance as fixes.",
        example: Example::Source(
            r#"component Counter {
    state count = 0;
    action increment() {
        count = cuont + 1;
    }
    view { Text { text: "counter"; } }
}
"#,
        ),
        fix: "Correct the spelling to an existing name, declare the symbol, or import it from \
            the module that exports it (adding `export` to its declaration if it is private).",
        fixed: Some(
            r#"count = count + 1;
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E2002",
        severity: Severity::Error,
        stage: Stage::Resolve,
        title: "Ambiguous Name",
        explanation: "One name refers to more than one declaration, so the resolver cannot pick one. \
            This covers a name declared twice in the same namespace and scope (including a \
            member that repeats the name of a `fn`, `action`, `task` or `const`), and two \
            source files that derive the same module path. Names must be unique within a \
            namespace so that every use resolves to exactly one symbol.",
        example: Example::Source(
            r#"component Counter {
    state count = 0;
    state count = 1;
    view { Text { text: "counter"; } }
}
"#,
        ),
        fix: "Rename or remove one of the declarations so each name is declared once; for \
            colliding module paths, rename or move one of the files.",
        fixed: Some(
            r#"state count = 0;
state total = 1;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2003",
        severity: Severity::Error,
        stage: Stage::Resolve,
        title: "Initialization Cycle",
        explanation: "Values whose initialization depends on each other in a cycle cannot be computed in \
            any order. This covers themes that reach each other through their base or item \
            expressions, styles whose base chain loops, and modules that import each other. \
            Each declaration on the cycle is reported.",
        example: Example::Source(
            r#"theme Dark: Night {}
theme Night: Dark {}

component Screen {
    view { Text { text: "screen"; } }
}
"#,
        ),
        fix: "Break the cycle: make one declaration stand on its own (for a theme, give it its \
            own `colors` and `elevation` items or base it on a theme outside the cycle), so \
            the dependencies form a chain instead of a loop.",
        fixed: None,
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2004",
        severity: Severity::Error,
        stage: Stage::Parse,
        title: "Missing Turbofish Or Const Marker",
        explanation: "Explicit generic arguments in an expression must be written with a turbofish, \
            `name::<T>(..)` or `Pair::<A, B> { .. }`; the form `name<T>(..)` is read as \
            comparisons and rejected. A const generic argument must be marked `const`, as in \
            `Matrix<F32, const 4, const 4>`. Both rules let the parser tell comparisons from \
            generics and types from constants without consulting the symbol table.",
        example: Example::Source(
            r#"component Loader {
    action load() {
        let items = decode<I64>("1");
    }
    view { Text { text: "loader"; } }
}
"#,
        ),
        fix: "Insert `::` before the generic arguments in an expression, and mark const \
            arguments with `const`.",
        fixed: Some(
            r#"let items = decode::<I64>("1");
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2005",
        severity: Severity::Error,
        stage: Stage::Resolve,
        title: "No Component To Mount",
        explanation: "A source compiled as a mountable unit must declare a component for the host to \
            mount. A file that declares only records, functions, constants or other \
            non-component items has nothing to mount. A file that declares only systems runs \
            under a scheduler and is exempt.",
        example: Example::Source(
            r#"record Point {
    x: F32;
    y: F32;
}
"#,
        ),
        fix: "Declare the component to mount in this file, or import this file's declarations \
            from the file that declares it.",
        fixed: Some(
            r#"component App {
    view { Text { text: "app"; } }
}
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2006",
        severity: Severity::Error,
        stage: Stage::Resolve,
        title: "Several Components To Mount",
        explanation: "A source that declares more than one component must say which one is mounted. When \
            no component, or more than one, is marked `export`, the choice is ambiguous and \
            the file is rejected. Exactly one exported component, or exactly one component in \
            total, makes the entry point unambiguous.",
        example: Example::Source(
            r#"component Header {
    view { Text { text: "header"; } }
}

component Page {
    view { Header {} }
}
"#,
        ),
        fix: "Mark the component to mount with `export`, or move the other components into their \
            own files.",
        fixed: Some(
            r#"export component Page {
    view { Header {} }
}
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2101",
        severity: Severity::Error,
        stage: Stage::Resolve,
        title: "Float Is Not A Type",
        explanation: "The language has no `Float` type; floating-point values are `F32` or `F64`. \
            Writing `Float` in any type position (a parameter, return type, field, variant \
            payload, `let`, `const`, member, generic argument, function type or `as` cast) is \
            reported once by name resolution and does not cascade into further type errors. It \
            usually comes from habits carried over from other languages.",
        example: Example::Source(
            r#"component Gauge {
    state level: Float = 0.5;
    view { Text { text: "gauge"; } }
}
"#,
        ),
        fix: "Write `F64` (or `F32` when single precision is enough) instead of `Float`.",
        fixed: Some(
            r#"state level: F64 = 0.5;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2102",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Illegal Implicit Numeric Conversion",
        explanation: "Only lossless widening happens implicitly: `I8` to `I64` along the signed ladder, \
            `U8` to `U64` along the unsigned ladder, and `F32` to `F64`. Narrowing, changing \
            signedness, and moving between integers and floats can lose information, so they \
            must be written as an explicit cast. This typically fires when an integer value is \
            returned or assigned where a float or a smaller integer is expected.",
        example: Example::Source(
            r#"component Counter {
    state n: I64 = 3;
    fn half() -> F64 { n }
    view { Text { text: "counter"; } }
}
"#,
        ),
        fix: "Convert the value with an explicit `as` cast to the expected numeric type.",
        fixed: Some(
            r#"fn half() -> F64 { n as F64 }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2103",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Type Mismatch",
        explanation: "An expression's type does not match the type its position expects, such as a \
            declared state type, a return type, a function argument or a view property. Types \
            are checked nominally and there are no implicit conversions between unrelated \
            types such as `String` and `I64` or `Dp` and `Px`. The message names the expected \
            and the found type.",
        example: Example::Source(
            r#"component A {
    state n: I64 = "x";
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Change the expression so it produces the expected type, or change the declared \
            type to match the value. For text, convert a displayable value with `format`.",
        fixed: Some(
            r#"state n: I64 = 0;
"#,
        ),
        commands: &["viso schema Text"],
        reserved: false,
    },
    CodeInfo {
        code: "E2104",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "State Initializer Forward Reference",
        explanation: "A `state` initializer may read only the states declared before it in source order. \
            States are initialized once, top to bottom, so reading a later state would observe \
            a value that does not exist yet. Computed values are evaluated on demand and may \
            refer forward; states may not.",
        example: Example::Source(
            r#"component A {
    state total = count * 2;
    state count = 1;
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Move the state being read above the one that reads it, or turn the dependent value \
            into a `computed`.",
        fixed: Some(
            r#"state count = 1;
state total = count * 2;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2105",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Computed Dependency Cycle",
        explanation: "The computed values of a component must form an acyclic dependency graph, because \
            each one is derived from the values it reads. When a computed depends on itself, \
            directly or through other computeds, it has no well-defined value. The message \
            lists the cycle path, such as `a` -> `b` -> `a`.",
        example: Example::Source(
            r#"component A {
    computed a: I64 = b + 1;
    computed b: I64 = a;
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Break the cycle: make one of the values on the path depend on a `state`, an \
            `input` or a constant instead of another computed in the loop.",
        fixed: Some(
            r#"state base: I64 = 0;
computed a: I64 = b + 1;
computed b: I64 = base;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2106",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "MixedLength Cannot Narrow Before Layout",
        explanation: "Adding lengths of different units, such as `10dp + 5px` or `100% - 8dp`, produces \
            a `MixedLength`, whose concrete size is known only during layout. Such a value \
            cannot be typed as a single unit like `Dp` or `Px` before layout. This fires when \
            a mixed sum is assigned, returned or passed where a concrete unit is declared.",
        example: Example::Source(
            r#"component A {
    fn gap() -> Dp { 10dp + 5px }
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Declare the target as `MixedLength`, or keep the arithmetic within a single unit.",
        fixed: Some(
            r#"fn gap() -> MixedLength { 10dp + 5px }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2107",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Illegal Dimensional Operation",
        explanation: "Some operations on lengths have no meaning before layout: a `MixedLength` cannot \
            be compared, multiplied by or divided by another length, a length cannot be \
            multiplied by a length or used with `%`, a scalar cannot be divided by a length, \
            and a `Percent` cannot be added to a plain scalar. These rules keep length \
            arithmetic dimensionally consistent. Scaling a length by a number remains allowed.",
        example: Example::Source(
            r#"component A {
    fn narrow() -> Bool { (100% - 8dp) < 200dp }
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Rewrite the expression so it only adds lengths or scales them by numbers, and \
            compare only values of one concrete unit.",
        fixed: Some(
            r#"fn narrow(width: Dp) -> Bool { width < 200dp }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2108",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Format Template Mismatch",
        explanation: "`format(template, args...)` is checked at compile time: the template must be a \
            string literal, each `{}` consumes the next positional argument, each `{name}` \
            consumes the named argument `name:`, and every argument must be used exactly once. \
            Each argument's type must implement `Display`; records, enums, lists, options and \
            similar types do not and must be mapped to text first. A missing, extra or \
            non-displayable argument is this error.",
        example: Example::Source(
            r#"component A {
    state n: I64 = 1;
    fn label() -> String { format("{} of {}", n) }
    view { Text { text: label(); } }
}
"#,
        ),
        fix: "Make the placeholders and the arguments match one to one, and pass only \
            displayable values.",
        fixed: Some(
            r#"fn label() -> String { format("{} of {total}", n, total: 3) }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2109",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Length Divided By Constant Zero",
        explanation: "Dividing a length-family value (`Dp`, `Px`, `Sp`, `Em`, `Percent` or \
            `MixedLength`) by an expression that folds to the constant `0` is rejected during \
            constant folding. The result would be an infinite or undefined size, which layout \
            cannot use.",
        example: Example::Source(
            r#"component A {
    fn cell() -> Dp { 8dp / (1 - 1) }
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Divide by a non-zero constant, or remove the division.",
        fixed: Some(
            r#"fn cell() -> Dp { 8dp / 2 }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2110",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Assignment Target Is Not Writable",
        explanation: "Only a `state`, a `let mut` local, a `mut` parameter, or a field or index path \
            rooted in one of them can be assigned or mutated in place. Inputs, computed \
            values, constants, resource values, plain `let` bindings and ordinary parameters \
            are read-only. The same rule applies to mutating list methods such as `push` and \
            `remove`.",
        example: Example::Source(
            r#"component A {
    input limit: I64;
    action reset() { limit = 0; }
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Assign to a writable place instead: declare the value as `state`, bind the local \
            with `let mut`, or mark the parameter `mut`. To change an input, emit an event and \
            let the parent update it.",
        fixed: Some(
            r#"state limit: I64 = 10;
action reset() { limit = 0; }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2111",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Context Read Outside Its Scope",
        explanation: "`env` and `theme` are context bindings of the view execution scope: they are read \
            in view items, property bindings, view conditions, view handlers and the arguments \
            of pure calls made there. `tr` reads `env.locale` and so is used only within a \
            component instance (its view, styles, computeds, actions and effects). State and \
            input initializers, member functions, tasks, constants, themes and module-level \
            declarations cannot read them implicitly; pass the needed value in as a parameter.",
        example: Example::Source(
            r#"component A {
    state scale = env.text_scale;
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Read the context value inside the view, or pass it as an argument to the function \
            that needs it.",
        fixed: Some(
            r#"fn scaled(scale: F32) -> F32 { scale * 2.0 }
view { Text { text: format("{}", scaled(env.text_scale)); } }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2112",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Compile-Time Argument Rejected",
        explanation: "Some native calls are evaluated by the compiler: `@const` natives that build \
            compile-time constants such as an `InputMap`, and native parameters declared as \
            tick durations, which are converted to whole ticks of the fixed step. When such an \
            argument is a constant but out of range, for example a dead-zone radius above 1 or \
            a negative duration, the compiler rejects it. A non-constant argument in these \
            positions is reported separately as an effect violation.",
        example: Example::Source(
            r#"import viso::game::{InputAction, InputMap};

@derive(InputAction) enum Act { Jump; }

const CONTROLS: InputMap<Act> = InputMap::new().dead_zone(1.5);

system Host {}
"#,
        ),
        fix: "Pass a value inside the accepted range: a dead zone of at least 0 and below 1, or \
            a duration of zero or more ticks that fits `I64`.",
        fixed: Some(
            r#"const CONTROLS: InputMap<Act> = InputMap::new().dead_zone(0.15);
"#,
        ),
        commands: &["viso schema InputMap"],
        reserved: false,
    },
    CodeInfo {
        code: "E2201",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Trait Bound Not Satisfied",
        explanation: "A generic argument must implement every trait its parameter is bounded by, and an \
            `impl` must match its trait exactly: provide every required member, add no foreign \
            members, and keep each signature. Generic bodies are checked against their bounds, \
            so an unbounded `T` cannot use `==` or call trait methods. This code also covers a \
            bound that is not a trait and a derive that does not apply to its type.",
        example: Example::Source(
            r#"trait Shape {
    fn area(self) -> I64;
}

fn total<T: Shape>(x: T) -> I64 { return x.area(); }

component A {
    state n = 0;
    action go() { n = total(1); }
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Pass a type that implements the trait, add the missing `impl`, or add the bound \
            the generic body needs.",
        fixed: Some(
            r#"record Square { side: I64; }
impl Shape for Square { fn area(self) -> I64 { return self.side * self.side; } }
action go() { n = total(Square { side: 2 }); }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2202",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Overlapping Or Ambiguous Impl",
        explanation: "Each trait may have at most one impl that can apply to a given type, so method \
            dispatch always has a single answer. Two impls of the same trait for the same type \
            overlap. A method call is ambiguous when several traits implemented for the \
            receiver provide a method of that name.",
        example: Example::Source(
            r#"trait Shape {
    fn area(self) -> I64;
}

record Square { side: I64; }

impl Shape for Square { fn area(self) -> I64 { return self.side * self.side; } }
impl Shape for Square { fn area(self) -> I64 { return 0; } }

component A {
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Remove or merge the duplicate impl. For an ambiguous method call, name the trait \
            explicitly, as in `Shape::area(value)`.",
        fixed: None,
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2203",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Unbounded Generic Instantiation",
        explanation: "Generic callables are monomorphized for each set of type arguments. When \
            instantiation recurses with ever-growing types, or exceeds the limits on chain \
            depth (32), total instances (4096) or type-argument size (256 nodes), the compiler \
            stops and reports it. This usually comes from a generic function that calls itself \
            with a type built from its own parameter.",
        example: Example::Source(
            r#"record Pair<A, B> { a: A; b: B; }

fn grow<T>(x: T, n: I64) -> I64 {
    if n == 0 { return 0; }
    return grow(Pair { a: x, b: x }, n - 1);
}

component A {
    state k = 0;
    action go() { k = grow(1, 3); }
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Make the recursive call use the same type arguments it was called with, or replace \
            the type-level recursion with a list or an enum that holds the growing structure \
            at runtime.",
        fixed: None,
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2301",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Non-Exhaustive Match",
        explanation: "A `match`, in expressions and in views, must cover every possible value of its \
            scrutinee. Missing a variant would leave some values without a result. The message \
            names the cases that are not covered.",
        example: Example::Source(
            r#"enum Tab { Home; Settings; }

component A {
    state tab = Tab::Home;
    view {
        match tab {
            Tab::Home => { Text { text: "home"; } },
        }
    }
}
"#,
        ),
        fix: "Add arms for the missing cases, or a final `_` arm when they should share one \
            result.",
        fixed: Some(
            r#"match tab {
    Tab::Home => { Text { text: "home"; } },
    Tab::Settings => { Text { text: "settings"; } },
}
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2302",
        severity: Severity::Warning,
        stage: Stage::Type,
        title: "Unreachable Pattern",
        explanation: "Match arms are tried in order, so an arm whose pattern is fully covered by the \
            arms before it can never run. This usually means a catch-all `_` or binding arm \
            was placed too early, or a case was written twice.",
        example: Example::Source(
            r#"enum Shape { Dot; Circle(I64); }

fn size(s: Shape) -> I64 {
    match s {
        _ => 0,
        Shape::Dot => 1,
    }
}

component A {
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Remove the unreachable arm, or move the more specific arms before the catch-all.",
        fixed: Some(
            r#"match s {
    Shape::Dot => 1,
    _ => 0,
}
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2303",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Refutable Pattern In Binding Position",
        explanation: "The patterns of `let`, `for` and closure parameters must match every value of \
            their type, because there is no other branch to take when they fail. A pattern \
            that can fail, such as `Some(x)`, an enum variant of a multi-variant enum or a \
            literal, is refutable and is only allowed in `match`.",
        example: Example::Source(
            r#"component A {
    fn first(o: Option<I64>) -> I64 {
        let Some(x) = o;
        x
    }
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Use a `match` with an arm for the values the pattern does not cover.",
        fixed: Some(
            r#"fn first(o: Option<I64>) -> I64 {
    match o {
        Some(x) => x,
        None => 0,
    }
}
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2401",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Closure Parameter Cannot Be Inferred",
        explanation: "A closure takes its parameter and result types from the function type it is \
            checked against, such as the parameter type of the function it is passed to or a \
            `let` annotation. When no such expected type exists and a parameter carries no \
            annotation, its type cannot be determined.",
        example: Example::Source(
            r#"component A {
    fn make() {
        let same = |x| x;
    }
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Annotate the parameter, or give the binding a function type.",
        fixed: Some(
            r#"let same = |x: I64| x;
let same: Fn(I64) -> I64 = |x| x;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2501",
        severity: Severity::Error,
        stage: Stage::Effect,
        title: "Effect Kind Violation",
        explanation: "Each kind of body allows a fixed set of effects. Only actions and event handlers \
            may write state, emit events or call actions; `fn` bodies, tasks, state, record \
            and constant initializers, and theme items must be pure. Positions that are \
            evaluated at compile time also reject non-constant expressions and non-`@const` \
            calls.",
        example: Example::Source(
            r#"component A {
    state n: I64 = 0;
    fn bump() { n += 1; }
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Move the mutation into an `action` (or a view handler), or make the function \
            return the new value and let an action store it.",
        fixed: Some(
            r#"action bump() { n += 1; }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2502",
        severity: Severity::Error,
        stage: Stage::Effect,
        title: "Side Effect In View Or Computed",
        explanation: "Views and computed values are re-evaluated whenever what they read changes, \
            possibly many times and in any order, so they must be pure. Writing state, \
            emitting events or calling an action inside a computed body or a view property \
            binding is rejected. Side effects belong in actions and event handlers.",
        example: Example::Source(
            r#"component A {
    state n: I64 = 0;
    computed doubled: I64 = { n = 2; n * 2 };
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Keep the computed a pure expression of what it reads, and perform the write in an \
            action or a handler such as `on click`.",
        fixed: Some(
            r#"computed doubled: I64 = n * 2;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2601",
        severity: Severity::Error,
        stage: Stage::Effect,
        title: "Missing Capability",
        explanation: "Native functions declare the capabilities they need, such as `network` or `timer`, \
            with a `requires` clause, and a call needs the capabilities of its callee. A \
            private callable without a clause infers its set from the call graph. An explicit \
            `requires` clause is a contract and an upper bound, so a callable whose call graph \
            needs a capability the clause does not list is a static error.",
        example: Example::Source(
            r#"native fn fetch(url: String) -> String requires { network };

fn load() -> String requires { timer } { fetch("https://example.com") }

component A {
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Add the missing capability to the `requires` clause of the callable (and of every \
            caller with a clause), or remove the call.",
        fixed: Some(
            r#"fn load() -> String requires { network } { fetch("https://example.com") }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2701",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Type Cannot Be A StableKey",
        explanation: "The `key` of a resource and the `key` of a keyed view `for` must have a \
            `StableKey` type, so that equal keys stay equal across runs and reloads. Integers, \
            `Bool`, `Char`, `String`, `Unit`, and tuples, records, options and enums built \
            only from them qualify. Floats, lists and other collections do not; the message \
            names the offending member, such as `K.b`.",
        example: Example::Source(
            r#"component A {
    state xs = [1.5, 2.5];
    view {
        Column {
            for x in xs key x { Text { text: "row"; } }
        }
    }
}
"#,
        ),
        fix: "Key by a stable value such as an integer id or a string; for rows, give each item \
            an id field.",
        fixed: Some(
            r#"record Row { id: I64; value: F64; }
state xs = [Row { id: 1, value: 1.5 }];
for x in xs key x.id { Text { text: "row"; } }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2702",
        severity: Severity::Error,
        stage: Stage::Runtime,
        title: "Duplicate Runtime Key",
        explanation: "The keys of a keyed `for` identify its rows, so that each row keeps its node, \
            state and animation when the list changes. Two items producing the same key value \
            at runtime make that identity ambiguous. The key type is checked at compile time, \
            but duplicates depend on the data and are detected while the app runs.",
        example: Example::Text(
            r#"A keyed list whose data contains two items with the same id:

    record Row { id: I64; name: String; }

    component List {
        state rows = [Row { id: 1, name: "a" }, Row { id: 1, name: "b" }];
        view {
            Column {
                for row in rows key row.id { Text { text: row.name; } }
            }
        }
    }

Rendering this list yields two rows with key `1`.
"#,
        ),
        fix: "Make every key unique within the list: key by a unique id, or deduplicate the data \
            before rendering it.",
        fixed: None,
        commands: &[],
        reserved: true,
    },
    CodeInfo {
        code: "E2801",
        severity: Severity::Error,
        stage: Stage::Parse,
        title: "Record Expression In Control Head Needs Parentheses",
        explanation: "In the head of `if`, `while`, `for` and `match`, a `{` after a path is read as the \
            start of the body block. A record expression written bare there would be ambiguous \
            with the body, so it must be parenthesized.",
        example: Example::Source(
            r#"record P { x: I64; }

fn check() -> Bool {
    if P { x: 1 }.x > 0 { return true; }
    return false;
}

component A {
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Wrap the record expression in parentheses.",
        fixed: Some(
            r#"if (P { x: 1 }).x > 0 { return true; }
"#,
        ),
        commands: &["viso fmt"],
        reserved: false,
    },
    CodeInfo {
        code: "E2802",
        severity: Severity::Error,
        stage: Stage::Parse,
        title: "Chained Non-Associative Operator",
        explanation: "Comparison, equality and range operators are non-associative: `a < b < c`, `a == b \
            != c` and `a .. b .. c` have no single obvious meaning, so chaining them at the \
            same level is an error rather than a silently chosen grouping.",
        example: Example::Source(
            r#"fn between(a: I64, b: I64, c: I64) -> Bool { a < b < c }

component A {
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Split the chain into separate comparisons joined by `&&`, or add parentheses that \
            state the intended grouping.",
        fixed: Some(
            r#"fn between(a: I64, b: I64, c: I64) -> Bool { a < b && b < c }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E2803",
        severity: Severity::Error,
        stage: Stage::Type,
        title: "Misplaced Control Transfer",
        explanation: "`break` and `continue` are valid only inside a loop body, and not across a closure \
            boundary; `break` with a value is allowed only in `loop`, not in `while` or `for`. \
            `return` is valid only inside a callable body. A control transfer outside these \
            positions has no target to jump to.",
        example: Example::Source(
            r#"component A {
    fn stop() { break; }
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Move the `break` or `continue` into a loop, use `return` to leave a function, and \
            use `loop` when the break carries a value.",
        fixed: Some(
            r#"fn stop() { return; }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3001",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Reserved Word Child",
        explanation: "`child` is a reserved word in a view and cannot name a node, a member or a node \
            prefix. A bare `Type { }` inside a node body is already an anonymous child node, \
            and `node id: Type { }` is a named one, so there is no `child` keyword for placing \
            children. The word is kept reserved so that code written in the style of other UI \
            languages gets a clear diagnostic instead of being read as a component name.",
        example: Example::Source(
            r#"component A {
    view {
        Column {
            child Text { text: "a"; }
        }
    }
}
"#,
        ),
        fix: "Delete `child`: write the child node directly in the parent's body, or give it a \
            name with `node id: Type { }`. The diagnostic carries a machine-applicable fix \
            that removes the word.",
        fixed: Some(
            r#"Column {
    Text { text: "a"; }
}
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3002",
        severity: Severity::Error,
        stage: Stage::View,
        title: "View Cardinality Not Satisfied",
        explanation: "A view block that must produce exactly one root node produces zero or several. A \
            component's `view` root and a `ui! { ... }` fragment embedded in Rust each have to \
            generate exactly one root node, because the runtime mounts them as a single node \
            with a single identity. Writing two sibling nodes at the top of such a block, or a \
            block that can produce no node at all, violates that cardinality.",
        example: Example::Text(
            r#"// Rust: a `ui!` fragment must generate exactly one root node.
let fragment = ui! {
    Text { text: "Title"; }
    Text { text: "Subtitle"; }
};
"#,
        ),
        fix: "Wrap the sibling nodes in one container (or a `Fragment` placed inside a \
            container) so the block has a single root.",
        fixed: Some(
            r#"let fragment = ui! {
    Column {
        Text { text: "Title"; }
        Text { text: "Subtitle"; }
    }
};
"#,
        ),
        commands: &[],
        reserved: true,
    },
    CodeInfo {
        code: "E3003",
        severity: Severity::Error,
        stage: Stage::View,
        title: "No Default Slot",
        explanation: "A node body contains bare child items, but the node's type has no default slot to \
            receive them. Bare structure items in a node body go into the type's `@default` \
            slot; leaf widgets such as `Text`, `Button` and `TextInput` have no slots at all, \
            and a user component without a `@default` slot only accepts children through \
            `fill`. This usually happens when a node is nested inside a leaf widget, or when a \
            component's slots are all named.",
        example: Example::Source(
            r#"component A {
    view {
        Column {
            Text {
                text: "Title";
                Text { text: "Subtitle"; }
            }
        }
    }
}
"#,
        ),
        fix: "Move the child out to a container that has a default slot, or fill one of the \
            component's named slots with `fill name { ... }`; if it is your own component, \
            mark one of its slots `@default`.",
        fixed: Some(
            r#"Column {
    Text { text: "Title"; }
    Text { text: "Subtitle"; }
}
"#,
        ),
        commands: &["viso schema <Component>"],
        reserved: false,
    },
    CodeInfo {
        code: "E3004",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Several Default Slots",
        explanation: "A component or template marks more than one slot `@default`. The default slot is \
            where bare child items of a node go, so there can be at most one per declaration; \
            with two, the compiler could not decide which slot a bare child fills. The \
            diagnostic points at the second `@default` and names the slot that already is the \
            default.",
        example: Example::Source(
            r#"component Panel {
    @default
    slot header: SlotList<Node> = empty;

    @default
    slot body: SlotList<Node> = empty;

    view {
        Column {
            SlotOutlet { slot: header; }
            SlotOutlet { slot: body; }
        }
    }
}
"#,
        ),
        fix: "Keep `@default` on one slot and remove it from the others; callers fill the other \
            slots by name with `fill`.",
        fixed: Some(
            r#"slot header: SlotList<Node> = empty;

@default
slot body: SlotList<Node> = empty;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3101",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Unknown Property",
        explanation: "A node binds a property that its type's schema does not declare. Widgets expose a \
            fixed schema, user components expose their `input`s, parent-provided properties \
            such as `grid.row` come from the parent container, and a template's `use` takes \
            the template's parameters; any other name is rejected. The usual cause is a \
            misspelling, and the diagnostic suggests the nearest known name. In a `style`, a \
            property that is not styleable is reported the same way.",
        example: Example::Source(
            r#"component A {
    view {
        Text { text: "a"; opacty: 0.5; }
    }
}
"#,
        ),
        fix: "Use a property the schema declares (often the suggested spelling); query the \
            schema to see every property of the type.",
        fixed: Some(
            r#"Text { text: "a"; opacity: 0.5; }
"#,
        ),
        commands: &["viso schema <Component>", "viso schema <Component> --json"],
        reserved: false,
    },
    CodeInfo {
        code: "E3102",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Property Bound Twice",
        explanation: "The same property is bound more than once in one node body (with `:` or `bind`), \
            one style block, or one template `use`. Each property of a node has exactly one \
            binding, so a second one would silently override the first. The diagnostic points \
            at the repeated binding and at the first one.",
        example: Example::Source(
            r#"component A {
    view {
        Text { text: "a"; text: "b"; }
    }
}
"#,
        ),
        fix: "Keep a single binding for the property; to choose between values, put the choice \
            in the expression.",
        fixed: Some(
            r#"Text { text: if wide { "long label" } else { "a" }; }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3103",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Property Is Not Two-Way",
        explanation: "`bind property <=> lens` is written on a property that does not support two-way \
            binding. Only properties the schema marks two-way can be bound: widget values such \
            as `Toggle.checked`, `Slider.value` or `TextInput.text`, and component inputs \
            marked `@bindable(event)`. Other properties are one-way and take a plain \
            `property: value;` binding.",
        example: Example::Source(
            r#"component A {
    state label: String = "a";

    view {
        Text { bind text <=> label; }
    }
}
"#,
        ),
        fix: "Bind the property one way with `:`; for a component input, mark it \
            `@bindable(event)` if the component should write it back.",
        fixed: Some(
            r#"Text { text: label; }
"#,
        ),
        commands: &["viso schema <Component>"],
        reserved: false,
    },
    CodeInfo {
        code: "E3104",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Percent Without Percent Basis",
        explanation: "A `Percent` value, or a `MixedLength` with a percent component, reaches a length \
            property whose schema declares no percent basis. A percentage only means something \
            relative to a known reference length, such as the parent's size for `width`; \
            properties like `translate` (`Offset`), `inset` or `estimated_extent` have none. \
            The value is followed through `state`, `computed`, locals and field access, and a \
            user component input that forwards to such a property has no basis either.",
        example: Example::Source(
            r#"component A {
    view {
        Text {
            text: "a";
            translate: Offset { x: 50%, y: 0dp };
        }
    }
}
"#,
        ),
        fix: "Use an absolute length (such as `dp`) for this property, or move the percentage to \
            a property that has a percent basis, like `width`.",
        fixed: Some(
            r#"Text {
    text: "a";
    translate: Offset { x: 24dp, y: 0dp };
}
"#,
        ),
        commands: &["viso schema <Component>.<property>"],
        reserved: false,
    },
    CodeInfo {
        code: "E3105",
        severity: Severity::Warning,
        stage: Stage::View,
        title: "Indefinite Percent Basis",
        explanation: "A debug-runtime warning: during layout a length with a percent component had no \
            definite reference length to resolve against. This happens when the parent is \
            `Fit` on that axis, along the scrolling axis of a `Scroll`, or under an unbounded \
            constraint; `fill` on an unbounded axis also degrades to `fit`. A value that is \
            only a percentage falls back to the property's default sizing, and the percent \
            part of a mixed length counts as zero. The runtime reports it once per node and \
            the release build stays silent.",
        example: Example::Text(
            r#"// Inside a Scroll the column axis is unbounded, so `50%` has no basis.
Scroll {
    Column {
        height: 50%;
        Text { text: "a"; }
    }
}
// debug runtime: E3105 warning: the percent basis of `height` is indefinite
"#,
        ),
        fix: "Give the parent a definite size on that axis (a fixed length or `fill` under a \
            bounded parent), or use an absolute length instead of a percentage where the axis \
            is unbounded.",
        fixed: None,
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3106",
        severity: Severity::Warning,
        stage: Stage::View,
        title: "Non-Finite Length",
        explanation: "A debug-runtime warning: a length resolved at run time to a non-finite value \
            (infinity or NaN), for example through a dynamic division by zero. Non-finite \
            values never enter layout or rendering; the property takes its schema default \
            instead, and the `length_nonfinite` counter is incremented. The release build does \
            not panic and reports nothing.",
        example: Example::Text(
            r#"component A {
    state parts: I64 = 0;

    view {
        Row {
            // `parts` is 0 at run time, so the width is infinite.
            width: 320dp / parts;
            Text { text: "a"; }
        }
    }
}
// debug runtime: E3106 warning: `width` resolved to a non-finite length
"#,
        ),
        fix: "Guard the computation so it always yields a finite length, for example by \
            branching on the divisor.",
        fixed: Some(
            r#"width: if parts > 0 { 320dp / parts } else { 320dp };
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3107",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Bind Source Is Not A State Lens",
        explanation: "The right-hand side of `bind property <=> lens` is not a writable state lens. A \
            two-way binding writes the new value back, so the lens must be a `state` of the \
            component or a field or index path into one (`settings.volume`, `items[0]`). An \
            `input`, `computed`, `const`, local binding, loop variable or function result \
            cannot be written and is rejected.",
        example: Example::Source(
            r#"component A {
    state on: Bool = true;
    computed off: Bool = !on;

    view {
        Toggle { bind checked <=> off; }
    }
}
"#,
        ),
        fix: "Bind to the `state` itself (or a path into it), or bind one way with `:` and \
            handle the widget's change event yourself.",
        fixed: Some(
            r#"Toggle { checked: off; on changed(e) { on = !e.value; } }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3108",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Invalid Grid Area Or Cell",
        explanation: "A grid's `areas` or a child's `grid.*` names no cells. Each string of `areas` \
            is a row of whitespace-separated area names, `.` an empty cell: every row names the \
            same number of cells, and each area fills a rectangle. A child's `grid.area` names \
            an area of its grid's `areas` and places the child by itself, so it is not combined \
            with `grid.column`, `grid.row` or a span; a `grid.column_span` or `grid.row_span` \
            spans at least one track. The names resolve at compile time, so none reaches the \
            runtime.",
        example: Example::Source(
            r#"component A {
    view {
        Grid {
            areas: ["head head", "side main"];
            Text { text: "a"; grid.area: "foot"; }
        }
    }
}
"#,
        ),
        fix: "Name an area of the grid's `areas`, or add the area to the template.",
        fixed: Some(
            r#"Text { text: "a"; grid.area: "main"; }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3201",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Arrow Event Handler",
        explanation: "An event handler is written with an arrow, `on event => statement`. Handlers are \
            blocks: `on click { ... }`, or `on click(event) { ... }` to name the payload. The \
            arrow form is rejected so that a handler is always a block of statements with one \
            shape.",
        example: Example::Source(
            r#"component A {
    state n: I64 = 0;

    view {
        Button { text: "Add"; on click => n += 1; }
    }
}
"#,
        ),
        fix: "Wrap the statement in a block; the diagnostic carries a machine-applicable fix \
            that does this.",
        fixed: Some(
            r#"Button { text: "Add"; on click { n += 1; } }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3202",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Unknown Event Or Wrong Payload",
        explanation: "A handler names an event the node does not accept, or an `emit` does not match its \
            event. A user component node accepts the standard events and the events the \
            component declares; a built-in widget accepts the events its schema declares plus \
            the standard ones, and `FocusScope`, `KeyShortcut` and `Fragment` take no standard \
            events. `emit name(...)` must name an event of its own component and give each \
            parameter exactly once, by position or as `name: value`. A misspelled event name \
            is the usual cause, and the diagnostic suggests the nearest one.",
        example: Example::Source(
            r#"component A {
    state n: I64 = 0;

    view {
        Button { text: "Add"; on clik { n += 1; } }
    }
}
"#,
        ),
        fix: "Use an event the node declares (often the suggested spelling), and give an `emit` \
            exactly the parameters its event declares.",
        fixed: Some(
            r#"Button { text: "Add"; on click { n += 1; } }
"#,
        ),
        commands: &["viso schema <Component>"],
        reserved: false,
    },
    CodeInfo {
        code: "E3301",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Invalid Preserve Identity",
        explanation: "`if condition preserve \"name\"` keeps a branch's nodes and state alive while it \
            is switched away. The name after `preserve` must be a compile-time string literal, \
            and it must be unique among the preserved branches of the component, because it is \
            the identity under which the branch instance is cached. A computed name, or the \
            same name used twice, is rejected; the diagnostic for a duplicate points at the \
            first use.",
        example: Example::Source(
            r#"component A {
    state open: Bool = false;

    view {
        Column {
            if open preserve "panel" { Text { text: "a"; } }
            if !open preserve "panel" { Text { text: "b"; } }
        }
    }
}
"#,
        ),
        fix: "Give each preserved branch its own string literal. For a dynamic collection, use a \
            keyed `for` instead of `preserve`.",
        fixed: Some(
            r#"if open preserve "open-panel" { Text { text: "a"; } }
if !open preserve "closed-panel" { Text { text: "b"; } }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3401",
        severity: Severity::Error,
        stage: Stage::View,
        title: "View For Without Key",
        explanation: "A `for` in a view has no `key` clause. Every view `for` must be written `for item \
            in items key expression { ... }`: the key gives each item a stable identity, so \
            reordering, inserting or removing items moves the retained nodes, child state, \
            focus and tasks with their item instead of rebuilding them by position. The key \
            must implement `StableKey` and should be a value of the item, such as an id.",
        example: Example::Source(
            r#"component A {
    state names: List<String> = ["a", "b"];

    view {
        Column {
            for name in names {
                Text { text: name; }
            }
        }
    }
}
"#,
        ),
        fix: "Add `key` followed by an expression that identifies the item independent of its \
            position.",
        fixed: Some(
            r#"for name in names key name {
    Text { text: name; }
}
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3402",
        severity: Severity::Warning,
        stage: Stage::View,
        title: "Unstable Key Expression",
        explanation: "Repeated content that carries per-item state (an event handler, a reactive binding \
            or a nested list) has no stable key. Without a key that follows the item, \
            reordering the list rebuilds each item's state against a different item instead of \
            moving it. The key expression may only read the loop pattern, immutable outer \
            values and pure functions, and must be unique per item. The compiler reports this \
            alongside the missing-key error when the `for` body is stateful.",
        example: Example::Source(
            r#"component A {
    state names: List<String> = ["a", "b"];
    state picked: String = "";

    view {
        Column {
            for name in names {
                Button { text: name; on click { picked = name; } }
            }
        }
    }
}
"#,
        ),
        fix: "Key the `for` by a stable value of the item, such as its id, not its position.",
        fixed: Some(
            r#"for name in names key name {
    Button { text: name; on click { picked = name; } }
}
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3501",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Unknown Slot Or Part",
        explanation: "A `fill`, `SlotOutlet`, `override part` or `replace part` names a slot or part the \
            target does not declare. `fill name { ... }` must name a slot of the placed \
            component or template, `override part`/`replace part` a part it exposes, and \
            `SlotOutlet { slot: name; }` a slot of the component whose view it is in. The \
            usual cause is a misspelling, and the diagnostic suggests the nearest name.",
        example: Example::Source(
            r#"component Card {
    slot footer: OptionalSlot<Node> = None;

    view {
        Column { SlotOutlet { slot: footer; } }
    }
}

export component App {
    view {
        Card {
            fill foter { Text { text: "Done"; } }
        }
    }
}
"#,
        ),
        fix: "Name a slot or part the target declares; query the component's schema to list \
            them.",
        fixed: Some(
            r#"Card {
    fill footer { Text { text: "Done"; } }
}
"#,
        ),
        commands: &["viso schema <Component>"],
        reserved: false,
    },
    CodeInfo {
        code: "E3502",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Slot Cardinality Conflict",
        explanation: "The nodes given to a slot do not fit its cardinality. A `Slot<Node>` takes exactly \
            one node and has no default, an `OptionalSlot<Node>` takes zero or one, and a \
            `SlotList<Node>` any number. Cardinality is checked statically over the number of \
            nodes each item can produce: a node counts one, `if` without `else` can produce \
            zero, and `for` any number. It is also a conflict to fill a single slot twice, to \
            mix bare children with `fill` of the default slot, to leave a `Slot<Node>` \
            unfilled, or to place a slot's `SlotOutlet` twice or inside a `for`.",
        example: Example::Source(
            r#"component A {
    view {
        Scroll {
            Text { text: "a"; }
            Text { text: "b"; }
        }
    }
}
"#,
        ),
        fix: "Give the slot as many nodes as its type allows, for example by wrapping several \
            nodes in one container; declare the slot `SlotList<Node>` if it should take any \
            number.",
        fixed: Some(
            r#"Scroll {
    Column {
        Text { text: "a"; }
        Text { text: "b"; }
    }
}
"#,
        ),
        commands: &["viso schema <Component>"],
        reserved: false,
    },
    CodeInfo {
        code: "E3601",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Invalid Template",
        explanation: "A template is a stateless piece of view structure expanded where each `use` places \
            it. Its members can only be `slot`, `const`, `fn` and `view`, and it must have a \
            `view`; `state`, `input`, `action` and other members are rejected. A `use` body \
            holds only `fill`, `override part` and `replace part`, since the template's \
            parameters are passed as arguments. A template that places itself, directly or \
            through another template, would expand forever and is also rejected.",
        example: Example::Source(
            r#"template Badge() {
    state count: I64 = 0;

    view { Text { text: "badge"; } }
}

export component App {
    view {
        Column { use Badge(); }
    }
}
"#,
        ),
        fix: "Keep only `slot`, `const`, `fn` and `view` in a template and pass values as \
            parameters; if the piece needs its own state, make it a `component`.",
        fixed: Some(
            r#"template Badge(label: String) {
    view { Text { text: label; } }
}
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3701",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Invalid Bindable Input",
        explanation: "`@bindable(event)` makes a component input two-way, so callers can write `bind \
            input <=> state;`. It marks only an `input`, takes exactly one event name, and \
            that event must be declared in the same component with a first parameter of the \
            input's type. The component never assigns the input; it emits the event with the \
            requested new value, and the binding writes it back to the caller's state.",
        example: Example::Source(
            r#"component Stepper {
    @bindable(moved)
    input value: I64;

    event changed(value: I64);

    view { Text { text: "step"; } }
}
"#,
        ),
        fix: "Name an event of the component whose first parameter has the input's type, and put \
            the attribute on an `input`.",
        fixed: Some(
            r#"@bindable(changed)
input value: I64;

event changed(value: I64);
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3702",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Misplaced Parent-Provided Property",
        explanation: "A parent-provided property (`grid.*`, `stack.*`, `absolute.*`) is written on a \
            node whose direct parent does not provide it, or whose parent cannot be determined \
            statically. These properties are declared by the parent container's schema and \
            only mean something there, such as a grid cell. `Fragment`, `if`, `match` and \
            `for` do not count as parents, but a user component's default slot does hide the \
            real container, so the parent is unknown.",
        example: Example::Source(
            r#"component A {
    view {
        Row {
            Text { text: "a"; grid.row: 1; }
        }
    }
}
"#,
        ),
        fix: "Place the node directly in the container that provides the prefix, or drop the \
            property.",
        fixed: Some(
            r#"Grid {
    Text { text: "a"; grid.row: 1; }
}
"#,
        ),
        commands: &["viso schema <Component>"],
        reserved: false,
    },
    CodeInfo {
        code: "E3703",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Invalid Transition",
        explanation: "`transition.p` animates changes of property `p` on the same node. `p` must be \
            marked animatable in the schema (such as `opacity`, `background` or `translate`), \
            and the value must be a `Transition` record. A transition on a non-animatable \
            property like `text`, or a bare duration instead of a `Transition`, is rejected.",
        example: Example::Source(
            r#"component A {
    view {
        Text {
            text: "a";
            transition.opacity: 200ms;
        }
    }
}
"#,
        ),
        fix: "Give an animatable property a `Transition` value.",
        fixed: Some(
            r#"Text {
    text: "a";
    transition.opacity: Transition { duration: 200ms };
}
"#,
        ),
        commands: &["viso schema <Component>"],
        reserved: false,
    },
    CodeInfo {
        code: "E3704",
        severity: Severity::Warning,
        stage: Stage::View,
        title: "Interactive Node Without Role Or Name",
        explanation: "A node that is not a standard interactive widget (for example a `Row`, or the root \
            of a component's view) handles `click`, `tap`, `long_press`, `drag_*` or \
            `key_down`, but assistive technology cannot describe it. Such a node needs a \
            `semantics.role` other than `Role::group` and an accessible name, from \
            `semantics.label` or text in its subtree. This is a warning by default; `viso \
            check --a11y strict` makes it an error.",
        example: Example::Source(
            r#"component A {
    state n: I64 = 0;

    view {
        Row {
            focusable: true;
            semantics.label: "Add";
            on click { n += 1; }
        }
    }
}
"#,
        ),
        fix: "Give the node a role and a name, or use a standard interactive widget such as \
            `Button`.",
        fixed: Some(
            r#"Row {
    focusable: true;
    semantics.role: Role::button;
    semantics.label: "Add";
    on click { n += 1; }
}
"#,
        ),
        commands: &["viso check --a11y strict"],
        reserved: false,
    },
    CodeInfo {
        code: "E3705",
        severity: Severity::Warning,
        stage: Stage::View,
        title: "Concatenated Localizable Text",
        explanation: "A localizable property (`Text.text`, `Button.text`, `Toggle.label`, \
            `TextInput.placeholder`, `semantics.label`, `semantics.hint` and similar) is built \
            by string concatenation or by a `format` whose template contains literal words or \
            more than one placeholder. Word order differs between languages, so such text \
            cannot be translated; a message with named arguments through `tr` can. \
            `format(\"{}\", n)` with a single placeholder is allowed. This is a warning by \
            default; `viso check --i18n strict` makes it an error and also reports plain \
            literals.",
        example: Example::Source(
            r#"component A {
    state name: String = "Ada";

    view {
        Text { text: "Hello " + name; }
    }
}
"#,
        ),
        fix: "Move the sentence into the message catalog and pass the variable parts as named \
            arguments of `tr`.",
        fixed: Some(
            r#"Text { text: tr("greeting", name: name); }
"#,
        ),
        commands: &["viso check --i18n strict"],
        reserved: false,
    },
    CodeInfo {
        code: "E3706",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Message Catalog Mismatch",
        explanation: "A `tr` call or `MessageKey` literal does not match the package's message catalog, \
            or the catalog itself has an error. A key written as a string literal must be a \
            message of the source-locale catalog (`i18n/en.toml` by default), each message \
            argument must be passed exactly once by name with a compatible kind, and a \
            `MessageKey` literal must name a message without arguments. Using `tr` in a \
            package with no catalog is also reported, as are catalog problems: invalid TOML or \
            message syntax, invalid or duplicate locales, a missing source catalog, or \
            translations using arguments the source does not declare.",
        example: Example::Source(
            r#"import viso::i18n::tr;

component A {
    view {
        Text { text: tr("inbox.title"); }
    }
}
"#,
        ),
        fix: "Add the message to the source catalog (`i18n/en.toml` beside `Viso.toml`), or use \
            a key the catalog has (the diagnostic suggests the nearest), and pass exactly the \
            arguments the message declares.",
        fixed: Some(
            r#"# i18n/en.toml
[inbox]
title = "Inbox"
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E3707",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Invalid VirtualList Item Template",
        explanation: "A `VirtualList` mounts only the items inside its viewport and overscan, so its \
            body is an item template rather than ordinary children. The body must contain \
            exactly one keyed `for`, which the compiler lowers to the virtualized template. A \
            `for` without `key`, several `for`s, or any other child node in the body is \
            rejected.",
        example: Example::Text(
            r#"VirtualList {
    Text { text: "Header"; }            // not allowed: a child beside the template
    for message in messages key message.id {
        MessageRow { message: message; }
    }
}
"#,
        ),
        fix: "Keep a single keyed `for` as the body of the `VirtualList`, and put headers or \
            other nodes outside it.",
        fixed: Some(
            r#"Column {
    Text { text: "Header"; }
    VirtualList {
        for message in messages key message.id {
            MessageRow { message: message; }
        }
    }
}
"#,
        ),
        commands: &["viso schema VirtualList"],
        reserved: false,
    },
    CodeInfo {
        code: "E3708",
        severity: Severity::Warning,
        stage: Stage::View,
        title: "Interactive Node Without Keyboard Path",
        explanation: "A node that is not a standard interactive widget handles a pointer or key gesture \
            but cannot be operated from the keyboard. An equivalent keyboard path means the \
            node is `focusable: true` and handles `click`, which keyboard activation also \
            triggers; handling only `tap` or `pointer_*` does not count. This is a warning by \
            default; `viso check --a11y strict` makes it an error.",
        example: Example::Source(
            r#"component A {
    state n: I64 = 0;

    view {
        Row {
            semantics.role: Role::button;
            Text { text: "Add"; }
            on click { n += 1; }
        }
    }
}
"#,
        ),
        fix: "Make the node focusable and handle `click`, or use a standard interactive widget \
            such as `Button`.",
        fixed: Some(
            r#"Row {
    focusable: true;
    semantics.role: Role::button;
    Text { text: "Add"; }
    on click { n += 1; }
}
"#,
        ),
        commands: &["viso check --a11y strict"],
        reserved: false,
    },
    CodeInfo {
        code: "E3709",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Property Not Yet Supported By Runtime",
        explanation: "A property whose schema entry is marked as not yet implemented by the runtime is \
            given a non-default value. The schema and type checking of such entries are fixed, \
            but the runtime does not apply them, so a non-default value would silently do \
            nothing. The debug runtime reports it and the release build rejects the value.",
        example: Example::Text(
            r#"// Suppose the schema marks `Text.max_lines` as pending in the runtime.
Text {
    text: "A long paragraph";
    max_lines: 2;          // non-default value: E3709
}
"#,
        ),
        fix: "Leave the property at its default until the runtime supports it, or reach the \
            effect through a supported property; `viso schema` shows which entries are \
            pending.",
        fixed: None,
        commands: &["viso schema <Component>"],
        reserved: true,
    },
    CodeInfo {
        code: "E3710",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Invalid Styleable Or Selector Member",
        explanation: "`@styleable` and `@selector` expose a component member to styles. `@styleable` \
            marks only an `input`, which a `style` for the component may then bind. \
            `@selector` marks a `Bool` `input`, `state` or `computed`, published as a selector \
            named after the member that a style's `when` can test. A selector may not take the \
            name of a standard selector the runtime decides: `hover`, `pressed`, `focused`, \
            `focus_visible`, `disabled` or `dragging`.",
        example: Example::Source(
            r#"component Chip {
    @selector
    input count: I64 = 0;

    view { Text { text: "chip"; } }
}
"#,
        ),
        fix: "Put `@selector` on a `Bool` member with a name of its own, and `@styleable` on an \
            `input`.",
        fixed: Some(
            r#"@selector
input selected: Bool = false;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3711",
        severity: Severity::Error,
        stage: Stage::View,
        title: "View Cannot Be Mounted",
        explanation: "Part of the view cannot be lowered to something the runtime mounts. Common causes: \
            a control-flow region (`if`, `match`, `for`) at the root of a view, where the root \
            must be one node; a component that mounts itself directly or indirectly; a \
            property or handler the runtime has no path for on that widget; `padding` on a node \
            without a content box; a grid's `areas` or a child's `grid.area` that reads a value; \
            a layout property a style binds to a value; a `bind` to a component input with `using`; properties forwarded \
            to a component whose view does not mount exactly one node; and styles switching a \
            user component's input by an interaction selector, or switching under `when` a \
            property the runtime does not deliver or a layout property they give no base. The message names the specific reason.",
        example: Example::Source(
            r#"component A {
    view {
        Column {
            Text { text: "a"; padding: 8dp; }
        }
    }
}
"#,
        ),
        fix: "Address the reason the message names. For `padding` on a node without a \
            content box, pad a container around it; for a region at the view root, wrap it in \
            a container so the root is one node.",
        fixed: Some(
            r#"Column {
    padding: 8dp;
    Text { text: "a"; }
}
"#,
        ),
        commands: &["viso check", "viso schema <Component>"],
        reserved: false,
    },
    CodeInfo {
        code: "E3712",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Invalid Migrate Function",
        explanation: "`@migrate(from: \"Type\")` declares a function that converts persisted state of an \
            older type into the current one when the declared type of a `state` changes. It \
            marks only a `fn`, `from` must be given as a string spelling a type, the function \
            takes exactly one parameter of that type, and it must declare its return type. Two \
            `@migrate` functions with the same `from` and return type are ambiguous and \
            rejected.",
        example: Example::Source(
            r#"@migrate(from: "I64")
fn widen(old: I64) { }

component A {
    view { Text { text: "a"; } }
}
"#,
        ),
        fix: "Mark a `fn` with one parameter of the `from` type and declare the type it returns.",
        fixed: Some(
            r#"@migrate(from: "I64")
fn widen(old: I64) -> F64 { old as F64 }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E3713",
        severity: Severity::Error,
        stage: Stage::View,
        title: "Invalid Asset",
        explanation: "`asset(\"path\")` names a file under the package's `assets/` directory and is \
            checked at compile time, so the package knows every asset it ships. It takes \
            exactly one argument, a string literal without interpolation; the path is relative \
            to `assets/` and may not climb out of it; the file must exist and be an image, an SVG \
            or a video. A compile without an assets directory, and a record expression building an \
            `ImageAsset`, `SvgAsset` or `VideoAsset` directly, are also rejected. To choose between assets \
            at run time, branch between `asset(\"...\")` values.",
        example: Example::Source(
            r#"component A {
    state dark: Bool = false;
    state logo: String = "images/logo.png";

    view {
        Image { source: asset(logo); width: 48dp; }
    }
}
"#,
        ),
        fix: "Pass a string literal naming an existing file under `assets/`, and branch between \
            literal assets when the choice is dynamic; for a missing file, use the path the \
            diagnostic suggests.",
        fixed: Some(
            r#"Image {
    source: if dark { asset("images/logo_dark.png") } else { asset("images/logo.png") };
    width: 48dp;
}
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E4101",
        severity: Severity::Error,
        stage: Stage::Async,
        title: "Await Outside a Task",
        explanation: "`await` marks a suspension point, and only a `task` body may suspend. Actions, \
            event handlers, effects, computed values and functions run to completion inside \
            one transaction on the UI thread, so they cannot wait for a future. This is raised \
            when `await` appears in any body that is not a `task`.",
        example: Example::Source(
            r#"import viso::time;

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

    view { Text { text: "timer"; on click { go(); } } }
}
"#,
        ),
        fix: "Move the waiting into a `task` and start it from the action; handle its result in \
            a `success` handler.",
        fixed: Some(
            r#"action go() {
    start wait() { success(n) { done = true; } };
}
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E4102",
        severity: Severity::Error,
        stage: Stage::Async,
        title: "Task Reads State After Suspending",
        explanation: "A task sees the state and inputs as they were when it started, but only until its \
            first suspension point (an `await` or a call to another task). After that the \
            component may have changed, so reading state, inputs, computed values, or a `fn` \
            that reads them is rejected, as is any such read inside a loop that suspends. A \
            read that slips past the check, for example through a closure created before the \
            suspension, ends the task with this code at run time.",
        example: Example::Source(
            r#"import viso::time;

export component Poller {
    state count = 0;
    state seen = 0;

    task look() -> I64 {
        await time::sleep(1s);
        count
    }

    view {
        Text {
            text: "poll";
            on click { start look() { success(v) { seen = v; } }; }
        }
    }
}
"#,
        ),
        fix: "Read what the task needs before it first suspends (or pass it as an argument), and \
            apply the result to current state in the `success` handler.",
        fixed: Some(
            r#"task look(start: I64) -> I64 {
    await time::sleep(1s);
    start
}
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E4201",
        severity: Severity::Error,
        stage: Stage::Effect,
        title: "Effect Reads an Undeclared Dependency",
        explanation: "An effect's `when (...)` list is its complete dependency list: the effect runs \
            again only when those values change. Reading a state, input or computed in the \
            body that the list does not cover would make the effect silently stale, so it is \
            rejected. A computed in the list does not cover the values it is derived from, \
            because it may stay equal while they change.",
        example: Example::Source(
            r#"export component Search {
    state query = "";
    state page = 0;

    effect log when (query) {
        let at = page;
    }

    view { Text { text: query; } }
}
"#,
        ),
        fix: "Add the value to the `when` list, or wrap a read that should not retrigger the \
            effect in `untracked(..)`.",
        fixed: Some(
            r#"effect log when (query, page) {
    let at = page;
}
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E4202",
        severity: Severity::Error,
        stage: Stage::Runtime,
        title: "Reactive Cycle",
        explanation: "A frame's state flush keeps scheduling computed values, bindings and effects while \
            new writes appear. If writes are still pending after a bounded number of rounds, \
            the reactive graph is feeding itself: typically an effect writes one of its own \
            dependencies, or a computed's evaluation reaches itself. The runtime stops that \
            frame's flush, reports the dropped cells and the effects they would wake, and \
            keeps the values already written without running their reactions.",
        example: Example::Text(
            r#"Running this component and clicking the text raises E4202: the effect
depends on `count` and writes `count`, so every run schedules another.

    export component Loop {
        state count = 0;
        view { Text { text: "tap"; on click { count += 1; } } }
        effect bump when (count) run EffectRun::change {
            transaction { count += 1; }
        }
    }
"#,
        ),
        fix: "Break the loop: do not write an effect's own dependencies from its body. Derive \
            the value with a `computed`, or move the write into the action that causes the \
            change.",
        fixed: Some(
            r#"computed shown = count + 1;
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E4203",
        severity: Severity::Error,
        stage: Stage::Effect,
        title: "Effect Run Policy Does Not Fit Its Dependencies",
        explanation: "The `run` policy of an effect must agree with its `when` list. Without `when`, an \
            effect can only run on mount (`EffectRun::mount`); `EffectRun::change` and \
            `EffectRun::mount_and_change` need a non-empty `when (...)`; and \
            `EffectRun::mount` cannot have one. An unknown policy name is also rejected.",
        example: Example::Source(
            r#"export component Theme {
    state name = "light";

    effect save run EffectRun::change {
    }

    view { Text { text: name; } }
}
"#,
        ),
        fix: "Give a change-driven effect the values it watches, or drop the policy to run once \
            on mount.",
        fixed: Some(
            r#"effect save when (name) run EffectRun::change {
}
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E4204",
        severity: Severity::Error,
        stage: Stage::Runtime,
        title: "Adaptive Cycle",
        explanation: "Within a frame, layout settles the adaptive environment (constraints and size \
            class), the structure it selects is updated, and layout runs again until nothing \
            changes. If the structure keeps moving the very constraints that select it, the \
            loop does not settle within its bound. The runtime then keeps the new environment \
            values, drops the reactions to them and keeps the last structure, instead of \
            measuring forever. It usually means a structural branch depends on a size the \
            subtree itself determines.",
        example: Example::Text(
            r#"A view whose `if env.size_class == SizeClass::Compact` branch mounts a
wide subtree, which pushes the enclosing adaptive scope out of compact,
which unmounts the subtree, which makes it compact again. The size class
flips on every layout and the frame stops with E4204.
"#,
        ),
        fix: "Let structural branches depend only on values fixed before the node is measured: \
            `env.constraints` and the size class of an adaptive scope whose basis comes from \
            its parent. Drive measured sizes into properties (width, spacing, color) rather \
            than into `if` / `match` / `for` structure.",
        fixed: None,
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E4301",
        severity: Severity::Error,
        stage: Stage::Resource,
        title: "Resource Missing Load or Key",
        explanation: "A `resource` block must say how to load its value (`load = ...`) and what \
            identifies a load (`key = ...`), each exactly once. Every other item (`policy`, \
            `scope`) may also appear at most once. This is raised when `load` or `key` is \
            missing or any item is repeated.",
        example: Example::Source(
            r#"import viso::time;

export component Profile {
    state user = 1;

    task fetch(id: I64) -> Result<I64, String> {
        await time::sleep(1s);
        Ok(id)
    }

    resource score: Resource<I64, String> {
        load = fetch(user);
    }

    view { Text { text: "profile"; } }
}
"#,
        ),
        fix: "Give the resource both a loader and a key, once each.",
        fixed: Some(
            r#"resource score: Resource<I64, String> {
    load = fetch(user);
    key = user;
}
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E4302",
        severity: Severity::Error,
        stage: Stage::Resource,
        title: "Invalid Resource or Task Policy",
        explanation: "A resource `policy` is a list of `ResourcePolicy::keep_latest`, `debounce(d)`, \
            `cache_for(d)` and `cache_errors`, each at most once, with `d` a constant duration \
            above zero; `cache_errors` needs `cache_for`, and `scope` accepts only \
            `ResourceScope::component`. A `start` block takes exactly one `TaskPolicy` \
            (`keep_latest`, `drop_new`, `queue` or `parallel(n)`), only on a named slot \
            (`start task() as slot { ... }`), and each handler at most once. Anything else is \
            rejected.",
        example: Example::Source(
            r#"import viso::time;

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

    view { Text { text: "upload"; on click { go(); } } }
}
"#,
        ),
        fix: "Name the slot the policy governs, and give one valid policy.",
        fixed: Some(
            r#"start upload() as uploads {
    policy = [TaskPolicy::queue];
    success(n) { sent += n; }
};
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E4401",
        severity: Severity::Error,
        stage: Stage::Async,
        title: "Start Target Is Not a Task",
        explanation: "`start` launches a task on the UI task protocol, and a resource's `load` runs one; \
            both need a call to a `task` (or a native task). A plain function, an action or \
            any other expression has nothing to run asynchronously, so it is rejected. Call an \
            action directly instead of starting it.",
        example: Example::Source(
            r#"export component Counter {
    state count = 0;

    action reset() {
        count = 0;
    }

    action go() {
        start reset();
    }

    view { Text { text: "reset"; on click { go(); } } }
}
"#,
        ),
        fix: "Call an action directly; use `start` only with a task call.",
        fixed: Some(
            r#"action go() {
    reset();
}
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E4501",
        severity: Severity::Error,
        stage: Stage::Async,
        title: "Detached Task",
        explanation: "Every started task belongs to a component instance: it is cancelled when that \
            instance unmounts or reloads, and its handlers run against that instance. A \
            module-level `action` has no instance, so a `start` in it would create a task \
            nobody owns. This is raised for a `start` in an action declared outside any \
            component.",
        example: Example::Source(
            r#"task wait() -> I64 { 3 }

action go() {
    start wait();
}

export component Panel {
    view { Text { text: "panel"; } }
}
"#,
        ),
        fix: "Start the task from an action, event handler or effect of the component that owns \
            it.",
        fixed: Some(
            r#"export component Panel {
    action go() { start wait(); }
    view { Text { text: "panel"; on click { go(); } } }
}
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E5101",
        severity: Severity::Warning,
        stage: Stage::HotReload,
        title: "State Reset on Hot Reload",
        explanation: "When a reload changes a state's type, the live value is carried into the new type \
            when the new type can hold it, or through a `@migrate` function from the old type. \
            If neither works, because the value does not fit or the migrate function faults, \
            the state restarts from its new initializer and the reload reports this warning. \
            The rest of the reload still commits.",
        example: Example::Text(
            r#"Before the edit, the running view holds `count == 300`:

    state count = 0;

After the edit the state is an `I8`, which cannot hold 300, and there is
no `@migrate` function, so `count` resets to 1 with E5101:

    state count: I8 = 1;
"#,
        ),
        fix: "If the value matters, add a `@migrate` function from the old type that always \
            produces a valid new value; otherwise the reset is expected and the warning can be \
            ignored.",
        fixed: Some(
            r#"@migrate(from: "I64")
fn clamp(old: I64) -> I8 { if old > 127 { 127 } else { 0 } }
"#,
        ),
        commands: &["viso run"],
        reserved: false,
    },
    CodeInfo {
        code: "E5102",
        severity: Severity::Error,
        stage: Stage::HotReload,
        title: "Stable Identity Collision on Reload",
        explanation: "A reload migrates each live state into the cell of the new build with the same \
            stable identity. If two states of the candidate build end up with the same \
            identity, the reload would migrate two states into one cell, so the whole reload \
            is rejected and the running build keeps its last good state. It typically follows \
            from a `@stable` pin that spells the identity another state already has.",
        example: Example::Text(
            r#"A view running with `state count = 0;` is edited to add a second state
pinned with `@stable(...)` to the identity that `count` already mints from
its name. The candidate has two states with one stable identity, the
reload is rejected with E5102, and the old build keeps running.
"#,
        ),
        fix: "Give every state its own identity: remove the conflicting `@stable` pin, or choose \
            an id no other declaration of the component uses.",
        fixed: None,
        commands: &["viso run"],
        reserved: false,
    },
    CodeInfo {
        code: "E5103",
        severity: Severity::Note,
        stage: Stage::HotReload,
        title: "Game Reload Tier",
        explanation: "On a game reload the compiler compares the running build with the candidate by \
            stable identity and picks the least disruptive layer every change allows: for \
            example presentation-only, logic-only, or a world rebuild. This note reports the \
            chosen tier, its primary span at the first change that decided it and every other \
            change as a related span. It is informational and does not block the reload.",
        example: Example::Text(
            r#"Editing a running game's simulation hook from `steps += 1;` to
`steps += 2;` and a presentation helper from `x * 0.5f32` to `x * 0.25f32`
reports:

    game reload: logic-only (simulation code changed: `Mover.fixed_update`)

with the presentation change as a related span.
"#,
        ),
        fix: "No fix is needed. If the tier is more disruptive than expected, check the changes \
            it lists: keep simulation code, state layout and the input map unchanged when only \
            presentation should reload.",
        fixed: None,
        commands: &["viso run"],
        reserved: false,
    },
    CodeInfo {
        code: "E5104",
        severity: Severity::Error,
        stage: Stage::Resolve,
        title: "Duplicate Stable Id",
        explanation: "`@stable(\"id\")` pins a declaration's identity so that it survives renames across \
            reloads. Two declarations or members of the same kind in one module that claim the \
            same id would mint the same symbol identity, so the second claim is rejected. This \
            usually comes from copying a pinned member and forgetting to change its id.",
        example: Example::Source(
            r#"export component Counter {
    @stable("count")
    state clicks = 0;
    @stable("count")
    state taps = 0;

    view { Text { text: "count"; } }
}
"#,
        ),
        fix: "Give each pinned declaration a distinct id.",
        fixed: Some(
            r#"@stable("count")
state clicks = 0;
@stable("taps")
state taps = 0;
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E6101",
        severity: Severity::Error,
        stage: Stage::Native,
        title: "Native Declaration Does Not Match the Schema",
        explanation: "Native APIs come from the generated native schema registry. A handwritten `native` \
            declaration must match the registry entry for its library exactly (same kind, \
            signature and capabilities) and be written only in native schema types: no records \
            or other DSL-only types, no generics, defaults or bounds. The code is also raised \
            when a module needs a native library version the host does not provide.",
        example: Example::Source(
            r#"record Point { x: I64 = 0; }

native fn measure(p: Point) -> I64;

export component Ruler {
    view { Text { text: "ruler"; } }
}
"#,
        ),
        fix: "Write the declaration in schema types (scalars, strings, lists, options and native \
            types), or better, import the native from its generated schema instead of \
            declaring it by hand.",
        fixed: Some(
            r#"native fn measure(x: I64) -> I64;
"#,
        ),
        commands: &["viso schema --list natives", "viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E6102",
        severity: Severity::Error,
        stage: Stage::Native,
        title: "Native Ownership or Thread Domain Violation",
        explanation: "Native values carry an ownership and a thread domain. A borrowed handle, such as a \
            game frame or world passed to a hook, is valid only for the call that receives it, \
            so no place that outlives the call (state, input, computed, event payload, record \
            or enum field, return value, const) may hold it. A native that runs on a worker \
            thread can only be called from a `task`, because other bodies run on the UI \
            thread.",
        example: Example::Source(
            r#"import viso::game::{FixedUpdate, FixedFrame};

system Recorder implements FixedUpdate {
    @local state last: Option<FixedFrame> = Option::None;

    action fixed_update(frame: FixedFrame) {
    }
}
"#,
        ),
        fix: "Use a borrowed handle only inside the call, passing it as a parameter or keeping \
            it in a local; store the plain values you need from it instead.",
        fixed: Some(
            r#"state last_tick = 0;

action fixed_update(frame: FixedFrame) {
    last_tick = frame.tick();
}
"#,
        ),
        commands: &["viso schema --list natives"],
        reserved: false,
    },
    CodeInfo {
        code: "E6103",
        severity: Severity::Error,
        stage: Stage::Runtime,
        title: "Capability Denied",
        explanation: "A native that needs a capability (clipboard, network, persistent storage and so \
            on) runs only when the package or host grants it. When a handler calls such a \
            native outside the grant, the call faults with this code, the handler's writes \
            roll back and the view keeps handling input. Persisted state without the \
            `storage.persist` grant likewise starts from its initializer and is not stored.",
        example: Example::Text(
            r#"A package whose `Viso.toml` does not grant `clipboard.write` runs:

    import viso::clipboard;

    export component Notes {
        state note = "hi";
        view { Text { text: note; on click { clipboard::write_text(note); } } }
    }

Clicking the text faults with E6103 and the click's writes are rolled back.
"#,
        ),
        fix: "Grant the capability in the package manifest, or avoid calling the native in \
            builds that should not have it.",
        fixed: Some(
            r#"[package]
capabilities = ["clipboard.write"]
"#,
        ),
        commands: &["viso config show"],
        reserved: false,
    },
    CodeInfo {
        code: "E7101",
        severity: Severity::Error,
        stage: Stage::Runtime,
        title: "Execution Budget Exceeded",
        explanation: "Every behavior call runs under an execution budget: an instruction count, a call \
            depth limit and a native call quota. A body that exceeds one of them, typically an \
            unbounded loop or deep recursion, faults with this code instead of freezing the \
            frame; its writes roll back. In a game's fixed tick the same exhaustion is \
            reported as the tick budget fault.",
        example: Example::Text(
            r#"An action whose loop never ends exhausts the instruction budget when it runs:

    action spin() {
        let mut n = 0;
        while true { n += 1; }
    }

Calling `spin()` faults with E7101 and its writes are rolled back.
"#,
        ),
        fix: "Bound the work: give loops a reachable exit, replace deep recursion with \
            iteration, and move long computations into a `task` or a native.",
        fixed: None,
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E7102",
        severity: Severity::Error,
        stage: Stage::Runtime,
        title: "Memory Budget Exceeded",
        explanation: "Behavior code runs under a memory budget that counts the values it allocates, such \
            as lists, strings and records. A body that grows data past the budget faults with \
            this code and its writes roll back, instead of exhausting the host's memory. It \
            usually comes from building a collection in an unbounded or very long loop.",
        example: Example::Text(
            r#"An action that keeps doubling a list runs out of its memory budget:

    action grow() {
        let mut xs = [1];
        loop { xs = xs + xs; }
    }

Calling `grow()` faults with E7102 and its writes are rolled back.
"#,
        ),
        fix: "Limit how much a single call builds: bound the loop, page or stream large data, or \
            keep it in a native-owned structure.",
        fixed: None,
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E7103",
        severity: Severity::Error,
        stage: Stage::Runtime,
        title: "Arithmetic Fault",
        explanation: "Integer arithmetic is checked. An overflow, a division or remainder by zero, or a \
            shift by an amount outside the type's bit width faults with this code at run time \
            instead of wrapping or producing an undefined value. The faulting body's writes \
            roll back.",
        example: Example::Text(
            r#"An action dividing by a state that is still zero:

    state total = 10;
    state parts = 0;
    action split() { total = total / parts; }

Calling `split()` faults with E7103 (integer division by zero).
"#,
        ),
        fix: "Guard the operation: check divisors and shift amounts before using them, and pick \
            an integer type wide enough for the values.",
        fixed: Some(
            r#"action split() { if parts != 0 { total = total / parts; } }
"#,
        ),
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E7104",
        severity: Severity::Error,
        stage: Stage::Runtime,
        title: "Index Out of Bounds",
        explanation: "Indexing a list with `xs[i]` requires `i` to be inside the list. An index past the \
            end or below zero faults with this code at run time and the body's writes roll \
            back; in a game the faulting hook is rolled back and the next system still runs.",
        example: Example::Text(
            r#"A fixed-step hook indexing a one-element list with a growing tick:

    action fixed_update(frame: FixedFrame) {
        let xs = [1];
        n = xs[frame.tick() + 5];
    }

The hook faults with E7104 every tick and its writes are rolled back.
"#,
        ),
        fix: "Check the index against `len()` first, or use `xs.get(i)`, which returns an \
            `Option`, and handle the missing case.",
        fixed: Some(
            r#"let n = match xs.get(i) { Option::Some(v) => v, Option::None => 0 };
"#,
        ),
        commands: &["viso test game"],
        reserved: false,
    },
    CodeInfo {
        code: "E7105",
        severity: Severity::Error,
        stage: Stage::Runtime,
        title: "Function Cannot Run",
        explanation: "A call could not run at all: the function has no body in this build, a native it \
            calls is not linked or needs a thread the interpreter does not provide, an \
            instance was created without a required input, or the code broke an invariant the \
            compiler guarantees. These are build or host integration problems rather than \
            mistakes in the logic of the code itself.",
        example: Example::Text(
            r#"A host mounts a component that declares `input title: String;` without
giving it a `title`; creating the instance faults with E7105
(a required input has no value).
"#,
        ),
        fix: "Provide every required input when mounting, link the native libraries the module \
            imports, and rebuild so the host runs the module it was compiled with. Report an \
            invariant fault as a compiler bug.",
        fixed: None,
        commands: &["viso doctor"],
        reserved: false,
    },
    CodeInfo {
        code: "E7106",
        severity: Severity::Error,
        stage: Stage::Runtime,
        title: "Native Function Failed",
        explanation: "A native function returned an error or panicked. The fault is caught at the native \
            boundary and reported with this code, the calling body's writes roll back, and the \
            host keeps running. The message carries the native's own error.",
        example: Example::Text(
            r#"A handler calls a host native that cannot complete, for example a native
whose service is not installed by the host or whose Rust implementation
returns `Err`; the call faults with E7106 and the click's writes roll back.
"#,
        ),
        fix: "Read the native's error in the fault message and fix its cause: install the \
            service it needs, pass valid arguments, or handle the failure on the Rust side and \
            return a `Result` the DSL can match on.",
        fixed: None,
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E8101",
        severity: Severity::Error,
        stage: Stage::Shader,
        title: "Host-Only Type in a Shader",
        explanation: "A shader may only use the closed set of GPU value types (scalars, vectors, \
            matrices, `ColorLinear`, textures, samplers) and `@shader_value` records made of \
            them. Host types such as `String`, `I64`, `List`, tuples, enums, unmarked records \
            and `Color` literals have no GPU representation, so using them in a shader member, \
            local or record field is rejected.",
        example: Example::Source(
            r#"shader Label {
    uniform name: String;
    vertex(vertex_id: U32) -> VertexOutput {
        return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
    }
    fragment() -> Vec4F32 {
        return Vec4F32(0.0);
    }
}
"#,
        ),
        fix: "Use a shader value type, or mark a record of shader value types with \
            `@shader_value`.",
        fixed: Some(
            r#"uniform tint: Vec4F32;
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E8102",
        severity: Severity::Error,
        stage: Stage::Shader,
        title: "F64 in a Shader",
        explanation: "Shaders compute in 32-bit types. `F64` members, locals and record fields, and \
            `f64` literals, are not portable across GPU backends, so they are rejected. \
            Unsuffixed float literals in a shader are already `F32` where the context needs \
            one.",
        example: Example::Source(
            r#"shader Fade {
    uniform amount: F64;
    vertex(vertex_id: U32) -> VertexOutput {
        return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
    }
    fragment() -> Vec4F32 {
        return Vec4F32(0.0);
    }
}
"#,
        ),
        fix: "Use `F32` (and `F32` literals) inside the shader; convert on the host side.",
        fixed: Some(
            r#"uniform amount: F32;
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E8103",
        severity: Severity::Error,
        stage: Stage::Shader,
        title: "Shader Loop Without a Static Bound",
        explanation: "GPU code needs a known upper bound on every loop. A `for` over an integer range is \
            bounded when both ends are literals; otherwise it must carry `@max_iterations(N)`, \
            which stops it after N iterations. `while` and `loop` have no bound and are always \
            rejected in a shader.",
        example: Example::Source(
            r#"shader Rings {
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
"#,
        ),
        fix: "Use literal range ends, or bound the loop with `@max_iterations`.",
        fixed: Some(
            r#"@max_iterations(16)
for i in 0..count { sum += 0.1; }
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E8104",
        severity: Severity::Error,
        stage: Stage::Shader,
        title: "Shader ABI Mismatch",
        explanation: "Uniform and instance members are laid out in GPU buffers, so each must have a \
            buffer layout. `Bool` has none, so a `Bool` uniform or instance member is \
            rejected. The same code is raised when a Rust `#[derive(GpuPod)]` instance struct \
            does not match the shader's instance layout field by field (name, format, offset \
            or stride).",
        example: Example::Source(
            r#"shader Toggle {
    instance on: Bool;
    vertex(vertex_id: U32) -> VertexOutput {
        return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
    }
    fragment() -> Vec4F32 {
        return Vec4F32(0.0);
    }
}
"#,
        ),
        fix: "Pass flags as a number (for example `U32` with 0 or 1) and keep any Rust instance \
            struct in the same order and formats as the shader's instance members.",
        fixed: Some(
            r#"instance on: U32;
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E8105",
        severity: Severity::Error,
        stage: Stage::Shader,
        title: "Construct Outside the Shader Subset",
        explanation: "Shaders accept only a subset of the language that every GPU backend can run. \
            Closures, recursion between shader functions, `?`, `??`, named arguments, \
            turbofish, `emit`, `transaction`, `await` and generic shaders are rejected, as are \
            `@max_iterations`, `@shader_value` and `@sampler` placed where they do not apply \
            or with bad settings.",
        example: Example::Source(
            r#"shader Glow {
    vertex(vertex_id: U32) -> VertexOutput {
        return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
    }
    fragment() -> Vec4F32 {
        let half = |x: F32| x * 0.5;
        return Vec4F32(half(1.0));
    }
}
"#,
        ),
        fix: "Write the helper as a shader `fn` (non-recursive, reading only its parameters) and \
            call it positionally.",
        fixed: Some(
            r#"fn half(x: F32) -> F32 { return x * 0.5; }
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E8106",
        severity: Severity::Error,
        stage: Stage::Shader,
        title: "Render Profile Stage Rule Violated",
        explanation: "A render shader has exactly one `vertex` and one `fragment` entry and no \
            `compute`. `vertex` takes only the builtins `vertex_id: U32` and `instance_id: \
            U32` and returns `VertexOutput`; `fragment` takes only `frag_coord: Vec4F32`. Only \
            `vertex` writes varyings, fragment-only intrinsics such as `dpdx` and `sample` \
            stay in `fragment`, and a shader `fn` may not read members.",
        example: Example::Source(
            r#"shader Tint {
    varying shade: F32;
    vertex(vertex_id: U32) -> VertexOutput {
        return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
    }
    fragment() -> Vec4F32 {
        shade = 1.0;
        return Vec4F32(shade);
    }
}
"#,
        ),
        fix: "Write varyings in `vertex` and only read them in `fragment`.",
        fixed: Some(
            r#"vertex(vertex_id: U32) -> VertexOutput {
    shade = 1.0;
    return VertexOutput { clip_position: Vec4F32(0.0, 0.0, 0.0, 1.0) };
}
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E8107",
        severity: Severity::Error,
        stage: Stage::Shader,
        title: "Brush Profile Violated",
        explanation: "A shader that paints a node's box through a `ShaderView` is a brush. The renderer \
            writes its reserved members, so every brush declares `uniform viewport_size: \
            Vec2F32`, `instance rect_pos: Vec2F32` and `instance rect_size: Vec2F32`, and no \
            other uniforms except `time: F32`. Its parameters are the other instance members, \
            limited to `F32 I32 U32 Vec2F32 Vec3F32 ColorLinear`, and image textures within \
            the texture and sampler limits. A shader that no view paints with is not checked \
            against these rules.",
        example: Example::Source(
            r#"shader Glow {
    uniform viewport_size: Vec2F32;
    instance rect_pos: Vec2F32;
    instance strength: F32;
    vertex(vertex_id: U32) -> VertexOutput {
        let unit = quad_vertex(vertex_id);
        return VertexOutput { clip_position: to_clip(rect_pos + unit, viewport_size) };
    }
    fragment() -> Vec4F32 {
        return Vec4F32(strength, 0.0, 0.0, 1.0);
    }
}

export component Card {
    view {
        ShaderView { shader: Glow { strength: 0.5 }; width: 120dp; height: 80dp; }
    }
}
"#,
        ),
        fix: "Declare all three reserved members and keep parameters to the host-convertible \
            types.",
        fixed: Some(
            r#"uniform viewport_size: Vec2F32;
instance rect_pos: Vec2F32;
instance rect_size: Vec2F32;
instance strength: F32;
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E9101",
        severity: Severity::Error,
        stage: Stage::Game,
        title: "System Order Cycle",
        explanation: "`@before(...)` and `@after(...)` order systems within a schedule. If the \
            constraints form a cycle, including a system ordered after itself, no run order \
            satisfies them, so the cycle is rejected and the message names the systems on it.",
        example: Example::Source(
            r#"import viso::game::{FixedUpdate, FixedFrame};

@after(Physics)
system Input implements FixedUpdate {
    action fixed_update(frame: FixedFrame) {}
}

@after(Input)
system Physics implements FixedUpdate {
    action fixed_update(frame: FixedFrame) {}
}
"#,
        ),
        fix: "Remove one of the constraints so the order is a chain.",
        fixed: Some(
            r#"@after(Input)
system Physics implements FixedUpdate {
    action fixed_update(frame: FixedFrame) {}
}
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E9102",
        severity: Severity::Error,
        stage: Stage::Runtime,
        title: "Fixed Tick Budget Exceeded",
        explanation: "All hooks of one fixed tick share one instruction and native call budget. When a \
            hook exhausts it, the hook is rolled back, the rest of that tick's hooks are \
            skipped, and the tick still counts, so the simulation keeps its fixed rate. The \
            same fault rejects a world rebuild or logic reload whose smoke tick runs out of \
            budget.",
        example: Example::Text(
            r#"A game whose `fixed_update` hooks together do more work per tick than the
tick budget allows, for example looping over every entity many times,
faults with E9102 in the first such hook; the systems after it skip that
tick.
"#,
        ),
        fix: "Do less work per tick: spread it over several ticks, move heavy work into natives, \
            or lower the per-entity cost. Use `viso test game` to find which system exhausts \
            the budget.",
        fixed: None,
        commands: &["viso test game"],
        reserved: false,
    },
    CodeInfo {
        code: "E9103",
        severity: Severity::Error,
        stage: Stage::Game,
        title: "Simulation Reaches Local State",
        explanation: "Simulation code (fixed-step hooks and everything they call) must be reproducible \
            from the snapshot and the inputs alone. It may not read or write `@local` state, \
            which only presentation owns, nor use the return value of a presentation method. \
            `@local` itself marks only a `state` of a `system`.",
        example: Example::Source(
            r#"import viso::game::{FixedUpdate, FrameUpdate, FixedFrame, RenderFrame};

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
"#,
        ),
        fix: "Keep `@local` state to presentation hooks; let the simulation write simulation \
            state, and have the frame hook read it.",
        fixed: Some(
            r#"action frame_update(frame: RenderFrame) {
    frames = ticks;
}
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E9104",
        severity: Severity::Error,
        stage: Stage::Game,
        title: "Non-Deterministic Source in the Simulation",
        explanation: "The simulation must give the same result for the same inputs, so replays, rollback \
            and snapshots agree. Wall clocks, tasks and `await`, unseeded randomness, \
            unordered hash iteration, UI state and the adaptive environment are \
            non-deterministic sources, as is any native or physics engine whose \
            reproducibility is below the game's `determinism` level.",
        example: Example::Source(
            r#"import viso::game::{FixedUpdate, FixedFrame};
import viso::time::Stopwatch;

system Clock implements FixedUpdate {
    state ticks = 0;

    action fixed_update(frame: FixedFrame) {
        let w = Stopwatch::start();
        ticks += 1;
    }
}
"#,
        ),
        fix: "Derive time from the tick (`frame.tick()` and the tick rate) and take randomness \
            from the game's seeded source; measure wall time only in presentation hooks.",
        fixed: Some(
            r#"action fixed_update(frame: FixedFrame) {
    ticks = frame.tick();
}
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E9105",
        severity: Severity::Error,
        stage: Stage::Game,
        title: "Simulation State Without Snapshot",
        explanation: "Simulation state is captured in snapshots for rollback, replay and reload, so its \
            type must be snapshottable. A native handle lives outside the interpreter and \
            cannot be captured, and neither can a record, list or option that contains one.",
        example: Example::Source(
            r#"import viso::game::{FixedUpdate, FixedFrame};
import viso::time::Stopwatch;

system Timer implements FixedUpdate {
    state held: Option<Stopwatch> = Option::None;

    action fixed_update(frame: FixedFrame) {
    }
}
"#,
        ),
        fix: "Keep plain values in simulation state, or mark state holding a handle `@local` so \
            it belongs to presentation.",
        fixed: Some(
            r#"@local state held: Option<Stopwatch> = Option::None;
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E9106",
        severity: Severity::Error,
        stage: Stage::Game,
        title: "Invalid Persisted State",
        explanation: "`@persist(\"key\")` stores a `state` of a system or component across runs. Each \
            key must be unique, the state's type must be persistable, the package must grant \
            the `storage.persist` capability, and the attribute applies only to `state`. A \
            component persisting state must be a view's own component, not one mounted inside \
            another component's view, because each key holds one value.",
        example: Example::Source(
            r#"import viso::game::{FixedUpdate, FixedFrame};

system Score implements FixedUpdate {
    @persist("best")
    state best = 0;

    action fixed_update(frame: FixedFrame) {
    }
}
"#,
        ),
        fix: "Grant the capability in `Viso.toml`, and give each persisted state its own key.",
        fixed: Some(
            r#"[package]
capabilities = ["storage.persist"]
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E9107",
        severity: Severity::Warning,
        stage: Stage::Game,
        title: "Input Action Missing a Device Binding",
        explanation: "When the target profile declares gamepad or touch devices, every input action in \
            an `InputMap` should be reachable from each of them. An action with no binding for \
            a target device is warned about, since players on that device could not trigger \
            it. Without such devices in the profile the check is silent.",
        example: Example::Text(
            r#"With a target profile that declares gamepad and touch input, this map warns
twice: `A::Jump` has no touch binding and `A::Fire` has no gamepad binding.

    @derive(InputAction) enum A { Jump; Fire; }
    const C: InputMap<A> = InputMap::new()
        .key(Key::Space, A::Jump).pad(PadButton::South, A::Jump)
        .key(Key::J, A::Fire).touch(TouchButton::Primary, A::Fire);
"#,
        ),
        fix: "Bind every action for each target device.",
        fixed: Some(
            r#".touch(TouchButton::Secondary, A::Jump).pad(PadButton::West, A::Fire)
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E9108",
        severity: Severity::Error,
        stage: Stage::Game,
        title: "Audio Realtime Rule Violated",
        explanation: "An `AudioProcess` hook runs on the realtime audio thread, which must never block \
            or allocate. The hook and everything it calls may not build lists, strings, \
            records, tuples or closures, call natives that are not realtime-safe, recurse, or \
            loop without a constant bound. An audio system also implements no trait that runs \
            on another thread.",
        example: Example::Source(
            r#"import viso::game::{AudioProcess, AudioBlock};

export system Synth implements AudioProcess {
    @local state gain: F32 = 0.5f32;

    action audio_process(block: AudioBlock) {
        let taps = [1, 2];
    }
}
"#,
        ),
        fix: "Fill the block in place with constant-bounded loops over its channels and frames, \
            and keep scratch data in `@local` state prepared outside the hook.",
        fixed: Some(
            r#"action audio_process(block: AudioBlock) {
    for c in 0..block.channels() {
        for f in 0..block.frames() {
            block.write(c, f, block.input(c, f) * gain);
        }
    }
}
"#,
        ),
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E9109",
        severity: Severity::Error,
        stage: Stage::Game,
        title: "UI Member in a System",
        explanation: "A `system` is game logic driven by the scheduler, not UI. It may not declare \
            `view`, `event`, `slot`, `effect` or `resource` members, run `start`, or call `tr` \
            for translated text. Those belong to components.",
        example: Example::Source(
            r#"system Score {
    event scored();
}
"#,
        ),
        fix: "Move the UI-facing member into a component, and let the system expose its results \
            through state that the component reads.",
        fixed: None,
        commands: &["viso check"],
        reserved: false,
    },
    CodeInfo {
        code: "E9110",
        severity: Severity::Error,
        stage: Stage::Game,
        title: "Misplaced Probe",
        explanation: "`@probe` exposes a simulation value to game tests and scenarios, which assert on \
            it tick by tick. It therefore marks only a simulation `state` of a `system`: not a \
            `@local` state, not an action or other member, and not a component's state.",
        example: Example::Source(
            r#"export component Hud {
    @probe state lives = 3;

    view { Text { text: "hud"; } }
}
"#,
        ),
        fix: "Put the probe on a simulation state of the system that owns the value.",
        fixed: Some(
            r#"system Player implements FixedUpdate {
    @probe state lives = 3;
    action fixed_update(frame: FixedFrame) {}
}
"#,
        ),
        commands: &["viso test game"],
        reserved: false,
    },
    CodeInfo {
        code: "E9111",
        severity: Severity::Error,
        stage: Stage::Runtime,
        title: "Persisted State Failed to Load or Store",
        explanation: "A persisted state is loaded before the view mounts or the game starts, and written \
            back when it changes and on suspend. If the stored value cannot be read or \
            converted into the state's current type, or the store fails to write, the runtime \
            reports this code; a state that does not load starts from its initializer. The \
            application keeps running.",
        example: Example::Text(
            r#"A game persisted `@persist("best") state best = 0;` as an integer. A later
build declares `@persist("best") state best = "none";` with no `@migrate`
function from `I64`; at start the stored value does not convert, `best`
starts as `"none"`, and the runtime reports E9111.
"#,
        ),
        fix: "When changing a persisted state's type, add a `@migrate` function from the old \
            type, or change the key so the new state starts fresh. Check the host's storage \
            when writes fail.",
        fixed: None,
        commands: &[],
        reserved: false,
    },
    CodeInfo {
        code: "E9112",
        severity: Severity::Error,
        stage: Stage::Game,
        title: "Invalid Game Query",
        explanation: "A view reads the game its root runs through the `@query` functions of the \
            game's systems, as `game.name()`; the session runs each query a view reads once a \
            frame and the bindings reading it update when its value changes. A query therefore \
            is a `fn` of a `system`, takes no parameters, returns a value type the view can \
            hold (no closure or native handle), and its name is unique among the package's \
            queries.",
        example: Example::Source(
            r#"system Score implements FixedUpdate {
    state points = 0;
    @query fn points_after(bonus: I64) -> I64 { points + bonus }
    action fixed_update(frame: FixedFrame) { points += 1; }
}
"#,
        ),
        fix: "Keep what the view needs in a state the system's hooks update, and return it from \
            a query with no parameters.",
        fixed: Some(
            r#"system Score implements FixedUpdate {
    state points = 0;
    @query fn points() -> I64 { points }
    action fixed_update(frame: FixedFrame) { points += 1; }
}
"#,
        ),
        commands: &[],
        reserved: false,
    },
];
