//! The web dev server: it serves the dev build's web
//! bundle on the loopback interface — on the network too only when asked
//! ([`Listen`]) — and carries the page's dev channel as a WebSocket at
//! [`WEB_DEV_PATH`] of the address the page was loaded from.
//!
//! Every request must name, in its `Host`, an address the server listens at:
//! an IP literal or `localhost`, never a domain name, so a page of another
//! site cannot reach it by rebinding its name to this machine.
//!
//! The page that loads the app is written per request: the bundle's own
//! `index.html`, which names its module and glue, with the launch variables
//! (`visoLaunch`) the app reads before it starts — the session's identity,
//! and, once after a warm restart, the restore — ahead of its `</head>`. A WebSocket is accepted only from a page this
//! server served (its `Origin`), and the dev channel behind it checks the
//! session token as the TCP channel does. Once a newer build is served, every
//! open page's channel is closed with [`WEB_RELOAD_CLOSE`] and the page loads
//! it.
//!
//! Each page also opens a channel of its own at [`PAGE_PATH`] from a script
//! that runs before the app: what the page writes to its console, and its
//! uncaught errors and rejections — a panic's message among them — reach the
//! session as the app's `log` lines, and a newer build reloads it even when
//! the app died before its dev channel opened. It carries text only: a
//! level, a tab and the line, each line at most [`MAX_LOG`] bytes.
//!
//! Each connection is served on a thread of its own; nothing is cached by the
//! browser (`Cache-Control: no-store`). Every file is sent with its type and
//! `nosniff`, kept to this origin (`Cross-Origin-Resource-Policy`), and as its
//! Brotli or gzip copy when the bundle has one the browser accepts; the page
//! runs only the scripts it was written with — each carries the response's
//! nonce — and the bundle's (`Content-Security-Policy`), WebAssembly
//! compiled.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

use viso_ende::JsonWriter;
use viso_package::web;
use viso_view::dev::wire::{
    DEV_RESTORE_ENV, DEV_RUNTIME_ENV, MAX_FRAME, MAX_LOG, WEB_DEV_PATH, WEB_RELOAD_CLOSE,
};

use crate::dev::link::{self, Deliver, DevStream, Expect};

/// The port tried first, and how many after it.
pub(crate) const DEFAULT_PORT: u16 = viso_project::settings::defaults::SERVE_PORT;
const PORT_TRIES: u16 = 100;

/// How long a request has to arrive whole, and the most a request head may
/// hold.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEAD: usize = 16 << 10;

/// Where a page's own channel opens.
const PAGE_PATH: &str = "/__viso/page";

/// Where the server hands each line a page logged: its level and text;
/// `false` once nobody listens.
pub(crate) type PageLog = Arc<dyn Fn(&'static str, String) -> bool + Send + Sync>;

/// The GUID RFC 6455 hashes a WebSocket key with.
const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// Where the web dev server listens: the loopback interface alone unless the
/// network was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listen {
    /// The addresses bound, the one the session's own browser loads first.
    ips: Vec<IpAddr>,
    /// The address other devices load the page at, when the network was
    /// asked for.
    lan: Option<IpAddr>,
    /// The port asked for, which alone will do; without one the configured
    /// port or the next free one after it is taken.
    port: Option<u16>,
}

/// Why a [`Listen`] cannot be had: the command line asked for something
/// unsafe or contradictory, or this machine has no network to offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ListenError {
    Usage(String),
    Environment(String),
}

impl Listen {
    /// The loopback interface.
    pub(crate) fn loopback() -> Listen {
        Listen {
            ips: vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
            lan: None,
            port: None,
        }
    }

    /// What `--host`, `--lan` and `--port` ask for. An address off the
    /// loopback interface needs `--lan`; `--lan` alone binds the private
    /// address this machine reaches its network by, beside the loopback one.
    pub(crate) fn new(
        host: Option<IpAddr>,
        lan: bool,
        port: Option<u16>,
    ) -> Result<Listen, ListenError> {
        let mut listen = Listen {
            port,
            ..Listen::loopback()
        };
        match (host, lan) {
            (None, false) => {}
            (Some(ip), false) if ip.is_loopback() => listen.ips = vec![ip],
            (Some(ip), false) => {
                return Err(ListenError::Usage(format!(
                    "`--host {ip}` serves the network: add `--lan` to mean it"
                )));
            }
            (Some(ip), true) if ip.is_loopback() => {
                return Err(ListenError::Usage(format!(
                    "`--lan` serves the network, but `--host {ip}` is a loopback address"
                )));
            }
            (Some(ip), true) if ip.is_unspecified() => {
                listen.ips = vec![ip];
                listen.lan = Some(route_address().ok_or_else(|| {
                    ListenError::Environment("this machine is on no network".into())
                })?);
            }
            (Some(ip), true) => {
                listen.ips = vec![loopback_of(ip), ip];
                listen.lan = Some(ip);
            }
            (None, true) => {
                let ip = route_address().ok_or_else(|| {
                    ListenError::Environment("this machine is on no network".into())
                })?;
                if !is_private(ip) {
                    return Err(ListenError::Environment(format!(
                        "the address this machine reaches its network by, {ip}, is not a \
                         private one; name the interface to serve with `--host <ip> --lan`"
                    )));
                }
                listen.ips = vec![loopback_of(ip), ip];
                listen.lan = Some(ip);
            }
        }
        Ok(listen)
    }

