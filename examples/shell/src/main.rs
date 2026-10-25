//! The app in the desktop shell beyond its window: an icon in the system tray
//! (the menu bar on macOS) with a tooltip and a menu. Picks of the tray menu
//! reach `on_menu_command`, as the application menu's do; clicks on the icon
//! reach `on_tray`. The buttons show, change and remove the icon.

use std::cell::Cell;
use std::rc::Rc;

use viso::prelude::*;
use viso::shell::{self, IconImage, Tray, TrayEvent};
use viso::ui::{Axis, Component, Inset, Size};

const HELLO: MenuCommandId = MenuCommandId(1);
const HIDE: MenuCommandId = MenuCommandId(2);

struct ShellDemo;

impl Application for ShellDemo {
    fn new(_cx: &mut AppCx) -> Self {
        shell::set_tray(Some(tray(0, false)));
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
                size: Size::fill(),
                ..FlexStyle::default()
            },
            |cx| {
                label(if shell::tray_supported() {
                    "The tray icon is in the menu bar or system tray."
                } else {
                    "This platform shows no tray icon."
                })
                .build(cx);
                let look = Rc::new(Cell::new((0, false)));
                let shown = look.clone();
                button("Change the icon")
                    .on_click(move |_| {
                        let (hue, on_click) = shown.get();
                        shown.set((hue + 1, on_click));
                        shell::set_tray(Some(tray(hue + 1, on_click)));
                    })
                    .build(cx);
                button("Open the menu on a click")
                    .on_click(move |_| {
                        let (hue, on_click) = look.get();
                        look.set((hue, !on_click));
                        shell::set_tray(Some(tray(hue, !on_click)));
                    })
                    .build(cx);
                button("Remove the icon")
                    .on_click(|_| shell::set_tray(None))
                    .build(cx);
            },
        );
    }

    fn on_menu_command(&mut self, command: MenuCommandId) {
        viso::println!("[shell] menu command {}", command.0);
        if command == HIDE {
            shell::set_tray(None);
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
            Menu::sub("More", [Menu::item_plain("Hide the icon", HIDE)]),
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
