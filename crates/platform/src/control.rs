//! Loop control, window identity, and configuration types.
//!
//! These are the values that cross the platform↔runtime boundary *besides*
//! events: the identity of a window, how to create one, what the runtime tells
//! the pump to do next, and how creation can fail. None of them reference a
//! runtime type — the platform layer stays the bottom of the DAG.

use core::time::Duration;

/// Opaque, process-stable identity of a native window.
///
/// Assigned by the platform layer at window creation and echoed back on every
/// event that targets that window. Distinct from [`crate::SurfaceId`], which is
/// the GPU-surface identity; today they map 1:1, but the split lets a window
/// later host several surfaces without churning event routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct WindowId(pub u32);

/// A rectangle in logical (pre-scale) points, top-left origin.
///
/// The platform layer holds no general geometry vocabulary (it stays narrow) —
/// events carry bare `f64` fields. This is the one shape that crosses
/// the boundary in both directions for self-drawn chrome: the app pushes
/// draggable caption regions down as a slice of these
/// ([`PlatformApp::set_draggable_regions`](crate::PlatformApp::set_draggable_regions)),
/// and the backend reports the native traffic-light bounding box back up in the
/// same coordinate space (via a chrome-geometry event). Four `f64` — compact
/// data, not a shared object, `Copy` so hit-testing a cached slice in
/// `mouseDown` allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalRect {
    /// Left edge, logical points.
    pub x: f64,
    /// Top edge, logical points (top-left origin).
    pub y: f64,
    /// Width, logical points.
    pub width: f64,
    /// Height, logical points.
    pub height: f64,
}

/// The focused text field the platform's input method serves: where its
/// caret is, so candidate windows and the soft keyboard's accessory views sit
/// next to it, whether it takes a secret, and what it holds, so a soft
/// keyboard shows the keys it needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextEntry {
    /// The caret, logical points (top-left origin).
    pub caret: LogicalRect,
    /// A password field's: the input method composes nothing and offers no
    /// candidates, nothing is learned or suggested from what it types, and
    /// where the platform can, other processes cannot read its keys.
    pub secure: bool,
    /// What the field holds.
    pub kind: InputKind,
    /// Whether it holds lines: the soft keyboard's return key breaks a line
    /// rather than finishing the entry.
    pub multiline: bool,
}

/// The text around the focused field's selection, which the input method
/// reads to reconvert committed text, correct a typed word and predict the
/// next. A stretch of the field's text, not all of it, so a long document
/// costs no more than a short one. Offsets are bytes; `anchor`, `focus` and
/// `composition` are into `text`, on `char` boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SurroundingText {
    /// The stretch of the field's text around the selection.
    pub text: String,
    /// Where `text` starts in the field's text.
    pub offset: usize,
    /// Where the selection starts.
    pub anchor: usize,
    /// Where the selection ends: the caret.
    pub focus: usize,
    /// The composition under way, which `text` holds as the field draws it.
    pub composition: Option<(usize, usize)>,
}

impl SurroundingText {
    /// The selection's ordered bounds.
    pub fn selection(&self) -> (usize, usize) {
        (self.anchor.min(self.focus), self.anchor.max(self.focus))
    }
}

/// The menu of edits a text field offers for its selection, shown beside
/// it: on a phone or tablet after a long press, a double tap or a tap on the
/// caret, as the platform draws it, and as a context menu where a secondary
/// click or the menu key asks for one. Paste is offered only while the
/// system clipboard holds text, which the platform checks itself.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EditMenu {
    /// What the menu points at and keeps clear of: the selection, or the
    /// caret; logical points (top-left origin).
    pub at: LogicalRect,
    /// Offer to cut the selection.
    pub cut: bool,
    /// Offer to copy the selection.
    pub copy: bool,
    /// Offer to paste over the selection.
    pub paste: bool,
    /// Offer to select the whole text.
    pub select_all: bool,
}

