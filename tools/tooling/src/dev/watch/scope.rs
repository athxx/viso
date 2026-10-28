//! The project half of the watch scope: the files of the project a dev
//! session acts on, and the directories it never descends into.
//!
//! In scope are the `.vs` sources, the message catalogs (`*.toml` in an
//! `i18n/` directory), every file under an `assets/` directory (its format is
//! read from its bytes, not its name) and the root `Viso.toml`. Out of scope
//! are Cargo's and the packager's outputs at the root (`target/`, `dist/`),
//! every hidden directory (`.git/`, `.viso/`, editor state), any directory
//! marked as a cache by a `CACHEDIR.TAG` (a relocated target directory,
//! generated caches), `node_modules/`, and editor temporaries (hidden, `#…#`,
//! `…~`). Shader and font sources join the scope with the domains that
//! patch them; the Rust half is Cargo's (`crate::dev::cargo`).

use std::path::Path;

/// Whether directory `dir` of the project rooted at `root` is left out.
pub fn excluded_dir(root: &Path, dir: &Path) -> bool {
    let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
        return true;
    };
    name.starts_with('.')
        || name == "node_modules"
        || (dir.parent() == Some(root) && matches!(name, "target" | "dist"))
        || dir.join("CACHEDIR.TAG").is_file()
}

/// Whether file `path` of the project rooted at `root` is in the scope.
pub fn in_scope(root: &Path, path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name.starts_with(['.', '#']) || name.ends_with('~') {
        return false;
    }
    if path.parent() == Some(root) && name == viso_project::MANIFEST_NAME {
        return true;
    }
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("vs") => true,
        Some("toml")
            if path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|dir| dir == viso_dsl::i18n::CATALOG_DIR) =>
        {
            true
        }
        _ => asset_dir(root, path).is_some(),
    }
}

/// The `assets/` directory file `path` of the project rooted at `root` lies
/// in: the outermost of its ancestors below the root so named.
pub fn asset_dir<'p>(root: &Path, path: &'p Path) -> Option<&'p Path> {
    let mut found = None;
    let mut dir = path.parent();
    while let Some(at) = dir {
        if at == root || !at.starts_with(root) {
            break;
        }
        if at
            .file_name()
            .is_some_and(|n| n == viso_dsl::assets::ASSET_DIR)
        {
            found = Some(at);
        }
        dir = at.parent();
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scope_is_sources_catalogs_assets_and_the_manifest() {
        let root = Path::new("/p");
        for file in [
            "/p/Viso.toml",
            "/p/src/a.vs",
            "/p/i18n/en.toml",
            "/p/x/i18n/fr.toml",
            "/p/assets/logo.png",
            "/p/assets/icons/star",
            "/p/pkg/assets/a.toml",
        ] {
            assert!(in_scope(root, Path::new(file)), "{file}");
        }
        for file in [
            "/p/src/Viso.toml",
            "/p/Cargo.toml",
            "/p/src/.a.vs.swp",
            "/p/src/.#a.vs",
            "/p/src/#a.vs#",
            "/p/src/a.vs~",
            "/p/src/a.rs",
            "/p/assets/.DS_Store",
            "/p/assets/logo.png~",
            "/p/assets.png",
        ] {
            assert!(!in_scope(root, Path::new(file)), "{file}");
        }
        assert_eq!(
            asset_dir(root, Path::new("/p/pkg/assets/icons/assets/a.svg")),
            Some(Path::new("/p/pkg/assets"))
        );
        assert_eq!(asset_dir(root, Path::new("/assets/a.png")), None);
        for dir in [
            "/p/target",
            "/p/dist",
            "/p/.git",
            "/p/src/.idea",
            "/p/node_modules",
        ] {
            assert!(excluded_dir(root, Path::new(dir)), "{dir}");
        }
        for dir in ["/p/src", "/p/src/target", "/p/i18n"] {
            assert!(!excluded_dir(root, Path::new(dir)), "{dir}");
        }
    }
}
