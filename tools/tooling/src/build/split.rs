//! The web module split into a startup and a lazy part, guided by a training
//! run: `wasm-split` instruments the module, the bundle's page loads it in a
//! headless browser served from loopback, and once the app has run for a
//! while the functions it called come back as a profile. Those stay in the
//! primary part the page starts; the rest move to a lazy part loaded in the
//! background ([`viso_package::web::lazy`]).
//!
//! `auto` splits a shipping build that keeps no debug info, when `wasm-split`
//! and a browser are at hand, and ships the module whole, with a warning,
//! when the run fails; `on` splits every build and fails without them.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use viso_package::web::{self as bundle, lazy};
use viso_project::{Auto, Profile, WebSettings};

use super::cargo::{BuildFailure, CargoEvent, Sink, is_cancelled};
use super::web::{FEATURES, WebPage, tool, write_dom_page};
use crate::deploy::web::{ISOLATED, headless_browser, headless_command};
use crate::process::ProcessTree;

/// How `wasm-split` is installed, for a message about its absence.
const WASM_SPLIT_INSTALL: &str = "it comes with Binaryen (`brew install binaryen`, or its release from github.com/WebAssembly/binaryen)";

/// How long a training run may take, from the browser's start.
const DEADLINE: Duration = Duration::from_secs(60);

/// The most a training page may post.
const MAX_POST: usize = 16 << 20;

/// Splits the module of the bundle in `dir`, the app `bin` whose page is
/// `page` (the canvas page when `None`), when `web` asks for it: the primary
/// part in place of the module and the lazy part beside it as
/// `<bin>_lazy.wasm`, which the returned [`lazy::Lazy`] names; `None` when
/// the module stays whole.
#[allow(clippy::too_many_arguments)]
pub(super) fn split(
    dir: &Path,
    bin: &str,
    page: Option<&WebPage>,
    profile: Profile,
    web: &WebSettings,
    threads: bool,
    cancelled: &mpsc::Receiver<()>,
    sink: &Sink,
) -> Result<Option<lazy::Lazy>, BuildFailure> {
    let required = match web.split.value {
        Auto::Off => return Ok(None),
        Auto::Auto if profile != Profile::Shipping || web.source_maps.value => return Ok(None),
        Auto::Auto => false,
        Auto::On => true,
    };
    let skipped = |why: String| {
        if required {
            return Err(BuildFailure::Tool(why));
        }
        sink(CargoEvent::Log {
            level: "warn",
            line: format!("{why}; the module is not split"),
        });
        Ok(None)
    };
    let Some(browser) = headless_browser() else {
        return skipped(format!(
            "no Chrome, Chromium or Edge for the training run (name one with `{}`)",
            crate::deploy::web::CHROME_ENV
        ));
    };
    let files = Files::new(dir, bin);
    let result = train_and_split(
        &files, dir, bin, page, profile, web, threads, &browser, cancelled,
    );
    files.clean();
    match result {
        Ok(lazy) => Ok(lazy),
        Err(BuildFailure::Spawn(why) | BuildFailure::Package(why)) if !required => skipped(why),
        Err(BuildFailure::Spawn(why)) => Err(BuildFailure::Tool(why)),
        Err(failure) => Err(failure),
    }
}

/// The files a split writes beside the bundle's.
struct Files {
    module: PathBuf,
    trained: PathBuf,
    glue: PathBuf,
    training_glue: PathBuf,
    profile: PathBuf,
    primary: PathBuf,
    placeholders: PathBuf,
    lazy: PathBuf,
}

impl Files {
    fn new(dir: &Path, bin: &str) -> Files {
        let primary = dir.join(format!("{bin}_bg.split.wasm"));
        Files {
            module: dir.join(format!("{bin}_bg.wasm")),
            trained: dir.join(format!("{bin}_bg.train.wasm")),
            glue: dir.join(format!("{bin}.js")),
            training_glue: dir.join(format!("{bin}.train.js")),
            profile: dir.join(format!("{bin}.profile")),
            placeholders: dir.join(format!("{bin}_bg.split.wasm.placeholders")),
            primary,
            lazy: dir.join(format!("{bin}_lazy.wasm")),
        }
    }

