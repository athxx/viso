//! An app links open: `Viso.toml` declares the `visolinks` scheme, so the
//! system starts the app for `visolinks://…`, or hands the link to the app
//! while it runs, and `on_open_url` receives each one. The app prints every
//! link and counts them in a state cell.

use viso::prelude::*;

struct Links {
    opened: Option<State<i32>>,
}

impl Application for Links {
    fn new(_cx: &mut AppCx) -> Self {
        Links { opened: None }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let opened = cx.state_of(0);
        self.opened = Some(opened);
        let page = cx.leaf(LeafStyle {
            size: Sizing::fill(),
            style: BoxStyle::solid(Rgba {
                r: 0.12,
                g: 0.13,
                b: 0.16,
                a: 1.0,
            }),
        });
        cx.bind(opened, page, DirtyClass::PAINT);
    }

    fn on_open_url(&mut self, cx: &mut EventCx<'_>, url: &str) {
        let Some(opened) = self.opened else {
            return;
        };
        let count = opened.get(cx).unwrap_or(0) + 1;
        opened.set(cx, count);
        viso::println!("opened link {count}: {url}");
    }
}

fn main() {
    viso::run::<Links>();
}
