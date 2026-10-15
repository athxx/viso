//! The systems a file declares, in the order a scheduler runs them: each
//! one's hooks with the trait hook they implement and their domain, its
//! states with the Simulation snapshot, `@local` and `@persist` sets, and the
//! package's fixed step and game profile.

use std::fmt::Write as _;

use viso_behavior::CollisionDelivery;
use viso_behavior::native::{HookDomain, NativeId, Natives};
use viso_ende::JsonWriter;

use super::{Dump, FileCx};
use crate::frontend::Compiled;

struct Hook {
    /// The trait hook's path, `viso::game::FixedUpdate::fixed_update`.
    path: String,
    domain: &'static str,
    function: String,
    index: u32,
}

struct State {
    name: String,
    slot: u32,
    /// `simulation`, `local` or `presentation`.
    domain: &'static str,
    probe: bool,
    /// The `@persist` key.
    persist: Option<String>,
}

struct System {
    name: String,
    order: usize,
    hooks: Vec<Hook>,
    states: Vec<State>,
}

/// The system IR of one file.
pub(super) struct SystemFile {
    systems: Vec<System>,
    tick_rate: Option<u32>,
    determinism: Option<&'static str>,
    delivery: Option<&'static str>,
}

pub(super) fn systems(cx: &FileCx<'_>, compiled: &Compiled) -> SystemFile {
    let program = &compiled.behavior;
    let natives = cx.package.model.graph.natives();
    let mut systems = Vec::new();
    for (order, system) in program.systems.iter().enumerate() {
        let Some(layout) = program.components.get(system.component as usize) else {
            continue;
        };
        if cx.focus.as_ref().is_some_and(|f| f.name != layout.name) {
            continue;
        }
        let hooks = system
            .hooks
            .iter()
            .map(|&(id, func)| {
                let (path, domain) = hook(natives, id);
                Hook {
                    path,
                    domain,
                    function: program
                        .functions
                        .get(func.0 as usize)
                        .map(|f| f.name.clone())
                        .unwrap_or_default(),
                    index: func.0,
                }
            })
            .collect();
        let states = system
            .states
            .iter()
            .map(|state| State {
                name: state.name.clone(),
                slot: state.slot,
                domain: if system.snapshot.iter().any(|s| s.1 == state.slot) {
                    "simulation"
                } else if system.locals.iter().any(|s| s.1 == state.slot) {
                    "local"
                } else {
                    "presentation"
                },
                probe: state.probe,
                persist: system
                    .persist
                    .iter()
                    .find(|p| p.slot == state.slot)
                    .map(|p| p.key.to_string()),
            })
            .collect();
        systems.push(System {
            name: layout.name.clone(),
            order,
            hooks,
            states,
        });
    }
    let (determinism, delivery) = match program.game_profile {
        Some((determinism, delivery)) => (
            Some(determinism.name()),
            Some(match delivery {
                CollisionDelivery::EventMajor => "event_major",
                CollisionDelivery::ListenerMajor => "listener_major",
            }),
        ),
        None => (None, None),
    };
    SystemFile {
        systems,
        tick_rate: program.tick_rate,
        determinism,
        delivery,
    }
}

/// The path and domain of the trait hook `id`.
fn hook(natives: &Natives, id: NativeId) -> (String, &'static str) {
    for entry in natives.traits() {
        for hook in entry.native_trait.hooks {
            if entry.hook_id(hook.name) == id {
                let domain = match hook.domain {
                    HookDomain::Simulation => "simulation",
                    HookDomain::Presentation => "presentation",
                    HookDomain::Realtime => "realtime",
                };
                return (format!("{}::{}", entry.path, hook.name), domain);
            }
        }
    }
    (format!("{:016x}", id.0), "unknown")
}

impl Dump for SystemFile {
    fn write_text(&self, out: &mut String) {
        if self.systems.is_empty() {
            out.push_str("no systems\n");
            return;
        }
        if let Some(rate) = self.tick_rate {
            let _ = writeln!(out, "tick_rate {rate}");
        }
        if let (Some(determinism), Some(delivery)) = (self.determinism, self.delivery) {
            let _ = writeln!(
                out,
                "game_profile determinism={determinism} contacts={delivery}"
            );
        }
        for system in &self.systems {
            let _ = writeln!(out, "system#{} {}", system.order, system.name);
            for hook in &system.hooks {
                let _ = writeln!(
                    out,
                    "  hook {} {} -> fn#{} {}",
                    hook.path, hook.domain, hook.index, hook.function
                );
            }
            for state in &system.states {
                let _ = write!(
                    out,
                    "  state[{}] {} {}",
                    state.slot, state.name, state.domain
                );
                if state.probe {
                    out.push_str(" probe");
                }
                if let Some(key) = &state.persist {
                    let _ = write!(out, " persist={key}");
                }
                out.push('\n');
            }
        }
    }

    fn write_json(&self, w: &mut JsonWriter) {
        w.begin_object();
        w.name("tick_rate");
        match self.tick_rate {
            Some(rate) => w.uint(u64::from(rate)),
            None => w.null(),
        }
        w.name("determinism");
        match self.determinism {
            Some(determinism) => w.string(determinism),
            None => w.null(),
        }
        w.name("contact_delivery");
        match self.delivery {
            Some(delivery) => w.string(delivery),
            None => w.null(),
        }
        w.name("systems");
        w.begin_array();
        for system in &self.systems {
            w.begin_object();
            w.name("name");
            w.string(&system.name);
            w.name("order");
            w.uint(system.order as u64);
            w.name("hooks");
            w.begin_array();
            for hook in &system.hooks {
                w.begin_object();
                w.name("hook");
                w.string(&hook.path);
                w.name("domain");
                w.string(hook.domain);
                w.name("function");
                w.uint(u64::from(hook.index));
                w.name("name");
                w.string(&hook.function);
                w.end_object();
            }
            w.end_array();
            w.name("states");
            w.begin_array();
            for state in &system.states {
                w.begin_object();
                w.name("name");
                w.string(&state.name);
                w.name("slot");
                w.uint(u64::from(state.slot));
                w.name("domain");
                w.string(state.domain);
                w.name("probe");
                w.bool(state.probe);
                w.name("persist");
                match &state.persist {
                    Some(key) => w.string(key),
                    None => w.null(),
                }
                w.end_object();
            }
            w.end_array();
            w.end_object();
        }
        w.end_array();
        w.end_object();
    }
}
