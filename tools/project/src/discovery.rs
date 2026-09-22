//! Finding the project root (`Viso_CLI.md` section 4).
//!
//! Walk up from the working directory to the first `Viso.toml`; stop at the
//! filesystem root; `--project <path>` overrides the walk entirely. The override
//! does not merely seed the walk — an explicit path that has no manifest is an
//! error, because a walk from there would silently build the *enclosing* project
//! instead of the one the user named.

use std::path::{Path, PathBuf};

use crate::diag::{ConfigCode, ConfigDiagnostic};
use crate::manifest::{Manifest, ParseOutcome};

/// The manifest file name. One spelling, everywhere.
pub const MANIFEST_NAME: &str = "Viso.toml";

/// A located project: its root, its parsed manifest, and its members.
#[derive(Debug, Clone)]
pub struct Project {
    /// The directory containing `Viso.toml`. Absolute when the caller's starting
    /// path was.
    pub root: PathBuf,
    /// The parsed manifest.
    pub manifest: Manifest,
    /// Resolved absolute roots of `[workspace] members`, in declaration order.
    /// Empty for a single-project repository.
    pub members: Vec<PathBuf>,
    /// Non-fatal problems found while parsing. Carried rather than printed: this
    /// crate does not own output (`Viso_CLI.md` section 40).
    pub warnings: Vec<ConfigDiagnostic>,
}

impl Project {
    /// Walks up from `start` to the first `Viso.toml` and loads it.
    pub fn discover(start: impl AsRef<Path>) -> Result<Self, Vec<ConfigDiagnostic>> {
        let start = start.as_ref();
        match find_root(start) {
            Some(root) => Self::at_root(root),
            None => Err(vec![
                ConfigDiagnostic::error(
                    ConfigCode::ManifestNotFound,
                    format!(
                        "no `{MANIFEST_NAME}` in `{}` or any parent",
                        start.display()
                    ),
                )
                .note("run `viso new <name>` to create a project, or pass `--project <path>`"),
            ]),
        }
    }

    /// Opens the project `path` names, with no upward walk.
    ///
    /// `path` may be the project directory or the manifest file itself; both
    /// spellings occur in the wild and neither is ambiguous.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Vec<ConfigDiagnostic>> {
        let path = path.as_ref();
        let root = if path.file_name().is_some_and(|n| n == MANIFEST_NAME) {
            path.parent().unwrap_or(Path::new(".")).to_path_buf()
        } else {
            path.to_path_buf()
        };
        if !root.join(MANIFEST_NAME).is_file() {
            return Err(vec![
                ConfigDiagnostic::error(
                    ConfigCode::ManifestNotFound,
                    format!("`{}` has no `{MANIFEST_NAME}`", root.display()),
                )
                .note("`--project` must name a project directory or its Viso.toml"),
            ]);
        }
        Self::at_root(root)
    }

    /// Either of the above, depending on whether `--project` was given.
    pub fn locate(
        explicit: Option<&Path>,
        cwd: impl AsRef<Path>,
    ) -> Result<Self, Vec<ConfigDiagnostic>> {
        match explicit {
            Some(path) => Self::open(path),
            None => Self::discover(cwd),
        }
    }

    /// Loads the manifest at a known root and resolves its members.
    fn at_root(root: PathBuf) -> Result<Self, Vec<ConfigDiagnostic>> {
        let manifest_path = root.join(MANIFEST_NAME);
        let (manifest, mut warnings) = match Manifest::load(&manifest_path) {
            ParseOutcome::Parsed { manifest, warnings } => (manifest, warnings),
            ParseOutcome::Failed(diags) => return Err(diags),
        };

        let mut members = Vec::with_capacity(manifest.workspace.members.len());
        let mut errors = Vec::new();
        for member in &manifest.workspace.members {
            let path = root.join(&member.value);
            if path.join(MANIFEST_NAME).is_file() {
                members.push(path);
            } else {
                // A declared member that is not there means the workspace does not
                // describe the repository. Building the subset that happens to
                // exist would be a different build than the one configured.
                errors.push(
                    ConfigDiagnostic::error(
                        ConfigCode::MemberMissing,
                        format!(
                            "workspace member `{}` has no `{MANIFEST_NAME}`",
                            member.value
                        ),
                    )
                    .at(&manifest_path)
                    .span(member.span)
                    .note(format!("expected `{}`", path.join(MANIFEST_NAME).display())),
                );
            }
        }
        if !errors.is_empty() {
            warnings.extend(errors);
            return Err(warnings);
        }

        Ok(Self {
            root,
            manifest,
            members,
            warnings,
        })
    }

    /// The manifest path.
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_NAME)
    }

    /// The build directory (`target/`), the parent of everything in
    /// [`crate::cache`].
    pub fn target_dir(&self) -> PathBuf {
        self.root.join("target")
    }

    /// The project name, or the diagnostic for its absence.
    pub fn name(&self) -> Result<&str, ConfigDiagnostic> {
        self.manifest.require_name()
    }
}

