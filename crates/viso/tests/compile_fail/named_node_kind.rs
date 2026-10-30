use viso::ui::{Handle, kind};

viso::component! {
    Form {
        view {
            Column {
                node save_button: Button("Save")
            }
        }
    }
}

fn main() {
    let _ = |cx: &mut viso::ui::BuildCx<'_>| {
        let (_, view) = Form::build(cx);
        let _: Handle<kind::Text> = view.nodes.save_button;
    };
}
