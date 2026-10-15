use viso::prelude::*;

/// The app, whose view is `app.vs`.
pub struct App;

impl Application for App {
    fn new(_cx: &mut AppCx) -> Self {
        App
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("app.vs")(cx);
    }
}
