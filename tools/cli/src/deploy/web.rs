//! The web dev server (`Viso_CLI.md`, `Viso_Hot_Reload.md`): it serves the dev build's web
//! bundle on the loopback interface and carries the page's dev channel as a
//! WebSocket at [`WEB_DEV_PATH`].
//!
//! The page that loads the app is written per request: the bundle's page
//! with the launch variables (`visoLaunch`) the app reads before it starts —
//! the dev channel's address and the session's identity, and, once after a
//! warm restart, the restore. A WebSocket is accepted only from a page this
//! server served (its `Origin`), and the dev channel behind it checks the
//! session token as the TCP channel does. Once a newer build is served, every
//! open page's channel is closed with [`WEB_RELOAD_CLOSE`] and the page loads
//! it.
//!
//! Each connection is served on a thread of its own; nothing is cached by the
//! browser (`Cache-Control: no-store`).

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::Duration;

use viso_ende::JsonWriter;
use viso_package::web;
use viso_view::dev::wire::{DEV_RESTORE_ENV, MAX_FRAME, WEB_DEV_PATH, WEB_RELOAD_CLOSE};

use crate::dev::link::{self, Deliver, DevStream, Expect};

/// The port tried first, and how many after it.
pub(crate) const DEFAULT_PORT: u16 = 8000;
const PORT_TRIES: u16 = 100;

/// How long a request has to arrive whole, and the most a request head may
/// hold.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_HEAD: usize = 16 << 10;

/// The GUID RFC 6455 hashes a WebSocket key with.
const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