/// The magnifier a phone shows above a text field's caret while a finger
/// carries it: from a long press, or dragging a selection handle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loupe {
    /// The finger, logical points (top-left origin).
    pub x: f64,
    pub y: f64,
    /// The caret it magnifies, logical points.
    pub caret: LogicalRect,
}

/// The titles of a context menu's edits, in the language of the app.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditLabels {
    pub cut: &'static str,
    pub copy: &'static str,
    pub paste: &'static str,
    pub select_all: &'static str,
}

impl EditLabels {
    /// The titles in the language of BCP 47 tag `locale`; English for a
    /// language without its own.
    pub fn for_locale(locale: &str) -> EditLabels {
        let mut parts = locale.split(['-', '_']);
        let language = parts.next().unwrap_or("").to_ascii_lowercase();
        let rest: Vec<String> = parts.map(str::to_ascii_lowercase).collect();
        let traditional = rest
            .iter()
            .any(|p| matches!(p.as_str(), "hant" | "tw" | "hk" | "mo"));
        let [cut, copy, paste, select_all] = match language.as_str() {
            "zh" if traditional => ["剪下", "拷貝", "貼上", "全選"],
            "zh" => ["剪切", "拷贝", "粘贴", "全选"],
            "ja" => ["カット", "コピー", "ペースト", "すべてを選択"],
            "ko" => ["잘라내기", "복사", "붙여넣기", "모두 선택"],
            "de" => ["Ausschneiden", "Kopieren", "Einfügen", "Alles auswählen"],
            "fr" => ["Couper", "Copier", "Coller", "Tout sélectionner"],
            "es" => ["Cortar", "Copiar", "Pegar", "Seleccionar todo"],
            "it" => ["Taglia", "Copia", "Incolla", "Seleziona tutto"],
            "pt" => ["Recortar", "Copiar", "Colar", "Selecionar tudo"],
            "nl" => ["Knippen", "Kopiëren", "Plakken", "Alles selecteren"],
            "ru" => ["Вырезать", "Копировать", "Вставить", "Выделить все"],
            "uk" => ["Вирізати", "Копіювати", "Вставити", "Виділити все"],
            "pl" => ["Wytnij", "Kopiuj", "Wklej", "Zaznacz wszystko"],
            "tr" => ["Kes", "Kopyala", "Yapıştır", "Tümünü seç"],
            "sv" => ["Klipp ut", "Kopiera", "Klistra in", "Markera allt"],
            "ar" => ["قص", "نسخ", "لصق", "تحديد الكل"],
            "he" => ["גזירה", "העתקה", "הדבקה", "בחירת הכול"],
            "hi" => ["काटें", "कॉपी करें", "चिपकाएँ", "सभी चुनें"],
            "th" => ["ตัด", "คัดลอก", "วาง", "เลือกทั้งหมด"],
            "vi" => ["Cắt", "Sao chép", "Dán", "Chọn tất cả"],
            "id" => ["Potong", "Salin", "Tempel", "Pilih semua"],
            _ => ["Cut", "Copy", "Paste", "Select All"],
        };
        EditLabels {
            cut,
            copy,
            paste,
            select_all,
        }
    }
}

/// What the user did with the [`EditMenu`] besides cutting, copying and
/// pasting, which arrive as [`RawEvent::CopyRequested`](crate::RawEvent::CopyRequested)
/// and [`RawEvent::PasteRequested`](crate::RawEvent::PasteRequested).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditMenuAction {
    /// Select the whole text.
    SelectAll,
    /// The menu closed without the app asking: an item was chosen, or the
    /// user touched elsewhere.
    Closed,
}

/// What a text field holds, which decides the keys a soft keyboard shows
/// and whether it suggests, corrects and capitalizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InputKind {
    /// Prose: the full keyboard, with suggestions.
    #[default]
    Text,
    /// Digits alone: a code, a count, a quantity.
    Digits,
    /// A whole number: digits and a sign.
    Integer,
    /// A number with a fraction: digits, a sign and a decimal separator.
    Decimal,
    /// A telephone number: the phone pad.
    Phone,
    /// An email address: no capitals or corrections, `@` at hand.
    Email,
    /// A web address: no capitals or corrections, `/` and `.` at hand.
    Url,
    /// A search query: the return key searches.
    Search,
}

