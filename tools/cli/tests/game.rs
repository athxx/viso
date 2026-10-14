//! `viso game` end to end: the session found by its lock, the request sent
//! to the control endpoint the lock names, the answer printed or the tape
//! written, and the failures when no session runs.

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use viso_ende::Encoder;

struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("viso-cli-ask-{label}-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("Viso.toml"),
            "[package]\nname = \"demo\"\nlanguage = \"1.0\"\n",
        )
        .unwrap();
        Self(dir)
    }

    /// Writes the dev session lock, naming `endpoint` when given.
    fn lock(&self, endpoint: Option<&str>) {
        let dir = self.0.join("target/viso/locks");
        fs::create_dir_all(&dir).unwrap();
        let mut text = format!("{}\n0\ndev session for `host`\n", std::process::id());
        if let Some(endpoint) = endpoint {
            text.push_str(endpoint);
            text.push('\n');
        }
        fs::write(dir.join("dev-host.lock"), text).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn viso(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_viso"))
        .args(args)
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).unwrap()
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).unwrap()
}

/// A session's control endpoint answering one request with `reply`, a
/// reply body; the request body it read.
fn session(reply: Vec<u8>) -> (String, thread::JoinHandle<Vec<u8>>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let endpoint = format!("{} secret", listener.local_addr().unwrap());
    let served = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut len = [0; 4];
        stream.read_exact(&mut len).unwrap();
        let mut request = vec![0; u32::from_le_bytes(len) as usize];
        stream.read_exact(&mut request).unwrap();
        stream
            .write_all(&(reply.len() as u32).to_le_bytes())
            .unwrap();
        stream.write_all(&reply).unwrap();
        request
    });
    (endpoint, served)
}

#[test]
fn without_a_session_there_is_nothing_to_ask() {
    let s = Scratch::new("none");
    let out = viso(&s.0, &["game", "peek", "main", "Player.score"]);
    assert_eq!(code(&out), 3);
    assert!(
        stderr(&out).contains("no `viso run` session is running for this project"),
        "{}",
        stderr(&out)
    );

    s.lock(None);
    let out = viso(
        &s.0,
        &["game", "record", "main", "--output", "run.tape", "--json"],
    );
    assert_eq!(code(&out), 3);
    assert!(
        stdout(&out).contains("\"code\":\"ENV_NO_DEV_SESSION\""),
        "{}",
        stdout(&out)
    );

    let out = viso(&s.0, &["game", "peek", "main", "score"]);
    assert_eq!(code(&out), 2, "not `System.state`");
}

#[test]
fn a_peek_prints_the_state_the_session_answers() {
    let s = Scratch::new("peek");
    let mut reply = Encoder::new();
    reply.write_u8(0);
    reply.write_bytes(b"src/main.vs");
    reply.write_bytes(br#"{"hp":3}"#);
    reply.write_varint(600);
    reply.write_varint(1);
    let (endpoint, served) = session(reply.into_bytes());
    s.lock(Some(&endpoint));
    let out = viso(&s.0, &["game", "peek", "main", "Player.stats", "--json"]);
    assert_eq!(code(&out), 0, "{}", stdout(&out));
    assert!(
        stdout(&out).contains(
            r#""payload":{"game":"src/main.vs","system":"Player","state":"stats","value":{"hp":3},"tick":600,"sessions":1}"#
        ),
        "{}",
        stdout(&out)
    );
    let request = served.join().unwrap();
    let text = String::from_utf8_lossy(&request);
    assert!(text.contains("secret") && text.contains("Player"), "{text}");
}

#[test]
fn a_recording_without_patches_is_written_as_text() {
    let s = Scratch::new("record");
    let tape = viso_dsl::scenario::read_tape(b"seed 7\nticks 3\n1: press jump\n", 0, 60).unwrap();
    let mut reply = Encoder::new();
    reply.write_u8(1);
    reply.write_bytes(b"src/main.vs");
    reply.write_bytes(&tape.encode());
    let (endpoint, served) = session(reply.into_bytes());
    s.lock(Some(&endpoint));
    let out = viso(
        &s.0,
        &[
            "game",
            "record",
            "main",
            "--output",
            "runs/jump.tape",
            "--ticks",
            "3",
        ],
    );
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    assert_eq!(
        stdout(&out),
        "recorded 3 ticks of src/main.vs to runs/jump.tape (text)\n"
    );
    let written = fs::read_to_string(s.0.join("runs/jump.tape")).unwrap();
    assert!(
        written.contains("seed 7") && written.contains("1: press jump"),
        "{written}"
    );
    served.join().unwrap();
}

#[test]
fn a_refusal_is_reported_under_its_code() {
    let s = Scratch::new("refused");
    let mut reply = Encoder::new();
    reply.write_u8(2);
    reply.write_bytes(b"GAME_UNKNOWN_STATE");
    reply.write_bytes(b"the game of `src/main.vs` has no state `Player.speed`");
    reply.write_varint(1);
    reply.write_bytes(b"`Player` has `jumps`");
    let (endpoint, served) = session(reply.into_bytes());
    s.lock(Some(&endpoint));
    let out = viso(&s.0, &["game", "peek", "main", "Player.speed"]);
    assert_eq!(code(&out), 1);
    let err = stderr(&out);
    assert!(
        err.contains("GAME_UNKNOWN_STATE") && err.contains("`Player` has `jumps`"),
        "{err}"
    );
    served.join().unwrap();
}
