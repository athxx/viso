//! What runs on its own clock inside a window: a client a mounted view
//! starts — a game session — that the frame loop advances once per frame by
//! the frame's wall delta and hands the keys no UI node takes, and the
//! gamepads of the window holding input focus.
//!
//! A client is owned by a node: it lives while that node does and is
//! dropped at the first frame after the node is freed. While a node holds
//! keyboard focus the keys go to the UI, and every client releases what it
//! holds (a key pressed before the focus moved is released, not stuck) —
//! unless the focused node shows a scene: a focused game viewport takes the
//! keys for the game.

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use viso_runtime::{GamepadEvent, Rumble};

use crate::component::NodeStore;
use crate::content::Content;
use crate::input::Key;
use crate::node::NodeId;
use crate::scenes::SceneTarget;
use crate::state::StateStore;

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

    /// A gamepad changed or went away while the client's window holds
    /// input focus and no UI node takes the keys; a pad that went away is
    /// reported whatever holds focus. Default: gamepads are ignored.
    fn gamepad(&mut self, event: &GamepadEvent) {
        let _ = event;
    }

    /// The rumble its frame asked the window's gamepads to play, taken
    /// once. Default: none.
    fn take_rumble(&mut self) -> Option<Rumble> {
        None
    }

    /// Whether it needs the next frame, as a running clock does.
    fn wants_frame(&self) -> bool;

    /// Fills the 3D scenes under its owner `owner` after its frame: `fill`
    /// visits each, and a target it reports changed is committed and
    /// repainted.
    fn fill_scenes(&mut self, owner: NodeId, scenes: &mut SceneFill<'_>) {
        let _ = (owner, scenes);
    }

    /// Writes what its frame changed that the UI reads into `states`, as the
    /// frame's transaction begins to settle.
    fn settle(&mut self, states: &mut StateStore) {
        let _ = states;
    }

    /// The app went to the background, or the client's window is about to
    /// close: stop what plays in real time. Default: nothing plays.
    fn suspend(&mut self) {}

    /// The app is back in the foreground: play again what
    /// [`suspend`](Self::suspend) stopped, and nothing else. Default:
    /// nothing plays.
    fn resume(&mut self) {}

    /// The client, to reach its concrete type.
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// The scenes a frame client fills.
pub struct SceneFill<'a> {
    arena: &'a crate::node::NodeArena,
    scenes: &'a mut crate::scenes::SceneSlots,
}

impl SceneFill<'_> {
    /// Calls `fill` with every scene whose node lies under `owner` (or is
    /// it), committing each it returns `true` for.
    pub fn under(&mut self, owner: NodeId, mut fill: impl FnMut(SceneTarget<'_>) -> bool) {
        self.scenes.fill_under(self.arena, owner, &mut fill);
    }
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
        if !self.keys_reach_clients() {
            self.release_client_input();
        }
        for (_, client) in &self.frame_clients().clients {
            if let Ok(mut client) = client.try_borrow_mut() {
                client.frame(dt);
            }
        }
        if self.scenes().is_empty() {
            return;
        }
        let (arena, scenes, clients) = self.scenes_and_frame_clients();
        let mut fill = SceneFill { arena, scenes };
        for (owner, client) in &clients.clients {
            if let Ok(mut client) = client.try_borrow_mut() {
                client.fill_scenes(*owner, &mut fill);
            }
        }
        self.mark_committed_scenes();
    }

    /// Tells every client the app went to the background or the window is
    /// about to close.
    pub(crate) fn suspend_frame_clients(&mut self) {
        for (_, client) in &self.frame_clients().clients {
            if let Ok(mut client) = client.try_borrow_mut() {
                client.suspend();
            }
        }
    }

    /// Tells every client the app is back in the foreground.
    pub(crate) fn resume_frame_clients(&mut self) {
        for (_, client) in &self.frame_clients().clients {
            if let Ok(mut client) = client.try_borrow_mut() {
                client.resume();
            }
        }
    }

    /// Lets every client write what its frame changed into `states`.
    pub fn settle_frame_clients(&mut self, states: &mut StateStore) {
        for (_, client) in &self.frame_clients().clients {
            if let Ok(mut client) = client.try_borrow_mut() {
                client.settle(states);
            }
        }
    }

    /// Whether the store has a frame client.
    pub fn has_frame_clients(&self) -> bool {
        !self.frame_clients().clients.is_empty()
    }

    /// Whether keys go to the clients rather than the UI: no node holds
    /// keyboard focus, or the one that does shows a scene.
    pub fn keys_reach_clients(&self) -> bool {
        match self.focused() {
            None => true,
            Some(node) => matches!(self.content_payload(node), Some(Content::Scene { .. })),
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

    /// Hands every client what a gamepad did. Called for a change only while
    /// the keys reach the clients.
    pub fn frame_clients_gamepad(&mut self, event: &GamepadEvent) {
        let clients = self.frame_clients_mut();
        clients.holding |= matches!(event, GamepadEvent::Changed(..));
        for (_, client) in &clients.clients {
            if let Ok(mut client) = client.try_borrow_mut() {
                client.gamepad(event);
            }
        }
    }

    /// Takes the rumble the clients' frames asked for: the last client's
    /// that asked.
    pub fn take_frame_client_rumble(&mut self) -> Option<Rumble> {
        let mut rumble = None;
        for (_, client) in &self.frame_clients().clients {
            if let Ok(mut client) = client.try_borrow_mut()
                && let Some(asked) = client.take_rumble()
            {
                rumble = Some(asked);
            }
        }
        rumble
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
        pads: Vec<GamepadEvent>,
        released: Cell<u32>,
        rumble: Option<Rumble>,
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
        fn gamepad(&mut self, event: &GamepadEvent) {
            self.pads.push(event.clone());
        }
        fn take_rumble(&mut self) -> Option<Rumble> {
            self.rumble.take()
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
        let pad = viso_runtime::GamepadId(1);
        let moved = GamepadEvent::Changed(pad, viso_runtime::GamepadState::default());
        store.frame_clients_gamepad(&moved);
        store.frame_clients_gamepad(&GamepadEvent::Disconnected(pad));
        assert_eq!(
            clock.borrow().pads,
            [moved, GamepadEvent::Disconnected(pad)]
        );
        store.release_client_input();
        assert_eq!(clock.borrow().released.get(), 2, "a pad's state is held");
        let rumble = Rumble {
            strong: 1.0,
            weak: 0.5,
            duration: std::time::Duration::from_millis(100),
        };
        clock.borrow_mut().rumble = Some(rumble);
        assert_eq!(store.take_frame_client_rumble(), Some(rumble));
        assert_eq!(store.take_frame_client_rumble(), None, "taken once");

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
