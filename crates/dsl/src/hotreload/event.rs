//! The record of one hot reload attempt the app compiled itself
//! (Viso_Hot_Reload.md sections 37.1 and 51; Viso_CLI.md section 36.4).
//!
//! Every staged edit of a `.vs` file yields one [`ReloadEvent`], applied or
//! rejected: its revisions, outcome, the stage it ended at, how long it took,
//! what the commit kept and lost, and its diagnostics with the candidate
//! source their spans index into. The app shows it in-app and, when launched by
//! `viso run`, sends its bytes ([`ReloadEvent::to_bytes`]) as the dev channel's
//! in-app reload report (`viso_view::dev::wire`); the CLI renders it as a `dev`
//! event and `diagnostic` events. It goes when the host compiles and the app
//! only applies patches.
//!
//! Decoding is bounded: a count larger than the bytes left or a malformed value
//! is rejected without allocating for it.

use std::sync::{Mutex, OnceLock};

use viso_ende::{Decode, DecodeError, Decoder, Encode, Encoder};

use crate::diag::{Applicability, Diagnostic, Fix, Related, Severity, TextEdit};
use crate::syntax::{TextRange, TextSize};

/// How a reload attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadOutcome {
    /// Committed with every state, focus and scroll offset kept.
    Applied,
    /// Committed, but a kept state was reset to its initializer, or a node lost
    /// its focus, scroll offset or handlers.
    ScopedReset,
    /// The candidate did not compile; every mount kept its last-good revision.
    Rejected,
}

impl ReloadOutcome {
    /// The `dev` event spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ReloadOutcome::Applied => "applied",
            ReloadOutcome::ScopedReset => "scoped_reset",
            ReloadOutcome::Rejected => "rejected",
        }
    }

    /// The patch class of a committed candidate; a rejected one has none.
    pub fn patch_class(self) -> Option<&'static str> {
        match self {
            ReloadOutcome::Applied => Some("PATCH"),
            ReloadOutcome::ScopedReset => Some("PATCH_WITH_SCOPED_RESET"),
            ReloadOutcome::Rejected => None,
        }
    }
}

/// The stage a reload attempt ended at (Viso_Hot_Reload.md section 51).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadStage {
    Parse,
    Resolve,
    Typecheck,
    Capability,
    StateCompat,
    ShaderCompile,
    /// The candidate committed.
    RuntimeCommit,
}

impl ReloadStage {
    /// The section 51 spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ReloadStage::Parse => "parse",
            ReloadStage::Resolve => "resolve",
            ReloadStage::Typecheck => "typecheck",
            ReloadStage::Capability => "capability",
            ReloadStage::StateCompat => "state-compat",
            ReloadStage::ShaderCompile => "shader-compile",
            ReloadStage::RuntimeCommit => "runtime-commit",
        }
    }

    /// The stage a candidate rejected with `diagnostics` failed at: that of its
    /// first error, by the code ranges of Viso_DSL_1.0.md appendix C.
    pub fn of_rejection(diagnostics: &[Diagnostic]) -> Self {
        let Some(code) = diagnostics
            .iter()
            .find(|d| d.severity == Severity::Error)
            .map(|d| d.code)
        else {
            return ReloadStage::Typecheck;
        };
        match code.as_bytes() {
            [b'E', b'1', ..] => ReloadStage::Parse,
            [b'E', b'2', b'0', ..] => ReloadStage::Resolve,
            [b'E', b'2', b'6', ..] | [b'E', b'6', ..] => ReloadStage::Capability,
            [b'E', b'5', ..] => ReloadStage::StateCompat,
            [b'E', b'8', ..] => ReloadStage::ShaderCompile,
            _ => ReloadStage::Typecheck,
        }
    }
}

/// One hot reload attempt of a `.vs` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReloadEvent {
    /// The file, as the build recorded its path.
    pub file: String,
    /// The candidate source, which the diagnostics' spans index into.
    pub source: String,
    /// The revision the mounts matched before the attempt; 0 is the source the
    /// build embedded.
    pub base_revision: u32,
    /// The attempt's own revision: each staged edit of the file numbers the
    /// next.
    pub candidate_revision: u32,
    /// The revision the mounts match after the attempt.
    pub last_good_revision: u32,
    pub outcome: ReloadOutcome,
    /// Where a rejected attempt failed; `RuntimeCommit` for a committed one.
    pub stage: ReloadStage,
    /// From the start of the compile to the end of the last commit.
    pub elapsed_us: u64,
    /// The mounts the candidate was committed to.
    pub mounts: u32,
    /// State cells that kept their live value, over every mount.
    pub migrated: u32,
    /// State cells set from an initializer, over every mount.
    pub reset: u32,
    /// Mounts whose focused node did not survive.
    pub focus_lost: u32,
    /// Scroll offsets that could not be restored, over every mount.
    pub scroll_lost: u32,
    /// Mounts whose recompiled behavior did not mount.
    pub handlers_lost: u32,
    /// A rejected candidate's diagnostics, or a committed one's notices.
    pub diagnostics: Vec<Diagnostic>,
}

