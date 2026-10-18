//! The host's source graph of a dev session: every
//! `.vs` file, message catalog and asset of the watch scope as it is on disk,
//! shared by every runtime of the session; and, per runtime ([`Mounts`]), what
//! it reported of the files it mounts, the compiled view each mount runs and
//! the assets it holds; and the compile of an edit exactly as the build
//! compiled the file for a mount.
//!
//! A `view!` compiles its file alone, against its package's catalogs, assets
//! and grants, so a file's candidate depends on its own text, its catalogs and
//! which assets exist, and nothing else: an edit re-checks only the files it
//! touched, every file of the package when a catalog changed, and a file that
//! did not compile when an asset appeared. Each file keeps an
//! [`IncrementalParse`] of its latest text, so an edit reparses only the
//! declaration it touched. Which version of a file the runtime runs is known
//! by its [`source_hash`]: the watcher read every version since before the
//! build, so the session finds the one the build compiled and compiles it as
//! the last-good the first edit is planned against. An edit saved while the
//! app was building is simply a file whose text the runtime does not run
//! yet.
//!
//! An asset is taken to run as the build embedded it — as first read, then as
//! read when the app last restarted on a new build — and as each patch
//! carrying it left it, per runtime. A changed asset goes to a runtime when it
//! holds it (a mount embedded it, or a patch carried it) or a candidate view
//! names it; at worst an asset edited while the app built is sent once more
//! than it needed.
//!
//! Runtimes that mount a file alike — the same origin, grants, catalogs and
//! assets — share one compile of each of its edits ([`Memo`]).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use viso_dev_protocol::inspect::FileSources;
use viso_dev_protocol::wire::{FileId, MAX_LINE, MAX_LINES, MountEntry, content_hash, source_hash};
use viso_dsl::assets::{AssetCatalog, AssetFormat, ResourceId, normalize_path};
use viso_dsl::frontend::Origin;
use viso_dsl::hir::{CapabilitySet, TargetProfile};
use viso_dsl::hotreload::{CandidatePlan, plan_view_for, plan_view_parsed};
use viso_dsl::i18n::{CatalogFile, CatalogIssue, Messages};
use viso_dsl::inspect::file_sources;
use viso_dsl::syntax::{Edit, Entry, IncrementalParse, TextRange, TextSize};
use viso_dsl::{Diagnostic, LineIndex, Severity};
use viso_text::font_format;

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
}

impl View {
    fn new(path: PathBuf) -> View {
        View {
            path,
            text: None,
            parse: None,
            versions: Vec::new(),
        }
    }

    /// The hash of its latest content.
    pub fn hash(&self) -> Option<u64> {
        self.text.as_deref().map(source_hash)
    }
}

/// A file a runtime mounts.
pub(super) struct Mount {
    pub file: FileId,
    origin: Origin,
    capabilities: Vec<String>,
    /// Its package's catalogs, by index.
    pub catalog: Option<usize>,
    /// Its package's assets, by index.
    pub assets: Option<usize>,
    /// The hash of the source the runtime runs.
    pub running: u64,
    /// The hash of the latest content that was a candidate (sent, or
    /// rejected), so the same content is not a candidate twice.
    pub settled: Option<u64>,
    /// The catalog revision the file was last checked against.
    pub checked: u64,
    /// The asset revision the file was last checked against, and whether
    /// that compile failed.
    checked_assets: u64,
    rejected: bool,
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

/// A package's assets: each file of its `assets/` directory as last read.
pub(super) struct AssetDir {
    /// The canonical directory.
    dir: PathBuf,
    /// Each file, by its path under the directory.
    pub files: BTreeMap<String, AssetFile>,
    /// The files in an asset format, as candidates compile against them.
    catalog: Rc<AssetCatalog>,
    /// Counts each change of the catalog.
    revision: u64,
}

/// A file of an asset directory.
pub(super) struct AssetFile {
    pub bytes: Vec<u8>,
    pub hash: u64,
    format: Option<AssetFormat>,
    /// Whether it is a font, told by its sfnt header: the app may draw with
    /// it though no view names it.
    pub font: bool,
    /// The hash of the content the build embedded.
    built: u64,
}

/// What one runtime runs of the source graph: the files it mounts, by their
/// view's index, and the assets it holds and runs.
#[derive(Default)]
pub(super) struct Mounts {
    pub views: BTreeMap<usize, Mount>,
    /// The resources it holds, by asset directory: those its mounts embed,
    /// and those a patch carried.
    held: BTreeMap<usize, BTreeSet<ResourceId>>,
    /// The assets a patch carried, or that were a candidate for it.
    assets: BTreeMap<AssetRef, AssetRun>,
}

/// An asset as one runtime has it.
#[derive(Default, Clone, Copy)]
struct AssetRun {
    /// The hash of the content it runs, once a patch carried it.
    running: Option<u64>,
    /// The hash of the latest content that was a candidate (sent, or
    /// rejected), so the same content is not a candidate twice.
    settled: Option<u64>,
}

/// How a mount compiles its file: what the build compiled it against.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct ProfileKey {
    origin: Origin,
    capabilities: Vec<String>,
    catalog: Option<usize>,
    assets: Option<usize>,
}

