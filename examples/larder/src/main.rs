//! Larder: a kitchen companion written in `.vs`.

use viso::prelude::*;

struct Larder;

impl Application for Larder {
    fn new(cx: &mut AppCx) -> Self {
        cx.use_packaged_fonts(viso::packaged_fonts!());
        Larder
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            title: "Larder".to_owned(),
            size: (1100.0, 720.0),
            ..WindowConfig::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        viso::view!("app.vs")(cx);
    }
}

fn main() {
    viso::run::<Larder>();
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use viso::__test_support::{DrivenApp, drive_scripted_frames};
    use viso::platform::{
        Modifiers, PointerButtons, PointerPhase, RawEvent, RawPointer, RawText, WindowId,
    };
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

    /// Frames for half a second: the recipe box arrives and transitions
    /// settle.
    fn settle() -> Vec<RawEvent> {
        (0..32).flat_map(|_| [RawEvent::Wakeup, redraw()]).collect()
    }

    /// A click at `at`, and frames for what it opens to settle.
    fn click(at: (f32, f32)) -> Vec<RawEvent> {
        let mut out = vec![
            mouse(at, PointerButtons::NONE, PointerPhase::Moved),
            mouse(at, PointerButtons::PRIMARY, PointerPhase::Down),
            redraw(),
            mouse(at, PointerButtons::PRIMARY, PointerPhase::Up),
        ];
        out.extend(settle());
        out
    }

    fn drive(script: &[RawEvent]) -> DrivenApp<super::Larder> {
        drive_scripted_frames::<super::Larder>(script.to_vec(), STEP, script.len() as u32 + 8)
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

    fn shows(app: &DrivenApp<super::Larder>, label: &str) -> bool {
        app.root()
            .is_some_and(|root| find(app.store(), root, label).is_some())
    }

    fn center_of(app: &DrivenApp<super::Larder>, label: &str) -> (f32, f32) {
        let store = app.store();
        let node = find(store, app.root().expect("mounted"), label)
            .unwrap_or_else(|| panic!("nothing shown is labelled `{label}`"));
        let r = store.world(node);
        (r.x + r.w * 0.5, r.y + r.h * 0.5)
    }

    /// The script so far, then a click on what it shows labelled `label`.
    fn then_click(script: &mut Vec<RawEvent>, label: &str) {
        let at = center_of(&drive(script), label);
        script.extend(click(at));
    }

    #[test]
    fn each_section_of_the_sidebar_shows_its_page() {
        let script = settle();
        assert!(
            shows(&drive(&script), "16 recipes"),
            "the recipe box arrived"
        );
        for (section, title) in [
            ("Shopping", "Shopping list"),
            ("Week", "This week"),
            ("Timers", "Timers"),
            ("Settings", "Settings"),
        ] {
            let mut script = script.clone();
            then_click(&mut script, section);
            let app = drive(&script);
            assert!(shows(&app, title), "`{section}` shows `{title}`");
        }
    }

    #[test]
    fn a_recipe_opens_over_its_list_and_goes_back_to_it() {
        let mut script = settle();
        then_click(&mut script, "Shakshuka");
        let app = drive(&script);
        assert!(shows(&app, "Ingredients") && shows(&app, "Method"));
        then_click(&mut script, "All recipes");
        let app = drive(&script);
        assert!(shows(&app, "16 recipes"), "back on the list");
    }

    #[test]
    fn ticking_an_item_counts_it_in_the_basket() {
        let mut script = settle();
        then_click(&mut script, "Shopping");
        assert!(shows(&drive(&script), "Clear 1 bought"));
        then_click(&mut script, "Bananas");
        let app = drive(&script);
        assert!(shows(&app, "Clear 2 bought") && shows(&app, "2 things to buy"));
    }

    /// The script so far, then `text` typed into the field labelled `label`.
    fn then_type(script: &mut Vec<RawEvent>, label: &str, text: &str) {
        then_click(script, label);
        script.push(RawEvent::Text(RawText {
            window: WIN,
            text: text.to_owned(),
        }));
        script.extend(settle());
    }

    #[test]
    fn an_item_typed_into_the_form_joins_the_list() {
        let mut script = settle();
        then_click(&mut script, "Shopping");
        then_type(&mut script, "What to buy", "Lemons");
        then_click(&mut script, "Add");
        let app = drive(&script);
        assert!(shows(&app, "Lemons") && shows(&app, "4 things to buy"));
    }

    #[test]
    fn a_recipe_of_ones_own_is_checked_saved_and_opened() {
        // A window tall enough to show the whole form.
        let mut script = vec![RawEvent::Resized {
            window: WIN,
            width: 2200,
            height: 3200,
        }];
        script.extend(settle());
        then_click(&mut script, "New recipe");
        then_click(&mut script, "Save recipe");
        let app = drive(&script);
        assert!(
            shows(&app, "Give the recipe a name."),
            "an empty form is refused"
        );
        assert!(shows(&app, "Add at least one ingredient."));
        then_type(&mut script, "Name", "Cheese toast");
        then_type(&mut script, "Ingredient", "Cheddar");
        then_type(&mut script, "What to do", "Grill it on the bread.");
        let app = drive(&script);
        assert!(
            !shows(&app, "Give the recipe a name."),
            "the issues clear as typed"
        );
        then_click(&mut script, "Save recipe");
        let app = drive(&script);
        assert!(shows(&app, "Cheese toast") && shows(&app, "Grill it on the bread."));
        then_click(&mut script, "All recipes");
        assert!(shows(&drive(&script), "17 recipes"), "it joins the list");
    }

    #[test]
    fn a_narrow_window_trades_the_sidebar_for_a_tab_bar() {
        let wide = drive(&settle());
        assert!(shows(&wide, "What to cook, and what to buy for it"));
        let mut script = vec![RawEvent::Resized {
            window: WIN,
            width: 800,
            height: 1440,
        }];
        script.extend(settle());
        let narrow = drive(&script);
        assert!(
            !shows(&narrow, "What to cook, and what to buy for it"),
            "no sidebar"
        );
        then_click(&mut script, "Timers");
        assert!(
            shows(&drive(&script), "No timers yet"),
            "the tab bar switches pages"
        );
    }

    /// The recipe list, as `golden/recipes.png` holds it.
    #[test]
    fn the_recipe_list_matches_its_golden() {
        let app = drive(&settle());
        let (w, h, bgra) = app.pixels().expect("the window has a surface");
        let image = Image::from_bgra8(w, h, &bgra);
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("golden/recipes.png");
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
}
