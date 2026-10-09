//! The host's source graph of a dev session (`Viso_Hot_Reload.md` §3, §10,
//! §11): every `.vs` file and message catalog of the watch scope as it is on
//! disk, what the runtime reported of the files it mounts, the compiled view
//! each mount runs, and the compile of an edit exactly as the build compiled
//! the file.
//!
//! A `view!` compiles its file alone, against its package's catalogs and
//! grants, so a file's candidate depends on its own text and its catalogs and
//! nothing else: an edit re-checks only the files it touched, or every file of
//! the package when a catalog changed. Each file keeps an
//! [`IncrementalParse`] of its latest text, so an edit reparses only the
//! declaration it touched. Which version of a file the runtime runs is known
//! by its [`source_hash`]: the watcher read every version since before the
//! build, so the session finds the one the build compiled and compiles it as
//! the last-good the first edit is planned against. An edit saved while the
//! app was building is simply a file whose text the runtime does not run
//! yet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use viso_dsl::frontend::Origin;
use viso_dsl::hir::{CapabilitySet, TargetProfile};
use viso_dsl::hotreload::{CandidatePlan, plan_view_for, plan_view_parsed};
use viso_dsl::i18n::{CatalogFile, CatalogIssue, Messages};
use viso_dsl::syntax::{Edit, Entry, IncrementalParse, TextRange, TextSize};
use viso_dsl::{Diagnostic, LineIndex, Severity};
use viso_view::dev::wire::{FileId, MAX_LINE, MAX_LINES, MountEntry, source_hash};

/// How many of a file's latest distinct versions the session keeps, to find
/// the one a build compiled.
const VERSIONS: usize = 8;

/// A `.vs` file of the scope.
pub(super) struct View {
    pub path: PathBuf,
    /// Its latest content, `None` until the watcher read it.
    pub text: Option<String>,
    /// The text last compiled and its parse, kept current across edits.
    parse: Option<(String, IncrementalParse)>,
    /// The latest distinct versions the watcher read, newest last.
    versions: Vec<(u64, String)>,
    /// What the runtime reported, while it mounts the file.
    pub mount: Option<Mount>,
}

impl View {
    fn new(path: PathBuf) -> View {
        View {
            path,
            text: None,
            parse: None,
            versions: Vec::new(),
            mount: None,
        }
    }

    /// The hash of its latest content.
    pub fn hash(&self) -> Option<u64> {
        self.text.as_deref().map(source_hash)
    }
}

/// A file the runtime mounts.
pub(super) struct Mount {
    pub file: FileId,
    origin: Origin,
    capabilities: Vec<String>,
    /// Its package's catalogs, by index.
    pub catalog: Option<usize>,
    /// The hash of the source the runtime runs.
    pub running: u64,
    /// The hash of the latest content that was a candidate (sent, or
    /// rejected), so the same content is not a candidate twice.
    pub settled: Option<u64>,
    /// The catalog revision the file was last checked against.
    pub checked: u64,
    /// Whether the runtime shows a failure for it.
    pub failing: bool,
    /// The compiled view the runtime runs, which an edit is planned
    /// against; `None` when the version the build compiled is unknown or
    /// does not compile on the host.
    pub last_good: Option<CandidatePlan>,
}

/// A package's message catalogs.
pub(super) struct Catalog {
    /// The canonical directory.
    dir: PathBuf,
    /// The directory as the build named it, which the runtime knows it by.
    pub wire_dir: String,
    /// The source locale, once a mount names it.
    locale: Option<String>,
    /// Each catalog file of the directory and its latest content.
    files: BTreeMap<PathBuf, String>,
    /// The catalogs as last compiled without an error.
    pub messages: Option<Rc<Messages>>,
    /// Counts each change that compiled; 0 before the first compile.
    pub revision: u64,
    /// Whether the files changed since the last compile.
    stale: bool,
}