impl InputKind {
    /// Whether it holds a number, which no input method composes.
    pub const fn numeric(self) -> bool {
        matches!(
            self,
            InputKind::Digits | InputKind::Integer | InputKind::Decimal | InputKind::Phone
        )
    }

    /// Whether the keyboard should leave what is typed as typed: no
    /// capitals, corrections or suggestions.
    pub const fn literal(self) -> bool {
        !matches!(self, InputKind::Text | InputKind::Search)
    }
}

impl LogicalRect {
    /// A rectangle from its top-left corner and size, in logical points.
    pub const fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Whether `(px, py)` (logical points, top-left origin) lies inside the
    /// rectangle. Left/top edges are inclusive, right/bottom exclusive — the
    /// half-open convention that keeps adjacent regions from double-hitting.
    pub fn contains(&self, px: f64, py: f64) -> bool {
        px >= self.x && px < self.x + self.width && py >= self.y && py < self.y + self.height
    }
}

/// What the pump should do after delivering the current event batch.
///
/// The runtime returns this from [`crate::AppHandler::handle`]; the backend
/// blocks, spins, sleeps, or exits accordingly. The deadline variant lets
/// timers sleep without busy-polling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlFlow {
    /// A frame is pending — do not block; process the next event immediately.
    Poll,
    /// Nothing is pending — block until the OS delivers the next event.
    Wait,
    /// Block until the OS delivers an event or this deadline elapses.
    WaitUntil(crate::Instant),
    /// Tear down the pump and return from `run`.
    Exit,
}

/// How to create the initial (or an additional) window.
///
/// Sizes are in logical points; the backend applies the display scale factor.
#[derive(Debug, Clone)]
pub struct WindowConfig {
    /// Title bar text.
    pub title: String,
    /// Inner (content) size in logical points.
    pub logical_size: (f64, f64),
    /// Who draws the window chrome (title bar / caption). See [`WindowChrome`].
    pub chrome: WindowChrome,
    /// Where the window's outer top-left corner goes on the desktop, in the
    /// coordinates [`Window::outer_position`](crate::Window::outer_position)
    /// reports; `None` lets the platform place it. A point on no screen is
    /// ignored, and so is any point where the platform places windows itself
    /// (Wayland, mobile, the browser). A window that would open partly off the
    /// screens (a monitor was unplugged, or the work area shrank) is moved, and
    /// shrunk if it must be, onto the screen it overlaps most.
    pub position: Option<(f64, f64)>,
    /// Open maximized, filling the screen's work area; leaving that returns
    /// it to `logical_size` at `position`. Ignored where windows have no such
    /// state (mobile, the browser).
    pub maximized: bool,
    /// The app draws its frames on the CPU and hands each one over as pixels:
    /// the window's [`raw_handle`](crate::Window::raw_handle) is a
    /// [`Scanout`](crate::RawWindowHandle::Scanout) that shows them, where the
    /// platform has one (macOS, and a KMS display, which always does), in
    /// place of the native surface a GPU renders into.
    pub cpu_frames: bool,
    /// The compositor's material behind the window's content, where the
    /// platform has it: the window's surface is then transparent where the
    /// app draws nothing. [`Window::material`](crate::Window::material)
    /// reports whether it is shown.
    pub material: WindowMaterial,
}

