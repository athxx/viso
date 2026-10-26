//! How the editor looks: its colors, and each token kind's.

use viso_ui::Rgba;

use crate::decoration::Severity;
use crate::highlight::TokenKind;

/// The linear color of sRGB `0xRRGGBB`.
fn hex(v: u32) -> Rgba {
    viso_ui::Srgb::from_rgba32((v << 8) | 0xff).into_linear_straight()
}

/// `c` with alpha `a`.
fn alpha(c: Rgba, a: f32) -> Rgba {
    Rgba { a, ..c }
}

/// How a token kind draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TokenLook {
    pub color: Rgba,
    pub bold: bool,
    pub italic: bool,
}

impl TokenLook {
    pub const fn color(color: Rgba) -> TokenLook {
        TokenLook {
            color,
            bold: false,
            italic: false,
        }
    }

    pub const fn italic(self) -> TokenLook {
        TokenLook {
            italic: true,
            ..self
        }
    }

    pub const fn bold(self) -> TokenLook {
        TokenLook { bold: true, ..self }
    }
}

/// The editor's colors.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    pub background: Rgba,
    /// Text no token colors.
    pub text: Rgba,
    pub gutter: Rgba,
    pub line_number: Rgba,
    /// The line numbers of the lines with a caret.
    pub line_number_current: Rgba,
    /// Behind the lines with a caret.
    pub current_line: Rgba,
    pub selection: Rgba,
    /// A selection while the editor does not have the focus.
    pub selection_unfocused: Rgba,
    pub caret: Rgba,
    /// Behind a search match.
    pub find_match: Rgba,
    /// Behind the match the cursors are on.
    pub find_current: Rgba,
    /// Behind the bracket paired with the one at the caret.
    pub bracket: Rgba,
    pub inlay_text: Rgba,
    pub inlay_background: Rgba,
    /// A folded line's placeholder.
    pub fold_text: Rgba,
    pub fold_background: Rgba,
    /// The fold marks in the gutter.
    pub fold_mark: Rgba,
    pub error: Rgba,
    pub warning: Rgba,
    pub info: Rgba,
    pub hint: Rgba,
    /// Behind another use of the symbol at the caret.
    pub highlight: Rgba,
    /// Behind a use that writes it.
    pub write_highlight: Rgba,
    /// A popup's: completions, hovers, the find bar.
    pub popup: Rgba,
    pub popup_border: Rgba,
    pub popup_selected: Rgba,
    /// Each token kind, in [`TokenKind::ALL`]'s order.
    pub tokens: [TokenLook; TokenKind::ALL.len()],
}

impl Theme {
    /// Light text on dark.
    pub fn dark() -> Theme {
        let text = hex(0xd4d4d4);
        let mut tokens = [TokenLook::color(text); TokenKind::ALL.len()];
        let mut set = |k: TokenKind, look: TokenLook| tokens[k as usize] = look;
        set(TokenKind::Keyword, TokenLook::color(hex(0xc586c0)));
        set(TokenKind::Builtin, TokenLook::color(hex(0x569cd6)));
        set(TokenKind::Type, TokenLook::color(hex(0x4ec9b0)));
        set(TokenKind::Function, TokenLook::color(hex(0xdcdcaa)));
        set(TokenKind::Macro, TokenLook::color(hex(0x569cd6)));
        set(TokenKind::Variable, TokenLook::color(hex(0x9cdcfe)));
        set(
            TokenKind::Parameter,
            TokenLook::color(hex(0x9cdcfe)).italic(),
        );
        set(TokenKind::Property, TokenLook::color(hex(0x9cdcfe)));
        set(TokenKind::Constant, TokenLook::color(hex(0x4fc1ff)));
        set(TokenKind::Namespace, TokenLook::color(hex(0x4ec9b0)));
        set(TokenKind::Number, TokenLook::color(hex(0xb5cea8)));
        set(TokenKind::String, TokenLook::color(hex(0xce9178)));
        set(TokenKind::Escape, TokenLook::color(hex(0xd7ba7d)));
        set(TokenKind::Char, TokenLook::color(hex(0xce9178)));
        set(TokenKind::Comment, TokenLook::color(hex(0x6a9955)).italic());
        set(
            TokenKind::DocComment,
            TokenLook::color(hex(0x6a9955)).italic(),
        );
        set(TokenKind::Operator, TokenLook::color(hex(0xd4d4d4)));
        set(TokenKind::Punctuation, TokenLook::color(hex(0xa0a0a0)));
        set(TokenKind::Bracket, TokenLook::color(hex(0xffd700)));
        set(TokenKind::Attribute, TokenLook::color(hex(0x4ec9b0)));
        set(TokenKind::Label, TokenLook::color(hex(0xc8c8c8)).italic());
        set(TokenKind::Invalid, TokenLook::color(hex(0xf44747)));
        Theme {
            background: hex(0x1e1e1e),
            text,
            gutter: hex(0x1e1e1e),
            line_number: hex(0x858585),
            line_number_current: hex(0xc6c6c6),
            current_line: alpha(hex(0xffffff), 0.05),
            selection: hex(0x264f78),
            selection_unfocused: hex(0x3a3d41),
            caret: hex(0xaeafad),
            find_match: alpha(hex(0x623315), 0.9),
            find_current: hex(0x515c6a),
            bracket: alpha(hex(0x888888), 0.3),
            inlay_text: hex(0x969696),
            inlay_background: alpha(hex(0x444444), 0.6),
            fold_text: hex(0xa0a0a0),
            fold_background: alpha(hex(0x444444), 0.8),
            fold_mark: hex(0xc5c5c5),
            error: hex(0xf14c4c),
            warning: hex(0xcca700),
            info: hex(0x3794ff),
            hint: alpha(hex(0xeeeeee), 0.7),
            highlight: alpha(hex(0x575757), 0.72),
            write_highlight: alpha(hex(0x004972), 0.72),
            popup: hex(0x252526),
            popup_border: hex(0x454545),
            popup_selected: hex(0x04395e),
            tokens,
        }
    }