/// A catalog compile that failed: each issue, with the file it is in.
pub(super) struct CatalogErrors {
    pub files: Vec<(PathBuf, String)>,
    pub diagnostics: Vec<(usize, Diagnostic)>,
}

/// The source graph.
#[derive(Default)]
pub(super) struct Sources {
    pub views: Vec<View>,
    pub catalogs: Vec<Catalog>,
}

/// What a changed file of the scope is.
pub(super) enum Changed {
    View,
    Catalog,
    Other,
}

impl Sources {
    /// Takes the latest content of `path`.
    pub fn change(&mut self, path: &Path, text: String) -> Changed {
        if path.extension().is_some_and(|ext| ext == "vs") {
            let at = match self.views.iter().position(|view| view.path == path) {
                Some(at) => at,
                None => {
                    self.views.push(View::new(path.to_owned()));
                    self.views.len() - 1
                }
            };
            let view = &mut self.views[at];
            let hash = source_hash(&text);
            view.versions.retain(|&(seen, _)| seen != hash);
            if view.versions.len() == VERSIONS {
                view.versions.remove(0);
            }
            view.versions.push((hash, text.clone()));
            view.text = Some(text);
            return Changed::View;
        }
        let Some(dir) = path.parent().filter(|dir| {
            dir.file_name()
                .is_some_and(|n| n == viso_dsl::i18n::CATALOG_DIR)
        }) else {
            return Changed::Other;
        };
        let catalog = self.catalog(dir, None);
        let catalog = &mut self.catalogs[catalog];
        catalog.files.insert(path.to_owned(), text);
        catalog.stale = true;
        Changed::Catalog
    }

    /// The catalogs of canonical directory `dir`, created on first use.
    fn catalog(&mut self, dir: &Path, named: Option<(&str, &str)>) -> usize {
        let at = match self.catalogs.iter().position(|c| c.dir == dir) {
            Some(at) => at,
            None => {
                self.catalogs.push(Catalog {
                    dir: dir.to_owned(),
                    wire_dir: dir.display().to_string(),
                    locale: None,
                    files: BTreeMap::new(),
                    messages: None,
                    revision: 0,
                    stale: true,
                });
                self.catalogs.len() - 1
            }
        };
        if let Some((locale, wire_dir)) = named {
            let catalog = &mut self.catalogs[at];
            if catalog.locale.is_none() {
                catalog.locale = Some(locale.to_owned());
                catalog.wire_dir = wire_dir.to_owned();
            }
        }
        at
    }