    /// Whether a request's `Host` names an address the server listens at on
    /// `port`: an IP literal bound (any, when all of them are), or
    /// `localhost` when loopback is.
    fn serves(&self, host: &str, port: u16) -> bool {
        let Some((name, named_port)) = split_host(host) else {
            return false;
        };
        if named_port != port {
            return false;
        }
        if name.eq_ignore_ascii_case("localhost") {
            return self
                .ips
                .iter()
                .any(|ip| ip.is_loopback() || ip.is_unspecified());
        }
        let Ok(ip) = name.parse::<IpAddr>() else {
            return false;
        };
        self.ips
            .iter()
            .any(|bound| *bound == ip || bound.is_unspecified())
    }
}

/// A `Host` header's name and port (80 when it names none); IPv6 literals
/// are bracketed.
fn split_host(host: &str) -> Option<(&str, u16)> {
    if let Some(rest) = host.strip_prefix('[') {
        let (name, after) = rest.split_once(']')?;
        return match after {
            "" => Some((name, 80)),
            port => Some((name, port.strip_prefix(':')?.parse().ok()?)),
        };
    }
    match host.split_once(':') {
        None => Some((host, 80)),
        Some((name, port)) => Some((name, port.parse().ok()?)),
    }
}

/// The loopback address of `ip`'s family.
fn loopback_of(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(_) => IpAddr::V6(Ipv6Addr::LOCALHOST),
    }
}

/// The address this machine's route to a private network leaves from:
/// connecting a UDP socket picks the interface and sends nothing.
fn route_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(10, 254, 254, 254), 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_loopback() && !ip.is_unspecified()).then_some(ip)
}

/// Whether `ip` is a private, shared, link-local or unique-local address,
/// one no other network routes to.
fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            v4.is_private() || v4.is_link_local() || (a == 100 && (64..128).contains(&b))
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

/// What the server serves.
#[derive(Default)]
struct Served {
    /// The bundle's directory, once a build is served.
    bundle: Option<PathBuf>,
    /// The launch variables of the page.
    vars: Vec<(String, String)>,
    /// Whether the next page restores the app before it.
    restore: bool,
    /// The write halves of the open dev channels, and of the pages' own.
    sockets: Vec<Weak<Mutex<TcpStream>>>,
    pages: Vec<Weak<Mutex<TcpStream>>>,
}

/// The web dev server.
pub(crate) struct WebServer {
    listen: Arc<Listen>,
    port: u16,
    served: Arc<Mutex<Served>>,
}

/// What every connection is answered with.
struct Context {
    listen: Arc<Listen>,
    port: u16,
    served: Arc<Mutex<Served>>,
    expect: Expect,
    deliver: Deliver,
    log: PageLog,
}

impl WebServer {
    /// Binds every address of `listen` at its port, or at `configured` or
    /// the next port after it free on all of them, and serves each on a
    /// thread; a dev channel's connection is checked against `expect` and
    /// delivered to `deliver`, and what a page logs is handed to `log`.
    pub(crate) fn bind(
        listen: Listen,
        configured: u16,
        expect: Expect,
        deliver: Deliver,
        log: PageLog,
    ) -> io::Result<WebServer> {
        let (first, tries) = listen
            .port
            .map_or((configured, PORT_TRIES), |port| (port, 1));
        let mut last = None;
        let mut bound = None;
        for port in (0..tries).filter_map(|n| first.checked_add(n)) {
            // The first address picks the port when it is 0; the rest follow.
            let all = TcpListener::bind((listen.ips[0], port)).and_then(|first| {
                let port = first.local_addr()?.port();
                let mut listeners = vec![first];
                for ip in &listen.ips[1..] {
                    listeners.push(TcpListener::bind((*ip, port))?);
                }
                Ok((port, listeners))
            });
            match all {
                Ok(all) => {
                    bound = Some(all);
                    break;
                }
                Err(error) => last = Some(error),
            }
        }
        let Some((port, listeners)) = bound else {
            let error = last.unwrap_or_else(|| io::ErrorKind::AddrInUse.into());
            let at = if listen.port.is_some() {
                format!("port {first}")
            } else {
                format!(
                    "a port from {first} to {}",
                    first.saturating_add(PORT_TRIES - 1)
                )
            };
            return Err(io::Error::new(
                error.kind(),
                format!("cannot listen at {at}: {error}"),
            ));
        };
        let listen = Arc::new(listen);
        let served = Arc::new(Mutex::new(Served::default()));
        for listener in listeners {
            let cx = Arc::new(Context {
                listen: Arc::clone(&listen),
                port,
                served: Arc::clone(&served),
                expect: expect.clone(),
                deliver: Arc::clone(&deliver),
                log: Arc::clone(&log),
            });
            thread::Builder::new()
                .name("viso-web-serve".into())
                .spawn(move || {
                    for stream in listener.incoming().filter_map(Result::ok) {
                        let cx = Arc::clone(&cx);
                        let _ = thread::Builder::new()
                            .name("viso-web-request".into())
                            .spawn(move || respond(stream, &cx));
                    }
                })?;
        }
        Ok(WebServer {
            listen,
            port,
            served,
        })
    }

    /// The address the session's own browser loads the page at.
    pub(crate) fn addr(&self) -> SocketAddr {
        let ip = self.listen.ips[0];
        let ip = if ip.is_unspecified() {
            loopback_of(ip)
        } else {
            ip
        };
        SocketAddr::new(ip, self.port)
    }

    /// The page's address for the session's own browser.
    pub(crate) fn url(&self) -> String {
        format!("http://{}/", self.addr())
    }

    /// The address other devices load the page at, when the network was
    /// asked for.
    pub(crate) fn network_addr(&self) -> Option<SocketAddr> {
        self.listen.lan.map(|ip| SocketAddr::new(ip, self.port))
    }