    /// Dark text on light.
    pub fn light() -> Theme {
        let text = hex(0x000000);
        let mut tokens = [TokenLook::color(text); TokenKind::ALL.len()];
        let mut set = |k: TokenKind, look: TokenLook| tokens[k as usize] = look;
        set(TokenKind::Keyword, TokenLook::color(hex(0xaf00db)));
        set(TokenKind::Builtin, TokenLook::color(hex(0x0000ff)));
        set(TokenKind::Type, TokenLook::color(hex(0x267f99)));
        set(TokenKind::Function, TokenLook::color(hex(0x795e26)));
        set(TokenKind::Macro, TokenLook::color(hex(0x0000ff)));
        set(TokenKind::Variable, TokenLook::color(hex(0x001080)));
        set(
            TokenKind::Parameter,
            TokenLook::color(hex(0x001080)).italic(),
        );
        set(TokenKind::Property, TokenLook::color(hex(0x001080)));
        set(TokenKind::Constant, TokenLook::color(hex(0x0070c1)));
        set(TokenKind::Namespace, TokenLook::color(hex(0x267f99)));
        set(TokenKind::Number, TokenLook::color(hex(0x098658)));
        set(TokenKind::String, TokenLook::color(hex(0xa31515)));
        set(TokenKind::Escape, TokenLook::color(hex(0xee0000)));
        set(TokenKind::Char, TokenLook::color(hex(0xa31515)));
        set(TokenKind::Comment, TokenLook::color(hex(0x008000)).italic());
        set(
            TokenKind::DocComment,
            TokenLook::color(hex(0x008000)).italic(),
        );
        set(TokenKind::Operator, TokenLook::color(hex(0x000000)));
        set(TokenKind::Punctuation, TokenLook::color(hex(0x505050)));
        set(TokenKind::Bracket, TokenLook::color(hex(0x0431fa)));
        set(TokenKind::Attribute, TokenLook::color(hex(0x267f99)));
        set(TokenKind::Label, TokenLook::color(hex(0x505050)).italic());
        set(TokenKind::Invalid, TokenLook::color(hex(0xcd3131)));
        Theme {
            background: hex(0xffffff),
            text,
            gutter: hex(0xffffff),
            line_number: hex(0x237893),
            line_number_current: hex(0x0b216f),
            current_line: alpha(hex(0x000000), 0.04),
            selection: hex(0xadd6ff),
            selection_unfocused: hex(0xe5ebf1),
            caret: hex(0x000000),
            find_match: alpha(hex(0xa8ac94), 0.6),
            find_current: hex(0xa8ac94),
            bracket: alpha(hex(0x888888), 0.3),
            inlay_text: hex(0x969696),
            inlay_background: alpha(hex(0xcccccc), 0.4),
            fold_text: hex(0x606060),
            fold_background: alpha(hex(0xd0d0d0), 0.8),
            fold_mark: hex(0x424242),
            error: hex(0xe51400),
            warning: hex(0xbf8803),
            info: hex(0x1a85ff),
            hint: alpha(hex(0x6c6c6c), 0.7),
            highlight: alpha(hex(0x575757), 0.16),
            write_highlight: alpha(hex(0x0e639c), 0.2),
            popup: hex(0xf3f3f3),
            popup_border: hex(0xc8c8c8),
            popup_selected: hex(0xd6ebff),
            tokens,
        }
    }

    /// How token kind `kind` draws.
    #[inline]
    pub fn token(&self, kind: TokenKind) -> TokenLook {
        self.tokens[kind as usize]
    }

    /// The color of a diagnostic of `severity`.
    pub fn severity(&self, severity: Severity) -> Rgba {
        match severity {
            Severity::Error => self.error,
            Severity::Warning => self.warning,
            Severity::Info => self.info,
            Severity::Hint => self.hint,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Theme::dark()
    }
}
