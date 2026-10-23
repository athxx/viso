//! Handing a URL to the app the system opens it with: a web page to the
//! browser, `mailto:` to the mail app, an app's own scheme to that app.

use crate::reply::{Reply, ServiceError, ServiceResult};

/// Opens URLs in other apps. The reply answers `Ok` once the system took
/// the URL — not once the other app showed it — and
/// [`ServiceError::Unsupported`] where no app here opens it.
///
/// Only an absolute URL opens, and not one whose scheme would run or read
/// something on this device rather than hand it to an app: `file:`,
/// `javascript:`, `data:`, `vbscript:`, `blob:` and `about:` answer
/// [`ServiceError::Denied`].
pub trait Launcher {
    fn open_url(&self, url: &str) -> Reply<()>;
}

/// The schemes [`Launcher::open_url`] refuses.
const REFUSED: [&str; 6] = ["file", "javascript", "data", "vbscript", "blob", "about"];

/// `url`, trimmed, when it is one a launcher hands on: an absolute URL with
/// a scheme of at least two characters (so no drive letter), no control
/// characters and no space, whose scheme is not refused.
pub(crate) fn openable(url: &str) -> ServiceResult<&str> {
    let url = url.trim();
    let malformed = || ServiceError::Failed(format!("not an absolute URL: {url}"));
    let (scheme, rest) = url.split_once(':').ok_or_else(malformed)?;
    let mut chars = scheme.chars();
    let starts = chars.next().is_some_and(|c| c.is_ascii_alphabetic());
    let scheme_ok = chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
    if !starts || !scheme_ok || scheme.len() < 2 || rest.is_empty() {
        return Err(malformed());
    }
    if url.chars().any(|c| c.is_control() || c == ' ') {
        return Err(malformed());
    }
    if REFUSED.iter().any(|r| scheme.eq_ignore_ascii_case(r)) {
        return Err(ServiceError::Denied);
    }
    Ok(url)
}

/// [`openable`] as the reply a launcher answers at once with when `url` does
/// not open, else `open` of it.
pub(crate) fn launch(url: &str, open: impl FnOnce(&str) -> Reply<()>) -> Reply<()> {
    match openable(url) {
        Ok(url) => open(url),
        Err(error) => Reply::err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_opens_only_with_a_scheme_that_hands_it_to_an_app() {
        assert_eq!(
            openable(" https://example.org/a?b "),
            Ok("https://example.org/a?b")
        );
        assert_eq!(openable("mailto:a@b.c"), Ok("mailto:a@b.c"));
        assert_eq!(openable("zoommtg://join"), Ok("zoommtg://join"));
        assert_eq!(openable("FILE:///etc/passwd"), Err(ServiceError::Denied));
        assert_eq!(openable("javascript:alert(1)"), Err(ServiceError::Denied));
        assert!(matches!(
            openable("C:\\x.exe"),
            Err(ServiceError::Failed(_))
        ));
        assert!(matches!(
            openable("example.org"),
            Err(ServiceError::Failed(_))
        ));
        assert!(matches!(
            openable("https://a b"),
            Err(ServiceError::Failed(_))
        ));
        assert!(matches!(openable("https:"), Err(ServiceError::Failed(_))));
        assert!(matches!(
            openable("1http://x"),
            Err(ServiceError::Failed(_))
        ));
    }
}