/// The nearest ancestor of `start` (inclusive) containing a manifest.
///
/// A plain `while let Some(parent)` loop: `Path::parent` terminates at the root,
/// and every step is one `is_file` probe. Nothing here needs to canonicalize, which
/// keeps the answer meaningful inside a symlinked checkout — the user's path is the
/// user's path.
pub fn find_root(start: impl AsRef<Path>) -> Option<PathBuf> {
    let mut dir = start.as_ref();
    loop {
        if dir.join(MANIFEST_NAME).is_file() {
            return Some(dir.to_path_buf());
        }
        dir = dir.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;

    const MINIMAL: &str = "[package]\nname = \"app\"\n";

    #[test]
    fn discovery_walks_up_from_a_nested_directory() {
        let s = Scratch::new("walk");
        s.write("Viso.toml", MINIMAL);
        let deep = s.dir("features/home/detail");

        let project = Project::discover(&deep).unwrap();
        assert_eq!(project.root, s.path());
        assert_eq!(project.name().unwrap(), "app");
        assert!(project.warnings.is_empty());

        // Starting at the root itself is the same answer, not a walk to the parent.
        assert_eq!(Project::discover(s.path()).unwrap().root, s.path());
    }

    /// The nearest manifest wins, so a nested project inside a workspace builds
    /// itself rather than its parent.
    #[test]
    fn the_nearest_manifest_wins() {
        let s = Scratch::new("nearest");
        s.write("Viso.toml", "[package]\nname = \"outer\"\n");
        s.write("apps/inner/Viso.toml", "[package]\nname = \"inner\"\n");
        let deep = s.dir("apps/inner/src");

        let project = Project::discover(&deep).unwrap();
        assert_eq!(project.name().unwrap(), "inner");
    }

    #[test]
    fn no_manifest_anywhere_is_a_diagnostic_not_a_panic() {
        // The system temporary directory has no Viso.toml above it in any sane
        // checkout; using the scratch dir keeps the walk inside our own tree.
        let s = Scratch::new("none");
        let deep = s.dir("a/b");
        let diags = Project::discover(&deep).unwrap_err();
        assert_eq!(diags[0].code, ConfigCode::ManifestNotFound);
        assert_eq!(diags[0].code.exit_code(), 1);
        assert!(diags[0].notes.join(" ").contains("--project"));
    }

    /// `--project` replaces the walk. The distinction this pins: a path with no
    /// manifest fails, instead of quietly resolving to the enclosing project.
    #[test]
    fn an_explicit_project_path_does_not_walk_upward() {
        let s = Scratch::new("explicit");
        s.write("Viso.toml", "[package]\nname = \"outer\"\n");
        let sub = s.dir("not-a-project");

        let diags = Project::open(&sub).unwrap_err();
        assert_eq!(diags[0].code, ConfigCode::ManifestNotFound);

        // Discovery from the same directory *does* find the parent — the two
        // behaviors differ on purpose.
        assert_eq!(Project::discover(&sub).unwrap().name().unwrap(), "outer");
    }

    #[test]
    fn an_explicit_path_may_name_the_directory_or_the_manifest() {
        let s = Scratch::new("explicit-forms");
        s.write("app/Viso.toml", MINIMAL);
        let dir = s.path().join("app");

        assert_eq!(Project::open(&dir).unwrap().root, dir);
        assert_eq!(
            Project::open(dir.join(MANIFEST_NAME)).unwrap().root,
            dir,
            "naming the manifest resolves to its directory"
        );
    }

    #[test]
    fn locate_switches_on_the_presence_of_an_override() {
        let s = Scratch::new("locate");
        s.write("Viso.toml", "[package]\nname = \"outer\"\n");
        s.write("apps/one/Viso.toml", "[package]\nname = \"one\"\n");
        let one = s.path().join("apps/one");

        assert_eq!(
            Project::locate(None, s.path()).unwrap().name().unwrap(),
            "outer"
        );
        assert_eq!(
            Project::locate(Some(&one), s.path())
                .unwrap()
                .name()
                .unwrap(),
            "one"
        );
    }

    #[test]
    fn workspace_members_resolve_to_absolute_roots() {
        let s = Scratch::new("members");
        s.write(
            "Viso.toml",
            "[package]\nname = \"root\"\n\n[workspace]\nmembers = [\"apps/one\", \"apps/two\"]\n",
        );
        s.write("apps/one/Viso.toml", "[package]\nname = \"one\"\n");
        s.write("apps/two/Viso.toml", "[package]\nname = \"two\"\n");

        let project = Project::discover(s.path()).unwrap();
        assert_eq!(
            project.members,
            vec![s.path().join("apps/one"), s.path().join("apps/two")],
            "declaration order is preserved"
        );
    }

    /// A member that is not there means the manifest does not describe the
    /// repository; building the subset that exists would be a different build.
    #[test]
    fn a_missing_member_is_an_error_pointing_at_its_line() {
        let s = Scratch::new("missing-member");
        s.write(
            "Viso.toml",
            "[package]\nname = \"root\"\n\n[workspace]\nmembers = [\n  \"apps/one\",\n  \"apps/gone\",\n]\n",
        );
        s.write("apps/one/Viso.toml", "[package]\nname = \"one\"\n");

        let diags = Project::discover(s.path()).unwrap_err();
        let diag = diags
            .iter()
            .find(|d| d.code == ConfigCode::MemberMissing)
            .expect("expected a member diagnostic");
        assert!(diag.message.contains("apps/gone"));
        assert_eq!(
            diag.span.unwrap().line,
            7,
            "points at the member, not the array"
        );
    }

    /// A manifest that does not parse fails discovery with the parse diagnostics
    /// themselves, not a second-hand "could not load project".
    #[test]
    fn a_broken_manifest_surfaces_its_own_diagnostics() {
        let s = Scratch::new("broken");
        s.write("Viso.toml", "[package\n");
        let diags = Project::discover(s.path()).unwrap_err();
        assert_eq!(diags[0].code, ConfigCode::ManifestSyntax);
        assert_eq!(
            diags[0].path.as_deref(),
            Some(s.path().join(MANIFEST_NAME).as_path())
        );
    }

    /// Warnings travel with the project instead of blocking it — an unknown key is
    /// worth reporting and not worth refusing to build over.
    #[test]
    fn warnings_are_carried_not_fatal() {
        let s = Scratch::new("warnings");
        s.write("Viso.toml", "[package]\nname = \"app\"\nnaem = \"typo\"\n");
        let project = Project::discover(s.path()).unwrap();
        assert_eq!(project.warnings.len(), 1);
        assert_eq!(project.warnings[0].code, ConfigCode::UnknownKey);
    }

    #[test]
    fn the_target_directory_hangs_off_the_root() {
        let s = Scratch::new("target-dir");
        s.write("Viso.toml", MINIMAL);
        let project = Project::discover(s.path()).unwrap();
        assert_eq!(project.target_dir(), s.path().join("target"));
        assert_eq!(project.manifest_path(), s.path().join(MANIFEST_NAME));
    }
}
