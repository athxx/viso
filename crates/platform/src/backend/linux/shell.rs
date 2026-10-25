//! The tray icon on the Linux desktop: an `org.kde.StatusNotifierItem` with
//! its menu served as `com.canonical.dbusmenu`, both on the session bus.
//!
//! A background thread owns the bus connection, started by the first tray
//! the app shows, so the UI thread never waits on the bus: each change is a
//! message to that thread, and each click or pick comes back as a wake of
//! the event loop. The item registers with the `StatusNotifierWatcher`, and
//! again whenever a watcher appears (the panel restarted, or started after
//! the app). Where no desktop component hosts items, nothing shows.

use std::collections::HashMap;
use std::sync::mpsc;

use zbus::blocking::{Connection, Proxy};
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Str, Structure};

use super::translate::{TrayMenu, TrayNode, TrayNodeKind, tray_pixmaps};
use super::{Wake, Waker};
use crate::shell::{Tray, TrayEvent};

const ITEM_PATH: &str = "/StatusNotifierItem";
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const MENU_INTERFACE: &str = "com.canonical.dbusmenu";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";

/// The app's side of the bus thread.
pub(crate) struct Shell {
    waker: Waker,
    /// The bus thread's inbox, once a tray started it.
    tx: Option<mpsc::Sender<Ask>>,
    /// Whether a tray is shown, so removing none sends nothing.
    shown: bool,
}

/// Work for the bus thread.
enum Ask {
    /// Show this tray, or remove the one shown.
    Tray(Option<Tray>),
    /// A watcher appeared: register the item with it.
    Register,
}

impl Shell {
    pub(crate) fn new(waker: Waker) -> Shell {
        Shell {
            waker,
            tx: None,
            shown: false,
        }
    }

    pub(crate) fn set_tray(&mut self, tray: Option<&Tray>) {
        if tray.is_none() && !self.shown {
            return;
        }
        self.shown = tray.is_some();
        let tx = self.tx.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel();
            let waker = self.waker.clone();
            let again = tx.clone();
            let _ = std::thread::Builder::new()
                .name("viso-tray".into())
                .spawn(move || serve(&rx, &again, &waker));
            tx
        });
        let _ = tx.send(Ask::Tray(tray.cloned()));
    }
}

/// The bus thread: connects, then applies each ask until the app goes.
fn serve(rx: &mpsc::Receiver<Ask>, again: &mpsc::Sender<Ask>, waker: &Waker) {
    let Ok(conn) = Connection::session() else {
        return;
    };
    let name = format!("{ITEM_INTERFACE}-{}-1", std::process::id());
    follow_watcher(&conn, again.clone());
    let mut shown = false;
    while let Ok(ask) = rx.recv() {
        match ask {
            Ask::Tray(Some(tray)) => {
                if shown {
                    update(&conn, &tray);
                } else if show(&conn, &name, &tray, waker) {
                    shown = true;
                    register(&conn, &name);
                }
            }
            Ask::Tray(None) => {
                if shown {
                    let _ = conn.release_name(name.as_str());
                    let server = conn.object_server();
                    let _ = server.remove::<Item, _>(ITEM_PATH);
                    let _ = server.remove::<DbusMenu, _>(MENU_PATH);
                    shown = false;
                }
            }
            Ask::Register => {
                if shown {
                    register(&conn, &name);
                }
            }
        }
    }
}

/// Serves `tray` and takes the item's name; `false` when the bus refused.
fn show(conn: &Connection, name: &str, tray: &Tray, waker: &Waker) -> bool {
    let server = conn.object_server();
    let item = Item {
        look: Look::of(tray),
        waker: waker.clone(),
    };
    let menu = DbusMenu {
        menu: TrayMenu::build(&tray.menu),
        revision: 1,
        waker: waker.clone(),
    };
    if server.at(ITEM_PATH, item).is_err() || server.at(MENU_PATH, menu).is_err() {
        return false;
    }
    conn.request_name(name).is_ok()
}