    /// Serves the bundle in `dir` of the app `bin`, its page launched with
    /// `vars`, the next one restoring the app before it when `restore`.
    pub(crate) fn serve(&self, dir: PathBuf, vars: Vec<(String, String)>, restore: bool) {
        let mut served = lock(&self.served);
        served.bundle = Some(dir);
        served.vars = vars;
        served.restore = restore;
    }

    /// Ends every open page's dev channel with [`WEB_RELOAD_CLOSE`] and
    /// tells every page to reload, so it loads what is served now — a page
    /// whose app died before its dev channel opened too.
    pub(crate) fn reload_pages(&self) {
        let (sockets, pages) = {
            let mut served = lock(&self.served);
            (
                std::mem::take(&mut served.sockets),
                std::mem::take(&mut served.pages),
            )
        };
        for socket in sockets.iter().filter_map(Weak::upgrade) {
            let mut socket = lock(&socket);
            let mut payload = WEB_RELOAD_CLOSE.to_be_bytes().to_vec();
            payload.extend_from_slice(b"reload");
            let _ = write_ws_frame(&mut *socket, 0x8, &payload);
        }
        for page in pages.iter().filter_map(Weak::upgrade) {
            let _ = write_ws_frame(&mut *lock(&page), 0x1, b"reload");
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A request's head: its method, target and headers (names lowercased).
struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// Reads a request head, `None` for one that is not HTTP or too long.
fn read_head(reader: &mut impl BufRead) -> Option<Request> {
    let mut line = String::new();
    let mut read = 0;
    reader.read_line(&mut line).ok()?;
    read += line.len();
    let mut words = line.split_whitespace();
    let method = words.next()?.to_owned();
    let target = words.next()?.to_owned();
    let mut headers = Vec::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).ok()?;
        read += n;
        if n == 0 || read > MAX_HEAD {
            return None;
        }
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        let (name, value) = header.split_once(':')?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    Some(Request {
        method,
        target,
        headers,
    })
}

/// Answers one connection.
fn respond(stream: TcpStream, cx: &Context) {
    let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let Some(request) = read_head(&mut reader) else {
        return;
    };
    let mut stream = stream;
    let host = request.header("host").unwrap_or("");
    if !cx.listen.serves(host, cx.port) {
        let _ = answer(
            &mut stream,
            "421 Misdirected Request",
            "text/plain",
            "",
            b"",
            false,
        );
        return;
    }
    let path = web::request_path(&request.target).unwrap_or("");
    if format!("/{path}") == WEB_DEV_PATH {
        return dev_channel(stream, reader, &request, cx);
    }
    if format!("/{path}") == PAGE_PATH {
        return page_channel(stream, reader, &request, cx);
    }
    let served = &cx.served;
    if request.method != "GET" && request.method != "HEAD" {
        let _ = answer(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            "",
            b"",
            true,
        );
        return;
    }
    let head = request.method == "HEAD";
    let reply = {
        let mut served = lock(served);
        match &served.bundle {
            None => Reply::text(
                "503 Service Unavailable",
                "the app is still building; load the page again in a moment",
            ),
            Some(dir) if path.is_empty() || path == "index.html" => {
                match std::fs::read_to_string(dir.join("index.html")) {
                    Ok(page) => {
                        // The page's dev channel is at the address it was
                        // loaded from, not at one it is told.
                        let mut vars: Vec<_> = served
                            .vars
                            .iter()
                            .filter(|(name, _)| name != DEV_RUNTIME_ENV)
                            .cloned()
                            .collect();
                        // The restore is for the first page after the restart.
                        if std::mem::take(&mut served.restore) && !head {
                            vars.push((DEV_RESTORE_ENV.to_owned(), "1".to_owned()));
                        }
                        let launch = format!("{}{PAGE_SCRIPT}", launch_script(&vars));
                        Reply::page(&page, &launch, &crate::dev::random_hex())
                    }
                    Err(_) => Reply::text("404 Not Found", "the bundle has no page"),
                }
            }
            Some(dir) => match web::resolve(dir, &request.target) {
                Some(file) => Reply::file(&file, request.header("accept-encoding")),
                None => None,
            }
            .unwrap_or_else(|| Reply::text("404 Not Found", "not found")),
        }
    };
    let _ = answer(
        &mut stream,
        reply.status,
        reply.kind,
        &reply.headers,
        &reply.body,
        head,
    );
}

/// A response to a request for a file of the bundle.
struct Reply {
    status: &'static str,
    kind: &'static str,
    /// Its headers beyond the ones every response has, each ending in CRLF.
    headers: String,
    body: Vec<u8>,
}

impl Reply {
    fn text(status: &'static str, text: &str) -> Reply {
        Reply {
            status,
            kind: "text/plain; charset=utf-8",
            headers: String::new(),
            body: text.as_bytes().to_vec(),
        }
    }

    /// The bundle's page `page` with `launch` ahead of its `</head>`, every
    /// script of it carrying `nonce`, the only inline scripts it may run.
    fn page(page: &str, launch: &str, nonce: &str) -> Reply {
        let page = match page.find("</head>") {
            Some(at) => format!("{}{launch}{}", &page[..at], &page[at..]),
            None => format!("{launch}{page}"),
        };
        let page = page.replace("<script", &format!("<script nonce=\"{nonce}\""));
        Reply {
            status: "200 OK",
            kind: web::mime(std::path::Path::new("index.html")),
            headers: format!("Content-Security-Policy: {}\r\n", page_policy(nonce)),
            body: page.into_bytes(),
        }
    }

    /// The bundle file `file`, as the copy of it `accept` (the request's
    /// `Accept-Encoding`) takes when there is one; `None` when it cannot be
    /// read.
    fn file(file: &std::path::Path, accept: Option<&str>) -> Option<Reply> {
        let mut headers = String::new();
        let mut body = None;
        let mut copies = false;
        for (encoding, extension) in [("br", "br"), ("gzip", "gz")] {
            let mut name = file.as_os_str().to_owned();
            name.push(".");
            name.push(extension);
            let copy = std::path::PathBuf::from(name);
            if !copy.is_file() {
                continue;
            }
            copies = true;
            if body.is_none() && accepts(accept, encoding) {
                body = std::fs::read(&copy).ok();
                if body.is_some() {
                    headers.push_str(&format!("Content-Encoding: {encoding}\r\n"));
                }
            }
        }
        if copies {
            headers.push_str("Vary: Accept-Encoding\r\n");
        }
        let body = match body {
            Some(body) => body,
            None => std::fs::read(file).ok()?,
        };
        Some(Reply {
            status: "200 OK",
            kind: web::mime(file),
            headers,
            body,
        })
    }
}

/// The page's `Content-Security-Policy`: scripts from this origin and the
/// ones carrying `nonce`, WebAssembly compiled, no plugins, no other base,
/// framed by this origin only. What the app loads and connects to is its own.
fn page_policy(nonce: &str) -> String {
    format!(
        "script-src 'self' 'nonce-{nonce}' 'wasm-unsafe-eval'; object-src 'none'; \
         base-uri 'none'; frame-ancestors 'self'"
    )
}

/// Whether an `Accept-Encoding` of `accept` takes `encoding`: named, or by
/// `*`, with a weight above zero.
fn accepts(accept: Option<&str>, encoding: &str) -> bool {
    let mut any = false;
    for item in accept.unwrap_or("").split(',') {
        let mut parts = item.split(';');
        let name = parts.next().unwrap_or("").trim();
        let weight = parts
            .filter_map(|p| p.trim().strip_prefix("q="))
            .find_map(|q| q.trim().parse::<f32>().ok())
            .unwrap_or(1.0);
        if name.eq_ignore_ascii_case(encoding) {
            return weight > 0.0;
        }
        if name == "*" {
            any = weight > 0.0;
        }
    }
    any
}

/// Writes a response: every one is typed, not sniffed, not cached and kept
/// to this origin, with `headers` besides.
fn answer(
    stream: &mut TcpStream,
    status: &str,
    kind: &str,
    headers: &str,
    body: &[u8],
    head: bool,
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         X-Content-Type-Options: nosniff\r\nCross-Origin-Resource-Policy: same-origin\r\n\
         {headers}Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if !head {
        stream.write_all(body)?;
    }
    stream.flush()
}

/// The script that sets the page's launch variables before the app loads.
fn launch_script(vars: &[(String, String)]) -> String {
    let mut w = JsonWriter::new();
    w.begin_object();
    for (name, value) in vars {
        w.name(name);
        w.string(value);
    }
    w.end_object();
    // `<` cannot end the script from inside the object.
    let object = w.into_string().replace('<', "\\u003c");
    format!("<script>globalThis.visoLaunch = {object};</script>\n")
}

/// The script a page runs before the app: its console, uncaught errors and
/// rejections go to the session over the page's own channel (queued until it
/// opens, at most a thousand lines), and `reload` loads the page again. It
/// reconnects while the server runs.
const PAGE_SCRIPT: &str = r#"<script>(() => {
const queue = [];
let socket;
const text = (value) => {
  if (typeof value === "string") return value;
  if (value instanceof Error) return value.stack || String(value);
  try { return JSON.stringify(value); } catch { return String(value); }
};
const send = (level, values) => {
  const line = level + "\t" + values.map(text).join(" ");
  if (socket && socket.readyState === 1) socket.send(line);
  else if (queue.length < 1000) queue.push(line);
};
for (const [name, level] of [["log", "info"], ["info", "info"], ["debug", "info"], ["warn", "warn"], ["error", "error"]]) {
  const original = console[name].bind(console);
  console[name] = (...values) => { original(...values); send(level, values); };
}
addEventListener("error", (e) => send("error", [e.error || e.message]));
addEventListener("unhandledrejection", (e) => send("error", [e.reason]));
let wait = 250;
const open = () => {
  socket = new WebSocket("ws://" + location.host + "/__viso/page");
  socket.onopen = () => { wait = 250; for (const line of queue.splice(0)) socket.send(line); };
  socket.onmessage = (e) => { if (e.data === "reload") location.reload(); };
  socket.onclose = () => { socket = undefined; setTimeout(open, wait); wait = Math.min(wait * 2, 5000); };
};
open();
})();</script>
"#;

