//! Progressive font readiness: render with what is resolved now and reflow when
//! a better face (a system fallback or a still-loading packaged family) becomes
//! available, without blocking the frame.
//!
//! The main thread never waits on font resolution. This tracks the last-good
//! face for a run and signals a reflow when resolution upgrades it.

/// Tracks progressive face readiness and reflow signalling.
#[derive(Debug, Default)]
pub struct Progressive {
    // TODO(TF-P1): per-run last-good face + pending-upgrade signals.
}

impl Progressive {
    /// Note that a better face resolved for a run and a reflow is due.
    pub fn note_upgrade(&mut self) {
        todo!("TF-P1: record face upgrade, schedule reflow")
    }
}
