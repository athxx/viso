use viso::prelude::*;

/// The app, whose view is `app.vs`.
pub struct App;

impl Application for App {
    fn new(cx: &mut AppCx) -> Self {
        // Text renders with the faces in `assets/fonts/`, the same on every
        // platform and in headless snapshots.
        cx.use_packaged_fonts(viso::packaged_fonts!());
        App
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("app.vs")(cx);
    }
}