/// Answers a WebSocket upgrade from a page this server served — its
/// `Origin` the address the request names, which the server listens at; the
/// stream switched to it, or `None` when it was refused.
fn upgrade(mut stream: TcpStream, request: &Request) -> Option<TcpStream> {
    let origin_ok = request
        .header("origin")
        .zip(request.header("host"))
        .is_some_and(|(origin, host)| origin.strip_prefix("http://") == Some(host));
    let upgrade = request
        .header("upgrade")
        .is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    let key = request.header("sec-websocket-key");
    let (Some(key), true, true) = (key, origin_ok, upgrade) else {
        let _ = answer(&mut stream, "403 Forbidden", "text/plain", "", b"", false);
        return None;
    };
    let accept = base64(&sha1(format!("{key}{WEBSOCKET_GUID}").as_bytes()));
    let switched = write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\r\n"
    )
    .and_then(|()| stream.flush());
    if switched.is_err() || stream.set_read_timeout(None).is_err() {
        return None;
    }
    Some(stream)
}

/// Serves a page's own channel: each text message a line it logged.
fn page_channel(
    stream: TcpStream,
    mut reader: BufReader<TcpStream>,
    request: &Request,
    cx: &Context,
) {
    let Some(stream) = upgrade(stream, request) else {
        return;
    };
    let writer = Arc::new(Mutex::new(stream));
    lock(&cx.served).pages.push(Arc::downgrade(&writer));
    let log = &cx.log;
    while let Ok(Some((opcode, payload))) = read_ws_frame(&mut reader) {
        match opcode {
            0x1 => {
                if let Some((level, line)) = page_line(&payload)
                    && !log(level, line)
                {
                    return;
                }
            }
            0x9 if write_ws_frame(&mut *lock(&writer), 0xA, &payload).is_err() => return,
            0x8 => return,
            _ => {}
        }
    }
}

