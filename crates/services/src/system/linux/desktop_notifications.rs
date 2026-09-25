//! Notifications through the desktop's `org.freedesktop.Notifications`
//! server. The server numbers notifications itself; the worker keeps the
//! number each app id was given, to replace and to withdraw by it.

use std::collections::HashMap;

use zbus::zvariant::Value;

use super::bus::{Worker, failure};
use crate::notifications::{Notification, Notifications};
use crate::reply::Reply;

const SERVER: &str = "org.freedesktop.Notifications";
const SERVER_PATH: &str = "/org/freedesktop/Notifications";

/// The server's number for each notification id shown.
type Shown = HashMap<String, u32>;

pub(super) struct DesktopNotifications {
    app: String,
    worker: Worker<Shown>,
}

impl DesktopNotifications {
    pub(super) fn new(app: &str) -> Self {
        Self {
            app: app.to_owned(),
            worker: Worker::new("viso-notifications"),
        }
    }
}

impl Notifications for DesktopNotifications {
    fn notify(&self, notification: Notification) -> Reply<()> {
        let app = self.app.clone();
        self.worker.ask(move |connection, shown: &mut Shown| {
            let replaces = shown.get(&notification.id).copied().unwrap_or(0);
            let actions: [&str; 0] = [];
            let hints: HashMap<&str, Value<'_>> = HashMap::new();
            let number: u32 = connection
                .call_method(
                    Some(SERVER),
                    SERVER_PATH,
                    Some(SERVER),
                    "Notify",
                    &(
                        app.as_str(),
                        replaces,
                        "",
                        notification.title.as_str(),
                        notification.body.as_str(),
                        &actions[..],
                        hints,
                        -1_i32,
                    ),
                )
                .map_err(failure)?
                .body()
                .deserialize()
                .map_err(failure)?;
            shown.insert(notification.id, number);
            Ok(())
        })
    }

    fn withdraw(&self, id: &str) {
        let id = id.to_owned();
        self.worker.run(move |connection, shown: &mut Shown| {
            let (Ok(connection), Some(number)) = (connection, shown.remove(&id)) else {
                return;
            };
            // A notification the user already closed is gone either way.
            let _ = connection.call_method(
                Some(SERVER),
                SERVER_PATH,
                Some(SERVER),
                "CloseNotification",
                &number,
            );
        });
    }
}
