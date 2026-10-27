// The app's shell: a toolbar over the router view. The title and the back
// button follow the route on top; the toolbar pushes pages; while the draft is
// being edited, a system back action keeps the page.
import mail::Route;

component Shell {
    input nav: Navigator<Route>;
    state editing = false;
    state kept = 0;

    computed title: String = match nav.current() {
        Route::inbox => "Inbox",
        Route::message { id } => format("Message {id}", id: id),
        Route::settings => "Settings",
    };

    view {
        Column {
            width: 100%;
            height: 100%;
            Row {
                height: 48dp;
                padding: 8dp;
                gap: 8dp;
                Button {
                    width: 80dp;
                    height: 32dp;
                    text: "Back";
                    visible: nav.can_pop();
                    on click { let _ = nav.pop(); }
                }
                Text { width: 160dp; height: 24dp; text: title; }
                Button {
                    width: 120dp;
                    height: 32dp;
                    text: "Message 5";
                    on click { nav.push(Route::message { id: 5 }); }
                }
                Button {
                    width: 120dp;
                    height: 32dp;
                    text: "Settings";
                    on click { nav.replace(Route::settings); }
                }
                Toggle { label: "Editing"; bind checked <=> editing; }
                Text { width: 120dp; height: 24dp; text: format("{n} kept", n: kept); }
            }
            RouterView {
                navigator: nav;
                on back_requested(e) {
                    if editing {
                        kept += 1;
                        e.prevent_default();
                    }
                }
            }
        }
    }
}
