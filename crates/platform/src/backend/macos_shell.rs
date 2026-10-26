//! The app's item in the macOS menu bar: an `NSStatusItem` whose button
//! shows the tray icon and tooltip, delivers clicks, and pops up the tray
//! menu; and the badge on its icon in the Dock.

use std::cell::RefCell;
use std::panic::{AssertUnwindSafe, catch_unwind};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{AnyThread, DefinedClass, MainThreadOnly, define_class, msg_send, sel};
use objc2_app_kit::{
    NSApplication, NSBitmapFormat, NSBitmapImageRep, NSDeviceRGBColorSpace, NSEventMask,
    NSEventModifierFlags, NSEventType, NSImage, NSMenu, NSStatusBar, NSStatusItem,
    NSVariableStatusItemLength,
};
use objc2_foundation::{MainThreadMarker, NSObject, NSObjectProtocol, NSSize, NSString};

use super::{AppDelegate, MenuTarget, Shared, build_menu_node};
use crate::event::RawEvent;
use crate::shell::{AppBadge, IconImage, Tray, TrayEvent};

/// The height a menu-bar icon draws at, in points.
const ICON_POINTS: f64 = 18.0;

/// The status item shown, with what it shows.
pub(super) struct StatusItem {
    item: Retained<NSStatusItem>,
    target: Retained<TrayTarget>,
    /// Kept alive: each custom item's `setTarget:` holds only a weak
    /// reference.
    menu_targets: Vec<Retained<MenuTarget>>,
    shown: Tray,
}

impl StatusItem {
    /// Shows `tray` in the menu bar.
    pub(super) fn new(
        mtm: MainThreadMarker,
        tray: &Tray,
        shared: &Shared,
        delegate: &AppDelegate,
    ) -> StatusItem {
        let item = NSStatusBar::systemStatusBar().statusItemWithLength(NSVariableStatusItemLength);
        let target = TrayTarget::new(mtm, shared.clone());
        if let Some(button) = item.button(mtm) {
            // SAFETY: the target outlives the button's use of it (both drop
            // together with this item), and `trayAction:` is implemented with
            // the `(&self, Option<&AnyObject>)` signature an action takes.
            unsafe {
                button.setTarget(Some(&target));
                button.setAction(Some(sel!(trayAction:)));
            }
            button.sendActionOn(NSEventMask::LeftMouseUp | NSEventMask::RightMouseUp);
        }
        let mut this = StatusItem {
            item,
            target,
            menu_targets: Vec::new(),
            shown: tray.clone(),
        };
        this.show_image(mtm, tray);
        this.show_tooltip(mtm, &tray.tooltip);
        this.show_menu(mtm, tray, shared, delegate);
        this
    }

    /// Shows `tray` in place of what the item shows, redoing only what
    /// changed.
    pub(super) fn update(
        &mut self,
        mtm: MainThreadMarker,
        tray: &Tray,
        shared: &Shared,
        delegate: &AppDelegate,
    ) {
        let image_changed = !tray.icon.same(&self.shown.icon)
            || match (&tray.template, &self.shown.template) {
                (Some(a), Some(b)) => !a.same(b),
                (None, None) => false,
                _ => true,
            };
        if image_changed {
            self.show_image(mtm, tray);
        }
        if tray.tooltip != self.shown.tooltip {
            self.show_tooltip(mtm, &tray.tooltip);
        }
        if tray.menu != self.shown.menu || tray.menu_on_click != self.shown.menu_on_click {
            self.show_menu(mtm, tray, shared, delegate);
        }
        self.shown = tray.clone();
    }

    fn show_image(&self, mtm: MainThreadMarker, tray: &Tray) {
        let (icon, template) = match &tray.template {
            Some(template) => (template, true),
            None => (&tray.icon, false),
        };
        let image = ns_image(icon, ICON_POINTS);
        if let Some(image) = &image {
            image.setTemplate(template);
        }
        if let Some(button) = self.item.button(mtm) {
            button.setImage(image.as_deref());
        }
    }

    fn show_tooltip(&self, mtm: MainThreadMarker, tooltip: &str) {
        if let Some(button) = self.item.button(mtm) {
            let tip = (!tooltip.is_empty()).then(|| NSString::from_str(tooltip));
            button.setToolTip(tip.as_deref());
        }
    }

    fn show_menu(
        &mut self,
        mtm: MainThreadMarker,
        tray: &Tray,
        shared: &Shared,
        delegate: &AppDelegate,
    ) {
        let mut targets = Vec::new();
        let menu = (!tray.menu.is_empty()).then(|| {
            let menu = NSMenu::new(mtm);
            for node in &tray.menu {
                build_menu_node(mtm, &menu, node, shared, delegate, &mut targets);
            }
            menu
        });
        // A menu that opens on any click is the item's own, as AppKit runs
        // it (and assistive technology opens it); one that opens on a
        // secondary click alone is lent for that click.
        let owned = menu.as_ref().filter(|_| tray.menu_on_click);
        self.item.setMenu(owned.map(|m| &**m));
        let ivars = self.target.ivars();
        ivars.menu.replace(menu);
        ivars.item.replace(Some(self.item.clone()));
        self.menu_targets = targets;
    }
}

