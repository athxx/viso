//! The URLs a desktop app is opened with: the OS starts the program with the
//! URL as an argument, and a second start hands it to the instance already
//! running.

use std::ffi::OsString;

/// The URL schemes the app declares, as its build names them
/// (`VISO_URL_SCHEMES`, comma-separated); none for a build that names none.
pub(crate) fn declared_schemes() -> impl Iterator<Item = &'static str> + Clone {
    option_env!("VISO_URL_SCHEMES")
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Whether `url` is an absolute URL of one of `schemes`, with no control
/// character in it.
pub(crate) fn is_link<'a>(url: &str, mut schemes: impl Iterator<Item = &'a str>) -> bool {
    let Some((scheme, rest)) = url.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && !url.chars().any(char::is_control)
        && schemes.any(|s| s.eq_ignore_ascii_case(scheme))
}

/// The arguments among `args` (the program's name excluded) that are URLs of
/// `schemes`, in order.
pub(crate) fn urls_in<'a>(
    args: impl IntoIterator<Item = OsString>,
    schemes: impl Iterator<Item = &'a str> + Clone,
) -> Vec<String> {
    args.into_iter()
        .filter_map(|arg| arg.into_string().ok())
        .filter(|arg| is_link(arg, schemes.clone()))
        .collect()
}

/// The URLs of the declared schemes the program was started with.
pub(crate) fn launch_urls() -> Vec<String> {
    urls_in(std::env::args_os().skip(1), declared_schemes())
}

/// A name for this app's running instance: the executable's name and a hash
/// of its full path, so two copies of one app are two apps, and two apps of
/// one name are too.
pub(crate) fn instance_name() -> String {
    let exe = std::env::current_exe().unwrap_or_default();
    let exe = exe.canonicalize().unwrap_or(exe);
    let stem: String = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .take(32)
        .collect();
    let path = exe.to_string_lossy();
    // Windows paths compare without case.
    let path = if cfg!(windows) {
        path.to_lowercase()
    } else {
        path.into_owned()
    };
    format!("viso-{stem}-{:016x}", fnv1a(path.as_bytes()))
}

/// The 64-bit FNV-1a hash of `bytes`.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_urls_of_the_declared_schemes_are_links() {
        let schemes = ["notes", "x-notes"];
        let args = [
            "--verbose",
            "notes://open/42",
            "localhost:8080",
            "NOTES:item",
            "https://example.com",
            "notes:",
            "x-notes://a\nb",
            "x-notes:/b",
        ]
        .map(OsString::from);
        assert_eq!(
            urls_in(args, schemes.iter().copied()),
            ["notes://open/42", "NOTES:item", "x-notes:/b"]
        );
        assert!(urls_in([OsString::from("notes://a")], [].into_iter()).is_empty());
    }

    #[test]
    fn an_instance_is_named_by_its_executable() {
        let name = instance_name();
        assert!(name.starts_with("viso-"), "{name}");
        assert_eq!(name, instance_name());
        assert_ne!(fnv1a(b"/a/notes"), fnv1a(b"/b/notes"));
    }
}
