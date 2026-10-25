//! The URLs the app is opened with. The desktop starts the program with the
//! URL as its argument (`Exec=… %u`), so each start first offers its URLs to
//! an instance of the app already running, through a socket named for the
//! app in the user's runtime directory, and ends once one takes them;
//! otherwise it listens there itself for the next start's.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::Mutex;

use super::super::links::{declared_schemes, instance_name, is_link, launch_urls};

/// The most a start hands over at once.
const MAX_HANDED: u64 = 64 * 1024;

/// The socket an instance listens on, and where it is.
type Socket = (UnixListener, PathBuf);

/// What [`claim`] found: the URLs this start opens, and the socket it
/// listens on, until [`serve`] takes them.
static CLAIMED: Mutex<Option<(Vec<String>, Option<Socket>)>> = Mutex::new(None);

/// Takes this start's URLs. With an instance of the app already running,
/// hands them to it and ends the process instead; otherwise listens for the
/// next start's. Nothing for an app that declares no scheme.
pub(crate) fn claim() {
    if declared_schemes().next().is_none() {
        return;
    }
    let urls = launch_urls();
    let listener = socket_path().and_then(|path| {
        if let Ok(mut stream) = UnixStream::connect(&path) {
            // A start with no URL is an instance of its own.
            if !urls.is_empty() && hand_over(&mut stream, &urls) {
                std::process::exit(0);
            }
            return None;
        }
        // Nothing answers: what is there was left by an instance that ended.
        let _ = std::fs::remove_file(&path);
        UnixListener::bind(&path).ok().map(|l| (l, path))
    });
    *CLAIMED.lock().unwrap_or_else(|e| e.into_inner()) = Some((urls, listener));
}

/// Writes `urls` to the running instance, one a line, and waits for it to
/// say it took them.
fn hand_over(stream: &mut UnixStream, urls: &[String]) -> bool {
    let mut text = String::new();
    for url in urls {
        text.push_str(url);
        text.push('\n');
    }
    if stream.write_all(text.as_bytes()).is_err()
        || stream.shutdown(std::net::Shutdown::Write).is_err()
    {
        return false;
    }
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
    let mut answer = [0u8; 1];
    matches!(std::io::Read::read(stream, &mut answer), Ok(1) if answer[0] == b'1')
}

/// The URLs this start opens, and a thread handing every URL a later start
/// offers to `open` for as long as the process runs. The socket goes when
/// the returned guard is dropped.
pub(crate) fn serve(open: impl Fn(String) + Send + 'static) -> (Vec<String>, Option<Unlink>) {
    let Some((urls, listener)) = CLAIMED.lock().unwrap_or_else(|e| e.into_inner()).take() else {
        return (Vec::new(), None);
    };
    let Some((listener, path)) = listener else {
        return (urls, None);
    };
    let spawned = std::thread::Builder::new()
        .name("viso-links".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else {
                    continue;
                };
                let reader = BufReader::new(std::io::Read::take(&stream, MAX_HANDED));
                for line in reader.lines().map_while(Result::ok) {
                    if is_link(&line, declared_schemes()) {
                        open(line);
                    }
                }
                let _ = stream.write_all(b"1");
            }
        });
    (urls, spawned.ok().map(|_| Unlink(path)))
}

/// Removes the instance's socket when the app ends.
pub(crate) struct Unlink(PathBuf);

impl Drop for Unlink {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The socket an instance of this app listens on: in the user's runtime
/// directory, which only the user can reach, so another user can neither
/// receive the app's URLs nor hand it any. None without that directory.
fn socket_path() -> Option<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty())?;
    Some(PathBuf::from(dir).join(format!("{}.sock", instance_name())))
}
