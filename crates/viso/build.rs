//! Keeps the development session out of shipped artifacts: `hot-reload` links
//! the compiler, the watcher and the dev channel, so a build the CLI marks as a
//! release or shipping artifact (`VISO_PROFILE`) refuses the feature. Cargo's
//! own profile is not the gate — an optimized dev build (the `edit_to_pixels`
//! measurement) is still a dev artifact.

fn main() {
    println!("cargo::rerun-if-env-changed=VISO_PROFILE");
    if std::env::var_os("CARGO_FEATURE_HOT_RELOAD").is_none() {
        return;
    }
    let profile = std::env::var("VISO_PROFILE").unwrap_or_default();
    if matches!(profile.as_str(), "release" | "shipping") {
        println!(
            "cargo::error=the `hot-reload` feature is development-only and cannot be part of a \
             `{profile}` artifact; build it without `--features viso/hot-reload`"
        );
    }
}
