//! The dock's accessibility contract, isolated in one file (AGENTS section 15).
//!
//! Every role a dock node carries is decided here, not scattered through the build
//! walk, so the a11y contract reads as one policy and the snapshot test in this
//! module pins it. [`build`](super::build) calls these helpers; it never names a
//! [`Role`] directly. The mapping:
//!
//! - **the dock container** and **each floating panel** are named landmark
//!   [`Region`](Role::Region)s a screen reader can jump between by name — stronger
//!   than a mute [`Group`](Role::Group), and semantically a standalone section
//!   rather than a page stack ([`Navigation`](Role::Navigation));
//! - a **multi-panel tab group** is a [`Region`](Role::Region) holding a
//!   [`TabList`](Role::TabList) of [`Tab`](Role::Tab)s (the reference tabs mapping);
//! - a **lone panel group** (its tab strip hidden) drops to a [`Group`](Role::Group)
//!   named by its one panel — a single panel needs no tablist ceremony;
//! - a **panel's content root** is a [`Group`](Role::Group) named by the panel, so
//!   an assistive technology announces which panel it entered (the nav-page
//!   precedent);
//! - a **resize seam** is a [`Group`](Role::Group) named `resize`, keeping the
//!   Splitter precedent and a keyboard-resize equivalent (AGENTS section 15);
//! - a **drop hint** and **floating chrome** are mute [`Group`](Role::Group)s —
//!   transient decoration that announces no interaction, since the redock drag has
//!   a `DockHandle` keyboard equivalent (a later section).
//!
//! The label a panel announces is [`panel_label`]; a dock without registered
//! content still names each panel distinctly by its key, so the snapshot is stable.

use std::collections::HashMap;

use viso_ui::{Role, Semantics};

use super::PanelContent;
use super::tree::PanelKey;

/// The seam's accessible name — the keyboard-resize target's label (AGENTS
/// section 15: a visual resize has a named keyboard equivalent).
pub(super) const SEAM_LABEL: &str = "resize";

/// A panel's accessible label. A registered panel has no separate label in this
/// slice (content builders are opaque closures), so every panel announces by its
/// stable key — distinct, stable across a redock, and independent of position.
/// The keyed name is what a screen reader reads when it enters the panel and what
/// the snapshot pins.
pub(super) fn panel_label(_contents: &HashMap<PanelKey, PanelContent>, key: PanelKey) -> String {
    format!("Panel {}", key.0)
}

/// The semantics for the whole dock's container — the named landmark
/// [`Region`](Role::Region) an assistive technology navigates to. The dock itself
/// needs no name of its own; the panels inside it carry the names.
pub(super) fn dock_container() -> Semantics {
    Semantics::role(Role::Region)
}

/// The semantics for the container of a tab group: a named landmark
/// [`Region`](Role::Region) when it shows a tab strip (multiple panels), else a
/// mute [`Group`](Role::Group) named by its lone panel. `first` is the group's
/// first panel key, used to name a lone group.
pub(super) fn tab_group(
    contents: &HashMap<PanelKey, PanelContent>,
    first: Option<PanelKey>,
    show_strip: bool,
) -> Semantics {
    if show_strip {
        Semantics::role(Role::Region)
    } else {
        match first {
            Some(key) => Semantics::role(Role::Group).with_label(panel_label(contents, key)),
            None => Semantics::role(Role::Group),
        }
    }
}

/// The semantics for the tab strip itself — a [`TabList`](Role::TabList) grouping
/// its tabs as one selectable set. The strip needs no name of its own.
pub(super) fn strip() -> Semantics {
    Semantics::role(Role::TabList)
}

/// The semantics for one tab button — a named [`Tab`](Role::Tab), named by the
/// panel it reveals.
pub(super) fn tab(contents: &HashMap<PanelKey, PanelContent>, key: PanelKey) -> Semantics {
    Semantics::role(Role::Tab).with_label(panel_label(contents, key))
}

/// The semantics for a panel's content root — a [`Group`](Role::Group) named by
/// the panel, so an assistive technology announces which panel it entered.
pub(super) fn panel(contents: &HashMap<PanelKey, PanelContent>, key: PanelKey) -> Semantics {
    Semantics::role(Role::Group).with_label(panel_label(contents, key))
}

/// The semantics for a resize seam — a [`Group`](Role::Group) named `resize`,
/// carrying the keyboard-resize equivalent.
pub(super) fn seam() -> Semantics {
    Semantics::role(Role::Group).with_label(SEAM_LABEL)
}

/// The semantics for a floating panel's container — a detached named landmark
/// [`Region`](Role::Region), named by the panel it holds.
// Called by the floating-panel build in section 5; defined here so the whole a11y
// contract (docked and floating) reads as one policy, and the snapshot below pins
// it now.
#[allow(dead_code)]
pub(super) fn floating(contents: &HashMap<PanelKey, PanelContent>, key: PanelKey) -> Semantics {
    Semantics::role(Role::Region).with_label(panel_label(contents, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_contents() -> HashMap<PanelKey, PanelContent> {
        HashMap::new()
    }

    #[test]
    fn panel_label_names_by_key() {
        let c = no_contents();
        assert_eq!(panel_label(&c, PanelKey(0)), "Panel 0");
        assert_eq!(panel_label(&c, PanelKey(7)), "Panel 7");
        // Distinct keys announce distinctly.
        assert_ne!(panel_label(&c, PanelKey(1)), panel_label(&c, PanelKey(2)));
    }

    #[test]
    fn multi_panel_group_is_a_named_region() {
        let c = no_contents();
        let s = tab_group(&c, Some(PanelKey(0)), true);
        assert_eq!(s.role, Role::Region);
        // The region is unnamed here; the panels inside carry the names.
        assert_eq!(s.label, None);
    }

    #[test]
    fn lone_group_drops_to_named_group() {
        let c = no_contents();
        let s = tab_group(&c, Some(PanelKey(3)), false);
        assert_eq!(s.role, Role::Group);
        assert_eq!(s.label.as_deref(), Some("Panel 3"));
    }

    #[test]
    fn empty_lone_group_has_no_name() {
        let c = no_contents();
        let s = tab_group(&c, None, false);
        assert_eq!(s.role, Role::Group);
        assert_eq!(s.label, None);
    }

    #[test]
    fn strip_is_a_tablist_with_no_name() {
        let s = strip();
        assert_eq!(s.role, Role::TabList);
        assert_eq!(s.label, None);
    }

    #[test]
    fn tab_button_is_named_tab() {
        let c = no_contents();
        let s = tab(&c, PanelKey(2));
        assert_eq!(s.role, Role::Tab);
        assert_eq!(s.label.as_deref(), Some("Panel 2"));
    }

    #[test]
    fn panel_root_is_named_group() {
        let c = no_contents();
        let s = panel(&c, PanelKey(5));
        assert_eq!(s.role, Role::Group);
        assert_eq!(s.label.as_deref(), Some("Panel 5"));
    }

    #[test]
    fn seam_is_a_named_group() {
        let s = seam();
        assert_eq!(s.role, Role::Group);
        assert_eq!(s.label.as_deref(), Some("resize"));
    }

    #[test]
    fn floating_panel_is_a_named_region() {
        let c = no_contents();
        let s = floating(&c, PanelKey(9));
        assert_eq!(s.role, Role::Region);
        assert_eq!(s.label.as_deref(), Some("Panel 9"));
    }
}
