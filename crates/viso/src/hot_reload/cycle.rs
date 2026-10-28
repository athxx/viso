//! A reactive cycle reported to the host as a diagnostic: each cell named by
//! the state it holds and where that state is declared, each effect by its
//! body.

use viso_ui::{EffectOrigin, NodeStore, ReactiveCycle, SETTLE_ROUNDS, state_origin};
use viso_view::dev::wire::{FileId, RuntimeDiagnostic, RuntimeLabel, RuntimeMessage, SourceAt};

use super::{HotReloadSession, LiveView};
use crate::driver::WindowState;

impl HotReloadSession {
    /// Sends `cycle`, which `ws`'s flush or effects raised, to the host as an
    /// `E4202` naming the states and effects on it. `false`, sending
    /// nothing, without a dev channel.
    pub(crate) fn report_cycle(&mut self, cycle: &ReactiveCycle, ws: &WindowState) -> bool {
        if self.link.is_none() {
            return false;
        }
        state_origin::take_sites(&mut self.sites);
        let views: Vec<&LiveView> = self
            .views
            .iter()
            .filter(|v| v.window == ws.window)
            .collect();
        let mut labels = Vec::new();
        let mut cells = Vec::with_capacity(cycle.cells.len());
        for &cell in &cycle.cells {
            let in_view = views.iter().find_map(|view| {
                let &(key, _) = view.cells.iter().find(|&&(_, id)| id == cell)?;
                Some((view, key))
            });
            if let Some((view, key)) = in_view {
                let name = view
                    .host
                    .as_ref()
                    .and_then(|host| Some(host.try_borrow().ok()?.cell_state(cell)?.1.to_owned()));
                let name = name.map_or_else(|| "a state".to_owned(), |name| format!("`{name}`"));
                labels.push(RuntimeLabel {
                    at: SourceAt::State {
                        file: FileId(view.file as u32),
                        key,
                    },
                    label: format!("{name} is written again"),
                });
                cells.push(name);
            } else if let Some(&(_, site)) = self.sites.iter().rev().find(|&&(id, _)| id == cell) {
                labels.push(RuntimeLabel {
                    at: SourceAt::Point {
                        path: site.file().to_owned(),
                        line: site.line(),
                        column: site.column(),
                    },
                    label: "this state is written again".to_owned(),
                });
                cells.push(format!("the state at {}:{}", site.file(), site.line()));
            } else {
                cells.push(format!("{cell:?}"));
            }
        }
        let mut effects = Vec::with_capacity(cycle.effects.len());
        for effect in &cycle.effects {
            let named = (effect.origin != EffectOrigin::NONE)
                .then(|| {
                    views
                        .iter()
                        .find(|view| within(&ws.store, effect.node, view.root))
                })
                .flatten()
                .and_then(|view| {
                    let host = view.host.as_ref()?.try_borrow().ok()?;
                    let (name, span) = host.entry_source(effect.origin.0)?;
                    Some((view.file, name.to_owned(), span))
                });
            match named {
                Some((file, name, span)) => {
                    if let Some((start, end)) = span {
                        labels.push(RuntimeLabel {
                            at: SourceAt::Span {
                                file: FileId(file as u32),
                                start,
                                end,
                            },
                            label: if cycle.across_frames {
                                "this effect writes what wakes it".to_owned()
                            } else {
                                "this effect runs again".to_owned()
                            },
                        });
                    }
                    effects.push(format!("`{name}`"));
                }
                None => effects.push(format!("the effect on {:?}", effect.node)),
            }
        }
        let message = if cycle.across_frames {
            let mut message = format!("effects kept waking each other for {SETTLE_ROUNDS} frames");
            if !cells.is_empty() {
                message.push_str(&format!(" through {}", cells.join(", ")));
            }
            message.push_str(&format!("; stopped {}", effects.join(", ")));
            message
        } else {
            let mut message = format!(
                "state writes did not settle within {SETTLE_ROUNDS} rounds; dropped the changes \
                 of {}",
                cells.join(", ")
            );
            if !effects.is_empty() {
                message.push_str(&format!(", which re-run {}", effects.join(", ")));
            }
            message
        };
        let note = if cycle.across_frames {
            "their last writes stand; they run again when something else changes what they read"
        } else {
            "the dropped values stay written; only the reactions to them did not run"
        };
        labels.truncate(viso_view::dev::wire::MAX_LABELS);
        if let Some(link) = &mut self.link {
            link.send(RuntimeMessage::Diagnostic(RuntimeDiagnostic {
                code: ReactiveCycle::CODE.to_owned(),
                message,
                labels,
                notes: vec![note.to_owned()],
            }));
        }
        true
    }
}

/// Whether `node` is `root` or below it.
fn within(store: &NodeStore, mut node: viso_ui::NodeId, root: viso_ui::NodeId) -> bool {
    loop {
        if node == root {
            return true;
        }
        match store.parent(node) {
            Some(parent) => node = parent,
            None => return false,
        }
    }
}