impl ReloadEvent {
    /// The codes of its diagnostics, in order, each once.
    pub fn codes(&self) -> Vec<&'static str> {
        let mut codes: Vec<&'static str> = Vec::new();
        for diagnostic in &self.diagnostics {
            if !codes.contains(&diagnostic.code) {
                codes.push(diagnostic.code);
            }
        }
        codes
    }
}

impl ReloadEvent {
    /// The report's bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut enc = Encoder::new();
        self.encode(&mut enc);
        enc.into_bytes()
    }

    /// The report `bytes` spell, all of them.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        Self::decode_from_slice(bytes)
    }
}

impl Encode for ReloadEvent {
    fn encode(&self, enc: &mut Encoder) {
        enc.write_str(&self.file);
        enc.write_str(&self.source);
        for value in [
            self.base_revision,
            self.candidate_revision,
            self.last_good_revision,
        ] {
            enc.write_varint(u64::from(value));
        }
        enc.write_u8(match self.outcome {
            ReloadOutcome::Applied => 0,
            ReloadOutcome::ScopedReset => 1,
            ReloadOutcome::Rejected => 2,
        });
        enc.write_u8(self.stage as u8);
        enc.write_varint(self.elapsed_us);
        for value in [
            self.mounts,
            self.migrated,
            self.reset,
            self.focus_lost,
            self.scroll_lost,
            self.handlers_lost,
        ] {
            enc.write_varint(u64::from(value));
        }
        enc.write_varint(self.diagnostics.len() as u64);
        for diagnostic in &self.diagnostics {
            encode_diagnostic(enc, diagnostic);
        }
    }
}

impl Decode for ReloadEvent {
    fn decode(dec: &mut Decoder<'_>) -> Result<Self, DecodeError> {
        let file = dec.read_str()?.to_owned();
        let source = dec.read_str()?.to_owned();
        let base_revision = read_u32(dec)?;
        let candidate_revision = read_u32(dec)?;
        let last_good_revision = read_u32(dec)?;
        let at = dec.position();
        let outcome = match dec.read_u8()? {
            0 => ReloadOutcome::Applied,
            1 => ReloadOutcome::ScopedReset,
            2 => ReloadOutcome::Rejected,
            _ => return Err(DecodeError::Malformed { offset: at }),
        };
        let at = dec.position();
        let stage = match dec.read_u8()? {
            0 => ReloadStage::Parse,
            1 => ReloadStage::Resolve,
            2 => ReloadStage::Typecheck,
            3 => ReloadStage::Capability,
            4 => ReloadStage::StateCompat,
            5 => ReloadStage::ShaderCompile,
            6 => ReloadStage::RuntimeCommit,
            _ => return Err(DecodeError::Malformed { offset: at }),
        };
        let elapsed_us = dec.read_varint()?;
        let mounts = read_u32(dec)?;
        let migrated = read_u32(dec)?;
        let reset = read_u32(dec)?;
        let focus_lost = read_u32(dec)?;
        let scroll_lost = read_u32(dec)?;
        let handlers_lost = read_u32(dec)?;
        let count = read_count(dec)?;
        let mut diagnostics = Vec::new();
        for _ in 0..count {
            diagnostics.push(decode_diagnostic(dec)?);
        }
        Ok(ReloadEvent {
            file,
            source,
            base_revision,
            candidate_revision,
            last_good_revision,
            outcome,
            stage,
            elapsed_us,
            mounts,
            migrated,
            reset,
            focus_lost,
            scroll_lost,
            handlers_lost,
            diagnostics,
        })
    }
}

