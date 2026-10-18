//! Game reload data.

/// What a World Rebuild keeps of the running game.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Rebuild {
    /// Nothing: the world as the start spawns it.
    #[default]
    Fresh,
    /// The place and motion of each character the start spawns under the
    /// stable key of a running one: its tags and its rank among the
    /// characters with those tags, in allocation order.
    KeepCharacters,
}