/// Shows `tray` in place of the item's look, telling hosts what changed.
fn update(conn: &Connection, tray: &Tray) {
    let server = conn.object_server();
    let look = Look::of(tray);
    if let Ok(item) = server.interface::<_, Item>(ITEM_PATH) {
        let mut item = item.get_mut();
        let old = std::mem::replace(&mut item.look, look.clone());
        drop(item);
        // A changed `ItemIsMenu` has no signal of its own; hosts read every
        // property again on a new icon.
        if old.pixmaps != look.pixmaps || old.item_is_menu != look.item_is_menu {
            emit(conn, ITEM_PATH, ITEM_INTERFACE, "NewIcon", &());
        }
        if old.tooltip != look.tooltip {
            emit(conn, ITEM_PATH, ITEM_INTERFACE, "NewToolTip", &());
            emit(conn, ITEM_PATH, ITEM_INTERFACE, "NewTitle", &());
        }
    }
    let laid = TrayMenu::build(&tray.menu);
    if let Ok(menu) = server.interface::<_, DbusMenu>(MENU_PATH) {
        let mut menu = menu.get_mut();
        if menu.menu != laid {
            menu.menu = laid;
            menu.revision = menu.revision.wrapping_add(1);
            let revision = menu.revision;
            drop(menu);
            emit(
                conn,
                MENU_PATH,
                MENU_INTERFACE,
                "LayoutUpdated",
                &(revision, 0i32),
            );
        }
    }
}

fn emit<B>(conn: &Connection, path: &str, interface: &str, member: &str, body: &B)
where
    B: zbus::export::serde::Serialize + zbus::zvariant::DynamicType,
{
    let _ = conn.emit_signal(None::<&str>, path, interface, member, body);
}

/// Tells the watcher, if one runs, the item is on the bus.
fn register(conn: &Connection, name: &str) {
    let _ = conn.call_method(
        Some(WATCHER),
        WATCHER_PATH,
        Some(WATCHER),
        "RegisterStatusNotifierItem",
        &(name,),
    );
}

/// Asks for a registration each time a watcher takes its name.
fn follow_watcher(conn: &Connection, again: mpsc::Sender<Ask>) {
    let conn = conn.clone();
    let _ = std::thread::Builder::new()
        .name("viso-tray-watch".into())
        .spawn(move || {
            let Ok(bus) = Proxy::new(
                &conn,
                "org.freedesktop.DBus",
                "/org/freedesktop/DBus",
                "org.freedesktop.DBus",
            ) else {
                return;
            };
            let Ok(changes) = bus.receive_signal_with_args("NameOwnerChanged", &[(0, WATCHER)])
            else {
                return;
            };
            for message in changes {
                let Ok((_, _, owner)) = message.body().deserialize::<(String, String, String)>()
                else {
                    continue;
                };
                if !owner.is_empty() && again.send(Ask::Register).is_err() {
                    return;
                }
            }
        });
}

/// An icon at one size: `(width, height, ARGB32 in network byte order)`.
type Pixmap = (i32, i32, Vec<u8>);

/// A tooltip: `(icon name, icon pixmaps, title, description)`.
type ToolTip = (String, Vec<Pixmap>, String, String);

/// What the item shows, as its properties carry it.
#[derive(Clone, PartialEq)]
struct Look {
    pixmaps: Vec<Pixmap>,
    tooltip: String,
    item_is_menu: bool,
}

impl Look {
    fn of(tray: &Tray) -> Look {
        Look {
            pixmaps: tray_pixmaps(&tray.icon),
            tooltip: tray.tooltip.clone(),
            item_is_menu: tray.menu_on_click && !tray.menu.is_empty(),
        }
    }
}

