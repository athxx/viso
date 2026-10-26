//! The platform fragments of the packages an app links (see
//! [`viso_package::fragment`]): read from each package's directory for the
//! platform the app is built for, the app's own after the others' so it adds
//! what only it knows (the ids of its background tasks), and checked before
//! anything is built, each problem a diagnostic at its place in the file.

use std::path::Path;

use viso_package::Fragment;
use viso_package::android::Shell;
use viso_package::apple;
use viso_package::fragment::{Kind, Problem};
use viso_project::{ConfigCode, ConfigDiagnostic, Span, Target};

use super::Platform;
use super::metadata::Declared;
use crate::dev::cargo::Linked;
use crate::output::{Output, Source};

/// The fragments an app is built with, by what they merge into.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub(crate) struct Fragments {
    /// `android/manifest.xml`: into the Android manifest.
    pub(crate) android: Vec<Fragment>,
    /// `ios/Info.plist` or `macos/Info.plist`: into the Info.plist.
    pub(crate) info: Vec<Fragment>,
    /// `ios/entitlements.plist` or `macos/entitlements.plist`: into the
    /// entitlements the app is signed with.
    pub(crate) entitlements: Vec<Fragment>,
    /// `web/worker.js`: into the service worker.
    pub(crate) worker: Vec<Fragment>,
}

/// The platform whose fragments a build for `target` takes: a package's
/// own, else the target's when it is packaged at all (a desktop build is a
/// bare executable); none for a platform no fragment is for.
pub(crate) fn platform(target: Target, package: Option<Platform>) -> Option<Platform> {
    package
        .or(match target {
            Target::Ios => Some(Platform::Ios),
            Target::Android => Some(Platform::Android),
            Target::WebGpu | Target::WebDom | Target::WebHybrid => Some(Platform::Web),
            Target::Host | Target::Headless | Target::Device(_) => None,
        })
        .filter(|platform| !matches!(platform, Platform::Windows | Platform::Linux))
}

