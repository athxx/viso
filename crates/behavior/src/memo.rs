//! Computed memoization: the static read graph of a module and the per-instance
//! cache it invalidates.
//!
//! A `computed` is pure, so its value is a function of the states and inputs it
//! reads. The read graph records, for every zero-argument computed chunk, those
//! slots — the `LoadState`/`LoadInput` operands of its body and of every chunk it
//! calls or makes a closure of, transitively — and a reverse index from each slot
//! to the computeds reading it. An instance caches each computed's value once it
//! is evaluated; a write to a slot empties exactly the entries of the computeds
//! that read it, so an unchanged dependency set never re-evaluates (spec computed
//! and reactive-graph sections).
//!
//! A computed whose body, or anything it calls, invokes a closure value is not
//! cached: the callee is not known statically, so neither are its reads.

use std::collections::BTreeSet;

use crate::module::{ChunkKind, Module};
use crate::op::Op;
use crate::value::Value;

/// The states and inputs a chunk reads, following its calls.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reads {
    /// The state slots, ascending.
    pub states: Vec<u32>,
    /// The input slots, ascending.
    pub inputs: Vec<u32>,
    /// Whether it calls a closure value, whose reads are not known statically.
    pub opaque: bool,
}

/// What `chunk` reads, following every call and closure it makes.
pub(crate) fn reads(module: &Module, chunk: u32) -> Reads {
    let mut states = BTreeSet::new();
    let mut inputs = BTreeSet::new();
    let mut opaque = false;
    let mut seen = vec![false; module.chunks().len()];
    let mut pending = vec![chunk];
    while let Some(at) = pending.pop() {
        let Some(visited) = seen.get_mut(at as usize) else {
            opaque = true;
            continue;
        };
        if std::mem::replace(visited, true) {
            continue;
        }
        let Ok(code) = &module.chunk(at).body else {
            continue;
        };
        for op in &code.ops {
            match *op {
                Op::LoadState { slot, .. } => {
                    states.insert(slot);
                }
                Op::LoadInput { slot, .. } => {
                    inputs.insert(slot);
                }
                Op::Call { ext, .. } | Op::Closure { ext, .. } => {
                    pending.push(code.ext[ext as usize]);
                }
                Op::CallValue { .. } => opaque = true,
                _ => {}
            }
        }
    }
    Reads {
        states: states.into_iter().collect(),
        inputs: inputs.into_iter().collect(),
        opaque,
    }
}

/// No cache entry: the chunk is not a cached computed.
const UNCACHED: u32 = u32::MAX;

/// The read graph of a module: which chunks are cached computeds, and which of
/// them each state and input slot invalidates.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ReadGraph {
    /// Each chunk's cache entry, or [`UNCACHED`].
    entry: Box<[u32]>,
    /// The number of cache entries.
    entries: usize,
    /// The entries reading each state slot.
    by_state: Box<[Box<[u32]>]>,
    /// The entries reading each input slot.
    by_input: Box<[Box<[u32]>]>,
}

impl ReadGraph {
    /// The read graph of `module`. Cold: it walks every computed's calls once.
    pub(crate) fn new(module: &Module) -> ReadGraph {
        let mut entry = vec![UNCACHED; module.chunks().len()];
        let mut by_state: Vec<Vec<u32>> = Vec::new();
        let mut by_input: Vec<Vec<u32>> = Vec::new();
        let mut entries = 0;
        for (index, chunk) in module.chunks().iter().enumerate() {
            let cacheable = chunk.kind == ChunkKind::Computed
                && chunk.params == 0
                && chunk.captures.is_empty()
                && chunk.body.is_ok();
            if !cacheable {
                continue;
            }
            let read = reads(module, index as u32);
            if read.opaque {
                continue;
            }
            let id = entries as u32;
            entries += 1;
            entry[index] = id;
            for (slots, index) in [(&read.states, &mut by_state), (&read.inputs, &mut by_input)] {
                for &slot in slots {
                    let slot = slot as usize;
                    if index.len() <= slot {
                        index.resize(slot + 1, Vec::new());
                    }
                    index[slot].push(id);
                }
            }
        }
        let boxed = |index: Vec<Vec<u32>>| index.into_iter().map(Vec::into_boxed_slice).collect();
        ReadGraph {
            entry: entry.into(),
            entries,
            by_state: boxed(by_state),
            by_input: boxed(by_input),
        }
    }

    /// The cache entry of `chunk`, when it is a cached computed.
    #[inline(always)]
    pub(crate) fn entry(&self, chunk: u32) -> Option<usize> {
        match self.entry.get(chunk as usize) {
            Some(&id) if id != UNCACHED => Some(id as usize),
            _ => None,
        }
    }

    /// The number of cache entries an instance keeps.
    pub(crate) fn entries(&self) -> usize {
        self.entries
    }

    /// The entries reading state `slot`.
    pub(crate) fn state_readers(&self, slot: usize) -> &[u32] {
        self.by_state.get(slot).map_or(&[], |e| e)
    }

    /// The entries reading input `slot`.
    pub(crate) fn input_readers(&self, slot: usize) -> &[u32] {
        self.by_input.get(slot).map_or(&[], |e| e)
    }
}

/// One computed's cache entry in an instance.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) enum Memo {
    /// Not evaluated since a slot it reads last changed.
    #[default]
    Empty,
    /// Being evaluated; reaching it again is a reactive cycle.
    Evaluating,
    /// Its value.
    Ready(Value),
}
