//! The diagnostics registry against what it describes: every code a source
//! of the compiler or its runtime raises is registered and every registered
//! code is raised (or marked reserved), the specification lists the same
//! codes, each source example raises its code, and the generated code table
//! is current.
//!
//! Set `BLESS=1` to rewrite `docs/diagnostics.md`.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use viso_dsl::diag::codes::{CODES, Example, lookup, nearest};
use viso_dsl::frontend::{Origin, compile_file};

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Every `"E####"` literal in the `.rs` files under `dir` but `skip`, Rust's
/// own `E0###` aside.
fn raised_in(dir: &Path, skip: &Path, out: &mut BTreeSet<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            raised_in(&path, skip, out);
        } else if path.extension().is_some_and(|e| e == "rs") && path != skip {
            let text = fs::read_to_string(&path).unwrap();
            let bytes = text.as_bytes();
            for (i, _) in text.match_indices("\"E") {
                let Some(code) = bytes.get(i + 1..i + 7) else {
                    continue;
                };
                if code[1..5].iter().all(u8::is_ascii_digit) && code[5] == b'"' && code[1] != b'0' {
                    out.insert(String::from_utf8(code[..5].to_vec()).unwrap());
                }
            }
        }
    }
}

#[test]
fn the_registry_is_the_codes_the_sources_raise() {
    let crates = workspace().join("crates");
    let registry = crates.join("dsl/src/diag/codes.rs");
    let mut raised = BTreeSet::new();
    for entry in fs::read_dir(&crates).unwrap().flatten() {
        raised_in(&entry.path().join("src"), &registry, &mut raised);
    }
    let unregistered: Vec<_> = raised.iter().filter(|c| lookup(c).is_none()).collect();
    assert!(
        unregistered.is_empty(),
        "raised, not registered: {unregistered:?}"
    );
    let reserved: Vec<_> = raised
        .iter()
        .filter(|c| lookup(c).is_some_and(|info| info.reserved))
        .collect();
    assert!(
        reserved.is_empty(),
        "raised, registered as reserved: {reserved:?}"
    );
    let unraised: Vec<_> = CODES
        .iter()
        .filter(|info| !info.reserved && !raised.contains(info.code))
        .map(|info| info.code)
        .collect();
    assert!(
        unraised.is_empty(),
        "registered, never raised: {unraised:?}"
    );
}

#[test]
fn the_specification_lists_the_registered_codes() {
    let spec = fs::read_to_string(workspace().join("Viso_DSL_1.0.md")).unwrap();
    let table = spec
        .split_once("# 附录 C")
        .map(|(_, rest)| rest)
        .expect("the stable code table");
    let listed: BTreeSet<&str> = table
        .lines()
        .take_while(|line| !line.starts_with("# ") || line.starts_with("# 附录 C"))
        .filter_map(|line| line.strip_prefix("| E"))
        .map(|rest| &rest[..4])
        .collect();
    let registered: BTreeSet<&str> = CODES.iter().map(|info| &info.code[1..]).collect();
    assert_eq!(listed, registered);
}

#[test]
fn codes_are_ordered_unique_and_written_out() {
    for pair in CODES.windows(2) {
        assert!(
            pair[0].code < pair[1].code,
            "{} before {}",
            pair[0].code,
            pair[1].code
        );
    }
    for info in CODES {
        let code = info.code;
        assert!(
            code.len() == 5
                && code.starts_with('E')
                && code[1..].bytes().all(|b| b.is_ascii_digit()),
            "{code}"
        );
        assert!(
            !info.title.is_empty() && !info.title.ends_with('.'),
            "{code}"
        );
        assert!(info.explanation.ends_with('.'), "{code}");
        assert!(info.fix.ends_with('.') || info.fix.ends_with(':'), "{code}");
        assert!(!info.example.text().trim().is_empty(), "{code}");
        assert!(
            info.commands.iter().all(|c| c.starts_with("viso ")),
            "{code}"
        );
        if info.reserved {
            assert!(matches!(info.example, Example::Text(_)), "{code}");
        }
        assert_eq!(lookup(code), Some(info));
    }
    assert_eq!(lookup("E0000"), None);
    assert!(nearest("E3110", CODES.iter().map(|c| c.code)).contains(&"E3101"));
}

#[test]
fn each_source_example_raises_its_code() {
    let origin = Origin {
        package: "app".into(),
        module: vec!["app".into()],
        language: None,
        ..Default::default()
    };
    let mut wrong = Vec::new();
    for info in CODES {
        let Example::Source(source) = info.example else {
            continue;
        };
        let compiled = compile_file(source, &origin);
        let raised: Vec<&str> = compiled.diagnostics.iter().map(|d| d.code).collect();
        match compiled.diagnostics.iter().find(|d| d.code == info.code) {
            Some(d) if d.severity == info.severity => {}
            Some(d) => wrong.push(format!(
                "{}: raised as {:?}, registered {:?}",
                info.code, d.severity, info.severity
            )),
            None => wrong.push(format!("{}: the example raises {raised:?}", info.code)),
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join("\n"));
}

/// The code table every tool documents, generated from the registry.
fn table() -> String {
    let mut out = String::from(
        "# Diagnostic codes\n\n\
         Generated from the compiler's diagnostics registry \
         (`crates/dsl/src/diag/codes.rs`) by its tests; `viso explain <code>` prints \
         the same text. Do not edit by hand.\n\n\
         | Code | Title | Severity | Stage |\n\
         | ---- | ----- | -------- | ----- |\n",
    );
    let severity = |info: &viso_dsl::diag::codes::CodeInfo| match info.severity {
        viso_dsl::Severity::Error => "error",
        viso_dsl::Severity::Warning => "warning",
        viso_dsl::Severity::Note => "note",
    };
    for info in CODES {
        let reserved = if info.reserved { " (reserved)" } else { "" };
        out.push_str(&format!(
            "| [{code}](#{lower}) | {title}{reserved} | {severity} | {stage} |\n",
            code = info.code,
            lower = info.code.to_lowercase(),
            title = info.title,
            severity = severity(info),
            stage = info.stage.name(),
        ));
    }
    for info in CODES {
        out.push_str(&format!(
            "\n## {}\n\n**{}** — {}, {}{}\n\n{}\n\n",
            info.code,
            info.title,
            severity(info),
            info.stage.name(),
            if info.reserved {
                ", reserved: specified, not raised yet"
            } else {
                ""
            },
            info.explanation
        ));
        let fence = match info.example {
            Example::Source(_) => "vs",
            Example::Text(_) => "text",
        };
        out.push_str(&format!("```{fence}\n{}```\n\n", info.example.text()));
        out.push_str(&format!("Fix: {}\n", info.fix));
        if let Some(fixed) = info.fixed {
            out.push_str(&format!("\n```{fence}\n{fixed}```\n"));
        }
        if !info.commands.is_empty() {
            let commands: Vec<String> = info.commands.iter().map(|c| format!("`{c}`")).collect();
            out.push_str(&format!("\nSee also: {}\n", commands.join(", ")));
        }
    }
    out
}

#[test]
fn the_generated_table_is_current() {
    let path = workspace().join("docs/diagnostics.md");
    let table = table();
    if std::env::var_os("BLESS").is_some() {
        fs::write(&path, &table).unwrap();
        return;
    }
    let current = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        current == table,
        "docs/diagnostics.md is stale; run with BLESS=1 to regenerate it"
    );
}
