//! What is never printed: a value whose name says it is a secret, in a
//! variable or a `name=value` argument, wherever `--verbose`, a log or a
//! diagnostic would show it.

/// The words a secret's name holds.
const WORDS: &[&str] = &[
    "TOKEN",
    "SECRET",
    "PASSWORD",
    "PASSWD",
    "KEY",
    "CREDENTIAL",
    "AUTH",
    "PRIVATE",
    "COOKIE",
    "SESSION",
];

/// What a hidden value prints as.
pub(crate) const HIDDEN: &str = "<hidden>";

/// Whether a variable's name says its value is a secret.
pub(crate) fn named(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    WORDS.iter().any(|word| name.contains(word))
}

/// `name=value`, the value hidden when the name is a secret's.
pub(crate) fn pair(name: &str, value: &str) -> String {
    if named(name) {
        format!("{name}={HIDDEN}")
    } else {
        format!("{name}={value}")
    }
}

/// A command-line argument as it may be shown: a `name=value` whose name
/// is a secret's (`--token=…`, `registry.token=…`) with its value hidden.
pub(crate) fn argument(arg: &str) -> String {
    match arg.rsplit_once('=') {
        Some((name, value)) if !value.is_empty() && named(name) => pair(name, value),
        _ => arg.to_owned(),
    }
}

/// Whether `arg` names a secret the next argument holds: a flag
/// (`--password`) or a variable name (`--es VISO_DEV_TOKEN value`) that
/// says so and carries no value of its own.
pub(crate) fn names_next(arg: &str) -> bool {
    let flag = arg.starts_with('-');
    let variable = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    (flag || variable) && !arg.contains('=') && named(arg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_value_is_hidden_by_its_name() {
        assert!(named("GITHUB_TOKEN"));
        assert!(named("viso_android_keystore_password"));
        assert!(!named("PATH"));
        assert_eq!(pair("API_KEY", "abc"), "API_KEY=<hidden>");
        assert_eq!(pair("MODE", "fast"), "MODE=fast");
        assert_eq!(
            argument("--config=registries.x.token=abc"),
            "--config=registries.x.token=<hidden>"
        );
        assert_eq!(argument("--features=a,b"), "--features=a,b");
        assert_eq!(argument("--release"), "--release");
    }
}
