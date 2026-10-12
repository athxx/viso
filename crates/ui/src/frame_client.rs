//! What runs on its own clock inside a window: a client a mounted view
//! starts — a game session — that the frame loop advances once per frame by
//! the frame's wall delta and hands the keys no UI node takes.
//!
//! A client is owned by a node: it lives while that node does and is
//! dropped at the first frame after the node is freed. While a node holds
//! keyboard focus the keys go to the UI, and every client releases what it
//! holds (a key pressed before the focus moved is released, not stuck).

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use crate::component::NodeStore;
use crate::input::Key;
use crate::node::NodeId;

/// A client advanced by the window's frame loop.
pub trait FrameClient: Any {
    /// Advances by `dt` wall seconds.
    fn frame(&mut self, dt: f64);

    /// Key `key` went down or up with no UI node focused; whether the
    /// client takes it.
    fn key(&mut self, key: Key, pressed: bool) -> bool;

    /// Releases every key and button it holds: the window lost focus, or a
    /// UI node took keyboard focus.
    fn release_input(&mut self);

    /// Whether it needs the next frame, as a running clock does.
    fn wants_frame(&self) -> bool;

    /// The client, to reach its concrete type.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// A shared client.
pub type SharedClient = Rc<RefCell<dyn FrameClient>>;

/// The frame clients of a store. Cold: empty in a window without one.
#[derive(Default)]
pub(crate) struct FrameClients {
    clients: Vec<(NodeId, SharedClient)>,
    /// Whether the clients hold input: cleared when the UI takes the keys.
    holding: bool,
}

impl FrameClients {
    pub(crate) fn clear(&mut self) {
        self.clients.clear();
        self.holding = false;
    }
}

impl NodeStore {
    /// Registers `client`, owned by `owner`, replacing a client `owner`
    /// already has.
    pub fn add_frame_client(&mut self, owner: NodeId, client: SharedClient) {
        let clients = &mut self.frame_clients_mut().clients;
        clients.retain(|(node, _)| *node != owner);
        clients.push((owner, client));
    }

    /// The client `owner` owns.
    pub fn frame_client(&self, owner: NodeId) -> Option<&SharedClient> {
        self.frame_clients()
            .clients
            .iter()
            .find(|(node, _)| *node == owner)
            .map(|(_, client)| client)
    }

    /// Moves the client `from` owns to `to`, as a reload that rebuilds a
    /// view's root does.
    pub fn move_frame_client(&mut self, from: NodeId, to: NodeId) {
        if from == to {
            return;
        }
        let clients = &mut self.frame_clients_mut().clients;
        if clients.iter().any(|(node, _)| *node == from) {
            clients.retain(|(node, _)| *node != to);
            for (node, _) in clients.iter_mut() {
                if *node == from {
                    *node = to;
                }
            }
        }
    }

    /// Drops the client `owner` owns.
    pub fn remove_frame_client(&mut self, owner: NodeId) {
        self.frame_clients_mut()
            .clients
            .retain(|(node, _)| *node != owner);
    }

    /// Advances every client by `dt`, first dropping those whose owner is
    /// gone, and releasing their input if a UI node holds keyboard focus.
    pub fn run_frame_clients(&mut self, dt: f64) {
        if self.frame_clients().clients.is_empty() {
            return;
        }
        let (arena, clients) = self.arena_and_frame_clients();
        clients.clients.retain(|(node, _)| arena.is_live(*node));
        if self.focused().is_some() {
            self.release_client_input();
        }
        for (_, client) in &self.frame_clients().clients {
            if let Ok(mut client) = client.try_borrow_mut() {
                client.frame(dt);
            }
        }
    }

    /// Hands key `key` to the clients, the first taking it winning; whether
    /// one did. Called only for a key no UI node is focused to take.
    pub fn frame_clients_key(&mut self, key: Key, pressed: bool) -> bool {
        let clients = self.frame_clients_mut();
        clients.holding |= pressed;
        clients.clients.iter().any(|(_, client)| {
            client
                .try_borrow_mut()
                .is_ok_and(|mut client| client.key(key, pressed))
        })
    }

    /// Releases every client's input: the window lost focus.
    pub fn release_client_input(&mut self) {
        let clients = self.frame_clients_mut();
        if !std::mem::take(&mut clients.holding) {
            return;
        }
        for (_, client) in &clients.clients {
            if let Ok(mut client) = client.try_borrow_mut() {
                client.release_input();
            }
        }
    }

    /// Whether a client needs the next frame.
    pub fn frame_clients_want_frame(&self) -> bool {
        self.frame_clients()
            .clients
            .iter()
            .any(|(_, client)| client.try_borrow().is_ok_and(|client| client.wants_frame()))
    }

    /// The number of clients registered.
    pub fn frame_client_count(&self) -> usize {
        self.frame_clients().clients.len()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::component::BuildCx;
    use crate::component::LeafStyle;

    #[derive(Default)]
    struct Clock {
        elapsed: f64,
        keys: Vec<(Key, bool)>,
        released: Cell<u32>,
    }

    impl FrameClient for Clock {
        fn frame(&mut self, dt: f64) {
            self.elapsed += dt;
        }
        fn key(&mut self, key: Key, pressed: bool) -> bool {
            self.keys.push((key, pressed));
            true
        }
        fn release_input(&mut self) {
            self.released.set(self.released.get() + 1);
        }
        fn wants_frame(&self) -> bool {
            true
        }
        fn as_any_mut(&mut self) -> &mut dyn Any {
            self
        }
    }

    #[test]
    fn a_client_runs_while_its_owner_lives() {
        let mut store = NodeStore::new();
        let owner = BuildCx::new(&mut store).leaf(LeafStyle::default()).id();
        let clock = Rc::new(RefCell::new(Clock::default()));
        store.add_frame_client(owner, clock.clone());
        assert!(store.frame_clients_want_frame());

        store.run_frame_clients(0.5);
        store.run_frame_clients(0.25);
        assert_eq!(clock.borrow().elapsed, 0.75);
        assert!(store.frame_clients_key(Key::Space, true));
        assert_eq!(clock.borrow().keys, [(Key::Space, true)]);
        store.release_client_input();
        store.release_client_input();
        assert_eq!(clock.borrow().released.get(), 1, "released once per hold");

        let mut effects = crate::reactive::EffectStore::new();
        store.free_tree(owner, &mut effects, &mut Vec::new());
        store.run_frame_clients(1.0);
        assert_eq!(store.frame_client_count(), 0, "dropped with its owner");
        assert_eq!(clock.borrow().elapsed, 0.75);
    }

    #[test]
    fn a_client_moves_to_a_rebuilt_root() {
        let mut store = NodeStore::new();
        let (old, new) = {
            let mut cx = BuildCx::new(&mut store);
            (
                cx.leaf(LeafStyle::default()).id(),
                cx.leaf(LeafStyle::default()).id(),
            )
        };
        store.add_frame_client(old, Rc::new(RefCell::new(Clock::default())));
        store.move_frame_client(old, new);
        assert!(store.frame_client(old).is_none());
        assert!(store.frame_client(new).is_some());
    }
}
