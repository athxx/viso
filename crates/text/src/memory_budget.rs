//! Text memory budgets, one per cache, resolved from a small set of memory
//! classes.
//!
//! Every text cache is budgeted in bytes on its own (spec section 19): the face
//! cache and the global shaping cache never share a ceiling, so exhausting one
//! never evicts from the other. The runtime does not expose byte figures; it
//! resolves a [`MemoryClass`] for the device and reads each cache's budget from
//! it. The figures are provisional — reference-device measurements set the
//! real ones — and are not part of any public ABI.

/// How much memory the text runtime may plan for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryClass {
    /// Phones and other memory-constrained devices.
    Compact,
    /// Laptops and desktops.
    Standard,
    /// Workstations running document-heavy apps.
    Large,
}

impl MemoryClass {
    /// The class the build target resolves to: mobile targets plan compact,
    /// everything else standard.
    pub fn for_target() -> Self {
        if cfg!(any(target_os = "ios", target_os = "android")) {
            Self::Compact
        } else {
            Self::Standard
        }
    }

    /// The per-cache byte budgets this class plans for.
    pub fn text_budgets(self) -> TextBudgets {
        const MIB: u64 = 1 << 20;
        let (face, shaping) = match self {
            Self::Compact => (64 * MIB, 4 * MIB),
            Self::Standard => (256 * MIB, 16 * MIB),
            Self::Large => (512 * MIB, 32 * MIB),
        };
        TextBudgets {
            face_cache_bytes: face,
            shaping_cache_bytes: shaping,
        }
    }
}

/// Independent byte budgets for the text caches the runtime owns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TextBudgets {
    /// Loaded faces: owned bytes plus the derived state charged with them.
    pub face_cache_bytes: u64,
    /// Shaped runs shared across paragraphs.
    pub shaping_cache_bytes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn larger_classes_never_plan_less() {
        let [compact, standard, large] = [
            MemoryClass::Compact,
            MemoryClass::Standard,
            MemoryClass::Large,
        ]
        .map(MemoryClass::text_budgets);
        assert!(compact.face_cache_bytes < standard.face_cache_bytes);
        assert!(standard.face_cache_bytes < large.face_cache_bytes);
        assert!(compact.shaping_cache_bytes < standard.shaping_cache_bytes);
        assert!(standard.shaping_cache_bytes < large.shaping_cache_bytes);
    }
}