impl Fragments {
    /// The fragments `linked` carry for `platform`, the app (the first)
    /// after the rest.
    pub(crate) fn read(
        linked: &[Linked],
        platform: Platform,
    ) -> Result<Fragments, ConfigDiagnostic> {
        let mut fragments = Fragments::default();
        let apple = match platform {
            Platform::Ios => Some("ios"),
            Platform::MacOs => Some("macos"),
            _ => None,
        };
        let order = linked.iter().skip(1).chain(linked.first());
        for package in order {
            let take = |file: String, into: &mut Vec<Fragment>| {
                let path = package.dir.join(&file);
                match std::fs::read_to_string(&path) {
                    Ok(text) => {
                        into.push(Fragment {
                            package: package.name.clone(),
                            path,
                            text,
                        });
                        Ok(())
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(ConfigDiagnostic::error(
                        ConfigCode::FragmentInvalid,
                        format!("`{}`'s `{file}` cannot be read: {error}", package.name),
                    )
                    .at(&path)),
                }
            };
            match (platform, apple) {
                (Platform::Android, _) => {
                    take("android/manifest.xml".into(), &mut fragments.android)?
                }
                (Platform::Web, _) => take("web/worker.js".into(), &mut fragments.worker)?,
                (_, Some(dir)) => {
                    take(format!("{dir}/Info.plist"), &mut fragments.info)?;
                    take(
                        format!("{dir}/entitlements.plist"),
                        &mut fragments.entitlements,
                    )?;
                }
                _ => {}
            }
        }
        Ok(fragments)
    }

    /// Whether they merge, for `platform`: the manifest fragments into
    /// `shell`'s template, the plists into what Viso writes of the app
    /// `info` describes.
    pub(crate) fn check(
        &self,
        platform: Platform,
        shell: Option<&Shell>,
        info: &apple::Info,
    ) -> Result<(), ConfigDiagnostic> {
        let checked = match platform {
            Platform::Android => match shell.map(|s| std::fs::read_to_string(&s.manifest)) {
                // Without its template the packager says what is missing.
                Some(Ok(template)) => Shell::check(&template, &self.android),
                _ => Ok(()),
            },
            Platform::Ios | Platform::MacOs => {
                let info = apple::Info {
                    fragments: self.info.clone(),
                    ..info.clone()
                };
                apple::check(&info, &self.entitlements, platform == Platform::MacOs)
            }
            _ => Ok(()),
        };
        checked.map_err(|problem| self.diagnostic(problem))
    }

    /// `problem` as a diagnostic at its place in its fragment.
    pub(crate) fn diagnostic(&self, problem: Problem) -> ConfigDiagnostic {
        let code = match problem.kind {
            Kind::Invalid => ConfigCode::FragmentInvalid,
            Kind::Conflict => ConfigCode::FragmentConflict,
        };
        let mut diagnostic = ConfigDiagnostic::error(code, problem.message).at(&problem.path);
        if let Some(fragment) = self.find(&problem.path) {
            diagnostic = diagnostic.span(Span::from_range(&fragment.text, problem.at));
        }
        if let Some(note) = problem.note {
            diagnostic = diagnostic.note(note);
        }
        diagnostic
    }

    fn find(&self, path: &Path) -> Option<&Fragment> {
        [&self.android, &self.info, &self.entitlements, &self.worker]
            .into_iter()
            .flatten()
            .find(|f| f.path == path)
    }
}

/// Shows `diagnostic`, about a fragment, at its place in the file, which is
/// named from the directory holding its package.
pub(crate) fn report(out: &mut Output, diagnostic: &ConfigDiagnostic) {
    let text = diagnostic
        .path
        .as_deref()
        .and_then(|path| std::fs::read_to_string(path).ok());
    match (&diagnostic.path, diagnostic.span, &text) {
        (Some(path), Some(_), Some(text)) => {
            let root = path.ancestors().nth(3).unwrap_or(Path::new(""));
            out.config(Some(&Source::new(path, root, text)), diagnostic);
        }
        _ => out.config(None, diagnostic),
    }
}

/// The `rustc` arguments that link the entitlements of the simulator app
/// `declared` describes into its executable, written under `dir`.
pub(crate) fn link_simulated(declared: &Declared, dir: &Path) -> Result<Vec<String>, String> {
    let entitlements = apple::Entitlements {
        hosts: &declared.links.hosts,
        fragments: &declared.fragments.entitlements,
    }
    .simulated()
    .map_err(|problem| problem.to_string())?;
    apple::simulated_entitlements(&entitlements, dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_platform_reads_its_own_files_the_app_last() {
        let dir = std::env::temp_dir().join(format!("viso-fragments-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let write = |package: &str, file: &str, text: &str| {
            let path = dir.join(package).join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        let modes = "<plist><dict><key>UIBackgroundModes</key><array><string>fetch</string></array></dict></plist>";
        write("app", "ios/Info.plist", modes);
        write("push", "ios/Info.plist", modes);
        write("push", "ios/entitlements.plist", "<plist><dict/></plist>");
        write("push", "android/manifest.xml", "<manifest/>");
        write("push", "web/worker.js", "self.push = 1;");
        let linked: Vec<Linked> = ["app", "platform", "push"]
            .iter()
            .map(|name| Linked {
                name: (*name).into(),
                dir: dir.join(name),
            })
            .collect();
        let ios = Fragments::read(&linked, Platform::Ios).unwrap();
        let packages: Vec<&str> = ios.info.iter().map(|f| f.package.as_str()).collect();
        assert_eq!(packages, ["push", "app"]);
        assert_eq!(ios.entitlements.len(), 1);
        assert!(ios.android.is_empty() && ios.worker.is_empty());
        assert_eq!(
            Fragments::read(&linked, Platform::Android)
                .unwrap()
                .android
                .len(),
            1
        );
        assert_eq!(
            Fragments::read(&linked, Platform::Web)
                .unwrap()
                .worker
                .len(),
            1
        );
        assert_eq!(
            Fragments::read(&linked, Platform::MacOs).unwrap(),
            Fragments::default()
        );
        assert_eq!(platform(Target::Host, None), None);
        assert_eq!(
            platform(Target::Host, Some(Platform::MacOs)),
            Some(Platform::MacOs)
        );

        // A problem is a diagnostic at its line.
        write(
            "bad",
            "ios/Info.plist",
            "<plist>\n<dict>\n  <key>a</key>\n</dict></plist>",
        );
        let mut with_bad = linked.clone();
        with_bad.push(Linked {
            name: "bad".into(),
            dir: dir.join("bad"),
        });
        let ios = Fragments::read(&with_bad, Platform::Ios).unwrap();
        let diagnostic = ios
            .check(Platform::Ios, None, &apple::Info::dev("app"))
            .unwrap_err();
        assert_eq!(diagnostic.code, ConfigCode::FragmentInvalid);
        assert_eq!(diagnostic.span.unwrap().line, 3);
        assert_eq!(
            diagnostic.path.as_deref(),
            Some(dir.join("bad/ios/Info.plist").as_path())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
