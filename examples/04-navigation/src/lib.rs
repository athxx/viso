//! A mail app's navigation: the routes are a Rust enum, the router view keeps
//! a page per route on its back stack, and the `.vs` shell reads and drives
//! it as a `Navigator<Route>`. A `mail://` link opens its page over the inbox,
//! the system back action pops a page unless the shell keeps it, and the
//! inbox keeps its state under every page pushed over it.

use viso::native::{NativeData, NativeLibrary};
use viso::prelude::*;

/// A page of the app.
#[derive(Debug, Clone, PartialEq, NativeData)]
#[native(path = "mail::Route")]
pub enum Route {
    Inbox,
    Message { id: i64 },
    Settings,
}

impl Route {
    /// The route a `mail://` link names.
    pub fn parse(url: &str) -> Option<Route> {
        let path = url.strip_prefix("mail://")?;
        Some(match path.split_once('/') {
            Some(("message", id)) => Route::Message {
                id: id.parse().ok()?,
            },
            None if path == "settings" => Route::Settings,
            None if path == "inbox" => Route::Inbox,
            _ => return None,
        })
    }
}

/// The app's native libraries: the route enum its views import.
pub static NATIVES: &[&NativeLibrary] = &[&MAIL];

static MAIL: NativeLibrary = NativeLibrary {
    path: "mail",
    version: 1,
    functions: &[],
    types: &[Route::TYPE],
    traits: &[],
    derives: &[],
    widgets: &[],
};

/// The app: the shell over the router view, the navigator kept for links.
pub struct Mail {
    nav: Option<Navigator<Route>>,
}

impl Application for Mail {
    fn new(_cx: &mut AppCx) -> Self {
        Mail { nav: None }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        let nav = cx.handle();
        self.nav = Some(nav);
        let router = router_view(
            Route::Inbox,
            routes! {
                Route::Inbox => viso::view!("inbox.vs"),
                Route::Message { id } => viso::view!("message.vs", id: *id),
                Route::Settings => viso::view!("settings.vs"),
            },
        )
        .navigator(nav);
        viso::view!("shell.vs", nav: router)(cx);
    }

    fn on_open_url(&mut self, cx: &mut EventCx<'_>, url: &str) {
        let (Some(nav), Some(route)) = (self.nav, Route::parse(url)) else {
            return;
        };
        // A link opens its page over the inbox, whatever showed before.
        nav.reset(cx, Route::Inbox);
        if route != Route::Inbox {
            nav.push(cx, route);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn schema() {
        viso::native::check(super::NATIVES, "natives.json");
    }
}