/// A page's line: its level (`info`, `warn` or `error`), a tab and its
/// text, cut to [`MAX_LOG`] bytes on a character boundary.
fn page_line(payload: &[u8]) -> Option<(&'static str, String)> {
    let text = std::str::from_utf8(payload).ok()?;
    let (level, line) = text.split_once('\t')?;
    let level = match level {
        "info" => "info",
        "warn" => "warn",
        "error" => "error",
        _ => return None,
    };
    let mut end = line.len().min(MAX_LOG);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    Some((level, line[..end].to_owned()))
}

/// Upgrades the connection to the dev channel's WebSocket when the page
/// asking is one this server served, and serves the channel on it.
fn dev_channel(stream: TcpStream, reader: BufReader<TcpStream>, request: &Request, cx: &Context) {
    let Some(stream) = upgrade(stream, request) else {
        return;
    };
    let writer = Arc::new(Mutex::new(stream));
    lock(&cx.served).sockets.push(Arc::downgrade(&writer));
    let (expect, deliver) = (&cx.expect, &cx.deliver);
    let socket = WebSocket {
        reader,
        writer,
        message: Vec::new(),
        at: 0,
    };
    link::serve(socket, expect, &**deliver);
}

/// A dev channel's WebSocket as the byte stream its frames travel on: what
/// arrives is the payload of each binary message in turn, and each write is
/// one binary message.
struct WebSocket {
    reader: BufReader<TcpStream>,
    writer: Arc<Mutex<TcpStream>>,
    /// The payload of the message being read, and how much of it was.
    message: Vec<u8>,
    at: usize,
}

/// The write half of a [`WebSocket`].
struct WebSocketWriter(Arc<Mutex<TcpStream>>);

impl Read for WebSocket {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.at == self.message.len() {
            self.message.clear();
            self.at = 0;
            let Some((opcode, payload)) = read_ws_frame(&mut self.reader)? else {
                return Ok(0);
            };
            match opcode {
                0x0 | 0x2 => self.message = payload,
                0x8 => return Ok(0),
                0x9 => write_ws_frame(&mut *lock(&self.writer), 0xA, &payload)?,
                0xA => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "a dev channel carries binary messages only",
                    ));
                }
            }
        }
        let n = out.len().min(self.message.len() - self.at);
        out[..n].copy_from_slice(&self.message[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

impl Write for WebSocket {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        write_ws_frame(&mut *lock(&self.writer), 0x2, buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        lock(&self.writer).flush()
    }
}

impl Write for WebSocketWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        write_ws_frame(&mut *lock(&self.0), 0x2, buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        lock(&self.0).flush()
    }
}

impl DevStream for WebSocket {
    type Writer = WebSocketWriter;

    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.reader.get_ref().set_read_timeout(timeout)
    }

    fn writer(&self) -> io::Result<WebSocketWriter> {
        Ok(WebSocketWriter(Arc::clone(&self.writer)))
    }
}

/// Reads one client frame: its opcode and unmasked payload; `None` at the
/// end of the stream. A client frame must be masked, and no larger than a
/// dev frame.
fn read_ws_frame(input: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 2];
    match input.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let invalid = |why: &str| io::Error::new(io::ErrorKind::InvalidData, why.to_owned());
    let opcode = head[0] & 0x0F;
    if head[1] & 0x80 == 0 {
        return Err(invalid("a client frame is not masked"));
    }
    let len = match head[1] & 0x7F {
        126 => {
            let mut len = [0u8; 2];
            input.read_exact(&mut len)?;
            u64::from(u16::from_be_bytes(len))
        }
        127 => {
            let mut len = [0u8; 8];
            input.read_exact(&mut len)?;
            u64::from_be_bytes(len)
        }
        len => u64::from(len),
    };
    if len > (MAX_FRAME + 4) as u64 {
        return Err(invalid("a frame larger than a dev frame"));
    }
    let mut mask = [0u8; 4];
    input.read_exact(&mut mask)?;
    let mut payload = vec![0u8; len as usize];
    input.read_exact(&mut payload)?;
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[i % 4];
    }
    Ok(Some((opcode, payload)))
}

