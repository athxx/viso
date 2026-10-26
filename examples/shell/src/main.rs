//! The app in the desktop shell beyond its window: an icon in the system tray
//! (the menu bar on macOS) with a tooltip and a menu, and a badge on its icon
//! in the dock or taskbar. Picks of the tray menu reach `on_menu_command`, as
//! the application menu's do; clicks on the icon reach `on_tray`. The
//! buttons and the tray menu change the icon and the badge.

use std::cell::Cell;
use std::rc::Rc;

use viso::prelude::*;
use viso::shell::{self, AppBadge, IconImage, Tray, TrayEvent};
use viso::ui::{Axis, Component, Inset, Sizing};

const HELLO: MenuCommandId = MenuCommandId(1);
const HIDE: MenuCommandId = MenuCommandId(2);
const BADGE_UP: MenuCommandId = MenuCommandId(3);
const BADGE_CLEAR: MenuCommandId = MenuCommandId(4);

/// Whether a primary click opens the tray menu at first: the macOS
/// convention for menu-bar items, not the tray's elsewhere.
const MENU_ON_CLICK: bool = cfg!(target_os = "macos");

thread_local! {
    /// The count the badge shows.
    static COUNT: Cell<u32> = const { Cell::new(0) };
}

/// Shows `count` on the app's icon; zero clears it.
fn show_count(count: u32) {
    COUNT.with(|c| c.set(count));
    viso::println!("[shell] badge {count}");
    shell::set_badge(Some(AppBadge::Count(count)));
}

struct ShellDemo;

impl Application for ShellDemo {
    fn new(_cx: &mut AppCx) -> Self {
        shell::set_tray(Some(tray(0, MENU_ON_CLICK)));
        ShellDemo
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            title: "Shell".to_string(),
            size: (420.0, 260.0),
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            FlexStyle {
                axis: Axis::Column,
                gap: 10.0,
                padding: Inset::all(16.0),
                size: Sizing::fill(),
                ..FlexStyle::default()
            },
            |cx| {
                label(if shell::tray_supported() {
                    "The tray icon is in the menu bar or system tray."
                } else {
                    "This platform shows no tray icon."
                })
                .build(cx);
                let look = Rc::new(Cell::new((0, MENU_ON_CLICK)));
                let shown = look.clone();
                button("Change the icon")
                    .on_click(move |_| {
                        let (hue, on_click) = shown.get();
                        shown.set((hue + 1, on_click));
                        shell::set_tray(Some(tray(hue + 1, on_click)));
                    })
                    .build(cx);
                button("Toggle opening the menu on a click")
                    .on_click(move |_| {
                        let (hue, on_click) = look.get();
                        look.set((hue, !on_click));
                        shell::set_tray(Some(tray(hue, !on_click)));
                    })
                    .build(cx);
                button("Remove the icon")
                    .on_click(|_| shell::set_tray(None))
                    .build(cx);
                if shell::badge_supported() {
                    button("Add one to the badge")
                        .on_click(|_| show_count(COUNT.with(Cell::get) + 1))
                        .build(cx);
                    button("Badge “new”")
                        .on_click(|_| shell::set_badge(Some(AppBadge::Label("new".into()))))
                        .build(cx);
                    button("Clear the badge")
                        .on_click(|_| show_count(0))
                        .build(cx);
                }
            },
        );
    }

    fn on_menu_command(&mut self, command: MenuCommandId) {
        viso::println!("[shell] menu command {}", command.0);
        match command {
            HIDE => shell::set_tray(None),
            BADGE_UP => show_count(COUNT.with(Cell::get) + 1),
            BADGE_CLEAR => show_count(0),
            _ => {}
        }
    }

    fn on_tray(&mut self, event: TrayEvent) {
        viso::println!("[shell] tray {event:?}");
    }
}

/// The tray: a dot whose color steps with `hue`, a template variant for the
/// macOS menu bar, a tooltip and a menu, opened on a primary click when
/// `on_click`.
fn tray(hue: u32, on_click: bool) -> Tray {
    let colors = [[0x15, 0x65, 0xc0], [0xc6, 0x28, 0x28], [0x2e, 0x7d, 0x32]];
    let [r, g, b] = colors[hue as usize % colors.len()];
    Tray {
        template: Some(dot(36, [0, 0, 0])),
        tooltip: format!("Viso shell demo ({})", hue + 1),
        menu: vec![
            Menu::item_plain("Say hello", HELLO),
            Menu::sub(
                "More",
                [
                    Menu::item_plain("Add one to the badge", BADGE_UP),
                    Menu::item_plain("Clear the badge", BADGE_CLEAR),
                    Menu::Line,
                    Menu::item_plain("Hide the icon", HIDE),
                ],
            ),
            Menu::Line,
            Menu::system(SystemAction::Quit),
        ],
        menu_on_click: on_click,
        ..Tray::new(dot(36, [r, g, b]))
    }
}

/// A `size`-pixel disc of `rgb` with a soft edge.
fn dot(size: u32, [r, g, b]: [u8; 3]) -> IconImage {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    let c = size as f32 / 2.0;
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 + 0.5 - c).powi(2) + (y as f32 + 0.5 - c).powi(2)).sqrt();
            let a = (c - 2.0 - d + 0.5).clamp(0.0, 1.0);
            rgba.extend_from_slice(&[r, g, b, (a * 255.0) as u8]);
        }
    }
    IconImage::from_rgba(size, size, rgba).expect("a square of RGBA")
}

fn main() {
    viso::run::<ShellDemo>();
}