    /// Removes what only the split needed.
    fn clean(&self) {
        for path in [
            &self.trained,
            &self.training_glue,
            &self.profile,
            &self.primary,
            &self.placeholders,
        ] {
            let _ = std::fs::remove_file(path);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn train_and_split(
    files: &Files,
    dir: &Path,
    bin: &str,
    page: Option<&WebPage>,
    profile: Profile,
    web: &WebSettings,
    threads: bool,
    browser: &Path,
    cancelled: &mpsc::Receiver<()>,
) -> Result<Option<lazy::Lazy>, BuildFailure> {
    let features = || {
        let mut features: Vec<&str> = FEATURES.to_vec();
        if threads {
            features.push("--enable-threads");
        }
        features
    };
    let failed = |path: &Path, error: std::io::Error| {
        BuildFailure::Package(format!("cannot write `{}`: {error}", path.display()))
    };

    let mut instrument = std::process::Command::new("wasm-split");
    instrument
        .arg("--instrument")
        .args(features())
        .arg(&files.module)
        .arg("-o")
        .arg(&files.trained);
    tool(&mut instrument, "wasm-split", WASM_SPLIT_INSTALL, cancelled)?;

    let glue = std::fs::read_to_string(&files.glue).map_err(|error| {
        BuildFailure::Package(format!("cannot read `{}`: {error}", files.glue.display()))
    })?;
    let training = lazy::training_glue(&glue).map_err(BuildFailure::Package)?;
    std::fs::write(&files.training_glue, training).map_err(|e| failed(&files.training_glue, e))?;
    let (glue_name, module_name) = (format!("{bin}.train.js"), format!("{bin}_bg.train.wasm"));
    match page {
        Some(page) => write_dom_page(dir, &glue_name, &module_name, None, page, profile)?,
        None => {
            bundle::write_page(dir, bin, &glue_name, Some(&module_name), None)
                .map_err(BuildFailure::Package)?;
        }
    }
    let trained = run(dir, browser, threads, cancelled)?;
    std::fs::write(&files.profile, trained).map_err(|e| failed(&files.profile, e))?;

    let mut split = std::process::Command::new("wasm-split");
    split
        .args(features())
        .arg(format!("--profile={}", files.profile.display()))
        .args(lazy::SPLIT_ARGS)
        .arg(&files.module)
        .arg("-o1")
        .arg(&files.primary)
        .arg("-o2")
        .arg(&files.lazy);
    split.arg(if web.source_maps.value || !web.strip.value {
        "-g"
    } else {
        "--strip-debug"
    });
    tool(&mut split, "wasm-split", WASM_SPLIT_INSTALL, cancelled)?;

    let read = |path: &Path| {
        std::fs::read(path).map_err(|error| {
            BuildFailure::Package(format!("cannot read `{}`: {error}", path.display()))
        })
    };
    let primary = read(&files.primary)?;
    let placeholders = String::from_utf8_lossy(&read(&files.placeholders)?).into_owned();
    // Every function the app ran: nothing is left to load later.
    if placeholders.lines().nth(1).is_none() {
        let _ = std::fs::remove_file(&files.lazy);
        return Ok(None);
    }
    let lazy = lazy::Lazy::read(&primary, &placeholders, format!("{bin}_lazy.wasm"))
        .map_err(BuildFailure::Package)?;
    std::fs::rename(&files.primary, &files.module).map_err(|error| {
        BuildFailure::Package(format!(
            "cannot replace `{}`: {error}",
            files.module.display()
        ))
    })?;
    Ok(Some(lazy))
}

/// What a training page sent.
enum Posted {
    Profile(Vec<u8>),
    Log(String),
}

/// Serves `dir` on loopback, loads its page in `browser` and waits for the
/// profile it posts.
fn run(
    dir: &Path,
    browser: &Path,
    isolated: bool,
    cancelled: &mpsc::Receiver<()>,
) -> Result<Vec<u8>, BuildFailure> {
    let failed = |why: String| BuildFailure::Package(format!("the training run {why}"));
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.set_nonblocking(true).map(|()| listener))
        .map_err(|error| failed(format!("cannot listen on loopback: {error}")))?;
    let addr = listener
        .local_addr()
        .map_err(|error| failed(format!("has no address: {error}")))?;
    let stop = Arc::new(AtomicBool::new(false));
    let (posted, received) = mpsc::channel();
    let server = {
        let (dir, stop) = (dir.to_owned(), Arc::clone(&stop));
        thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let (dir, posted) = (dir.clone(), posted.clone());
                        thread::spawn(move || answer(stream, &dir, isolated, &posted));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        })
    };

