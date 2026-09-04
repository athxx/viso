//! The `viso-lsp` stdio language server: a synchronous `Content-Length`-framed
//! JSON-RPC loop over stdin/stdout.
//!
//! This is the thin transport at the very top of the crate's two-layer design. It
//! owns no analysis logic and **no async runtime**: it reads one framed message,
//! hands it to [`Server::handle`](viso_lsp::server::Server::handle), writes the reply
//! and any diagnostics, and repeats until the client sends `exit`. Every real
//! decision lives in the engine below it; a frontend tool adapts a protocol, it does
//! not host an executor (AGENTS 25).
//!
//! Cold-path throughout (AGENTS 7.2): a handful of messages per editor keystroke.

use std::io::{self, BufReader, BufWriter};

use viso_lsp::rpc;
use viso_lsp::server::Server;

fn main() -> io::Result<()> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin.lock());
    // `rpc::write_message` flushes after each message, so the client sees replies
    // immediately rather than waiting for the buffer to fill.
    let mut writer = BufWriter::new(stdout.lock());
    let mut server = Server::new();

    // Read framed messages until the stream ends or the client sends `exit`.
    while let Some(message) = rpc::read_message(&mut reader)? {
        let (out, should_exit) = server.handle(&message);
        if let Some(reply) = out.reply {
            rpc::write_message(&mut writer, &reply)?;
        }
        for notification in out.notifications {
            rpc::write_message(&mut writer, &notification)?;
        }
        if should_exit {
            break;
        }
    }
    Ok(())
}