    /// Records a file the runtime mounts, and returns its index.
    pub fn mount(&mut self, entry: &MountEntry) -> usize {
        let catalog = entry.catalog.as_ref().map(|(locale, dir)| {
            let canonical = Path::new(dir)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(dir));
            self.catalog(&canonical, Some((locale, dir)))
        });
        let path = PathBuf::from(&entry.path);
        let mount = Mount {
            file: entry.file,
            origin: Origin {
                package: entry.package.clone(),
                module: entry.module.clone(),
                language: entry.language.clone(),
            },
            capabilities: entry.capabilities.clone(),
            catalog,
            running: entry.source_hash,
            settled: None,
            checked: 0,
            failing: false,
            last_good: None,
        };
        let at = match self.views.iter().position(|view| view.path == path) {
            Some(at) => at,
            None => {
                self.views.push(View::new(path));
                self.views.len() - 1
            }
        };
        self.views[at].mount = Some(mount);
        at
    }

    /// Forgets what the runtime reported.
    pub fn unmount(&mut self) {
        for view in &mut self.views {
            view.mount = None;
        }
    }

    /// Compiles the version of view `at` its mount runs as the last-good its
    /// edits are planned against, once. `Err` with the reason when that
    /// version is not one the watcher read lately, or does not compile on the
    /// host.
    pub fn baseline(&mut self, at: usize) -> Result<(), String> {
        let catalogs = &self.catalogs;
        let view = &mut self.views[at];
        let Some(mount) = view.mount.as_mut() else {
            return Ok(());
        };
        if mount.last_good.is_some() {
            return Ok(());
        }
        let running = mount.running;
        let Some((_, text)) = view
            .versions
            .iter()
            .rev()
            .find(|&&(hash, _)| hash == running)
        else {
            return Err("the version the app runs is not one the session read".into());
        };
        let profile = mount.profile(catalogs);
        let plan = plan_view_for(text, &mount.origin, profile).map_err(|errors| {
            let codes: Vec<&str> = errors.iter().map(|d| d.code).collect();
            format!(
                "the version the app runs does not compile on the host ({})",
                codes.join(", ")
            )
        })?;
        mount.last_good = Some(plan);
        Ok(())
    }

    /// Compiles the catalogs that changed and that a mount names, each
    /// directory whose files have an error kept at its last good compile.
    pub fn compile_catalogs(&mut self) -> Vec<CatalogErrors> {
        let mut failed = Vec::new();
        for catalog in &mut self.catalogs {
            let Some(locale) = &catalog.locale else {
                continue;
            };
            if !catalog.stale {
                continue;
            }
            catalog.stale = false;
            let files: Vec<CatalogFile> = catalog
                .files
                .iter()
                .filter_map(|(path, text)| {
                    Some(CatalogFile {
                        locale: path.file_stem()?.to_str()?.to_owned(),
                        path: path.display().to_string(),
                        text: text.clone(),
                    })
                })
                .collect();
            let messages = (!files.is_empty()).then(|| Messages::compile(locale, &files));
            if let Some(messages) = &messages
                && messages.issues().iter().any(|issue| issue.error)
            {
                failed.push(CatalogErrors {
                    diagnostics: messages
                        .issues()
                        .iter()
                        .map(|issue| {
                            let range = TextRange::new(
                                TextSize::new(issue.range.start as u32),
                                TextSize::new(issue.range.end as u32),
                            );
                            let diagnostic = if issue.error {
                                Diagnostic::error(CatalogIssue::CODE, range, issue.message.clone())
                            } else {
                                Diagnostic::warning(
                                    CatalogIssue::CODE,
                                    range,
                                    issue.message.clone(),
                                )
                            };
                            (issue.file, diagnostic)
                        })
                        .collect(),
                    files: files
                        .into_iter()
                        .map(|file| (PathBuf::from(file.path), file.text))
                        .collect(),
                });
                continue;
            }
            catalog.messages = messages.map(Rc::new);
            catalog.revision += 1;
        }
        // A file the runtime runs as built was checked against the catalogs
        // of its build: their first compile is not a change to it.
        for view in &mut self.views {
            let hash = view.hash();
            if let Some(mount) = view.mount.as_mut()
                && mount.checked == 0
                && hash == Some(mount.running)
                && let Some(c) = mount.catalog
            {
                mount.checked = self.catalogs[c].revision;
            }
        }
        failed
    }

    /// The mounted views that are candidates: their latest content is not
    /// what the runtime runs and was not a candidate already, or their
    /// catalogs changed since they were checked.
    pub fn candidates(&self) -> Vec<usize> {
        (0..self.views.len())
            .filter(|&at| {
                let view = &self.views[at];
                let (Some(mount), Some(hash)) = (&view.mount, view.hash()) else {
                    return false;
                };
                let catalog_changed = mount
                    .catalog
                    .is_some_and(|c| self.catalogs[c].revision > mount.checked);
                catalog_changed || (hash != mount.running && mount.settled != Some(hash))
            })
            .collect()
    }

    /// Compiles view `at`'s latest content as the build would, reparsing
    /// only what changed since its last compile, into its candidate; its
    /// errors when it does not plan.
    pub fn compile(&mut self, at: usize) -> Option<Result<CandidatePlan, Vec<Diagnostic>>> {
        let catalogs = &self.catalogs;
        let view = &mut self.views[at];
        let (Some(text), Some(mount)) = (view.text.as_deref(), view.mount.as_mut()) else {
            return None;
        };
        let parse = match view.parse.take() {
            Some((old, mut parse)) if old != text => {
                parse.edit(&edit_between(&old, text), text);
                parse
            }
            Some((_, parse)) => parse,
            None => IncrementalParse::new(text, Entry::CompilationUnit),
        };
        let profile = mount.profile(catalogs);
        let planned = plan_view_parsed(text, parse.parse().clone(), &mount.origin, profile);
        mount.checked = mount.catalog.map_or(0, |c| catalogs[c].revision);
        view.parse = Some((text.to_owned(), parse));
        Some(planned)
    }
}

