//! A view's entries at export time: the interpreter runs each against the
//! component's initial states, its inputs' defaults, the default theme and
//! the page's locale, and what the entry reads says whether the value it
//! shows can change. A page that picks its locale as it loads changes what
//! reads the locale once, then; the rest of the environment is the
//! browser's, which the page follows.

use std::rc::Rc;

use viso_behavior::native::Natives;
use viso_behavior::{Aggregate, Budget, Fault, Instance, Module, Value, Vm};
use viso_schema::EnvField;

use super::{Finding, Need};
use crate::behavior::ir::{ComponentLayout, FuncId, Inst, Program};
use crate::syntax::TextRange;

/// The state slots whose change re-evaluates an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Deps {
    /// These, ascending; none for an entry that never changes.
    Slots(Vec<u32>),
    /// Any: it calls a closure value, whose reads are not known statically.
    Any,
}

impl Deps {
    pub(super) fn is_empty(&self) -> bool {
        matches!(self, Deps::Slots(slots) if slots.is_empty())
    }
}

/// The interpreter over the view's component, as it starts.
pub(super) struct Eval<'a> {
    pub(super) program: &'a Program,
    pub(super) layout: &'a ComponentLayout,
    vm: Vm,
    instance: Instance,
    /// The `env` slots holding the theme, which an export fixes.
    themes: Vec<u32>,
    /// The `env` slots holding the locale, fixed to the page's language.
    locales: Vec<u32>,
    /// Whether the page picks its locale as it loads, so what reads the
    /// locale is written by the script rather than into the page.
    picks_locale: bool,
    /// Whether the inputs change, as a component's props do, rather than
    /// hold their defaults for good, as a page's do.
    pub(super) props: bool,
}

impl<'a> Eval<'a> {
    /// The interpreter of `module`'s component `name`, laid out as `layout`,
    /// linked to `natives`, the libraries the view compiled against, and
    /// instantiated with the default theme and the locale `lang` in its
    /// `env` slots. A native of the app's packages is known by its schema
    /// alone here, and faults when it is called.
    pub(super) fn new(
        module: Rc<Module>,
        program: &'a Program,
        layout: &'a ComponentLayout,
        natives: &Natives,
        lang: &str,
        at: TextRange,
    ) -> Result<Self, Finding> {
        let fault = |fault: Fault| Finding {
            need: Need::Unsupported,
            at,
            message: format!("the component does not start: {}", fault.message),
            notes: Vec::new(),
        };
        let index = module.component(&layout.name).ok_or_else(|| Finding {
            need: Need::Unsupported,
            at,
            message: format!("internal: `{}` has no compiled layout", layout.name),
            notes: Vec::new(),
        })?;
        let mut vm = Vm::new(module, Budget::default());
        // Every capability is withheld: a native that needs one is a call an
        // export cannot make, and faults.
        vm.link(natives, &[]).map_err(|conflict| Finding {
            need: Need::Unsupported,
            at,
            message: format!("the component's natives do not link: {conflict:?}"),
            notes: Vec::new(),
        })?;
        let mut instance = vm.instantiate(index, []).map_err(fault)?;
        let slots = |field: EnvField| -> Vec<u32> {
            layout
                .env
                .iter()
                .filter(|env| env.field == field)
                .map(|env| env.slot)
                .collect()
        };
        let (themes, locales) = (slots(EnvField::Theme), slots(EnvField::Locale));
        for &slot in &themes {
            instance.set_state(slot as usize, viso_behavior::theme::default_theme());
        }
        for &slot in &locales {
            let locale = Aggregate {
                tag: 0,
                fields: Box::new([Value::str(lang)]),
            };
            instance.set_state(slot as usize, Value::Agg(Rc::new(locale)));
        }
        instance.clear_dirty();
        Ok(Eval {
            program,
            layout,
            vm,
            instance,
            themes,
            locales,
            picks_locale: false,
            props: false,
        })
    }

    /// The function behind handler-table entry `entry`.
    pub(super) fn func(&self, entry: u32) -> FuncId {
        self.layout.handlers[entry as usize].1
    }

