//! `viso package [<target>]`: build the app with the shipping profile (or
//! `--profile release`) and make it what its platform ships — a signed
//! `.app` and `.dmg`, an `.ipa`, an APK and app bundle, an archive — into
//! `dist/<platform>/` with its `artifact.json`.
//!
//! The metadata is checked and the signing decided before anything is
//! built, so a missing identity fails in a second rather than after the
//! compile, and `--dry-run` shows the whole plan without building. The
//! package lock of the target is held throughout, and the build cache's
//! while it compiles.

use std::time::Instant;

use viso_package::android::Abi;
use viso_project::{ArtifactKind, LockKind, Profile, Target};

use super::build::{self, Check, Described, Signed, relative_name, seconds};
use crate::args::{PackageArgs, PackageTarget, SigningPolicy};
use crate::build as service;
use crate::context::{Context, failure_code, in_its_file};
use crate::delivery::signing::{self, Machine};
use crate::delivery::{Plan, Platform, Policy, metadata};
use crate::error::{CLI_USAGE, ENV_BUILD_WRITE, PACKAGE_SIGNING, PACKAGE_UNSIGNED, SUCCESS};
use crate::output::sayln;
use crate::output::{Artifact, Output, human};

pub fn run(ctx: &Context<'_>, args: &PackageArgs, out: &mut Output) -> u8 {
    let target = args.target.map_or(Target::Host, PackageTarget::target);
    let profile = match ctx.global.profile.map(|p| p.profile()) {
        Some(Profile::Dev) => {
            return out.failure_next(
                CLI_USAGE,
                "a package never holds the Dev Runtime, so it is not built with `--profile dev`",
                &[
                    "`viso package` builds `shipping`, or `release` with `--profile release`"
                        .to_owned(),
                ],
                &[command_line(target, None)],
            );
        }
        Some(profile) => profile,
        None => Profile::Shipping,
    };
    let policy = match args.signing {
        SigningPolicy::Auto => Policy::Auto,
        SigningPolicy::Required => Policy::Required,
        SigningPolicy::Off => Policy::Off,
    };
    if let Err(code) = build::available(target, out) {
        return code;
    }
    let Some(platform) = Platform::of(target) else {
        return out.failure(
            CLI_USAGE,
            "this machine is not one Viso packages desktop apps for",
            &["a desktop package is made on macOS, Windows or Linux".to_owned()],
        );
    };
    let loaded = match ctx.load_as(target, ArtifactKind::Package, Some(profile), out) {
        Ok(loaded) => loaded,
        Err(code) => return code,
    };
    let project = &loaded.project;
    let root = project.root.clone();

    let (metadata, diagnostics) = metadata::read(project, platform);
    for diagnostic in &diagnostics {
        in_its_file(out, diagnostic);
    }
    let Some(metadata) = metadata else {
        return failure_code(&diagnostics);
    };
    let signing = match signing::resolve(policy, platform, project, &metadata, &Machine::this()) {
        Ok(signing) => signing,
        Err(problem) => return out.failure(PACKAGE_SIGNING, &problem.message, &problem.notes),
    };
    let abis = match platform {
        Platform::Android => {
            let named: Vec<Abi> = project
                .manifest
                .package
                .android
                .abis
                .iter()
                .filter_map(|abi| Abi::parse(&abi.value))
                .collect();
            if named.is_empty() {
                vec![Abi::Arm64]
            } else {
                named
            }
        }
        _ => Vec::new(),
    };
    let plan = Plan::new(target, platform, metadata, signing, abis);
    let platform_name = service::platform(&loaded.layout, target);
    let dist = loaded.layout.dist(target);
    let dist_name = relative_name(&root, &dist, false);
    if let Some(shortfall) = &plan.signing.shortfall {
        out.warning(PACKAGE_UNSIGNED, &shortfall.message, &shortfall.notes);
    }
    if args.dry_run {
        out.result(
            |w| plan.write(w, &platform_name, profile.as_str(), &dist_name, &root),
            || plan.text(&platform_name, profile.as_str(), &dist_name, &root),
        );
        return SUCCESS;
    }

    let _lock = match ctx.lock(&loaded.layout, LockKind::PackageOutput { target }, out) {
        Ok(lock) => lock,
        Err(code) => return code,
    };
    let started = Instant::now();
    let built = match build::stage_loaded(ctx, target, loaded, Check::Run, Some(plan.clone()), out)
    {
        Ok(built) => built,
        Err(code) => return code,
    };
    let version = plan.metadata.version.clone();
    let identity = plan.signing.signer.identity(&root);
    let written = match service::publish(&built.staged, &dist, |artifacts| {
        build::manifest_json(&Described {
            project: &plan.metadata.name,
            version: Some(&version),
            target: &platform_name,
            profile: profile.as_str(),
            build_id: &built.build_id,
            build: Some(plan.metadata.build),
            toolchain: &built.toolchain,
            signing: Signed {
                policy: Some(policy.as_str()),
                status: plan.signing.signer.status(),
                identity: identity.clone(),
            },
            dist: &dist,
            artifacts,
        })
    }) {
        Ok(written) => written,
        Err(why) => return out.failure(ENV_BUILD_WRITE, &why, &[]),
    };

    if out.human() {
        sayln!(
            "Packaged {platform_name} ({profile}) in {}, {}",
            seconds(started.elapsed()),
            plan.signing.signer.summary()
        );
    }
    let directories = ["app-bundle", "site"];
    let paths: Vec<String> = written
        .iter()
        .map(|artifact| relative_name(&root, &artifact.path, directories.contains(&artifact.kind)))
        .collect();
    let width = paths.iter().map(String::len).max().unwrap_or(0);
    for (artifact, path) in written.iter().zip(&paths) {
        out.artifact(
            &Artifact {
                kind: artifact.kind,
                path: path.trim_end_matches('/'),
                target: &platform_name,
                profile: profile.as_str(),
                build_id: &built.build_id,
                size: artifact.size,
                hash: &artifact.hash,
            },
            || {
                format!(
                    "  {:<10} {path:<width$}  {}\n",
                    artifact.kind,
                    human::size(artifact.size)
                )
            },
        );
    }
    SUCCESS
}

/// `viso package` of `target`, with `profile` when one is named.
fn command_line(target: Target, profile: Option<Profile>) -> String {
    let mut line = match target {
        Target::Host => "viso package".to_owned(),
        target => format!("viso package {target}"),
    };
    if let Some(profile) = profile {
        line.push_str(&format!(" --profile {profile}"));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dev_profile_names_the_command_it_meant() {
        assert_eq!(command_line(Target::Host, None), "viso package");
        assert_eq!(
            command_line(Target::Android, Some(Profile::Release)),
            "viso package android --profile release"
        );
    }
}