/// A material the system compositor draws behind a window, from the desktop
/// behind it: the app's frames pay nothing for it. Windows 11 (22H2 and later)
/// draws the DWM system backdrops, macOS an `NSVisualEffectView` behind the
/// content (each following the window's active state); elsewhere, and on CPU
/// frames, the window has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowMaterial {
    /// No material: the app draws the whole window.
    #[default]
    None,
    /// The desktop's colors, heavily blurred and dimmed, for a long-lived
    /// main window: Windows Mica, macOS's under-window background.
    Mica,
    /// Mica more strongly tinted, for a window with tabs: Windows Mica Alt,
    /// macOS's window background.
    MicaAlt,
    /// A lighter frost of what lies behind, for transient windows: Windows
    /// Acrylic, a macOS popover.
    Acrylic,
    /// macOS's sidebar vibrancy; Mica on Windows.
    Vibrancy,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            title: "Viso".to_string(),
            logical_size: (800.0, 600.0),
            chrome: WindowChrome::Native,
            position: None,
            maximized: false,
            cpu_frames: false,
            material: WindowMaterial::None,
        }
    }
}

/// How a window shows on the desktop, as [`Window::mode`](crate::Window::mode)
/// reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowMode {
    /// At its own size and place.
    #[default]
    Normal,
    /// Filling the screen's work area.
    Maximized,
    /// Down in the dock or taskbar.
    Minimized,
    /// Covering the whole screen.
    Fullscreen,
}

/// Who owns the window's title bar / caption region.
///
/// [`Native`](Self::Native) keeps the OS-drawn title bar (default, unchanged
/// behavior). [`SelfDrawn`](Self::SelfDrawn) asks the backend for a full-size
/// content area with the native title bar de-decorated (transparent, no title
/// text), so the app paints its own caption content while the OS keeps native
/// affordances that cannot be reproduced (on macOS: the traffic-light buttons).
/// The backend measures those native affordances and reports their geometry so
/// the app can align its caption bar around them; the app in turn declares which
/// regions of its caption are draggable (see
/// [`PlatformApp::set_draggable_regions`](crate::PlatformApp::set_draggable_regions)).
///
/// A backend with no self-drawn-chrome support treats this as `Native`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowChrome {
    /// OS-drawn title bar (default).
    #[default]
    Native,
    /// App-drawn caption over a full-size content area.
    SelfDrawn,
}

/// Why the platform layer could not satisfy a request.
///
/// Kept deliberately coarse: the runtime reacts to the *category*, not to
/// OS-specific error codes, which stay in the backend as the `detail` string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlatformError {
    /// No native backend is compiled/available for this target.
    NoBackend,
    /// The OS refused to create a window or surface.
    WindowCreation(String),
    /// A backend call failed for a reason the runtime cannot act on.
    Backend(String),
}

impl core::fmt::Display for PlatformError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PlatformError::NoBackend => write!(f, "no native platform backend for this target"),
            PlatformError::WindowCreation(d) => write!(f, "window creation failed: {d}"),
            PlatformError::Backend(d) => write!(f, "platform backend error: {d}"),
        }
    }
}

impl std::error::Error for PlatformError {}

/// Convenience: the animation-frame budget the backend targets when polling.
///
/// Backends without a real display link (headless) use this to pace synthetic
/// redraw beats so tests and idle-cost benches stay deterministic.
pub const DEFAULT_FRAME_BUDGET: Duration = Duration::from_micros(16_666);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_labels_follow_the_language_and_its_script() {
        assert_eq!(EditLabels::for_locale("zh-Hans-CN").copy, "拷贝");
        assert_eq!(EditLabels::for_locale("zh_TW").copy, "拷貝");
        assert_eq!(EditLabels::for_locale("zh-Hant").paste, "貼上");
        assert_eq!(EditLabels::for_locale("FR-ca").cut, "Couper");
        assert_eq!(EditLabels::for_locale("und").select_all, "Select All");
    }

    #[test]
    fn logical_rect_contains_is_half_open() {
        let r = LogicalRect::new(10.0, 20.0, 100.0, 30.0);
        // Interior.
        assert!(r.contains(10.0, 20.0), "top-left corner is inclusive");
        assert!(r.contains(50.0, 35.0), "an interior point is inside");
        // Right/bottom edges are exclusive.
        assert!(!r.contains(110.0, 35.0), "right edge is exclusive");
        assert!(!r.contains(50.0, 50.0), "bottom edge is exclusive");
        assert!(
            !r.contains(110.0, 50.0),
            "bottom-right corner is exclusive on both axes"
        );
        // Left/top edges just outside.
        assert!(!r.contains(9.999, 35.0), "left of the left edge is outside");
        assert!(!r.contains(50.0, 19.999), "above the top edge is outside");
    }

    #[test]
    fn window_chrome_defaults_to_native() {
        assert_eq!(WindowChrome::default(), WindowChrome::Native);
        assert_eq!(
            WindowConfig::default().chrome,
            WindowChrome::Native,
            "a default window keeps the OS-drawn title bar"
        );
    }
}