    /// The state slots `entry` reads that can change — the `env` slots the
    /// browser fills among them — and the `env` fields beside the theme and
    /// the locale it reads.
    pub(super) fn deps(&self, entry: u32) -> (Deps, Vec<EnvField>) {
        let reads = self.vm.reads(self.func(entry).0);
        let mut env = Vec::new();
        let mut slots = Vec::new();
        for slot in reads.states {
            if self.themes.contains(&slot) || self.locales.contains(&slot) {
                continue;
            }
            if let Some(read) = self.layout.env.iter().find(|e| e.slot == slot) {
                env.push(read.field);
            }
            slots.push(slot);
        }
        env.dedup();
        let deps = if reads.opaque {
            Deps::Any
        } else {
            Deps::Slots(slots)
        };
        (deps, env)
    }

    /// Whether `entry` reads the locale, which the page takes from its
    /// reader's browser.
    pub(super) fn reads_locale(&self, entry: u32) -> bool {
        let reads = self.vm.reads(self.func(entry).0);
        reads.opaque || reads.states.iter().any(|s| self.locales.contains(s))
    }

    /// Whether function `f` reads a state of a component a region mounts,
    /// which each mount of the content keeps for itself.
    pub(super) fn reads_regional(&self, f: FuncId) -> bool {
        let reads = self.vm.reads(f.0);
        reads.opaque
            || reads.states.iter().any(|&slot| {
                self.layout
                    .regional
                    .iter()
                    .any(|r| (r.base..r.base + r.inits.len() as u32).contains(&slot))
            })
    }

    /// Has the page pick its locale as it loads, among the catalog's.
    pub(super) fn pick_locale(&mut self) {
        self.picks_locale = true;
    }

    /// The `env` slots holding the locale.
    pub(super) fn locale_slots(&self) -> &[u32] {
        &self.locales
    }

    /// The `env` slots holding the theme.
    pub(super) fn theme_slots(&self) -> &[u32] {
        &self.themes
    }

    /// The `env` slots the browser fills, `(slot, field tag, instance)` each:
    /// every field beside the theme and the locale.
    pub(super) fn browser_env(&self) -> impl Iterator<Item = (u32, u8, u32)> + '_ {
        self.layout
            .env
            .iter()
            .filter(|e| !matches!(e.field, EnvField::Theme | EnvField::Locale))
            .map(|e| (e.slot, e.field.tag(), e.instance))
    }

    /// Whether `entry` shows one value for as long as the page is open: it
    /// reads no state that changes, no input that does, no locale the page
    /// picks, and takes no region's binding.
    pub(super) fn constant(&self, entry: u32) -> bool {
        let (deps, env) = self.deps(entry);
        let reads = self.vm.reads(self.func(entry).0);
        deps.is_empty()
            && env.is_empty()
            && self.program.function(self.func(entry)).params == 0
            && !(self.props && !reads.inputs.is_empty())
            && !(self.picks_locale && reads.states.iter().any(|s| self.locales.contains(s)))
    }

    /// The value `entry` evaluates to now, with `args` as its region bindings.
    pub(super) fn value(&mut self, entry: u32, args: &[Value]) -> Result<Value, Fault> {
        let chunk = self.func(entry).0;
        let outcome = self.vm.call(&mut self.instance, chunk, args);
        self.instance.clear_dirty();
        outcome.map(|outcome| outcome.value)
    }

    /// The theme path `entry` reads and returns as it is (`theme.colors.primary`
    /// is `[0, 4]`), `None` for an entry that computes anything else.
    pub(super) fn theme_path(&self, entry: u32) -> Option<Vec<u32>> {
        let body = self.program.function(self.func(entry)).body.as_ref().ok()?;
        let mut at = None;
        let mut path = Vec::new();
        for inst in &body.insts {
            match *inst {
                Inst::LoadState { dst, slot } if at.is_none() && self.themes.contains(&slot) => {
                    at = Some(dst);
                }
                Inst::Field { dst, src, index } if at == Some(src) => {
                    path.push(index);
                    at = Some(dst);
                }
                Inst::Move { dst, src } if at == Some(src) => at = Some(dst),
                Inst::Return { src } if at == Some(src) && !path.is_empty() => {
                    return Some(path);
                }
                _ => return None,
            }
        }
        None
    }

    /// The component's states as it starts, by slot.
    pub(super) fn states(&self) -> &[Value] {
        self.instance.states()
    }

    /// The component's inputs, each its default.
    pub(super) fn inputs(&self) -> &[Value] {
        self.instance.inputs()
    }
}
