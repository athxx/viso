// The app's shell: a toolbar over the router view. The title and the back
// button follow the route on top; the toolbar pushes pages; while the draft is
// being edited, a system back action keeps the page.
import mail::Route

component Shell {
    input nav: Navigator<Route>
    state editing = false
    state kept = 0

    computed title: String = match nav.current() {
        Route::inbox => "Inbox"
        Route::message { id } => "Message {id}"
        Route::settings => "Settings"
    }

    view {
        Column {
            Row {
                width: fill
                padding: 8dp
                gap: 8dp
                align: center
                Button("Back") {
                    visible: nav.can_pop()
                    on click { let _ = nav.pop() }
                }
                Text(title)
                Button("Message 5") { on click { nav.push(Route::message { id: 5 }) } }
                Button("Settings") { on click { nav.replace(Route::settings) } }
                Toggle("Editing") { bind checked <=> editing }
                Text("{kept} kept")
            }
            RouterView {
                navigator: nav
                on back_requested(e) {
                    if editing {
                        kept += 1
                        e.prevent_default()
                    }
                }
            }
        }
    }
}
