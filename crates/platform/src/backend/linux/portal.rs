//! System appearance from the XDG desktop portal's Settings interface over
//! the session bus. A background thread owns the D-Bus connection, reads the
//! initial values and then follows `SettingChanged`, waking the event loop
//! with each new [`Appearance`].

use std::sync::mpsc;
use std::time::Duration;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

use super::translate;
use super::{Wake, Waker};
use crate::event::Appearance;

const APPEARANCE: &str = "org.freedesktop.appearance";
const GNOME_INTERFACE: &str = "org.gnome.desktop.interface";

/// How long startup waits for the portal's first answer before painting
/// with the default appearance (the thread keeps going and reports later).
const FIRST_READ_BUDGET: Duration = Duration::from_millis(250);

/// Start following the portal; returns the appearance to start with.
pub(crate) fn spawn(waker: Waker) -> Appearance {
    let (first_tx, first_rx) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("viso-portal".into())
        .spawn(move || follow(&waker, &first_tx));
    if spawned.is_err() {
        return Appearance::default();
    }
    first_rx.recv_timeout(FIRST_READ_BUDGET).unwrap_or_default()
}

fn follow(waker: &Waker, first: &mpsc::SyncSender<Appearance>) {
    let Some(settings) = Settings::connect() else {
        let _ = first.send(Appearance::default());
        return;
    };
    // Subscribe before the first read so no change slips between them.
    let changes = settings.proxy.receive_signal("SettingChanged").ok();
    let mut current = settings.read();
    if first.try_send(current).is_err() {
        // Startup stopped waiting: deliver the value as a change instead.
        waker.send(Wake::Appearance(current));
    }
    let Some(changes) = changes else { return };
    for message in changes {
        let Ok((namespace, _key, _value)) =
            message.body().deserialize::<(String, String, OwnedValue)>()
        else {
            continue;
        };
        if namespace != APPEARANCE && namespace != GNOME_INTERFACE {
            continue;
        }
        let next = settings.read();
        if next != current {
            current = next;
            if !waker.send(Wake::Appearance(next)) {
                return;
            }
        }
    }
}

struct Settings {
    proxy: Proxy<'static>,
}

impl Settings {
    fn connect() -> Option<Self> {
        let connection = Connection::session().ok()?;
        let proxy = Proxy::new_owned(
            connection,
            "org.freedesktop.portal.Desktop",
            "/org/freedesktop/portal/desktop",
            "org.freedesktop.portal.Settings",
        )
        .ok()?;
        Some(Self { proxy })
    }

    fn read(&self) -> Appearance {
        translate::portal_appearance(
            self.value(APPEARANCE, "color-scheme").and_then(as_u32),
            self.value(APPEARANCE, "contrast").and_then(as_u32),
            self.value(GNOME_INTERFACE, "enable-animations")
                .and_then(|v| bool::try_from(&*v).ok()),
        )
    }

    /// One setting: `ReadOne` where the portal has it (version 2), else the
    /// original `Read`, which wraps the value in an extra variant.
    fn value(&self, namespace: &str, key: &str) -> Option<OwnedValue> {
        self.proxy
            .call::<_, _, OwnedValue>("ReadOne", &(namespace, key))
            .or_else(|_| {
                self.proxy
                    .call::<_, _, OwnedValue>("Read", &(namespace, key))
            })
            .ok()
            .and_then(|v| unwrap_variant(&v).try_to_owned().ok())
    }
}

fn unwrap_variant<'a>(mut value: &'a Value<'a>) -> &'a Value<'a> {
    while let Value::Value(inner) = value {
        value = inner;
    }
    value
}

fn as_u32(value: OwnedValue) -> Option<u32> {
    match &*value {
        Value::U32(n) => Some(*n),
        Value::I32(n) => u32::try_from(*n).ok(),
        _ => None,
    }
}
