//! Compiler identity at 100k: interning names, minting and tabling durable
//! symbols, and linking a package's modules by their imports.
//!
//! Run release (`cargo bench -p viso-dsl --bench identity`); criterion
//! defaults to a release profile. Debug timing is not a perf result.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use viso_dsl::resolve::{
    ModuleGraph, ModulePath, ModuleSymbol, NameInterner, Namespace, SourceUnit, SymbolIdentity,
    SymbolKind, SymbolTable, fingerprint, resolve,
};
use viso_dsl::syntax::{Entry, Parse, parse_entry, tokenize};

const COUNT: usize = 100_000;

/// The members each component of the symbol build declares, and their kinds.
const MEMBERS: [(&str, SymbolKind); 9] = [
    ("id", SymbolKind::Input),
    ("count", SymbolKind::State),
    ("label", SymbolKind::State),
    ("total", SymbolKind::Computed),
    ("changed", SymbolKind::Event),
    ("bump", SymbolKind::Action),
    ("reset", SymbolKind::Action),
    ("load", SymbolKind::Task),
    ("format_total", SymbolKind::Function),
];

/// Library modules the link reads, the exports each declares, and the
/// importers: each importer names every export of one library, so the link
/// binds 100k names against 10k exported symbols.
const LIBRARIES: usize = 10;
const EXPORTS: usize = 1_000;
const IMPORTERS: usize = COUNT / EXPORTS;

/// `COUNT` distinct identifier spellings of the shapes source declares.
fn names() -> Vec<String> {
    (0..COUNT)
        .map(|i| match i % 4 {
            0 => format!("count{i}"),
            1 => format!("on_tap_{i}"),
            2 => format!("TodoRow{i}"),
            _ => format!("is_selected_{i}"),
        })
        .collect()
}

fn name_intern(c: &mut Criterion) {
    let names = names();
    let mut full = NameInterner::new();
    for name in &names {
        full.intern(name);
    }
    assert_eq!(full.len(), COUNT, "every spelling is a distinct name");
    eprintln!("identity_name_intern_100k: {COUNT} distinct ASCII names, fresh and re-interned");

    let mut group = c.benchmark_group("identity_name_intern_100k");
    group.bench_function("mint", |b| {
        b.iter(|| {
            let mut interner = NameInterner::new();
            for name in &names {
                black_box(interner.intern(black_box(name)));
            }
            interner
        });
    });
    group.bench_function("hit", |b| {
        b.iter(|| {
            for name in &names {
                black_box(full.intern(black_box(name)));
            }
        });
    });
    group.finish();
}

/// One module's table as resolution builds it: each component and its members
/// interned, fingerprinted by package, module, kind and declaration path, and
/// defined in the module table or the component's member table.
fn build_table(components: &[String], interner: &mut NameInterner) -> SymbolTable {
    let (package, module_path) = ("app", "features::home");
    let mut table = SymbolTable::new();
    let mut path = String::new();
    for owner in components {
        let name = interner.intern(owner);
        let id = fingerprint(SymbolIdentity {
            package,
            module_path,
            kind: SymbolKind::Component,
            decl_path: owner,
        });
        let symbol = ModuleSymbol { id, exported: true };
        table
            .define(name, Namespace::Type, symbol)
            .expect("a distinct component");
        let members = table.members_mut(id);
        for (member, kind) in MEMBERS {
            let name = interner.intern(member);
            path.clear();
            path.push_str(owner);
            path.push_str("::");
            path.push_str(member);
            let id = fingerprint(SymbolIdentity {
                package,
                module_path,
                kind,
                decl_path: &path,
            });
            let namespace = match kind {
                SymbolKind::Event => Namespace::Event,
                _ => Namespace::Value,
            };
            let symbol = ModuleSymbol {
                id,
                exported: false,
            };
            members
                .define(name, namespace, symbol)
                .expect("a distinct member");
        }
    }
    table
}

