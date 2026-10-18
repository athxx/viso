//! The counter with a 10 000-row `PortalList` under it, each row a label, in
//! the pattern of Makepad's `examples/portallist_hit`: the Makepad side of the
//! `list` scene.

pub use makepad_widgets;

use makepad_widgets::*;

#[path = "harness.rs"]
mod harness;

#[global_allocator]
static ALLOCATOR: harness::Counting = harness::Counting;

app_main!(App);

pub const ROW_COUNT: usize = 10_000;

script_mod! {
    use mod.prelude.widgets.*
    use mod.widgets.*

    let state = {
        counter: 0
    }
    mod.state = state

    mod.widgets.RowListBase = #(RowList::register_widget(vm))
    mod.widgets.RowList = set_type_default() do mod.widgets.RowListBase{
        width: Fill
        height: Fill
        flow: Down

        list := PortalList{
            width: Fill
            height: Fill

            Row := View{
                width: Fill
                height: 24
                title := Label{
                    width: Fill
                    text: ""
                }
            }
        }
    }

    startup() do #(App::script_component(vm)){
        ui: Root{
            on_startup:||{
                ui.main_view.render()
            }
            main_window := Window{
                window.inner_size: vec2(420, 600)
                body +: {
                    main_view := View{
                        width: Fill
                        height: Fit
                        flow: Down
                        spacing: 12
                        align: Center
                        on_render: ||{
                            counter_label := Label{
                                text: "Count: " + state.counter
                                draw_text.text_style.font_size: 24
                            }
                        }
                    }
                    increment_button := Button{
                        text: "Increment"
                    }
                    rows := mod.widgets.RowList{}
                }
            }
        }
    }
}

#[derive(Script, ScriptHook, Widget)]
pub struct RowList {
    #[deref]
    view: View,
}

impl Widget for RowList {
    fn draw_walk(&mut self, cx: &mut Cx2d, scope: &mut Scope, walk: Walk) -> DrawStep {
        while let Some(item) = self.view.draw_walk(cx, scope, walk).step() {
            if let Some(mut list) = item.borrow_mut::<PortalList>() {
                list.set_item_range(cx, 0, ROW_COUNT);
                while let Some(index) = list.next_visible_item(cx) {
                    if index >= ROW_COUNT {
                        continue;
                    }
                    let item = list.item(cx, index, id!(Row));
                    item.label(cx, ids!(title))
                        .set_text(cx, &format!("Row {index}"));
                    item.draw_all(cx, &mut Scope::empty());
                }
            }
        }
        DrawStep::done()
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event, scope: &mut Scope) {
        self.view.handle_event(cx, event, scope);
    }
}

#[derive(Script, ScriptHook)]
pub struct App {
    #[live]
    ui: WidgetRef,
}

impl MatchEvent for App {
    fn handle_actions(&mut self, cx: &mut Cx, actions: &Actions) {
        if self.ui.button(cx, ids!(increment_button)).clicked(actions) {
            script_eval!(cx,{
                mod.state.counter += 1
                ui.main_view.render()
            });
        }
    }
}

impl AppMain for App {
    fn script_mod(vm: &mut ScriptVm) -> ScriptValue {
        crate::makepad_widgets::script_mod(vm);
        self::script_mod(vm)
    }

    fn handle_event(&mut self, cx: &mut Cx, event: &Event) {
        if let Event::Startup = event {
            harness::listen();
        }
        self.match_event(cx, event);
        self.ui.handle_event(cx, event, &mut Scope::empty());
    }
}
