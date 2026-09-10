//! The typed font request: the family / weight / width / style / features a
//! caller asks for, before any face is resolved.
//!
//! A request is intent, not identity. The resolver turns a request into a
//! concrete [`crate::FontFaceId`]; a request never carries a resolved face or a
//! loaded byte buffer.

/// A named font role the application can bind a family to, so callers request
/// by role rather than repeating family strings.
///
/// The roles mirror the categories a platform system-font resolver exposes, so
/// a role can bind to an app family when the manifest declares one and
/// otherwise fall through to the matching system role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontRole {
    /// Default proportional UI / body text.
    Ui,
    /// Proportional serif text.
    Serif,
    /// Monospace / code.
    Mono,
    /// CJK text (Han / Kana / Hangul); resolved with a language hint.
    Cjk,
    /// Emoji; resolved to a color-capable face.
    Emoji,
}

/// Weight on the standard 1–1000 axis (400 = regular, 700 = bold).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontWeight(pub u16);

impl FontWeight {
    /// Regular (400).
    pub const REGULAR: Self = Self(400);
    /// Bold (700).
    pub const BOLD: Self = Self(700);
}

impl Default for FontWeight {
    fn default() -> Self {
        Self::REGULAR
    }
}

/// Width / stretch on the standard 1–9 usWidthClass axis (5 = normal).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FontWidth(pub u8);

impl FontWidth {
    /// Normal width (5).
    pub const NORMAL: Self = Self(5);
}

impl Default for FontWidth {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// Slant of the requested face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FontSlant {
    /// Upright.
    #[default]
    Normal,
    /// Italic (a dedicated italic design when available).
    Italic,
    /// Oblique (slanted upright).
    Oblique,
}

/// What a caller wants a face to be, before resolution and fallback.
///
/// The target is either a named [`FontRole`] or an explicit family string. The
/// family string is the only string in the request; it is consumed at
/// resolution time and never reaches a steady-state path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FontTarget {
    /// Resolve through a manifest-bound role.
    Role(FontRole),
    /// Resolve an explicitly named family.
    Family(String),
}

/// A resolution-independent request for a face: what the caller wants to render
/// with, prior to resolution and fallback.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FontRequest {
    /// Role or explicit family to resolve.
    pub target: FontTarget,
    /// Requested weight.
    pub weight: FontWeight,
    /// Requested width / stretch.
    pub width: FontWidth,
    /// Requested slant.
    pub slant: FontSlant,
}

impl FontRequest {
    /// A request for a role at regular weight, normal width, upright slant.
    pub fn role(role: FontRole) -> Self {
        Self {
            target: FontTarget::Role(role),
            weight: FontWeight::default(),
            width: FontWidth::default(),
            slant: FontSlant::default(),
        }
    }

    /// A request for an explicitly named family at regular weight.
    pub fn family(name: impl Into<String>) -> Self {
        Self {
            target: FontTarget::Family(name.into()),
            weight: FontWeight::default(),
            width: FontWidth::default(),
            slant: FontSlant::default(),
        }
    }
}