/// Writes one server frame (unmasked, final) of `opcode` carrying `payload`.
fn write_ws_frame(out: &mut impl Write, opcode: u8, payload: &[u8]) -> io::Result<()> {
    let mut head = Vec::with_capacity(10);
    head.push(0x80 | opcode);
    match payload.len() {
        len @ 0..=125 => head.push(len as u8),
        len @ 126..=0xFFFF => {
            head.push(126);
            head.extend_from_slice(&(len as u16).to_be_bytes());
        }
        len => {
            head.push(127);
            head.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    out.write_all(&head)?;
    out.write_all(payload)
}

/// SHA-1 (FIPS 180-4) of `data`: the WebSocket handshake's hash.
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [
        0x6745_2301,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut message = data.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in message.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in block.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &word) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (h, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *h = h.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (chunk, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(h) {
        *chunk = word.to_be_bytes();
    }
    out
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Opens `url` in `browser` (`chrome`, `safari`, `firefox`), or in the
/// system's default browser.
pub(crate) fn open_browser(url: &str, browser: Option<&str>) -> Result<(), String> {
    let mut command = if cfg!(target_os = "macos") {
        let mut open = Command::new("open");
        if let Some(browser) = browser {
            let app = match browser {
                "chrome" => "Google Chrome",
                "safari" => "Safari",
                "firefox" => "Firefox",
                "edge" => "Microsoft Edge",
                other => other,
            };
            open.args(["-a", app]);
        }
        open.arg(url);
        open
    } else if cfg!(windows) {
        let mut start = Command::new("cmd");
        start.args(["/C", "start", ""]);
        match browser {
            Some("chrome") => start.arg("chrome"),
            Some("firefox") => start.arg("firefox"),
            Some("edge") => start.arg("msedge"),
            _ => &mut start,
        };
        start.arg(url);
        start
    } else {
        let mut open = Command::new(match browser {
            Some("chrome") => "google-chrome",
            Some("firefox") => "firefox",
            Some(other) => other,
            None => "xdg-open",
        });
        open.arg(url);
        open
    };
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
        .map_err(|e| format!("cannot open the browser: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_handshake_hashes_as_rfc_6455_says() {
        assert_eq!(
            base64(&sha1(b"abc")),
            base64(&[
                0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50,
                0xc2, 0x6c, 0x9c, 0xd0, 0xd8, 0x9d
            ])
        );
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        assert_eq!(
            base64(&sha1(format!("{key}{WEBSOCKET_GUID}").as_bytes())),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
        assert_eq!(base64(b"a"), "YQ==");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(base64(b""), "");
    }

    #[test]
    fn frames_round_trip_and_unmask() {
        let mut out = Vec::new();
        write_ws_frame(&mut out, 0x2, &[1, 2, 3]).unwrap();
        assert_eq!(out, [0x82, 3, 1, 2, 3]);
        let mut long = Vec::new();
        write_ws_frame(&mut long, 0x2, &[0; 300]).unwrap();
        assert_eq!(&long[..4], &[0x82, 126, 1, 44]);

        let mask = [9u8, 8, 7, 6];
        let payload = [1u8, 2, 3, 4, 5];
        let mut frame = vec![0x82, 0x80 | payload.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        let (opcode, read) = read_ws_frame(&mut &frame[..]).unwrap().unwrap();
        assert_eq!((opcode, read.as_slice()), (0x2, &payload[..]));
        assert!(read_ws_frame(&mut &[][..]).unwrap().is_none());
        assert!(read_ws_frame(&mut &[0x82, 1, 0][..]).is_err(), "unmasked");
    }

    #[test]
    fn an_encoding_is_taken_when_named_or_by_a_wildcard_with_weight() {
        assert!(accepts(Some("gzip, deflate, br, zstd"), "br"));
        assert!(accepts(Some("br;q=0.5"), "br"));
        assert!(!accepts(Some("br;q=0, gzip"), "br"));
        assert!(accepts(Some("*"), "gzip"));
        assert!(!accepts(Some("*, gzip;q=0"), "gzip"));
        assert!(!accepts(Some("identity"), "br"));
        assert!(!accepts(None, "gzip"));
    }

    #[test]
    fn the_page_runs_its_own_scripts_and_the_launch_before_the_app() {
        let page = "<html><head><title>a</title></head><body>\
                    <script type=\"module\">import init from \"./a.1.js\"; init();</script></body></html>";
        let reply = Reply::page(
            page,
            "<script>globalThis.visoLaunch = {};</script>\n",
            "n0nce",
        );
        let body = String::from_utf8(reply.body).unwrap();
        let launch = body.find("visoLaunch").unwrap();
        assert!(launch < body.find("</head>").unwrap());
        assert!(launch < body.find("./a.1.js").unwrap());
        assert_eq!(body.matches("<script nonce=\"n0nce\"").count(), 2, "{body}");
        assert_eq!(body.matches("<script").count(), 2);
        assert!(
            reply
                .headers
                .contains("script-src 'self' 'nonce-n0nce' 'wasm-unsafe-eval'"),
            "{}",
            reply.headers
        );
        assert!(reply.headers.contains("object-src 'none'"));
    }

    #[test]
    fn a_file_goes_as_the_copy_the_browser_takes() {
        let dir = std::env::temp_dir().join(format!("viso-serve-copies-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let module = dir.join("a_bg.1.wasm");
        std::fs::write(&module, b"\0asm plain").unwrap();
        std::fs::write(dir.join("a_bg.1.wasm.br"), b"brotli").unwrap();
        std::fs::write(dir.join("a_bg.1.wasm.gz"), b"gzip").unwrap();
        let reply = Reply::file(&module, Some("gzip, br")).unwrap();
        assert_eq!(reply.body, b"brotli");
        assert_eq!(reply.kind, "application/wasm");
        assert!(reply.headers.contains("Content-Encoding: br\r\n"));
        assert!(reply.headers.contains("Vary: Accept-Encoding\r\n"));
        let reply = Reply::file(&module, Some("gzip")).unwrap();
        assert_eq!(reply.body, b"gzip");
        let reply = Reply::file(&module, None).unwrap();
        assert_eq!(reply.body, b"\0asm plain");
        assert!(!reply.headers.contains("Content-Encoding"));
        assert!(reply.headers.contains("Vary: Accept-Encoding"));

        // A file with no copies varies by nothing.
        let page = dir.join("index.html");
        std::fs::write(&page, "<html></html>").unwrap();
        assert_eq!(Reply::file(&page, Some("br")).unwrap().headers, "");
        assert!(Reply::file(&dir.join("gone.js"), None).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_server_hands_out_the_bundles_page_and_files_with_their_headers() {
        use viso_view::dev::wire::{BuildId, DevSessionId, ProjectFingerprint, SchemaFingerprint};

        let dir = std::env::temp_dir().join(format!("viso-serve-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        web::write_page(&dir, "probe", "probe.0a.js", Some("probe_bg.1b.wasm")).unwrap();
        std::fs::write(dir.join("probe_bg.1b.wasm"), b"\0asm").unwrap();
        let expect = Expect {
            token: "token".into(),
            dev_session: DevSessionId(1),
            build_id: BuildId(2),
            project: ProjectFingerprint(3),
            schema: SchemaFingerprint(4),
        };
        let log: PageLog = Arc::new(|_, _| true);
        let server =
            WebServer::bind(Listen::loopback(), 0, expect, Arc::new(|_| true), log).unwrap();
        let get = |path: &str| {
            let mut stream = TcpStream::connect(server.addr()).unwrap();
            write!(
                stream,
                "GET {path} HTTP/1.1\r\nHost: {}\r\nAccept-Encoding: br\r\n\r\n",
                server.addr()
            )
            .unwrap();
            let mut response = String::new();
            let _ = stream.read_to_string(&mut response);
            response
        };
        assert!(get("/").starts_with("HTTP/1.1 503"), "nothing is built yet");
        server.serve(
            dir.clone(),
            vec![("VISO_DEV_TOKEN".into(), "token".into())],
            false,
        );
        let page = get("/");
        assert!(page.starts_with("HTTP/1.1 200 OK"), "{page}");
        for header in [
            "Content-Type: text/html; charset=utf-8",
            "X-Content-Type-Options: nosniff",
            "Cross-Origin-Resource-Policy: same-origin",
            "Content-Security-Policy: script-src 'self' 'nonce-",
            "Cache-Control: no-store",
        ] {
            assert!(page.contains(header), "{header}: {page}");
        }
        assert!(page.contains(r#"init({ module_or_path: "./probe_bg.1b.wasm" })"#));
        assert!(page.contains("\"VISO_DEV_TOKEN\":\"token\""));
        let module = get("/probe_bg.1b.wasm");
        assert!(
            module.contains("Content-Type: application/wasm"),
            "{module}"
        );
        assert!(module.contains("X-Content-Type-Options: nosniff"));
        assert!(!module.contains("Content-Security-Policy"));
        assert!(get("/missing.js").starts_with("HTTP/1.1 404"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_launch_script_cannot_end_its_element() {
        let script = launch_script(&[("VISO_DEV_TOKEN".into(), "</script>".into())]);
        assert!(script.starts_with("<script>globalThis.visoLaunch = {\"VISO_DEV_TOKEN\":"));
        assert_eq!(script.matches("</script>").count(), 1);
    }

    #[test]
    fn a_page_line_is_a_level_and_its_text_cut_to_the_log_bound() {
        assert_eq!(
            page_line(b"warn\tslow frame"),
            Some(("warn", "slow frame".to_owned()))
        );
        assert_eq!(
            page_line(b"error\ta\tb"),
            Some(("error", "a\tb".to_owned()))
        );
        assert_eq!(page_line(b"trace\tx"), None);
        assert_eq!(page_line(b"no tab"), None);
        assert_eq!(page_line(&[b'i', b'n', b'f', b'o', b'\t', 0xFF]), None);
        let long = format!("info\t{}", "é".repeat(MAX_LOG));
        let (_, cut) = page_line(long.as_bytes()).unwrap();
        assert!(cut.len() <= MAX_LOG && cut.chars().all(|c| c == 'é'));
    }

    #[test]
    fn the_page_script_opens_the_page_channel() {
        assert!(PAGE_SCRIPT.contains(PAGE_PATH));
        assert_eq!(PAGE_SCRIPT.matches("</script>").count(), 1);
    }

    /// A page's own channel opened on `server` from `origin`: the stream
    /// switched to it, or `None` when it was refused.
    fn open_page(server: &WebServer, origin: &str) -> Option<TcpStream> {
        let mut stream = TcpStream::connect(server.addr()).unwrap();
        write!(
            stream,
            "GET {PAGE_PATH} HTTP/1.1\r\nHost: {}\r\nOrigin: {origin}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            server.addr()
        )
        .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut status = String::new();
        reader.read_line(&mut status).unwrap();
        if !status.contains(" 101 ") {
            return None;
        }
        let mut line = String::new();
        while reader.read_line(&mut line).unwrap() > 2 {
            line.clear();
        }
        Some(stream)
    }

    #[test]
    fn the_network_is_served_only_when_asked_for() {
        let v4 = |a, b, c, d| IpAddr::V4(Ipv4Addr::new(a, b, c, d));
        let local = Listen::new(None, false, None).unwrap();
        assert_eq!(local, Listen::loopback());
        assert_eq!(local.lan, None);
        assert!(matches!(
            Listen::new(Some(v4(192, 168, 1, 5)), false, None),
            Err(ListenError::Usage(_))
        ));
        assert!(matches!(
            Listen::new(Some(v4(127, 0, 0, 1)), true, None),
            Err(ListenError::Usage(_))
        ));
        let lan = Listen::new(Some(v4(192, 168, 1, 5)), true, Some(9000)).unwrap();
        assert_eq!(lan.ips, [v4(127, 0, 0, 1), v4(192, 168, 1, 5)]);
        assert_eq!((lan.port, lan.lan), (Some(9000), Some(v4(192, 168, 1, 5))));

        assert!(lan.serves("127.0.0.1:9000", 9000));
        assert!(lan.serves("localhost:9000", 9000));
        assert!(lan.serves("192.168.1.5:9000", 9000));
        assert!(
            !lan.serves("192.168.1.6:9000", 9000),
            "an address not bound"
        );
        assert!(!lan.serves("192.168.1.5:9001", 9000), "another port");
        assert!(!lan.serves("dev.example:9000", 9000), "a name");
        assert!(!lan.serves("", 9000));
        assert!(
            !local.serves("[::1]:8000", 8000),
            "IPv6 loopback is not bound"
        );
        let mut any = Listen::loopback();
        any.ips = vec![IpAddr::V6(Ipv6Addr::UNSPECIFIED)];
        assert!(any.serves("[fd00::1]", 80) && any.serves("10.0.0.2", 80));
        assert!(!any.serves("rebound.example", 80));
    }

    #[test]
    fn private_addresses_are_the_ones_no_other_network_routes_to() {
        for private in [
            "10.1.2.3",
            "172.16.0.1",
            "192.168.0.9",
            "169.254.1.1",
            "100.64.0.1",
            "fd12::1",
            "fe80::1",
        ] {
            assert!(is_private(private.parse().unwrap()), "{private}");
        }
        for public in ["8.8.8.8", "172.32.0.1", "100.128.0.1", "2001:db8::1"] {
            assert!(!is_private(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn a_page_logs_through_its_channel_and_reloads_on_a_new_build() {
        use viso_view::dev::wire::{BuildId, DevSessionId, ProjectFingerprint, SchemaFingerprint};

        let expect = Expect {
            token: "token".into(),
            dev_session: DevSessionId(1),
            build_id: BuildId(2),
            project: ProjectFingerprint(3),
            schema: SchemaFingerprint(4),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let log: PageLog = Arc::new(move |level, line| tx.send((level, line)).is_ok());
        let server =
            WebServer::bind(Listen::loopback(), 0, expect, Arc::new(|_| true), log).unwrap();

        assert!(
            open_page(&server, "http://example.com").is_none(),
            "another origin"
        );
        let mut rebound = TcpStream::connect(server.addr()).unwrap();
        write!(
            rebound,
            "GET / HTTP/1.1\r\nHost: rebound.example:{}\r\n\r\n",
            server.addr().port()
        )
        .unwrap();
        let mut status = String::new();
        BufReader::new(rebound).read_line(&mut status).unwrap();
        assert!(status.contains(" 421 "), "a name, not an address: {status}");
        let mut page = open_page(&server, &format!("http://{}", server.addr())).expect("it opens");

        let line = b"error\tpanicked at src/main.rs";
        let mask = [1u8, 2, 3, 4];
        let mut frame = vec![0x81, 0x80 | line.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(line.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        page.write_all(&frame).unwrap();
        let logged = rx.recv_timeout(Duration::from_secs(5)).expect("it logs");
        assert_eq!(logged, ("error", "panicked at src/main.rs".to_owned()));

        server.reload_pages();
        page.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut reload = [0u8; 8];
        page.read_exact(&mut reload).unwrap();
        assert_eq!(reload, *b"\x81\x06reload");
    }

    /// A page in a headless Chrome: its console and uncaught error reach the
    /// log, and a new build reloads it although its app never opened a dev
    /// channel.
    #[test]
    #[ignore = "opens the page in a headless Chrome"]
    fn a_browser_page_forwards_its_console_and_reloads_without_an_app() {
        use viso_view::dev::wire::{BuildId, DevSessionId, ProjectFingerprint, SchemaFingerprint};

        let dir = std::env::temp_dir().join(format!("viso-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("chrome")).unwrap();
        // An app that logs, then dies before it opens a dev channel.
        std::fs::write(
            dir.join("probe.js"),
            "export default function init() { console.warn(\"page\", { n: 1 }); \
             throw new Error(\"boom\"); }",
        )
        .unwrap();
        let expect = Expect {
            token: "token".into(),
            dev_session: DevSessionId(1),
            build_id: BuildId(2),
            project: ProjectFingerprint(3),
            schema: SchemaFingerprint(4),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let log: PageLog = Arc::new(move |level, line| tx.send((level, line)).is_ok());
        let server =
            WebServer::bind(Listen::loopback(), 0, expect, Arc::new(|_| true), log).unwrap();
        web::write_index(&dir, "probe").unwrap();
        server.serve(dir.clone(), Vec::new(), false);

        let chrome = std::env::var_os("VISO_E2E_CHROME").map_or_else(
            || {
                if cfg!(target_os = "macos") {
                    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome".into()
                } else {
                    "google-chrome".into()
                }
            },
            PathBuf::from,
        );
        let mut page = Command::new(chrome)
            .args(["--headless=new", "--no-first-run"])
            .arg(format!("--user-data-dir={}", dir.join("chrome").display()))
            .arg(server.url())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("Chrome runs");
        let next = || rx.recv_timeout(Duration::from_secs(60)).expect("a line");
        let lines = [next(), next()];
        server.reload_pages();
        let again = next();
        let _ = page.kill();
        let _ = page.wait();
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(lines[0], ("warn", "page {\"n\":1}".to_owned()));
        assert_eq!(lines[1].0, "error");
        assert!(lines[1].1.contains("boom"), "{:?}", lines[1]);
        assert_eq!(again, lines[0], "the page loaded again");
    }

    #[test]
    fn a_request_head_reads_its_headers() {
        let text = "GET /__viso/dev HTTP/1.1\r\nHost: 127.0.0.1:8000\r\nUpgrade: websocket\r\n\r\n";
        let request = read_head(&mut text.as_bytes()).unwrap();
        assert_eq!((&*request.method, &*request.target), ("GET", "/__viso/dev"));
        assert_eq!(request.header("upgrade"), Some("websocket"));
        assert!(read_head(&mut "GET /\r\n".as_bytes()).is_none());
    }
}
