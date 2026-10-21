//! The fields of the adaptive environment a view reads as `env`.

/// A field of the adaptive environment, in the order the prelude's
/// `Environment` record declares it, then the `theme` context, which the
/// store publishes alongside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EnvField {
    /// `env.window`: the window's logical size and scale factor.
    Window,
    /// `env.constraints`: the constraints the reading component's root
    /// receives from its parent.
    Constraints,
    /// `env.size_class`: the class of the nearest adaptive scope.
    SizeClass,
    /// `env.safe_area`: the insets the system reserves.
    SafeArea,
    /// `env.keyboard_inset`: the software keyboard's occlusion.
    KeyboardInset,
    /// `env.display_features`: hinges, folds and cutouts.
    DisplayFeatures,
    /// `env.input`: the available input devices.
    Input,
    /// `env.text_scale`: the user's text scale.
    TextScale,
    /// `env.reduced_motion`: whether the user asked for less motion.
    ReducedMotion,
    /// `env.orientation`: portrait or landscape.
    Orientation,
    /// `env.layout_direction`: left-to-right or right-to-left.
    LayoutDirection,
    /// `env.locale`: the user's locale.
    Locale,
    /// `theme`: the theme value views read, replaced whole.
    Theme,
}

impl EnvField {
    /// Every field, in declaration order.
    pub const ALL: [EnvField; 13] = [
        EnvField::Window,
        EnvField::Constraints,
        EnvField::SizeClass,
        EnvField::SafeArea,
        EnvField::KeyboardInset,
        EnvField::DisplayFeatures,
        EnvField::Input,
        EnvField::TextScale,
        EnvField::ReducedMotion,
        EnvField::Orientation,
        EnvField::LayoutDirection,
        EnvField::Locale,
        EnvField::Theme,
    ];

    /// The fields of the prelude's `Environment` record, in declaration order.
    pub const ENVIRONMENT: [EnvField; 12] = [
        EnvField::Window,
        EnvField::Constraints,
        EnvField::SizeClass,
        EnvField::SafeArea,
        EnvField::KeyboardInset,
        EnvField::DisplayFeatures,
        EnvField::Input,
        EnvField::TextScale,
        EnvField::ReducedMotion,
        EnvField::Orientation,
        EnvField::LayoutDirection,
        EnvField::Locale,
    ];

    /// The field's source name.
    pub const fn name(self) -> &'static str {
        match self {
            EnvField::Window => "window",
            EnvField::Constraints => "constraints",
            EnvField::SizeClass => "size_class",
            EnvField::SafeArea => "safe_area",
            EnvField::KeyboardInset => "keyboard_inset",
            EnvField::DisplayFeatures => "display_features",
            EnvField::Input => "input",
            EnvField::TextScale => "text_scale",
            EnvField::ReducedMotion => "reduced_motion",
            EnvField::Orientation => "orientation",
            EnvField::LayoutDirection => "layout_direction",
            EnvField::Locale => "locale",
            EnvField::Theme => "theme",
        }
    }

    /// The `Environment` field named `name`.
    pub fn named(name: &str) -> Option<EnvField> {
        EnvField::ENVIRONMENT.into_iter().find(|f| f.name() == name)
    }

    /// The field's wire tag, its declaration index.
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// The field of wire tag `tag`.
    pub fn from_tag(tag: u8) -> Option<EnvField> {
        EnvField::ALL.get(tag as usize).copied()
    }

    /// Whether the field's value depends on where the reader sits in the
    /// tree rather than on the window alone.
    pub const fn anchored(self) -> bool {
        matches!(
            self,
            EnvField::Constraints | EnvField::SizeClass | EnvField::LayoutDirection
        )
    }
}

/// The direction text and layout flow in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LayoutDirection {
    /// Left to right.
    #[default]
    Ltr,
    /// Right to left.
    Rtl,
}

impl LayoutDirection {
    /// The direction a BCP 47 locale's script writes in: its script subtag
    /// when it has one, otherwise its language's usual script.
    pub fn of_locale(locale: &str) -> LayoutDirection {
        const RTL_SCRIPTS: [&str; 10] = [
            "adlm", "arab", "hebr", "mand", "mend", "nkoo", "rohg", "samr", "syrc", "thaa",
        ];
        const RTL_LANGUAGES: [&str; 22] = [
            "ar", "arc", "azb", "ckb", "dv", "fa", "glk", "he", "iw", "ji", "khw", "ks", "lrc",
            "mzn", "nqo", "pnb", "ps", "sd", "syr", "ug", "ur", "yi",
        ];
        let mut parts = locale.split(['-', '_']);
        let language = parts.next().unwrap_or_default();
        let script = parts
            .next()
            .filter(|part| part.len() == 4 && part.bytes().all(|b| b.is_ascii_alphabetic()));
        let rtl = match script {
            Some(script) => RTL_SCRIPTS
                .iter()
                .any(|rtl| rtl.eq_ignore_ascii_case(script)),
            None => RTL_LANGUAGES
                .iter()
                .any(|rtl| rtl.eq_ignore_ascii_case(language)),
        };
        if rtl {
            LayoutDirection::Rtl
        } else {
            LayoutDirection::Ltr
        }
    }
}

/// What an avoiding region keeps its content clear of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Avoid {
    /// The edges the system reserves: the environment's safe area.
    SafeArea,
    /// The software keyboard: the environment's keyboard inset at the bottom.
    Keyboard,
}
