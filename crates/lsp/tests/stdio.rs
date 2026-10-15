//! The `viso-lsp` binary over its stdio: framed replies on stdout and the
//! exit code the protocol gives each way the session ends.

use std::io::Write;
use std::process::{Command, Stdio};

fn frame(body: &str) -> String {
    format!("Content-Length: {}\r\n\r\n{body}", body.len())
}

/// Runs the server on `messages` and returns its stdout and exit code.
fn session(messages: &[&str]) -> (String, i32) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_viso-lsp"))
        .arg("--stdio")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start viso-lsp");
    let mut stdin = child.stdin.take().unwrap();
    for message in messages {
        stdin.write_all(frame(message).as_bytes()).unwrap();
    }
    drop(stdin);
    let output = child.wait_with_output().unwrap();
    (
        String::from_utf8(output.stdout).unwrap(),
        output.status.code().unwrap(),
    )
}

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
const SHUTDOWN: &str = r#"{"jsonrpc":"2.0","id":2,"method":"shutdown"}"#;
const EXIT: &str = r#"{"jsonrpc":"2.0","method":"exit"}"#;

#[test]
fn shutdown_then_exit_ends_cleanly() {
    let (stdout, code) = session(&[INITIALIZE, SHUTDOWN, EXIT]);
    assert_eq!(code, 0, "{stdout}");
    assert!(stdout.starts_with("Content-Length: "), "{stdout}");
    assert!(stdout.contains(r#""hoverProvider":true"#), "{stdout}");
    assert!(
        stdout.contains(r#""id":2,"jsonrpc":"2.0","result":null"#),
        "{stdout}"
    );
}

#[test]
fn exit_without_shutdown_or_no_exit_fails() {
    assert_eq!(session(&[INITIALIZE, EXIT]).1, 1);
    assert_eq!(session(&[INITIALIZE]).1, 1);
}