/// The compiles of one pass over the runtimes, by view and profile.
#[derive(Default)]
pub(super) struct Memo(Vec<(usize, ProfileKey, Result<CandidatePlan, Vec<Diagnostic>>)>);

/// An asset a patch is to carry: its directory, by index, and its path.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct AssetRef {
    pub dir: usize,
    pub path: String,
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
    pub assets: Vec<AssetDir>,
}

/// What a changed file of the scope is.
pub(super) enum Changed {
    View,
    Catalog,
    Asset,
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

    /// Takes the latest content of `path`, a file of the asset directory
    /// `dir`.
    pub fn change_asset(&mut self, dir: &Path, path: &Path, bytes: Vec<u8>) -> Changed {
        let Some(name) = path
            .strip_prefix(dir)
            .ok()
            .and_then(Path::to_str)
            .and_then(normalize_path)
        else {
            return Changed::Other;
        };
        let at = self.asset_dir(dir);
        let assets = &mut self.assets[at];
        let hash = content_hash(&bytes);
        let format = AssetFormat::sniff(&bytes);
        let font = font_format::detect(&bytes).is_some();
        let moved = match assets.files.get_mut(&name) {
            Some(file) => {
                let moved = file.format != format;
                file.bytes = bytes;
                file.hash = hash;
                file.format = format;
                file.font = font;
                moved
            }
            None => {
                assets.files.insert(
                    name,
                    AssetFile {
                        bytes,
                        hash,
                        format,
                        font,
                        built: hash,
                    },
                );
                format.is_some()
            }
        };
        if moved {
            assets.catalog = Rc::new(AssetCatalog::of(
                assets.dir.clone(),
                assets
                    .files
                    .iter()
                    .filter_map(|(path, file)| Some((path.clone(), file.format?))),
            ));
            assets.revision += 1;
        }
        Changed::Asset
    }

