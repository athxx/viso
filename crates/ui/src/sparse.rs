//! Per-node columns lighter than a value a slot: [`Sparse`], for a value
//! most nodes leave empty, four bytes a slot instead of a whole `Option<T>`;
//! and [`SlotBits`], one bit a slot.

/// No value.
const NONE: u32 = u32::MAX;

/// A value for some of the store's slots. Each slot holds the index of its
/// entry, [`NONE`] for none; the entries are packed, each with its slot, so
/// taking one out moves the last into its place.
pub(crate) struct Sparse<T> {
    at: Vec<u32>,
    entries: Vec<(u32, T)>,
}

impl<T> Default for Sparse<T> {
    fn default() -> Self {
        Sparse {
            at: Vec::new(),
            entries: Vec::new(),
        }
    }
}

impl<T> Sparse<T> {
    /// The bytes every slot takes, whether or not it holds a value.
    pub(crate) const SLOT_BYTES: usize = size_of::<u32>();

    /// Whether slot `i` holds a value.
    #[inline(always)]
    pub(crate) fn contains(&self, i: usize) -> bool {
        self.at[i] != NONE
    }

    /// Slot `i`'s value.
    #[inline(always)]
    pub(crate) fn get(&self, i: usize) -> Option<&T> {
        match self.at[i] {
            NONE => None,
            // SAFETY: a slot's entry index is always in bounds: `insert`
            // records the index it pushes at, `remove` re-points the entry
            // it moves, and `clear` empties the slots with the entries.
            e => Some(unsafe { &self.entries.get_unchecked(e as usize).1 }),
        }
    }

    /// Slot `i`'s value, to change in place.
    #[inline(always)]
    pub(crate) fn get_mut(&mut self, i: usize) -> Option<&mut T> {
        match self.at[i] {
            NONE => None,
            // SAFETY: as in `get`.
            e => Some(unsafe { &mut self.entries.get_unchecked_mut(e as usize).1 }),
        }
    }

    /// Gives slot `i` `value`, in place of any it held.
    pub(crate) fn insert(&mut self, i: usize, value: T) {
        match self.at[i] {
            NONE => {
                self.at[i] = self.entries.len() as u32;
                self.entries.push((i as u32, value));
            }
            e => self.entries[e as usize].1 = value,
        }
    }

    /// Takes slot `i`'s value out.
    pub(crate) fn remove(&mut self, i: usize) -> Option<T> {
        let e = core::mem::replace(&mut self.at[i], NONE);
        if e == NONE {
            return None;
        }
        let (_, value) = self.entries.swap_remove(e as usize);
        if let Some(&(moved, _)) = self.entries.get(e as usize) {
            self.at[moved as usize] = e;
        }
        Some(value)
    }

    /// Gives slot `i` `value`, or takes its value out for `None`.
    pub(crate) fn set(&mut self, i: usize, value: Option<T>) {
        match value {
            Some(value) => self.insert(i, value),
            None => {
                self.remove(i);
            }
        }
    }

    /// The bytes it holds: a word a slot and the values with their slots.
    pub(crate) fn held_bytes(&self) -> usize {
        self.at.len() * size_of::<u32>() + self.entries.len() * size_of::<(u32, T)>()
    }

    /// How many slots hold a value.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Adds an empty slot at the end.
    #[inline]
    pub(crate) fn push_empty(&mut self) {
        self.at.push(NONE);
    }

    pub(crate) fn reserve(&mut self, additional: usize) {
        self.at.reserve(additional);
    }

    pub(crate) fn reserve_exact(&mut self, additional: usize) {
        self.at.reserve_exact(additional);
    }

    /// Drops every slot and value, keeping the storage.
    pub(crate) fn clear(&mut self) {
        self.at.clear();
        self.entries.clear();
    }
}

impl<T: Copy> Sparse<T> {
    /// A copy of slot `i`'s value.
    #[inline(always)]
    pub(crate) fn copied(&self, i: usize) -> Option<T> {
        self.get(i).copied()
    }
}

/// One bit for each slot, off for a slot never set.
#[derive(Default)]
pub(crate) struct SlotBits(Vec<u64>);

impl SlotBits {
    /// Room for `slots` slots, so setting any of them never reallocates.
    pub(crate) fn with_capacity(slots: usize) -> Self {
        SlotBits(Vec::with_capacity(slots.div_ceil(64)))
    }

    /// The bytes it holds.
    pub(crate) fn held_bytes(&self) -> usize {
        self.0.len() * size_of::<u64>()
    }

    #[inline(always)]
    pub(crate) fn get(&self, i: usize) -> bool {
        self.0.get(i / 64).is_some_and(|w| w >> (i % 64) & 1 == 1)
    }

    #[inline]
    pub(crate) fn set(&mut self, i: usize, on: bool) {
        let word = i / 64;
        if word >= self.0.len() {
            if !on {
                return;
            }
            self.0.resize(word + 1, 0);
        }
        let bit = 1 << (i % 64);
        if on {
            self.0[word] |= bit;
        } else {
            self.0[word] &= !bit;
        }
    }

    /// Turns every bit off, keeping the storage.
    pub(crate) fn clear(&mut self) {
        self.0.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn taking_a_value_out_keeps_every_other_slots_value() {
        let mut column = Sparse::default();
        for _ in 0..6 {
            column.push_empty();
        }
        for i in [1, 3, 4] {
            column.insert(i, i * 10);
        }
        assert_eq!(column.remove(1), Some(10));
        assert_eq!(column.remove(1), None);
        column.insert(3, 31);
        column.set(5, Some(50));
        column.set(4, None);
        let values: Vec<_> = (0..6).map(|i| column.copied(i)).collect();
        assert_eq!(values, [None, None, None, Some(31), None, Some(50)]);
        assert_eq!(column.len(), 2);
        *column.get_mut(5).unwrap() += 1;
        assert_eq!(column.get(5), Some(&51));
    }

    #[test]
    fn a_slot_bit_reads_back_what_was_set_and_off_past_the_last() {
        let mut bits = SlotBits::default();
        bits.set(3, true);
        bits.set(130, true);
        bits.set(3, false);
        bits.set(700, false);
        let on: Vec<_> = (0..800).filter(|&i| bits.get(i)).collect();
        assert_eq!(on, [130]);
        bits.clear();
        assert!(!bits.get(130));
    }
}