impl Mount {
    /// The profile the build compiled the file with: its grants, and its
    /// package's catalogs as last compiled.
    fn profile(&self, catalogs: &[Catalog]) -> TargetProfile {
        let mut capabilities = CapabilitySet::new();
        for capability in &self.capabilities {
            capabilities.insert(capability.as_str());
        }
        TargetProfile {
            capabilities,
            messages: self.catalog.and_then(|c| catalogs[c].messages.clone()),
            ..TargetProfile::default()
        }
    }
}

/// The one contiguous edit that turns `old` into `new`: their common prefix
/// and suffix kept, on character boundaries.
pub(super) fn edit_between<'a>(old: &str, new: &'a str) -> Edit<'a> {
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let most = old.len().min(new.len()) - prefix;
    let mut suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take(most)
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    let range = TextRange::new(
        TextSize::new(prefix as u32),
        TextSize::new((old.len() - suffix) as u32),
    );
    Edit::new(range, &new[prefix..new.len() - suffix])
}

/// The overlay line of each error of a rejected edit of `name`, at its line
/// and column in `text`, as many as a failure carries.
pub(super) fn failure_lines(name: &str, text: &str, diagnostics: &[Diagnostic]) -> Vec<String> {
    let lines = LineIndex::new(text);
    diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .take(MAX_LINES)
        .map(|d| {
            let at = lines.line_col_utf8(d.primary.start());
            let mut line = format!(
                "{name}:{}:{}: {} {}",
                at.line + 1,
                at.column + 1,
                d.code,
                d.message
            );
            if line.len() > MAX_LINE {
                let mut end = MAX_LINE;
                while !line.is_char_boundary(end) {
                    end -= 1;
                }
                line.truncate(end);
            }
            line
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use viso_dsl::aot::emit_view_package;
    use viso_dsl::syntax::parse_entry;
    use viso_dsl::syntax::tokenize;

    use super::*;

    #[test]
    fn the_edit_between_two_texts_keeps_their_common_ends() {
        let cases = [
            ("width: 120dp;", "width: 140dp;"),
            ("abc", "abc"),
            ("", "x"),
            ("aaa", "aa"),
            ("é1é", "é2é"),
            ("ab", "b"),
            ("x€y", "x£y"),
        ];
        for (old, new) in cases {
            let edit = edit_between(old, new);
            assert_eq!(edit.apply(old), new, "{old:?} -> {new:?}");
        }
        let edit = edit_between("width: 120dp;", "width: 140dp;");
        assert_eq!((edit.range.as_usize(), edit.insert), (8..9, "4"));
    }

    const VIEW: &str = "component Counter {\n    state count = 0;\n    view {\n        Column {\n            Text { width: 120dp; }\n        }\n    }\n}\n";

    fn mounted(text: &str) -> Sources {
        let mut sources = Sources::default();
        assert!(matches!(
            sources.change(Path::new("/p/src/view.vs"), text.to_owned()),
            Changed::View
        ));
        let entry = MountEntry {
            file: FileId(0),
            path: "/p/src/view.vs".into(),
            package: "app".into(),
            module: vec!["view".into()],
            language: None,
            catalog: None,
            capabilities: Vec::new(),
            source_hash: source_hash(text),
        };
        assert_eq!(sources.mount(&entry), 0);
        sources
    }

    #[test]
    fn an_edit_is_a_candidate_until_it_settles_and_reparses_incrementally() {
        let mut sources = mounted(VIEW);
        assert!(sources.candidates().is_empty(), "the runtime runs it");
        assert!(sources.compile(0).unwrap().is_ok());

        let edited = VIEW.replace("120dp", "140dp");
        sources.change(Path::new("/p/src/view.vs"), edited.clone());
        assert_eq!(sources.candidates(), [0]);
        assert!(sources.compile(0).unwrap().is_ok());
        let (text, parse) = sources.views[0].parse.as_ref().unwrap();
        assert_eq!(*text, edited);
        let fresh = parse_entry(&tokenize(&edited), &edited, Entry::CompilationUnit);
        assert_eq!(
            format!("{:?}", parse.parse().root),
            format!("{:?}", fresh.root),
            "the incremental parse is the fresh one"
        );
        sources.views[0].mount.as_mut().unwrap().settled = Some(source_hash(&edited));
        assert!(
            sources.candidates().is_empty(),
            "a settled edit is not resent"
        );

        let broken = VIEW.replace("state count = 0;", "state count = ;");
        sources.change(Path::new("/p/src/view.vs"), broken.clone());
        let errors = sources.compile(0).unwrap().unwrap_err();
        assert!(
            errors.iter().any(|d| d.code.starts_with("E1")),
            "{errors:?}"
        );
        let lines = failure_lines("src/view.vs", &broken, &errors);
        assert!(lines[0].starts_with("src/view.vs:2:"), "{lines:?}");

        // Reverted to what the runtime runs: no candidate.
        sources.change(Path::new("/p/src/view.vs"), VIEW.to_owned());
        assert!(sources.candidates().is_empty());
    }

    /// Full versus incremental parse of a one-property edit in a large file,
    /// beside the whole host compile of it. A release measurement:
    /// `cargo test --release -p viso-cli --bin viso -- --ignored
    /// incremental_parse_cost --nocapture`.
    #[test]
    #[ignore = "a release measurement"]
    fn incremental_parse_cost() {
        use std::time::{Duration, Instant};
        const ROUNDS: usize = 50;
        let mut text = String::new();
        for i in 0..200 {
            text.push_str(&format!(
                "component Card{i} {{\n    state count = {i};\n    view {{\n        Column {{\n            Text {{ width: 120dp; text: \"card {i}\"; }}\n            Text {{ visible: count > 0; }}\n        }}\n    }}\n}}\n"
            ));
        }
        text.push_str("export component Main {\n    view { Column { Card100 { } } }\n}\n");
        let edited = |n: usize| {
            text.replacen(
                "width: 120dp; text: \"card 100\"",
                &format!("width: {}dp; text: \"card 100\"", 121 + n % 2),
                1,
            )
        };
        let mut full = Vec::new();
        let mut incremental = Vec::new();
        let mut parse = IncrementalParse::new(&text, Entry::CompilationUnit);
        let mut old = text.clone();
        let mut in_place = 0;
        for n in 0..ROUNDS {
            let new = edited(n);
            let started = Instant::now();
            let fresh = IncrementalParse::new(&new, Entry::CompilationUnit);
            full.push(started.elapsed());
            let started = Instant::now();
            in_place += usize::from(parse.edit(&edit_between(&old, &new), &new));
            incremental.push(started.elapsed());
            assert_eq!(
                format!("{:?}", parse.parse().root),
                format!("{:?}", fresh.parse().root)
            );
            old = new;
        }
        let origin = Origin {
            package: "app".into(),
            module: vec!["cards".into()],
            language: None,
        };
        let mut compile = Vec::new();
        for _ in 0..10 {
            let started = Instant::now();
            plan_view_parsed(
                &old,
                parse.parse().clone(),
                &origin,
                TargetProfile::default(),
            )
            .expect("compiles");
            compile.push(started.elapsed());
        }
        let median = |samples: &mut Vec<Duration>| {
            samples.sort();
            samples[samples.len() / 2].as_secs_f64() * 1e3
        };
        println!(
            "{} bytes: full parse {:.3} ms, incremental {:.3} ms ({in_place}/{ROUNDS} in place), \
             host compile {:.3} ms",
            text.len(),
            median(&mut full),
            median(&mut incremental),
            median(&mut compile),
        );
    }

    #[test]
    fn a_catalog_change_rechecks_the_views_of_its_package() {
        let dir = std::env::temp_dir().join(format!("viso-sources-{}", std::process::id()));
        let i18n = dir.join("i18n");
        std::fs::create_dir_all(&i18n).unwrap();
        let i18n = i18n.canonicalize().unwrap();
        let view = "import viso::i18n::tr;\ncomponent Title {\n    view { Text { text: tr(\"title\"); } }\n}\n";
        let mut sources = Sources::default();
        sources.change(&i18n.join("en.toml"), "title = \"Title\"\n".into());
        sources.mount(&MountEntry {
            file: FileId(0),
            path: "/p/src/title.vs".into(),
            package: "app".into(),
            module: vec!["title".into()],
            language: None,
            catalog: Some(("en".into(), i18n.display().to_string())),
            capabilities: Vec::new(),
            source_hash: source_hash(view),
        });
        sources.change(Path::new("/p/src/title.vs"), view.into());
        assert!(sources.compile_catalogs().is_empty());
        assert!(
            sources.candidates().is_empty(),
            "the build checked the file against the catalogs"
        );

        sources.change(&i18n.join("en.toml"), "title = \"Title\nbroken".into());
        let failed = sources.compile_catalogs();
        assert_eq!(failed.len(), 1, "an error keeps the last good catalogs");
        assert!(sources.candidates().is_empty());

        sources.change(&i18n.join("en.toml"), "headline = \"H\"\n".into());
        assert!(sources.compile_catalogs().is_empty());
        assert_eq!(
            sources.candidates(),
            [0],
            "a catalog change rechecks the view"
        );
        let errors = sources.compile(0).unwrap().unwrap_err();
        assert!(
            !errors.is_empty(),
            "the view uses a key the catalogs dropped"
        );
    }

    #[test]
    fn the_last_good_is_the_version_the_build_compiled() {
        // Saved before the build read it, then edited while it built.
        let mut sources = Sources::default();
        let path = Path::new("/p/src/view.vs");
        sources.change(path, VIEW.to_owned());
        let edited = VIEW.replace("120dp", "130dp");
        sources.change(path, edited.clone());
        let entry = |text: &str| MountEntry {
            file: FileId(0),
            path: "/p/src/view.vs".into(),
            package: "app".into(),
            module: vec!["view".into()],
            language: None,
            catalog: None,
            capabilities: Vec::new(),
            source_hash: source_hash(text),
        };
        let at = sources.mount(&entry(VIEW));
        assert_eq!(sources.baseline(at), Ok(()));
        let package = |text: &str| {
            let origin = Origin {
                package: "app".into(),
                module: vec!["view".into()],
                language: None,
            };
            emit_view_package(&plan_view_for(text, &origin, TargetProfile::default()).unwrap())
        };
        let last_good = sources.views[at].mount.as_ref().unwrap().last_good.as_ref();
        let last_good = emit_view_package(last_good.unwrap());
        assert_eq!(last_good, package(VIEW), "the built version compiled");
        assert_ne!(last_good, package(&edited));
        assert_eq!(sources.candidates(), [at], "the later edit is a candidate");

        // An app built from a version the session never read.
        let at = sources.mount(&entry(
            "component Other { view { Text {} } }
",
        ));
        assert!(sources.baseline(at).is_err());
        assert!(
            sources.views[at]
                .mount
                .as_ref()
                .unwrap()
                .last_good
                .is_none()
        );
    }
}