    /// The assets of canonical directory `dir`, created on first use.
    fn asset_dir(&mut self, dir: &Path) -> usize {
        if let Some(at) = self.assets.iter().position(|a| a.dir == dir) {
            return at;
        }
        self.assets.push(AssetDir {
            dir: dir.to_owned(),
            files: BTreeMap::new(),
            catalog: Rc::new(AssetCatalog::of(dir.to_owned(), [])),
            revision: 0,
        });
        self.assets.len() - 1
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

    /// Records a file `mounts`' runtime mounts, and returns its index.
    pub fn mount(&mut self, mounts: &mut Mounts, entry: &MountEntry) -> usize {
        let catalog = entry.catalog.as_ref().map(|(locale, dir)| {
            let canonical = Path::new(dir)
                .canonicalize()
                .unwrap_or_else(|_| PathBuf::from(dir));
            self.catalog(&canonical, Some((locale, dir)))
        });
        let assets = entry.assets.as_ref().map(|(dir, held)| {
            let at = self.asset_dir(&canonical_dir(Path::new(dir)));
            mounts
                .held
                .entry(at)
                .or_default()
                .extend(held.iter().copied());
            at
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
            assets,
            running: entry.source_hash,
            settled: None,
            checked: 0,
            checked_assets: 0,
            rejected: false,
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
        mounts.views.insert(at, mount);
        at
    }

    /// The app restarted on a new build, which embeds every asset as it is
    /// now; the runtimes of `others`, which did not restart, run each asset
    /// a patch has not carried to them as their build embedded it.
    pub fn rebuilt<'a>(&mut self, others: impl IntoIterator<Item = &'a mut Mounts>) {
        let mut others: Vec<&mut Mounts> = others.into_iter().collect();
        for (dir, assets) in self.assets.iter_mut().enumerate() {
            for (path, file) in &mut assets.files {
                if file.built == file.hash {
                    continue;
                }
                for mounts in others.iter_mut() {
                    let asset = AssetRef {
                        dir,
                        path: path.clone(),
                    };
                    let run = mounts.assets.entry(asset).or_default();
                    run.running.get_or_insert(file.built);
                }
                file.built = file.hash;
            }
        }
    }

    /// What each node of view `at` is in the source its mount in `mounts`
    /// runs, shown as `name`; `None` while the session holds no compile of
    /// that source.
    pub fn file_sources(&self, mounts: &Mounts, at: usize, name: String) -> Option<FileSources> {
        let view = &self.views[at];
        let mount = mounts.views.get(&at)?;
        let plan = mount.last_good.as_ref()?;
        let (_, text) = view
            .versions
            .iter()
            .rev()
            .find(|&&(hash, _)| hash == mount.running)?;
        Some(file_sources(
            plan,
            text,
            view.path.to_string_lossy().into_owned(),
            name,
            mount.origin.module.clone(),
        ))
    }

    /// Compiles the version of view `at` its mount in `mounts` runs as the
    /// last-good its edits are planned against, once. `Err` with the reason
    /// when that version is not one the watcher read lately, or does not
    /// compile on the host.
    pub fn baseline(&self, mounts: &mut Mounts, at: usize) -> Result<(), String> {
        let catalogs = &self.catalogs;
        let view = &self.views[at];
        let Some(mount) = mounts.views.get_mut(&at) else {
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
        let profile = mount.profile(catalogs, &self.assets);
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
        failed
    }

    /// Marks the files `mounts`' runtime runs as built checked against the
    /// catalogs as first compiled: the build checked them against those.
    pub fn catalogs_checked(&self, mounts: &mut Mounts) {
        for (&at, mount) in &mut mounts.views {
            if mount.checked == 0
                && self.views[at].hash() == Some(mount.running)
                && let Some(c) = mount.catalog
            {
                mount.checked = self.catalogs[c].revision;
            }
        }
    }

    /// The views `mounts` mounts that are candidates: their latest content
    /// is not what the runtime runs and was not a candidate already, their
    /// catalogs changed since they were checked, or they did not compile and
    /// an asset appeared since.
    pub fn candidates(&self, mounts: &Mounts) -> Vec<usize> {
        mounts
            .views
            .iter()
            .filter(|&(&at, mount)| {
                let Some(hash) = self.views[at].hash() else {
                    return false;
                };
                let catalog_changed = mount
                    .catalog
                    .is_some_and(|c| self.catalogs[c].revision > mount.checked);
                let assets_appeared = mount.rejected
                    && mount
                        .assets
                        .is_some_and(|a| self.assets[a].revision > mount.checked_assets);
                catalog_changed
                    || assets_appeared
                    || (hash != mount.running && mount.settled != Some(hash))
            })
            .map(|(&at, _)| at)
            .collect()
    }

    /// The assets a patch to `mounts`' runtime is to carry: each it holds
    /// whose content is not what it runs, and each `named` (by a candidate
    /// view of a mount of directory `dir`) that it does not hold yet —
    /// neither a candidate already at this content.
    pub fn asset_candidates(&self, mounts: &Mounts, named: &[AssetRef]) -> Vec<AssetRef> {
        let mut out = BTreeSet::new();
        for (dir, assets) in self.assets.iter().enumerate() {
            let held = mounts.held.get(&dir);
            for (path, file) in &assets.files {
                let asset = AssetRef {
                    dir,
                    path: path.clone(),
                };
                let run = mounts.run(&asset);
                if run.running.unwrap_or(file.built) != file.hash
                    && run.settled != Some(file.hash)
                    && (file.font || held.is_some_and(|h| h.contains(&ResourceId::of(path))))
                {
                    out.insert(asset);
                }
            }
        }
        for named in named {
            let Some(file) = self.assets[named.dir].files.get(&named.path) else {
                continue;
            };
            let held = mounts
                .held
                .get(&named.dir)
                .is_some_and(|h| h.contains(&ResourceId::of(&named.path)));
            let run = mounts.run(named);
            if (!held || run.running.unwrap_or(file.built) != file.hash)
                && run.settled != Some(file.hash)
            {
                out.insert(named.clone());
            }
        }
        out.into_iter().collect()
    }

    /// The file `asset` names.
    pub fn asset(&self, asset: &AssetRef) -> &AssetFile {
        &self.assets[asset.dir].files[&asset.path]
    }

    /// The path of `asset` on disk.
    pub fn asset_path(&self, asset: &AssetRef) -> PathBuf {
        self.assets[asset.dir].dir.join(&asset.path)
    }

    /// Compiles view `at`'s latest content as the build would for `mount`,
    /// reparsing only what changed since its last compile, into its
    /// candidate; its errors when it does not plan. A compile `memo` holds
    /// for a mount alike is taken from it.
    pub fn compile(
        &mut self,
        at: usize,
        mount: &mut Mount,
        memo: &mut Memo,
    ) -> Option<Result<CandidatePlan, Vec<Diagnostic>>> {
        let catalogs = &self.catalogs;
        let view = &mut self.views[at];
        let text = view.text.as_deref()?;
        let key = mount.key();
        mount.checked = mount.catalog.map_or(0, |c| catalogs[c].revision);
        mount.checked_assets = mount.assets.map_or(0, |a| self.assets[a].revision);
        if let Some((_, _, planned)) = memo.0.iter().find(|(v, k, _)| *v == at && *k == key) {
            mount.rejected = planned.is_err();
            return Some(planned.clone());
        }
        let parse = match view.parse.take() {
            Some((old, mut parse)) if old != text => {
                parse.edit(&edit_between(&old, text), text);
                parse
            }
            Some((_, parse)) => parse,
            None => IncrementalParse::new(text, Entry::CompilationUnit),
        };
        let profile = mount.profile(catalogs, &self.assets);
        let planned = plan_view_parsed(text, parse.parse().clone(), &mount.origin, profile);
        mount.rejected = planned.is_err();
        memo.0.push((at, key, planned.clone()));
        view.parse = Some((text.to_owned(), parse));
        Some(planned)
    }
}

impl Mounts {
    /// How it has `asset`.
    fn run(&self, asset: &AssetRef) -> AssetRun {
        self.assets.get(asset).copied().unwrap_or_default()
    }

    /// Records that it runs `asset` at `hash`, a patch having carried it.
    pub fn asset_applied(&mut self, asset: &AssetRef, hash: u64) {
        self.held
            .entry(asset.dir)
            .or_default()
            .insert(ResourceId::of(&asset.path));
        let run = self.assets.entry(asset.clone()).or_default();
        run.running = Some(hash);
        run.settled = Some(hash);
    }

    /// Records that `asset` at `hash` was a candidate it does not run: it
    /// does not decode, or a patch carrying it was refused.
    pub fn asset_settled(&mut self, asset: &AssetRef, hash: u64) {
        self.assets.entry(asset.clone()).or_default().settled = Some(hash);
    }
}

impl Mount {
    /// How it compiles its file.
    fn key(&self) -> ProfileKey {
        ProfileKey {
            origin: self.origin.clone(),
            capabilities: self.capabilities.clone(),
            catalog: self.catalog,
            assets: self.assets,
        }
    }

    /// The profile the build compiled the file with: its grants, its
    /// package's catalogs as last compiled and its package's assets as they
    /// are.
    fn profile(&self, catalogs: &[Catalog], assets: &[AssetDir]) -> TargetProfile {
        let mut capabilities = CapabilitySet::new();
        for capability in &self.capabilities {
            capabilities.insert(capability.as_str());
        }
        TargetProfile {
            capabilities,
            messages: self.catalog.and_then(|c| catalogs[c].messages.clone()),
            assets: self.assets.map(|a| Rc::clone(&assets[a].catalog)),
            ..TargetProfile::default()
        }
    }
}

/// `dir` canonical, or its parent's canonical path and its name while it does
/// not exist yet.
fn canonical_dir(dir: &Path) -> PathBuf {
    if let Ok(canonical) = dir.canonicalize() {
        return canonical;
    }
    match (dir.parent().map(Path::canonicalize), dir.file_name()) {
        (Some(Ok(parent)), Some(name)) => parent.join(name),
        _ => dir.to_owned(),
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

    fn mounted(text: &str) -> (Sources, Mounts) {
        let mut sources = Sources::default();
        let mut mounts = Mounts::default();
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
            assets: None,
        };
        assert_eq!(sources.mount(&mut mounts, &entry), 0);
        (sources, mounts)
    }

    fn compile(
        sources: &mut Sources,
        mounts: &mut Mounts,
        at: usize,
    ) -> Result<CandidatePlan, Vec<Diagnostic>> {
        let mount = mounts.views.get_mut(&at).unwrap();
        sources.compile(at, mount, &mut Memo::default()).unwrap()
    }

    #[test]
    fn an_edit_is_a_candidate_until_it_settles_and_reparses_incrementally() {
        let (mut sources, mut mounts) = mounted(VIEW);
        assert!(
            sources.candidates(&mounts).is_empty(),
            "the runtime runs it"
        );
        assert!(compile(&mut sources, &mut mounts, 0).is_ok());

        let edited = VIEW.replace("120dp", "140dp");
        sources.change(Path::new("/p/src/view.vs"), edited.clone());
        assert_eq!(sources.candidates(&mounts), [0]);
        assert!(compile(&mut sources, &mut mounts, 0).is_ok());
        let (text, parse) = sources.views[0].parse.as_ref().unwrap();
        assert_eq!(*text, edited);
        let fresh = parse_entry(&tokenize(&edited), &edited, Entry::CompilationUnit);
        assert_eq!(
            format!("{:?}", parse.parse().root),
            format!("{:?}", fresh.root),
            "the incremental parse is the fresh one"
        );
        mounts.views.get_mut(&0).unwrap().settled = Some(source_hash(&edited));
        assert!(
            sources.candidates(&mounts).is_empty(),
            "a settled edit is not resent"
        );

        let broken = VIEW.replace("state count = 0;", "state count = ;");
        sources.change(Path::new("/p/src/view.vs"), broken.clone());
        let errors = compile(&mut sources, &mut mounts, 0).unwrap_err();
        assert!(
            errors.iter().any(|d| d.code.starts_with("E1")),
            "{errors:?}"
        );
        let lines = failure_lines("src/view.vs", &broken, &errors);
        assert!(lines[0].starts_with("src/view.vs:2:"), "{lines:?}");

        // Reverted to what the runtime runs: no candidate.
        sources.change(Path::new("/p/src/view.vs"), VIEW.to_owned());
        assert!(sources.candidates(&mounts).is_empty());
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
        let mut mounts = Mounts::default();
        sources.change(&i18n.join("en.toml"), "title = \"Title\"\n".into());
        sources.mount(
            &mut mounts,
            &MountEntry {
                file: FileId(0),
                path: "/p/src/title.vs".into(),
                package: "app".into(),
                module: vec!["title".into()],
                language: None,
                catalog: Some(("en".into(), i18n.display().to_string())),
                capabilities: Vec::new(),
                source_hash: source_hash(view),
                assets: None,
            },
        );
        sources.change(Path::new("/p/src/title.vs"), view.into());
        assert!(sources.compile_catalogs().is_empty());
        sources.catalogs_checked(&mut mounts);
        assert!(
            sources.candidates(&mounts).is_empty(),
            "the build checked the file against the catalogs"
        );

        sources.change(&i18n.join("en.toml"), "title = \"Title\nbroken".into());
        let failed = sources.compile_catalogs();
        assert_eq!(failed.len(), 1, "an error keeps the last good catalogs");
        assert!(sources.candidates(&mounts).is_empty());

        sources.change(&i18n.join("en.toml"), "headline = \"H\"\n".into());
        assert!(sources.compile_catalogs().is_empty());
        assert_eq!(
            sources.candidates(&mounts),
            [0],
            "a catalog change rechecks the view"
        );
        let errors = compile(&mut sources, &mut mounts, 0).unwrap_err();
        assert!(
            !errors.is_empty(),
            "the view uses a key the catalogs dropped"
        );
    }

    #[test]
    fn the_last_good_is_the_version_the_build_compiled() {
        // Saved before the build read it, then edited while it built.
        let mut sources = Sources::default();
        let mut mounts = Mounts::default();
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
            assets: None,
        };
        let at = sources.mount(&mut mounts, &entry(VIEW));
        assert_eq!(sources.baseline(&mut mounts, at), Ok(()));
        let package = |text: &str| {
            let origin = Origin {
                package: "app".into(),
                module: vec!["view".into()],
                language: None,
            };
            emit_view_package(&plan_view_for(text, &origin, TargetProfile::default()).unwrap())
        };
        let last_good = mounts.views[&at].last_good.as_ref();
        let last_good = emit_view_package(last_good.unwrap());
        assert_eq!(last_good, package(VIEW), "the built version compiled");
        assert_ne!(last_good, package(&edited));
        assert_eq!(
            sources.candidates(&mounts),
            [at],
            "the later edit is a candidate"
        );

        // An app built from a version the session never read.
        let mut mounts = Mounts::default();
        let at = sources.mount(
            &mut mounts,
            &entry(
                "component Other { view { Text {} } }
",
            ),
        );
        assert!(sources.baseline(&mut mounts, at).is_err());
        assert!(mounts.views[&at].last_good.is_none());
    }

    #[test]
    fn runtimes_that_mount_a_file_alike_share_its_compile() {
        let (mut sources, mut first) = mounted(VIEW);
        let mut second = Mounts::default();
        let entry = MountEntry {
            file: FileId(0),
            path: "/p/src/view.vs".into(),
            package: "app".into(),
            module: vec!["view".into()],
            language: None,
            catalog: None,
            capabilities: Vec::new(),
            source_hash: source_hash(VIEW),
            assets: None,
        };
        sources.mount(&mut second, &entry);
        let mut granted = Mounts::default();
        sources.mount(
            &mut granted,
            &MountEntry {
                capabilities: vec!["clipboard.write".into()],
                ..entry
            },
        );
        sources.change(Path::new("/p/src/view.vs"), VIEW.replace("120dp", "140dp"));
        let mut memo = Memo::default();
        for mounts in [&mut first, &mut second, &mut granted] {
            assert_eq!(sources.candidates(mounts), [0]);
            let mount = mounts.views.get_mut(&0).unwrap();
            assert!(sources.compile(0, mount, &mut memo).unwrap().is_ok());
        }
        assert_eq!(memo.0.len(), 2, "one compile per profile");
    }

    #[test]
    fn a_runtime_that_did_not_restart_keeps_the_assets_its_build_embedded() {
        let dir = std::env::temp_dir().join(format!("viso-assets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dir = dir.canonicalize().unwrap();
        let mut sources = Sources::default();
        sources.change_asset(&dir, &dir.join("logo.svg"), b"<svg/>".to_vec());
        let entry = MountEntry {
            file: FileId(0),
            path: "/p/src/view.vs".into(),
            package: "app".into(),
            module: vec!["view".into()],
            language: None,
            catalog: None,
            capabilities: Vec::new(),
            source_hash: 0,
            assets: Some((dir.display().to_string(), vec![ResourceId::of("logo.svg")])),
        };
        let (mut restarted, mut other) = (Mounts::default(), Mounts::default());
        sources.mount(&mut restarted, &entry);
        sources.mount(&mut other, &entry);
        sources.change_asset(&dir, &dir.join("logo.svg"), b"<svg></svg>".to_vec());
        assert_eq!(sources.asset_candidates(&restarted, &[]).len(), 1);
        assert_eq!(sources.asset_candidates(&other, &[]).len(), 1);

        // The restarted app embeds the asset as it is now; the other runs
        // what its build embedded.
        sources.rebuilt([&mut other]);
        assert!(sources.asset_candidates(&restarted, &[]).is_empty());
        assert_eq!(sources.asset_candidates(&other, &[]).len(), 1);
    }
}
