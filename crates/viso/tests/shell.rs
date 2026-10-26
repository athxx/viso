//! The shell through the facade: a tray set while the app starts reaches the
//! platform, clicks on it and picks from its menu reach the app, and a
//! removal asked from a menu pick takes it away; a badge set at launch
//! reaches the platform, and a zero count clears it.
#![cfg(feature = "headless")]

use std::cell::RefCell;
use std::time::Duration;

use viso::__test_support::drive_scripted;
use viso::platform::{MenuCommandId, RawEvent};
use viso::prelude::*;
use viso::shell::{self, AppBadge, IconImage, Tray, TrayEvent};

const STEP: Duration = Duration::from_millis(16);
const HIDE: MenuCommandId = MenuCommandId(2);
const LABEL: MenuCommandId = MenuCommandId(3);
const ZERO: MenuCommandId = MenuCommandId(4);

thread_local! {
    static SEEN: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn seen() -> Vec<String> {
    SEEN.with(|s| std::mem::take(&mut *s.borrow_mut()))
}

fn tray() -> Tray {
    let icon = IconImage::from_rgba(2, 2, vec![255; 16]).unwrap();
    Tray {
        tooltip: "Demo".into(),
        menu: vec![
            Menu::item_plain("Hello", MenuCommandId(1)),
            Menu::item_plain("Hide", HIDE),
        ],
        ..Tray::new(icon)
    }
}

struct TrayApp;

impl Application for TrayApp {
    fn new(_cx: &mut AppCx) -> Self {
        SEEN.with(|s| {
            s.borrow_mut()
                .push(format!("supported {}", shell::tray_supported()));
        });
        shell::set_tray(Some(tray()));
        shell::set_badge(Some(AppBadge::Count(3)));
        TrayApp
    }

    fn build(&mut self, _cx: &mut BuildCx<'_>) {}

    fn on_tray(&mut self, event: TrayEvent) {
        SEEN.with(|s| s.borrow_mut().push(format!("{event:?}")));
    }

    fn on_menu_command(&mut self, command: MenuCommandId) {
        SEEN.with(|s| s.borrow_mut().push(format!("command {}", command.0)));
        match command {
            HIDE => shell::set_tray(None),
            LABEL => shell::set_badge(Some(AppBadge::Label("new".into()))),
            ZERO => shell::set_badge(Some(AppBadge::Count(0))),
            _ => {}
        }
    }
}

#[test]
fn a_tray_set_at_launch_reaches_the_platform_and_its_clicks_reach_the_app() {
    let driven = drive_scripted::<TrayApp>(
        vec![
            RawEvent::Tray(TrayEvent::Click),
            RawEvent::Tray(TrayEvent::DoubleClick),
            RawEvent::MenuCommand {
                id: MenuCommandId(1),
            },
        ],
        STEP,
    );
    assert_eq!(driven.tray(), Some(&tray()));
    assert_eq!(
        seen(),
        ["supported true", "Click", "DoubleClick", "command 1"]
    );
}

#[test]
fn a_menu_pick_can_take_the_tray_away() {
    let driven = drive_scripted::<TrayApp>(vec![RawEvent::MenuCommand { id: HIDE }], STEP);
    assert_eq!(driven.tray(), None);
    assert_eq!(seen(), ["supported true", "command 2"]);
}

#[test]
fn a_badge_reaches_the_platform_and_a_zero_count_clears_it() {
    let driven = drive_scripted::<TrayApp>(vec![], STEP);
    assert_eq!(driven.badge(), Some(&AppBadge::Count(3)));
    let driven = drive_scripted::<TrayApp>(vec![RawEvent::MenuCommand { id: LABEL }], STEP);
    assert_eq!(driven.badge(), Some(&AppBadge::Label("new".into())));
    let driven = drive_scripted::<TrayApp>(vec![RawEvent::MenuCommand { id: ZERO }], STEP);
    assert_eq!(driven.badge(), None);
    seen();
}
