//! The live node state a hot reload carries across a rebuilt tree.

/// The live state of a widget's node a hot reload carries to the node that
/// replaces it in a rebuilt tree, when the diff keeps the node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct MigratableState(u8);

impl MigratableState {
    /// Nothing carries.
    pub const NONE: MigratableState = MigratableState(0);
    /// Keyboard focus.
    pub const FOCUS: MigratableState = MigratableState(1);
    /// A scroll viewport's offset.
    pub const SCROLL: MigratableState = MigratableState(1 << 1);
    /// A text field's edit buffer: its caret, selection and text.
    pub const SELECTION: MigratableState = MigratableState(1 << 2);
    /// A look property's transition in flight.
    pub const ANIMATION: MigratableState = MigratableState(1 << 3);
    /// Which immediate child subtree is showing: the one with its `hidden`
    /// flag clear, among siblings a widget shows one of at a time (a `Tabs`
    /// panel, a `NavigationStack` page). Selecting and paging never differ in
    /// this respect, so one bit names both.
    pub const ACTIVE_CHILD: MigratableState = MigratableState(1 << 4);

    /// Both sets.
    pub const fn with(self, other: MigratableState) -> MigratableState {
        MigratableState(self.0 | other.0)
    }

    /// Whether every state in `other` carries.
    pub const fn contains(self, other: MigratableState) -> bool {
        self.0 & other.0 == other.0
    }

    /// Whether nothing carries.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The set as its bits, for a wire form.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// The set of `bits`, `None` when a bit names no state.
    pub const fn from_bits(bits: u8) -> Option<MigratableState> {
        if bits & !0b1_1111 == 0 {
            Some(MigratableState(bits))
        } else {
            None
        }
    }
}