/// What the server serves.
#[derive(Default)]
struct Served {
    /// The bundle's directory and the app's name, once a build is served.
    bundle: Option<(PathBuf, String)>,
    /// The launch variables of the page.
    vars: Vec<(&'static str, String)>,
    /// Whether the next page restores the app before it.
    restore: bool,
    /// The write halves of the open dev channels.
    sockets: Vec<Weak<Mutex<TcpStream>>>,
}

/// The web dev server.
pub(crate) struct WebServer {
    addr: SocketAddr,
    served: Arc<Mutex<Served>>,
}

impl WebServer {
    /// Binds the loopback interface at `port`, or the next free port after
    /// it, and serves on a thread; a dev channel's connection is checked
    /// against `expect` and delivered to `deliver`.
    pub(crate) fn bind(port: u16, expect: Expect, deliver: Deliver) -> io::Result<WebServer> {
        let listener = (0..PORT_TRIES)
            .filter_map(|n| port.checked_add(n))
            .find_map(|port| TcpListener::bind((Ipv4Addr::LOCALHOST, port)).ok())
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!(
                        "no free port from {port} to {}",
                        port.saturating_add(PORT_TRIES - 1)
                    ),
                )
            })?;
        let addr = listener.local_addr()?;
        let served = Arc::new(Mutex::new(Served::default()));
        let state = Arc::clone(&served);
        thread::Builder::new()
            .name("viso-web-serve".into())
            .spawn(move || {
                for stream in listener.incoming().filter_map(Result::ok) {
                    let state = Arc::clone(&state);
                    let expect = expect.clone();
                    let deliver = Arc::clone(&deliver);
                    let _ = thread::Builder::new()
                        .name("viso-web-request".into())
                        .spawn(move || respond(stream, addr, &state, &expect, &deliver));
                }
            })?;
        Ok(WebServer { addr, served })
    }

    /// Where it listens.
    pub(crate) fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The page's address.
    pub(crate) fn url(&self) -> String {
        format!("http://{}/", self.addr)
    }

    /// Serves the bundle in `dir` of the app `bin`, its page launched with
    /// `vars`, the next one restoring the app before it when `restore`.
    pub(crate) fn serve(
        &self,
        dir: PathBuf,
        bin: String,
        vars: Vec<(&'static str, String)>,
        restore: bool,
    ) {
        let mut served = lock(&self.served);
        served.bundle = Some((dir, bin));
        served.vars = vars;
        served.restore = restore;
    }

    /// Ends every open page's dev channel with [`WEB_RELOAD_CLOSE`], so it
    /// loads what is served now.
    pub(crate) fn reload_pages(&self) {
        let sockets = std::mem::take(&mut lock(&self.served).sockets);
        for socket in sockets.iter().filter_map(Weak::upgrade) {
            let mut socket = lock(&socket);
            let mut payload = WEB_RELOAD_CLOSE.to_be_bytes().to_vec();
            payload.extend_from_slice(b"reload");
            let _ = write_ws_frame(&mut *socket, 0x8, &payload);
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
fn respond(
    stream: TcpStream,
    addr: SocketAddr,
    served: &Arc<Mutex<Served>>,
    expect: &Expect,
    deliver: &Deliver,
) {
    let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let Some(request) = read_head(&mut reader) else {
        return;
    };
    let mut stream = stream;
    let path = web::request_path(&request.target).unwrap_or("");
    if format!("/{path}") == WEB_DEV_PATH {
        return dev_channel(stream, reader, &request, addr, served, expect, deliver);
    }
    if request.method != "GET" && request.method != "HEAD" {
        let _ = answer(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain",
            b"",
            true,
        );
        return;
    }
    let head = request.method == "HEAD";
    let (status, kind, body) = {
        let mut served = lock(served);
        match &served.bundle {
            None => (
                "503 Service Unavailable",
                "text/plain; charset=utf-8",
                b"the app is still building; load the page again in a moment".to_vec(),
            ),
            Some((_, bin)) if path.is_empty() || path == "index.html" => {
                let bin = bin.clone();
                let mut vars = served.vars.clone();
                // The restore is for the first page after the restart.
                if std::mem::take(&mut served.restore) && !head {
                    vars.push((DEV_RESTORE_ENV, "1".to_owned()));
                }
                let page = web::index_html(&bin, &launch_script(&vars));
                (
                    "200 OK",
                    web::mime(std::path::Path::new("index.html")),
                    page.into_bytes(),
                )
            }
            Some((dir, _)) => match web::resolve(dir, &request.target)
                .and_then(|file| std::fs::read(&file).ok().map(|body| (file, body)))
            {
                Some((file, body)) => ("200 OK", web::mime(&file), body),
                None => ("404 Not Found", "text/plain", b"not found".to_vec()),
            },
        }
    };
    let _ = answer(&mut stream, status, kind, &body, head);
}

/// Writes a response.
fn answer(
    stream: &mut TcpStream,
    status: &str,
    kind: &str,
    body: &[u8],
    head: bool,
) -> io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    if !head {
        stream.write_all(body)?;
    }
    stream.flush()
}

/// The script that sets the page's launch variables before the app loads.
fn launch_script(vars: &[(&str, String)]) -> String {
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

/// Upgrades the connection to the dev channel's WebSocket when the page
/// asking is one this server served, and serves the channel on it.
fn dev_channel(
    mut stream: TcpStream,
    reader: BufReader<TcpStream>,
    request: &Request,
    addr: SocketAddr,
    served: &Arc<Mutex<Served>>,
    expect: &Expect,
    deliver: &Deliver,
) {
    let origin_ok = request.header("origin").is_some_and(|origin| {
        origin == format!("http://{addr}") || origin == format!("http://localhost:{}", addr.port())
    });
    let upgrade = request
        .header("upgrade")
        .is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
    let key = request.header("sec-websocket-key");
    let (Some(key), true, true) = (key, origin_ok, upgrade) else {
        let _ = answer(&mut stream, "403 Forbidden", "text/plain", b"", false);
        return;
    };
    let accept = base64(&sha1(format!("{key}{WEBSOCKET_GUID}").as_bytes()));
    let switched = write!(
        stream,
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {accept}\r\n\r\n"
    )
    .and_then(|()| stream.flush());
    if switched.is_err() || stream.set_read_timeout(None).is_err() {
        return;
    }
    let writer = Arc::new(Mutex::new(stream));
    lock(served).sockets.push(Arc::downgrade(&writer));
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
    fn the_launch_script_cannot_end_its_element() {
        let script = launch_script(&[("VISO_DEV_TOKEN", "</script>".into())]);
        assert!(script.starts_with("<script>globalThis.visoLaunch = {\"VISO_DEV_TOKEN\":"));
        assert_eq!(script.matches("</script>").count(), 1);
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