fn encode_diagnostic(enc: &mut Encoder, d: &Diagnostic) {
    enc.write_u8(match d.severity {
        Severity::Note => 0,
        Severity::Warning => 1,
        Severity::Error => 2,
    });
    enc.write_str(d.code);
    enc.write_str(&d.message);
    encode_range(enc, d.primary);
    enc.write_varint(d.related.len() as u64);
    for related in &d.related {
        encode_module(enc, related.module.as_deref());
        encode_range(enc, related.range);
        enc.write_str(&related.label);
    }
    encode_strings(enc, &d.expected);
    match &d.actual {
        Some(actual) => {
            enc.write_bool(true);
            enc.write_str(actual);
        }
        None => enc.write_bool(false),
    }
    encode_strings(enc, &d.notes);
    enc.write_varint(d.fixes.len() as u64);
    for fix in &d.fixes {
        enc.write_str(&fix.title);
        enc.write_u8(match fix.applicability {
            Applicability::MachineApplicable => 0,
            Applicability::MaybeIncorrect => 1,
        });
        enc.write_varint(fix.edits.len() as u64);
        for edit in &fix.edits {
            encode_module(enc, edit.module.as_deref());
            encode_range(enc, edit.range);
            enc.write_str(&edit.replacement);
        }
    }
}

fn decode_diagnostic(dec: &mut Decoder<'_>) -> Result<Diagnostic, DecodeError> {
    let at = dec.position();
    let severity = match dec.read_u8()? {
        0 => Severity::Note,
        1 => Severity::Warning,
        2 => Severity::Error,
        _ => return Err(DecodeError::Malformed { offset: at }),
    };
    let at = dec.position();
    let code = intern_code(dec.read_str()?).ok_or(DecodeError::Malformed { offset: at })?;
    let message = dec.read_str()?.to_owned();
    let primary = decode_range(dec)?;
    let mut related = Vec::new();
    for _ in 0..read_count(dec)? {
        related.push(Related {
            module: decode_module(dec)?,
            range: decode_range(dec)?,
            label: dec.read_str()?.to_owned(),
        });
    }
    let expected = decode_strings(dec)?;
    let actual = if dec.read_bool()? {
        Some(dec.read_str()?.to_owned())
    } else {
        None
    };
    let notes = decode_strings(dec)?;
    let mut fixes = Vec::new();
    for _ in 0..read_count(dec)? {
        let title = dec.read_str()?.to_owned();
        let at = dec.position();
        let applicability = match dec.read_u8()? {
            0 => Applicability::MachineApplicable,
            1 => Applicability::MaybeIncorrect,
            _ => return Err(DecodeError::Malformed { offset: at }),
        };
        let mut edits = Vec::new();
        for _ in 0..read_count(dec)? {
            edits.push(TextEdit {
                module: decode_module(dec)?,
                range: decode_range(dec)?,
                replacement: dec.read_str()?.to_owned(),
            });
        }
        fixes.push(Fix {
            title,
            applicability,
            edits,
        });
    }
    Ok(Diagnostic {
        severity,
        code,
        primary,
        related,
        expected,
        actual,
        notes,
        fixes,
        message,
    })
}

fn encode_range(enc: &mut Encoder, range: TextRange) {
    enc.write_varint(u64::from(range.start().to_u32()));
    enc.write_varint(u64::from(range.end().to_u32()));
}

fn decode_range(dec: &mut Decoder<'_>) -> Result<TextRange, DecodeError> {
    let at = dec.position();
    let start = read_u32(dec)?;
    let end = read_u32(dec)?;
    if start > end {
        return Err(DecodeError::Malformed { offset: at });
    }
    Ok(TextRange::new(TextSize::new(start), TextSize::new(end)))
}

fn encode_module(enc: &mut Encoder, module: Option<&str>) {
    match module {
        Some(module) => {
            enc.write_bool(true);
            enc.write_str(module);
        }
        None => enc.write_bool(false),
    }
}

fn decode_module(dec: &mut Decoder<'_>) -> Result<Option<String>, DecodeError> {
    Ok(if dec.read_bool()? {
        Some(dec.read_str()?.to_owned())
    } else {
        None
    })
}

fn encode_strings(enc: &mut Encoder, strings: &[String]) {
    enc.write_varint(strings.len() as u64);
    for s in strings {
        enc.write_str(s);
    }
}

fn decode_strings(dec: &mut Decoder<'_>) -> Result<Vec<String>, DecodeError> {
    let mut strings = Vec::new();
    for _ in 0..read_count(dec)? {
        strings.push(dec.read_str()?.to_owned());
    }
    Ok(strings)
}

