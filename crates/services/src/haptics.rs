//! Haptic feedback.

use crate::reply::ServiceResult;

/// A haptic pattern, named by intent; each OS plays its closest effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Haptic {
    /// A selection moved (a picker detent, a toggle).
    Selection,
    Light,
    Medium,
    Heavy,
    Success,
    Warning,
    Error,
}

/// Haptic feedback, fire and forget.
pub trait Haptics {
    fn play(&self, haptic: Haptic) -> ServiceResult<()>;
}
