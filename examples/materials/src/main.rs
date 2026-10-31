//! Materials: six looks over a moving picture, and a sidebar over the
//! window's system material.
//!
//! Skeuomorphism and Aqua are paint data alone — layered fills, inset and
//! outer shadows, an embossed text shadow. Background blur, glassmorphism,
//! acrylic and liquid glass are each one fused backdrop pass over a capture
//! the glass on the picture shares, blurred on the downsampled ladder. Press
//! the liquid glass to light it; its pills melt into one surface. The
//! sidebar draws nothing: the window shows the system's material there
//! (vibrancy on macOS, Mica on Windows 11) and the theme's background where
//! there is none. Holding the picture still keeps the capture across frames.

use viso::prelude::*;

struct Materials;

impl Application for Materials {
    fn new(cx: &mut AppCx) -> Self {
        // A face of its own, so its text draws the same on every host,
        // headless included.
        cx.use_packaged_fonts(viso::packaged_fonts!());
        Materials
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            title: "Materials".to_owned(),
            size: (1080.0, 640.0),
            material: viso::WindowMaterial::Vibrancy,
            ..WindowConfig::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("materials.vs")(cx);
    }
}

fn main() {
    viso::run::<Materials>();
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::time::Duration;

    use viso::__test_support::{DrivenApp, drive_scripted_frames, probe_frames};
    use viso::platform::{Modifiers, PointerButtons, PointerPhase, RawEvent, RawPointer, WindowId};
    use viso::render::FrameStats;
    use viso::ui::{NodeId, NodeStore};
    use viso_image::snapshot::{Image, compare};

    const STEP: Duration = Duration::from_millis(16);
    const WIN: WindowId = WindowId(1);

    fn redraw() -> RawEvent {
        RawEvent::RedrawRequested { window: WIN }
    }

    fn mouse(at: (f32, f32), buttons: PointerButtons, phase: PointerPhase) -> RawEvent {
        RawEvent::Pointer(RawPointer::mouse(
            WIN,
            f64::from(at.0),
            f64::from(at.1),
            buttons,
            Modifiers::default(),
            phase,
        ))
    }

    /// A click at `at`, and frames for it to land.
    fn click(at: (f32, f32)) -> Vec<RawEvent> {
        let mut out = vec![
            mouse(at, PointerButtons::NONE, PointerPhase::Moved),
            mouse(at, PointerButtons::PRIMARY, PointerPhase::Down),
            redraw(),
            mouse(at, PointerButtons::PRIMARY, PointerPhase::Up),
        ];
        out.extend((0..4).map(|_| redraw()));
        out
    }

    /// The gallery driven by `script` for `frames` frames, and each
    /// submitted frame's renderer counters with whether it uploaded.
    fn drive(
        script: Vec<RawEvent>,
        frames: u32,
    ) -> (DrivenApp<super::Materials>, Vec<(FrameStats, bool)>) {
        let seen = Rc::new(RefCell::new(Vec::new()));
        let sink = seen.clone();
        probe_frames(Some(Box::new(move |stats, uploaded| {
            sink.borrow_mut().push((*stats, uploaded));
        })));
        let app = drive_scripted_frames::<super::Materials>(script, STEP, frames);
        probe_frames(None);
        let seen = seen.borrow().clone();
        (app, seen)
    }

    /// The shown node under `id` labelled `label`.
    fn find(store: &NodeStore, id: NodeId, label: &str) -> Option<NodeId> {
        if store.hidden(id) {
            return None;
        }
        if store
            .semantics(id)
            .is_some_and(|s| s.label.as_deref() == Some(label))
        {
            return Some(id);
        }
        let kids: Vec<_> = store.arena().children(id).collect();
        kids.into_iter().find_map(|kid| find(store, kid, label))
    }

    fn center_of(app: &DrivenApp<super::Materials>, label: &str) -> (f32, f32) {
        let store = app.store();
        let node = find(store, app.root().expect("mounted"), label).expect(label);
        let r = store.world(node);
        (r.x + r.w * 0.5, r.y + r.h * 0.5)
    }

    /// The gallery's frame, as `golden/materials.png` holds it.
    #[test]
    fn the_gallery_frame_matches_its_golden() {
        let (app, _) = drive(vec![redraw()], 6);
        let (w, h, bgra) = app.pixels().expect("the window has a surface");
        let image = Image::from_bgra8(w, h, &bgra);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("golden/materials.png");
        if std::env::var_os("BLESS").is_some() {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, image.to_png()).unwrap();
            return;
        }
        let golden = Image::from_png(&std::fs::read(&path).expect("the golden exists; BLESS=1"))
            .expect("the golden decodes");
        let comparison = compare(&golden, &image, 0.0);
        assert!(
            comparison.same_size && comparison.differing == 0,
            "the frame departs from its golden: {comparison:?}"
        );
    }

    /// Over the moving picture each frame reads the backdrop once per glass,
    /// each glass one composite — the pills melted into one — over no more
    /// than the glass and the reach of its blur, never the whole window.
    #[test]
    fn each_glass_is_one_pass_over_a_capture_of_its_own_area() {
        let (app, seen) = drive(vec![redraw()], 6);
        let (w, h, _) = app.pixels().expect("a surface");
        let (last, _) = *seen.last().expect("frames");
        assert!(seen.len() >= 6, "the picture moves: every frame draws");
        // Background blur, glassmorphism, acrylic, liquid glass, its pills.
        assert_eq!(last.material_composites, 5, "{last:?}");
        assert!(last.backdrop_captures <= 5, "{last:?}");
        assert!(
            last.backdrop_capture_pixels < (w * h) as usize,
            "captures stay to the glass: {last:?}"
        );
        // Paint data alone draws skeuomorphism and Aqua: the blurred text
        // shadow is the one offscreen layer besides the glass.
        assert!(last.layers_planned <= 2, "{last:?}");
    }

    /// Held still, the picture stops drawing; a press lights the liquid
    /// glass, and once its glow covers the glass nothing draws until it is
    /// let go.
    #[test]
    fn a_still_picture_and_a_held_glow_draw_nothing() {
        let (app, _) = drive(vec![redraw()], 3);
        let toggle = center_of(&app, "Moving picture");
        let mut script = vec![redraw()];
        script.extend(click(toggle));
        let (app, seen) = drive(script.clone(), 60);
        assert!(
            seen.len() < 60 && !app.wants_animation(),
            "the switch settles, then nothing draws: {} frames",
            seen.len()
        );

        let glass = center_of(&app, "Liquid glass");
        script.push(mouse(glass, PointerButtons::NONE, PointerPhase::Moved));
        script.push(mouse(glass, PointerButtons::PRIMARY, PointerPhase::Down));
        script.extend((0..6).map(|_| redraw()));
        let (app, seen) = drive(script, 60);
        assert!(
            seen.len() < 60 && !app.wants_animation(),
            "held over the glass, the glow waits: {} frames",
            seen.len()
        );
        let (lit, _) = *seen.last().expect("frames");
        assert_eq!(lit.shader_instances, 0, "the picture stays still: {lit:?}");
        assert_eq!(lit.material_composites, 5, "{lit:?}");
    }
}
