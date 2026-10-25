//! The app in the desktop shell beyond its windows: its icon in the system
//! tray (Windows, Linux) or menu bar (macOS), with a tooltip and a menu.
//!
//! Callable from anywhere on the app's thread — `Application::new`, a
//! handler, a task's continuation, `Application::on_menu_command`. Each call
//! records what to show and kicks the loop, which hands it to the platform
//! once the current callback returns; a later call before then replaces an
//! earlier one.
//!
//! ```no_run
//! use viso::prelude::*;
//! use viso::shell::{self, Tray, TrayEvent};
//!
//! struct App;
//!
//! impl Application for App {
//!     fn new(_cx: &mut AppCx) -> Self {
//!         if let Ok(icon) = shell::icon(&viso::ui::Asset::file("tray.png")) {
//!             shell::set_tray(Some(Tray {
//!                 tooltip: "Viso".into(),
//!                 menu: vec![Menu::item_plain("Open", MenuCommandId(1))],
//!                 ..Tray::new(icon)
//!             }));
//!         }
//!         App
//!     }
//!
//!     fn on_tray(&mut self, event: TrayEvent) {
//!         let _ = event;
//!     }
//! }
//! ```
//!
//! Picks of the tray menu reach `Application::on_menu_command`, as the
//! application menu's do; clicks on the icon reach `Application::on_tray`.

use std::cell::RefCell;

use viso_platform::LoopWaker;
pub use viso_platform::{IconImage, Tray, TrayEvent};
pub use viso_render::asset::AssetError;
use viso_runtime::RuntimeCx;
use viso_ui::{Asset, AssetSource};

/// What the app asked the shell to show and the loop has not handed down
/// yet, and what this session's platform offers.
#[derive(Default)]
struct Asked {
    /// A tray to show (`Some(Some)`), to remove (`Some(None)`), or no change.
    tray: Option<Option<Tray>>,
    tray_supported: bool,
    waker: Option<LoopWaker>,
}

thread_local! {
    static ASKED: RefCell<Asked> = RefCell::new(Asked::default());
}

/// Shows `tray` as the app's icon in the system tray or menu bar, replacing
/// the one shown; `None` removes it. Nothing shows where
/// [`tray_supported`] is false.
pub fn set_tray(tray: Option<Tray>) {
    ASKED.with(|asked| {
        let mut asked = asked.borrow_mut();
        asked.tray = Some(tray);
        if let Some(waker) = &asked.waker {
            waker.wake();
        }
    });
}

/// Whether the platform shows a tray icon: on the desktop, yes (on Linux,
/// where the desktop runs a StatusNotifier host); on phones and the web,
/// no.
pub fn tray_supported() -> bool {
    ASKED.with(|asked| asked.borrow().tray_supported)
}

/// The image `asset` holds, decoded now at its pixel size, as an icon.
///
/// # Errors
///
/// When the asset is not a raster image (an SVG, a video), is fetched from a
/// URL, or does not read or decode.
pub fn icon(asset: &Asset) -> Result<IconImage, AssetError> {
    let bytes = match &asset.source {
        AssetSource::Bytes(bytes) => bytes.clone(),
        AssetSource::File(path) => std::fs::read(path)
            .map(|bytes| viso_ui::AssetBytes::Shared(bytes.into()))
            .map_err(|e| AssetError::Decode(format!("`{}`: {e}", path.display())))?,
        AssetSource::Url(url) => {
            return Err(AssetError::Decode(format!(
                "`{url}` is fetched, and an icon is read at once"
            )));
        }
    };
    let image = viso_render::asset::decode_raster_asset(&bytes)?;
    let rgba = image
        .to_straight_rgba8()
        .ok_or_else(|| AssetError::Decode(format!("{image:?} does not hold its texels")))?;
    IconImage::from_rgba(image.width, image.height, rgba)
        .ok_or_else(|| AssetError::Decode(format!("{image:?} has no pixels")))
}

/// What the session showed in the shell, for the driver.
#[derive(Default)]
pub(crate) struct Shown {
    pub(crate) tray: Option<Tray>,
}

/// Starts a session on this thread: forgets what an earlier one asked.
pub(crate) fn begin() {
    ASKED.with(|asked| *asked.borrow_mut() = Asked::default());
}

/// Tells calls on this thread what the session's platform offers and how
/// to kick its loop.
pub(crate) fn launch(cx: &RuntimeCx<'_>) {
    let caps = cx.capabilities();
    let waker = cx.loop_waker();
    ASKED.with(|asked| {
        let mut asked = asked.borrow_mut();
        asked.tray_supported = caps.tray;
        asked.waker = Some(waker);
    });
}

/// Hands what the app asked down to the platform, recording it in `shown`.
pub(crate) fn apply(cx: &mut RuntimeCx<'_>, shown: &mut Shown) {
    let tray = ASKED.with(|asked| asked.borrow_mut().tray.take());
    if let Some(tray) = tray
        && tray != shown.tray
    {
        cx.set_tray(tray.as_ref());
        shown.tray = tray;
    }
}