fn read_u32(dec: &mut Decoder<'_>) -> Result<u32, DecodeError> {
    let at = dec.position();
    u32::try_from(dec.read_varint()?).map_err(|_| DecodeError::Malformed { offset: at })
}

/// An element count, which cannot exceed the bytes left since every element
/// takes at least one.
fn read_count(dec: &mut Decoder<'_>) -> Result<usize, DecodeError> {
    let at = dec.position();
    let count = dec.read_varint()?;
    if count > dec.remaining() as u64 {
        return Err(DecodeError::Malformed { offset: at });
    }
    Ok(count as usize)
}

/// The `'static` spelling of a decoded diagnostic code. Only the `E####`
/// shape of appendix C is accepted, so the interned set is bounded by its
/// ten thousand codes.
fn intern_code(code: &str) -> Option<&'static str> {
    let bytes = code.as_bytes();
    if bytes.len() != 5 || bytes[0] != b'E' || !bytes[1..].iter().all(u8::is_ascii_digit) {
        return None;
    }
    static CODES: OnceLock<Mutex<Vec<&'static str>>> = OnceLock::new();
    let mut codes = CODES
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(&interned) = codes.iter().find(|&&c| c == code) {
        return Some(interned);
    }
    let interned: &'static str = Box::leak(code.to_owned().into_boxed_str());
    codes.push(interned);
    Some(interned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn range(start: u32, end: u32) -> TextRange {
        TextRange::new(TextSize::new(start), TextSize::new(end))
    }

    fn event() -> ReloadEvent {
        let mut error = Diagnostic::error("E2001", range(4, 9), "unresolved symbol `cuont`");
        error
            .related
            .push(Related::in_module("app", range(1, 2), "here"));
        error.expected.push("count".into());
        error.actual = Some("cuont".into());
        error.notes.push("a note".into());
        error.fixes.push(Fix {
            title: "replace with `count`".into(),
            applicability: Applicability::MaybeIncorrect,
            edits: vec![TextEdit::new(range(4, 9), "count")],
        });
        ReloadEvent {
            file: "src/view.vs".into(),
            source: "let cuont = 1;".into(),
            base_revision: 2,
            candidate_revision: 3,
            last_good_revision: 2,
            outcome: ReloadOutcome::Rejected,
            stage: ReloadStage::of_rejection(std::slice::from_ref(&error)),
            elapsed_us: 1234,
            mounts: 0,
            migrated: 0,
            reset: 0,
            focus_lost: 0,
            scroll_lost: 0,
            handlers_lost: 0,
            diagnostics: vec![error, Diagnostic::warning("E5101", range(0, 3), "reset")],
        }
    }

    #[test]
    fn a_reload_event_round_trips() {
        let bytes = event().to_bytes();
        assert_eq!(ReloadEvent::from_bytes(&bytes), Ok(event()));
    }

    #[test]
    fn the_stage_is_that_of_the_first_error() {
        let stage = |code| ReloadStage::of_rejection(&[Diagnostic::error(code, range(0, 0), "")]);
        assert_eq!(stage("E1404"), ReloadStage::Parse);
        assert_eq!(stage("E2001"), ReloadStage::Resolve);
        assert_eq!(stage("E2103"), ReloadStage::Typecheck);
        assert_eq!(stage("E2601"), ReloadStage::Capability);
        assert_eq!(stage("E5102"), ReloadStage::StateCompat);
        assert_eq!(event().stage.as_str(), "resolve");
        assert_eq!(event().codes(), ["E2001", "E5101"]);
    }

    #[test]
    fn a_malformed_report_is_refused() {
        let bytes = event().to_bytes();

        // A count no remaining bytes could hold.
        let mut corrupt = bytes.clone();
        let at = corrupt.len() - 1;
        corrupt[at] = 0xff;
        assert!(ReloadEvent::from_bytes(&corrupt).is_err());

        // A code outside appendix C's shape.
        let mut bad = event();
        bad.diagnostics[0].code = "HOT_RELOAD_FAILED";
        assert!(ReloadEvent::from_bytes(&bad.to_bytes()).is_err());

        // Truncated, or with bytes after it.
        assert!(ReloadEvent::from_bytes(&bytes[..bytes.len() - 1]).is_err());
        let mut long = bytes.clone();
        long.push(0);
        assert!(ReloadEvent::from_bytes(&long).is_err());
    }
}