impl Drop for StatusItem {
    fn drop(&mut self) {
        self.target.ivars().item.replace(None);
        NSStatusBar::systemStatusBar().removeStatusItem(&self.item);
    }
}

/// Shows `badge` on the app's Dock icon, or clears it.
pub(super) fn set_badge(mtm: MainThreadMarker, badge: Option<&AppBadge>) {
    let label = badge
        .and_then(|b| b.text(false))
        .map(|text| NSString::from_str(&text));
    NSApplication::sharedApplication(mtm)
        .dockTile()
        .setBadgeLabel(label.as_deref());
}

/// `icon` as an image `points` tall, keeping its aspect.
pub(super) fn ns_image(icon: &IconImage, points: f64) -> Option<Retained<NSImage>> {
    let (w, h) = (icon.width() as isize, icon.height() as isize);
    // SAFETY: a null plane pointer asks the rep to allocate its own buffer
    // of `w * h * 4` bytes, which the copy below fills exactly: four 8-bit
    // samples a pixel, `w * 4` bytes a row, as the arguments describe.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            w,
            h,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat::AlphaNonpremultiplied,
            w * 4,
            32,
        )
    }?;
    let data = rep.bitmapData();
    if data.is_null() {
        return None;
    }
    let pixels = icon.rgba();
    // SAFETY: `data` is the rep's own buffer of `w * h * 4` bytes, the
    // length of `pixels`, and the two do not overlap.
    unsafe { std::ptr::copy_nonoverlapping(pixels.as_ptr(), data, pixels.len()) };
    let size = NSSize::new(points * icon.width() as f64 / icon.height() as f64, points);
    let image = NSImage::initWithSize(NSImage::alloc(), size);
    image.addRepresentation(&rep);
    Some(image)
}

/// Ivars for the status item button's target.
pub(super) struct TrayTargetIvars {
    shared: Shared,
    menu: RefCell<Option<Retained<NSMenu>>>,
    /// The item the button belongs to, for popping its menu up.
    item: RefCell<Option<Retained<NSStatusItem>>>,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoTrayTarget"]
    #[ivars = TrayTargetIvars]
    pub(super) struct TrayTarget;

    unsafe impl NSObjectProtocol for TrayTarget {}

    impl TrayTarget {
        /// A click on the status item's button, which has no menu of its
        /// own: a secondary click (or a Control-click) pops the menu up; a
        /// primary one is reported.
        #[unsafe(method(trayAction:))]
        fn tray_action(&self, _sender: Option<&AnyObject>) {
            let _ = catch_unwind(AssertUnwindSafe(|| self.clicked()));
        }
    }
);

impl TrayTarget {
    fn new(mtm: MainThreadMarker, shared: Shared) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(TrayTargetIvars {
            shared,
            menu: RefCell::new(None),
            item: RefCell::new(None),
        });
        // SAFETY: `NSObject`'s designated initializer on a freshly allocated
        // instance.
        unsafe { msg_send![super(this), init] }
    }

    fn clicked(&self) {
        let ivars = self.ivars();
        let mtm = self.mtm();
        let event = NSApplication::sharedApplication(mtm).currentEvent();
        let secondary = event.as_ref().is_some_and(|e| {
            e.r#type() == NSEventType::RightMouseUp
                || e.modifierFlags().contains(NSEventModifierFlags::Control)
        });
        let menu = ivars.menu.borrow().clone();
        if let Some(menu) = menu
            && secondary
        {
            let item = ivars.item.borrow().clone();
            if let Some(item) = item {
                // A status item with a menu opens it on a click, anchored
                // under the item and highlighting it; the menu is lent for
                // this click alone so the next one reaches this target again.
                item.setMenu(Some(&menu));
                if let Some(button) = item.button(mtm) {
                    // SAFETY: a click on our own live button, on the main
                    // thread; it tracks the menu until it closes.
                    unsafe { button.performClick(None) };
                }
                item.setMenu(None);
            }
            return;
        }
        if secondary {
            return;
        }
        let double = event.as_ref().is_some_and(|e| e.clickCount() == 2);
        let tray = if double {
            TrayEvent::DoubleClick
        } else {
            TrayEvent::Click
        };
        ivars
            .shared
            .borrow_mut()
            .events
            .push_back(RawEvent::Tray(tray));
    }
}