    static RUNS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let user_dir = std::env::temp_dir().join(format!(
        "viso-train-{}-{}",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&user_dir);
    let url = format!("http://{addr}/");
    let result = ProcessTree::spawn(&mut headless_command(browser, &url, &user_dir))
        .map_err(|error| failed(format!("cannot start `{}`: {error}", browser.display())))
        .and_then(|mut chrome| {
            let result = wait(&mut chrome, &received, cancelled);
            chrome.kill();
            result.map_err(|why| match why {
                Some(why) => failed(why),
                None => BuildFailure::Cancelled,
            })
        });
    stop.store(true, Ordering::Relaxed);
    let _ = server.join();
    let _ = std::fs::remove_dir_all(&user_dir);
    result
}

/// The profile the page posts; `Err(None)` when the command ends first,
/// `Err(Some(why))` when the run fails.
fn wait(
    chrome: &mut ProcessTree,
    received: &mpsc::Receiver<Posted>,
    cancelled: &mpsc::Receiver<()>,
) -> Result<Vec<u8>, Option<String>> {
    let start = Instant::now();
    let mut said = Vec::new();
    let why = |what: &str, said: &[String]| {
        let said = said
            .last()
            .map_or(String::new(), |line| format!(": {line}"));
        Some(format!("{what}{said}"))
    };
    loop {
        if is_cancelled(cancelled) {
            return Err(None);
        }
        match received.recv_timeout(Duration::from_millis(50)) {
            Ok(Posted::Profile(profile)) => return Ok(profile),
            Ok(Posted::Log(line)) => said.push(line),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(why("lost its server", &said));
            }
        }
        if let Ok(Some(status)) = chrome.child.try_wait() {
            return Err(why(&format!("ended its browser ({status})"), &said));
        }
        if start.elapsed() > DEADLINE {
            return Err(why(
                &format!("sent no profile within {} s", DEADLINE.as_secs()),
                &said,
            ));
        }
    }
}

/// Answers one request of the training page: a file of `dir`, or what it
/// posts.
fn answer(mut stream: TcpStream, dir: &Path, isolated: bool, posted: &mpsc::Sender<Posted>) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return;
    };
    let (method, target) = (method.to_owned(), target.to_owned());
    let mut length = 0usize;
    loop {
        line.clear();
        if reader.read_line(&mut line).is_err() || line.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let isolation = if isolated { ISOLATED } else { "" };
    let respond = |stream: &mut TcpStream, status: &str, kind: &str, body: &[u8]| {
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n{isolation}Connection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(body);
    };
    let path = bundle::request_path(&target).unwrap_or("");
    if method == "POST" {
        let mut body = vec![0; length.min(MAX_POST)];
        if reader.read_exact(&mut body).is_err() {
            return;
        }
        let sent = match format!("/{path}").as_str() {
            lazy::PROFILE_PATH => posted.send(Posted::Profile(body)),
            lazy::LOG_PATH => posted.send(Posted::Log(String::from_utf8_lossy(&body).into())),
            _ => Ok(()),
        };
        drop(sent);
        respond(&mut stream, "204 No Content", "text/plain", b"");
        return;
    }
    match bundle::resolve(dir, &target).and_then(|file| Some((std::fs::read(&file).ok()?, file))) {
        Some((bytes, file)) => respond(&mut stream, "200 OK", bundle::mime(&file), &bytes),
        None => respond(&mut stream, "404 Not Found", "text/plain", b"not found"),
    }
}