fn symbol_build(c: &mut Criterion) {
    let components: Vec<String> = (0..COUNT / (MEMBERS.len() + 1))
        .map(|i| format!("Card{i}"))
        .collect();
    let mut interner = NameInterner::new();
    let table = build_table(&components, &mut interner);
    let members: usize = table.member_tables().map(|(_, t)| t.len()).sum();
    assert_eq!(table.len() + members, COUNT, "every declaration is tabled");
    eprintln!(
        "identity_symbol_build_100k: {} components x {} members, {COUNT} SymbolIds minted and defined",
        components.len(),
        MEMBERS.len()
    );

    c.bench_function("identity_symbol_build_100k", |b| {
        b.iter(|| {
            let mut interner = NameInterner::new();
            build_table(black_box(&components), &mut interner)
        });
    });
}

/// The link's sources: `lib::m{l}` exporting `EXPORTS` consts, and
/// `app::u{i}` importing every export of `lib::m{i % LIBRARIES}` by name.
fn link_sources() -> Vec<(Vec<String>, Parse)> {
    let parse = |text: String| {
        let parse = parse_entry(&tokenize(&text), &text, Entry::CompilationUnit);
        let diagnostics = parse.diagnostics();
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        parse
    };
    let mut sources = Vec::with_capacity(LIBRARIES + IMPORTERS);
    for l in 0..LIBRARIES {
        let text: String = (0..EXPORTS)
            .map(|j| format!("export const c{j}: I64 = {j};\n"))
            .collect();
        sources.push((vec!["lib".to_owned(), format!("m{l}")], parse(text)));
    }
    let items: Vec<String> = (0..EXPORTS).map(|j| format!("c{j}")).collect();
    for i in 0..IMPORTERS {
        let text = format!(
            "import lib::m{}::{{{}}};\nconst own: I64 = c0 + c{};\n",
            i % LIBRARIES,
            items.join(", "),
            EXPORTS - 1
        );
        sources.push((vec!["app".to_owned(), format!("u{i}")], parse(text)));
    }
    sources
}

/// A fresh interner and the units over `sources`, as a build starts from.
fn units(sources: &[(Vec<String>, Parse)]) -> (NameInterner, Vec<SourceUnit>) {
    let mut interner = NameInterner::new();
    let units = sources
        .iter()
        .map(|(path, parse)| {
            let segments: Vec<&str> = path.iter().map(String::as_str).collect();
            SourceUnit::new(ModulePath::intern(&mut interner, &segments), parse.clone())
        })
        .collect();
    (interner, units)
}

fn symbol_link(c: &mut Criterion) {
    let sources = link_sources();
    let (mut interner, mut probe) = units(&sources);
    let graph = ModuleGraph::build(&probe, &interner);
    assert!(graph.errors().is_empty(), "{:?}", graph.errors());
    let edges: usize = graph.modules().iter().map(|m| m.imports.len()).sum();
    assert_eq!(edges, IMPORTERS, "each importer reaches its library");
    let resolved = resolve(&graph, &mut probe, &mut interner, "app");
    let errors: Vec<_> = resolved.iter().flat_map(|m| &m.errors).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let exported: usize = resolved
        .iter()
        .map(|m| {
            m.table
                .names(Namespace::Value)
                .filter(|(_, s)| s.exported)
                .count()
        })
        .sum();
    assert_eq!(exported, LIBRARIES * EXPORTS);
    eprintln!(
        "identity_symbol_link_100k: {} modules, {COUNT} imported names bound to {exported} exported symbols",
        sources.len()
    );

    c.bench_function("identity_symbol_link_100k", |b| {
        b.iter_batched(
            || units(&sources),
            |(mut interner, mut units)| {
                let graph = ModuleGraph::build(&units, &interner);
                let resolved = resolve(&graph, &mut units, &mut interner, "app");
                (resolved, units, interner)
            },
            BatchSize::LargeInput,
        );
    });
}

criterion_group!(benches, name_intern, symbol_build, symbol_link);
criterion_main!(benches);
