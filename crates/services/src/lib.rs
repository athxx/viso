//! `viso-services` — the app service protocol (§9).
//!
//! Unified access to cold-path OS capabilities: file, share, notifications,
//! permissions, camera, location, secure storage, haptics, media, networking.
//! Because these are rare calls, `dyn Trait` registries are appropriate here
//! (§7.2) — keeping this logic out of `viso-platform`/`viso-runtime`.
//!
//! Phase 0 status: contract-only skeleton.

#![forbid(unsafe_op_in_unsafe_fn)]

/// Marker trait for an app service resolved from the service registry.
pub trait Service: 'static {
    /// Stable, human-readable service name (cold path — string is fine here).
    fn name(&self) -> &'static str;
}