/// The `org.kde.StatusNotifierItem` object.
struct Item {
    look: Look,
    waker: Waker,
}

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    /// A primary click on the item, which hosts send only when it is not a
    /// menu.
    fn activate(&self, _x: i32, _y: i32) {
        if !self.look.item_is_menu {
            self.waker.send(Wake::Tray(TrayEvent::Click));
        }
    }

    fn secondary_activate(&self, _x: i32, _y: i32) {}

    /// The host shows the menu itself, from the `Menu` property.
    fn context_menu(&self, _x: i32, _y: i32) {}

    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(property)]
    fn category(&self) -> String {
        "ApplicationStatus".into()
    }

    #[zbus(property)]
    fn id(&self) -> String {
        app_id()
    }

    #[zbus(property)]
    fn title(&self) -> String {
        if self.look.tooltip.is_empty() {
            app_id()
        } else {
            self.look.tooltip.clone()
        }
    }

    #[zbus(property)]
    fn status(&self) -> String {
        "Active".into()
    }

    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }

    #[zbus(property)]
    fn icon_name(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    fn icon_pixmap(&self) -> Vec<Pixmap> {
        self.look.pixmaps.clone()
    }

    #[zbus(property)]
    fn overlay_icon_name(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    fn overlay_icon_pixmap(&self) -> Vec<Pixmap> {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_icon_name(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    fn attention_icon_pixmap(&self) -> Vec<Pixmap> {
        Vec::new()
    }

    #[zbus(property)]
    fn attention_movie_name(&self) -> String {
        String::new()
    }

    #[zbus(property)]
    fn tool_tip(&self) -> ToolTip {
        (
            String::new(),
            Vec::new(),
            self.look.tooltip.clone(),
            String::new(),
        )
    }

    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        self.look.item_is_menu
    }

    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(MENU_PATH).expect("a valid object path")
    }

    #[zbus(signal)]
    async fn new_title(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
}

/// The name hosts show for the app when the tray has no tooltip: the
/// Flatpak id, or the executable's name.
fn app_id() -> String {
    std::env::var("FLATPAK_ID")
        .ok()
        .filter(|id| !id.is_empty())
        .or_else(|| {
            std::env::current_exe()
                .ok()?
                .file_stem()?
                .to_str()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "viso".into())
}

/// The `com.canonical.dbusmenu` object.
struct DbusMenu {
    menu: TrayMenu,
    revision: u32,
    waker: Waker,
}

/// An entry's properties: `a{sv}`.
type Props = HashMap<String, OwnedValue>;

/// An entry laid out with its children: `(ia{sv}av)`.
type Layout = (i32, Props, Vec<OwnedValue>);

impl DbusMenu {
    fn props(node: &TrayNode, names: &[String]) -> Props {
        let mut props = Props::new();
        let mut put = |name: &str, value: OwnedValue| {
            if names.is_empty() || names.iter().any(|n| n == name) {
                props.insert(name.to_owned(), value);
            }
        };
        let text = |s: &str| OwnedValue::from(Str::from(s.to_owned()));
        match node.kind {
            TrayNodeKind::Root => put("children-display", text("submenu")),
            TrayNodeKind::Separator => put("type", text("separator")),
            TrayNodeKind::Item | TrayNodeKind::Submenu => {
                put("label", text(&node.label));
                if !node.enabled {
                    put("enabled", OwnedValue::from(false));
                }
                if node.kind == TrayNodeKind::Submenu {
                    put("children-display", text("submenu"));
                }
            }
        }
        props
    }

    fn layout(&self, id: i32, depth: i32, names: &[String]) -> Option<Layout> {
        let node = self.menu.node(id)?;
        let children = if depth == 0 {
            Vec::new()
        } else {
            node.children
                .iter()
                .filter_map(|&child| {
                    let (id, props, children) = self.layout(child, depth - 1, names)?;
                    OwnedValue::try_from(Structure::from((id, props, children))).ok()
                })
                .collect()
        };
        Some((id, Self::props(node, names), children))
    }

    fn clicked(&self, id: i32, event: &str) {
        if event == "clicked"
            && let Some(action) = self.menu.action(id)
        {
            self.waker.send(Wake::Menu(action));
        }
    }
}

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    #[zbus(out_args("revision", "layout"))]
    fn get_layout(
        &self,
        parent_id: i32,
        recursion_depth: i32,
        property_names: Vec<String>,
    ) -> zbus::fdo::Result<(u32, Layout)> {
        let layout = self
            .layout(parent_id, recursion_depth, &property_names)
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no entry {parent_id}")))?;
        Ok((self.revision, layout))
    }

    fn get_group_properties(
        &self,
        ids: Vec<i32>,
        property_names: Vec<String>,
    ) -> Vec<(i32, Props)> {
        ids.into_iter()
            .filter_map(|id| Some((id, Self::props(self.menu.node(id)?, &property_names))))
            .collect()
    }

    fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedValue> {
        self.menu
            .node(id)
            .and_then(|node| Self::props(node, std::slice::from_ref(&name)).remove(&name))
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no `{name}` on {id}")))
    }

    fn event(&self, id: i32, event_id: &str, _data: OwnedValue, _timestamp: u32) {
        self.clicked(id, event_id);
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        let mut missing = Vec::new();
        for (id, event, _, _) in events {
            if self.menu.node(id).is_some() {
                self.clicked(id, &event);
            } else {
                missing.push(id);
            }
        }
        missing
    }

    /// The menu is laid out ahead; nothing changes on opening.
    fn about_to_show(&self, _id: i32) -> bool {
        false
    }

    #[zbus(out_args("updates_needed", "id_errors"))]
    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        let missing = ids
            .into_iter()
            .filter(|&id| self.menu.node(id).is_none())
            .collect();
        (Vec::new(), missing)
    }

    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }

    #[zbus(property)]
    fn text_direction(&self) -> String {
        "ltr".into()
    }

    #[zbus(property)]
    fn status(&self) -> String {
        "normal".into()
    }

    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        Vec::new()
    }

    #[zbus(signal)]
    async fn items_properties_updated(
        emitter: &SignalEmitter<'_>,
        updated: Vec<(i32, Props)>,
        removed: Vec<(i32, Vec<String>)>,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn layout_updated(
        emitter: &SignalEmitter<'_>,
        revision: u32,
        parent: i32,
    ) -> zbus::Result<()>;

    #[zbus(signal)]
    async fn item_activation_requested(
        emitter: &SignalEmitter<'_>,
        id: i32,
        timestamp: u32,
    ) -> zbus::Result<()>;
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use zbus::blocking::Connection;
    use zbus::zvariant::{OwnedValue, Structure};

    use super::super::{Wake, wake_channel};
    use super::{ITEM_INTERFACE, ITEM_PATH, MENU_INTERFACE, MENU_PATH, Shell, WATCHER};
    use crate::menu::{Menu, MenuCommandId, SystemAction};
    use crate::shell::{IconImage, Tray, TrayEvent};

    /// A stand-in panel: records each item that registers.
    struct Watcher(Arc<Mutex<Vec<String>>>);

    #[zbus::interface(name = "org.kde.StatusNotifierWatcher")]
    impl Watcher {
        fn register_status_notifier_item(&self, service: String) {
            self.0.lock().unwrap().push(service);
        }
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !done() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::yield_now();
        }
    }

    fn property(bus: &Connection, name: &str, path: &str, iface: &str, prop: &str) -> OwnedValue {
        let reply = bus
            .call_method(
                Some(name),
                path,
                Some("org.freedesktop.DBus.Properties"),
                "Get",
                &(iface, prop),
            )
            .unwrap_or_else(|e| panic!("{prop}: {e}"));
        reply.body().deserialize::<OwnedValue>().unwrap()
    }

    #[test]
    #[ignore = "needs a D-Bus session bus (run under dbus-run-session)"]
    fn the_tray_is_served_on_the_session_bus() {
        let panel = Connection::session().unwrap();
        let registered = Arc::new(Mutex::new(Vec::new()));
        panel
            .object_server()
            .at("/StatusNotifierWatcher", Watcher(registered.clone()))
            .unwrap();
        panel.request_name(WATCHER).unwrap();

        let (waker, wakes) = wake_channel().unwrap();
        let mut shell = Shell::new(waker);
        let icon = IconImage::from_rgba(2, 1, vec![255, 0, 0, 255, 0, 0, 255, 128]).unwrap();
        let tray = Tray {
            tooltip: "Hello".into(),
            menu: vec![
                Menu::item_plain("Say_hi", MenuCommandId(7)),
                Menu::sub("More", [Menu::system(SystemAction::Quit)]),
            ],
            ..Tray::new(icon)
        };
        shell.set_tray(Some(&tray));
        let name = format!("{ITEM_INTERFACE}-{}-1", std::process::id());
        until("registration", || {
            registered.lock().unwrap().contains(&name)
        });

        let pixmaps: Vec<super::Pixmap> =
            property(&panel, &name, ITEM_PATH, ITEM_INTERFACE, "IconPixmap")
                .try_into()
                .unwrap();
        assert_eq!(pixmaps, [(2, 1, vec![255, 255, 0, 0, 128, 0, 0, 255])]);
        let tip: super::ToolTip = property(&panel, &name, ITEM_PATH, ITEM_INTERFACE, "ToolTip")
            .try_into()
            .unwrap();
        assert_eq!(tip.2, "Hello");
        let is_menu: bool = property(&panel, &name, ITEM_PATH, ITEM_INTERFACE, "ItemIsMenu")
            .try_into()
            .unwrap();
        assert!(!is_menu);
        let menu: zbus::zvariant::OwnedObjectPath =
            property(&panel, &name, ITEM_PATH, ITEM_INTERFACE, "Menu")
                .try_into()
                .unwrap();
        assert_eq!(menu.as_str(), MENU_PATH);

        let reply = panel
            .call_method(
                Some(name.as_str()),
                MENU_PATH,
                Some(MENU_INTERFACE),
                "GetLayout",
                &(0i32, -1i32, Vec::<String>::new()),
            )
            .unwrap();
        assert_eq!(
            reply.body().signature().to_string_no_parens(),
            "u(ia{sv}av)"
        );
        let (revision, (root, _, children)): (u32, super::Layout) =
            reply.body().deserialize().unwrap();
        assert_eq!((revision, root, children.len()), (1, 0, 2));
        let first: Structure<'_> = children[0].downcast_ref().unwrap();
        let (id, props, _): super::Layout = first.try_into().unwrap();
        assert_eq!(id, 1);
        let label: String = props["label"].try_clone().unwrap().try_into().unwrap();
        assert_eq!(label, "Say__hi");

        let call = |member: &str, body: &(i32, &str, OwnedValue, u32)| {
            panel
                .call_method(
                    Some(name.as_str()),
                    MENU_PATH,
                    Some(MENU_INTERFACE),
                    member,
                    body,
                )
                .unwrap();
        };
        call("Event", &(1, "clicked", OwnedValue::from(0i32), 0));
        let wake = wakes.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            matches!(
                wake,
                Wake::Menu(super::super::MenuAction::Command(MenuCommandId(7)))
            ),
            "{wake:?}"
        );
        panel
            .call_method(
                Some(name.as_str()),
                ITEM_PATH,
                Some(ITEM_INTERFACE),
                "Activate",
                &(0i32, 0i32),
            )
            .unwrap();
        let wake = wakes.rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(wake, Wake::Tray(TrayEvent::Click)), "{wake:?}");

        shell.set_tray(Some(&Tray {
            tooltip: "Changed".into(),
            ..tray.clone()
        }));
        until("the new tooltip", || {
            let tip: super::ToolTip = property(&panel, &name, ITEM_PATH, ITEM_INTERFACE, "ToolTip")
                .try_into()
                .unwrap();
            tip.2 == "Changed"
        });

        shell.set_tray(None);
        let bus = zbus::blocking::fdo::DBusProxy::new(&panel).unwrap();
        until("the item to leave the bus", || {
            !bus.name_has_owner(name.as_str().try_into().unwrap())
                .unwrap()
        });
    }
}
