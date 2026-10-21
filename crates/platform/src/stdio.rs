//! The app's standard output and error.
//!
//! Native targets write [`std::io::stdout`] and [`std::io::stderr`] as
//! `print!` and `eprint!` do. `wasm32-unknown-unknown` has no stream behind
//! `std` (what it prints is dropped), so on the web each line goes to the
//! page's console instead — output to `console.log`, errors to
//! `console.error` — where the browser's devtools and a dev session read
//! it. A line is written whole once its newline arrives.

use std::fmt;

/// Which of the two streams a write goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stream {
    /// Standard output: `console.log` on the web.
    Out,
    /// Standard error: `console.error` on the web.
    Err,
}

/// Writes `args` to `stream`, as `print!`/`eprint!` would where `std` has a
/// stream.
///
/// # Panics
///
/// Natively, when writing the stream fails, as `print!` does.
pub fn write(stream: Stream, args: fmt::Arguments<'_>) {
    imp::write(stream, args);
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod imp {
    use std::fmt;

    use super::Stream;

    pub(super) fn write(stream: Stream, args: fmt::Arguments<'_>) {
        match stream {
            Stream::Out => print!("{args}"),
            Stream::Err => eprint!("{args}"),
        }
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod imp {
    use std::cell::RefCell;
    use std::fmt::{self, Write};

    use wasm_bindgen::JsValue;

    use super::Stream;

    thread_local! {
        /// What each stream holds past its last newline.
        static PENDING: RefCell<[String; 2]> = const { RefCell::new([String::new(), String::new()]) };
    }

    pub(super) fn write(stream: Stream, args: fmt::Arguments<'_>) {
        PENDING.with(|pending| {
            let Ok(mut pending) = pending.try_borrow_mut() else {
                // A value's `Display` printing as it is printed: write it alone.
                line(stream, &args.to_string());
                return;
            };
            let held = &mut pending[stream as usize];
            let _ = held.write_fmt(args);
            super::take_lines(held, |text| line(stream, text));
        });
    }

    fn line(stream: Stream, text: &str) {
        let text = JsValue::from_str(text);
        match stream {
            Stream::Out => web_sys::console::log_1(&text),
            Stream::Err => web_sys::console::error_1(&text),
        }
    }
}

/// Hands `emit` each whole line `held` holds, without its newline, and keeps
/// what follows the last one.
#[cfg_attr(
    not(all(target_arch = "wasm32", target_os = "unknown")),
    allow(dead_code)
)]
fn take_lines(held: &mut String, mut emit: impl FnMut(&str)) {
    let Some(end) = held.rfind('\n') else {
        return;
    };
    held[..end].split('\n').for_each(&mut emit);
    held.drain(..=end);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(writes: &[&str]) -> (Vec<String>, String) {
        let (mut held, mut out) = (String::new(), Vec::new());
        for write in writes {
            held.push_str(write);
            take_lines(&mut held, |line| out.push(line.to_owned()));
        }
        (out, held)
    }

    #[test]
    fn a_line_is_written_whole_once_its_newline_arrives() {
        assert_eq!(lines(&["a", "b"]), (vec![], "ab".to_owned()));
        assert_eq!(
            lines(&["a", "b\nc\n\nd"]),
            (vec!["ab".into(), "c".into(), "".into()], "d".to_owned())
        );
        assert_eq!(lines(&["\n"]), (vec!["".into()], String::new()));
    }
}