/// What a backend offers beyond the one window every backend has. Tooling
/// checks a target's capabilities at build time; at run time an app asks
/// before it reaches for one (a second window, a menu bar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformCaps {
    /// More than one window at a time.
    pub multi_window: bool,
    /// A system menu bar or application menu ([`PlatformApp::set_menu`](crate::PlatformApp::set_menu));
    /// without one, a menu's accelerators still work.
    pub menus: bool,
    /// Desktop portals: system appearance, file pickers and the like.
    pub portals: bool,
    /// A clipboard shared with other applications; without one, copy and
    /// paste stay inside the app.
    pub system_clipboard: bool,
    /// An input method: composition and an on-screen keyboard.
    pub ime: bool,
    /// An accessibility bridge that announces the published semantics to
    /// assistive technology; without one, the app derives semantics for
    /// tests and the Inspector only and publishes nothing.
    pub accessibility: bool,
    /// A system edit menu for a text field's selection
    /// ([`PlatformApp::set_edit_menu`](crate::PlatformApp::set_edit_menu)).
    pub edit_menu: bool,
    /// A system context menu at the pointer
    /// ([`PlatformApp::show_context_menu`](crate::PlatformApp::show_context_menu)).
    pub context_menu: bool,
    /// A system magnifier over a carried caret
    /// ([`PlatformApp::set_loupe`](crate::PlatformApp::set_loupe)).
    pub loupe: bool,
    /// Image cursors ([`CursorIcon::Custom`](crate::CursorIcon::Custom));
    /// without them a window shows the default arrow in their place.
    pub custom_cursors: bool,
    /// Locking the mouse's pointer for relative motion
    /// ([`PlatformApp::set_pointer_lock`](crate::PlatformApp::set_pointer_lock)).
    pub pointer_lock: bool,
    /// An icon in the system tray or menu bar
    /// ([`PlatformApp::set_tray`](crate::PlatformApp::set_tray)).
    pub tray: bool,
    /// A badge on the app's icon
    /// ([`PlatformApp::set_badge`](crate::PlatformApp::set_badge)).
    pub badge: bool,
}

impl PlatformCaps {
    /// A desktop backend: everything.
    pub const DESKTOP: PlatformCaps = PlatformCaps {
        multi_window: true,
        menus: true,
        portals: true,
        system_clipboard: true,
        ime: true,
        accessibility: true,
        edit_menu: false,
        context_menu: true,
        loupe: false,
        custom_cursors: true,
        pointer_lock: true,
        tray: true,
        badge: true,
    };
    /// A phone or tablet backend: one window, the system's clipboard and IME.
    pub const MOBILE: PlatformCaps = PlatformCaps {
        multi_window: false,
        menus: false,
        portals: false,
        system_clipboard: true,
        ime: true,
        accessibility: true,
        edit_menu: true,
        context_menu: false,
        loupe: true,
        custom_cursors: false,
        pointer_lock: false,
        tray: false,
        badge: false,
    };
    /// A display driven with no display server (KMS): one window, nothing
    /// from a desktop and no accessibility bridge.
    pub const FULLSCREEN: PlatformCaps = PlatformCaps {
        multi_window: false,
        menus: false,
        portals: false,
        system_clipboard: false,
        ime: false,
        accessibility: false,
        edit_menu: false,
        context_menu: false,
        loupe: false,
        custom_cursors: true,
        pointer_lock: true,
        tray: false,
        badge: false,
    };
}
