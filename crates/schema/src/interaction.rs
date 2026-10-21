//! The interaction states a style's `when` reads.

/// One interaction state of a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Interaction {
    /// The pointer is over the node or one of its descendants.
    Hovered = 0,
    /// The primary button went down on the node or a descendant and has not
    /// come up, the pointer is inside the node's box and no pan took the
    /// contact.
    Pressed = 1,
    /// The node holds focus.
    Focused = 2,
    /// The node holds focus the keyboard or an assistive technology moved to
    /// it.
    FocusVisible = 3,
    /// The node's tab strip or radio group selects it.
    Selected = 4,
}

impl Interaction {
    /// Every state, by tag.
    pub const ALL: [Interaction; 5] = [
        Interaction::Hovered,
        Interaction::Pressed,
        Interaction::Focused,
        Interaction::FocusVisible,
        Interaction::Selected,
    ];

    /// The state with tag `tag`.
    pub fn from_tag(tag: u8) -> Option<Interaction> {
        Interaction::ALL.get(usize::from(tag)).copied()
    }

    /// Its tag.
    pub fn tag(self) -> u8 {
        self as u8
    }
}
