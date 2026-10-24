//! A camera's live picture: the platform's default camera, or the one facing
//! the other way, mirrored when it faces the user. Pausing lets the camera
//! go; playing asks for it again. The cameras come from the `viso-camera`
//! extension, installed before the app runs.

use viso::prelude::*;

struct Camera;

impl Application for Camera {
    fn new(_cx: &mut AppCx) -> Self {
        Camera
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("camera.vs")(cx);
    }
}

fn main() {
    viso_camera::install();
    viso::run::<Camera>();
}
