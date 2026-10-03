//! Locale tags: what each backend reads from its OS, normalized to BCP 47.

/// `tag` as a BCP 47 language tag: a POSIX locale (`en_US.UTF-8@euro`) or
/// an OS tag with underscores becomes `en-US`, `C` and `POSIX` become `und`,
/// and anything that is not a well-formed tag is `und`.
pub(crate) fn bcp47(tag: &str) -> String {
    let tag = tag.split(['.', '@']).next().unwrap_or_default().trim();
    if tag.is_empty() || tag.eq_ignore_ascii_case("c") || tag.eq_ignore_ascii_case("posix") {
        return "und".to_owned();
    }
    let mut out = String::with_capacity(tag.len());
    for (i, part) in tag.split(['_', '-']).enumerate() {
        let ok =
            !part.is_empty() && part.len() <= 8 && part.bytes().all(|b| b.is_ascii_alphanumeric());
        if !ok || (i == 0 && !(2..=8).contains(&part.len())) {
            return if i == 0 { "und".to_owned() } else { out };
        }
        if i > 0 {
            out.push('-');
        }
        // Language lower case, a 4-letter script title case, a region upper
        // case (RFC 5646 §2.1.1).
        match (i, part.len()) {
            (0, _) => out.push_str(&part.to_ascii_lowercase()),
            (_, 4) if part.bytes().all(|b| b.is_ascii_alphabetic()) => {
                out.push_str(&part[..1].to_ascii_uppercase());
                out.push_str(&part[1..].to_ascii_lowercase());
            }
            (_, 2) | (_, 3) => out.push_str(&part.to_ascii_uppercase()),
            _ => out.push_str(&part.to_ascii_lowercase()),
        }
    }
    out
}

/// The locale a Unix process runs in: `LC_ALL`, then `LC_MESSAGES`, then
/// `LANG`, the first one set and not empty.
#[cfg_attr(
    not(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly",
        test
    )),
    allow(dead_code)
)]
pub(crate) fn from_environment(var: impl Fn(&str) -> Option<String>) -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|name| var(name))
        .find(|value| !value.is_empty())
        .map_or_else(|| "und".to_owned(), |value| bcp47(&value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_locales_normalize_to_bcp47() {
        assert_eq!(bcp47("en_US.UTF-8"), "en-US");
        assert_eq!(bcp47("de_DE@euro"), "de-DE");
        assert_eq!(bcp47("zh-hans-cn"), "zh-Hans-CN");
        assert_eq!(bcp47("sr_Latn_RS"), "sr-Latn-RS");
        assert_eq!(bcp47("es-419"), "es-419");
        assert_eq!(bcp47("ar"), "ar");
        assert_eq!(bcp47("C"), "und");
        assert_eq!(bcp47("POSIX"), "und");
        assert_eq!(bcp47(""), "und");
        assert_eq!(bcp47("x"), "und");
        assert_eq!(bcp47("en_??"), "en");
    }

    #[test]
    fn the_environment_is_read_in_posix_order() {
        let vars = |set: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                set.iter()
                    .find(|(n, _)| *n == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        assert_eq!(
            from_environment(vars(&[("LANG", "fr_FR.UTF-8"), ("LC_ALL", "ja_JP")])),
            "ja-JP"
        );
        assert_eq!(
            from_environment(vars(&[("LC_ALL", ""), ("LANG", "fr_FR.UTF-8")])),
            "fr-FR"
        );
        assert_eq!(from_environment(vars(&[])), "und");
    }
}
