//! `ComboBox` — a text field with a list of options it filters as the user
//! types, choosing from a closed set of them or taking free text.
//!
//! The focus stays in the field while the list shows: the highlighted option
//! is the field's active descendant, which assistive technology reads.
//! Typing opens the list and keeps the options whose label holds what was
//! typed, in their order, the matched letters in bold. Over a closed set the
//! first option starting with it is highlighted, and only an option is ever
//! the value: Enter or Tab chooses the highlighted one, and Escape, a press
//! outside or the focus leaving puts the chosen option's label back; with no
//! match the list says so ([`ComboBox::empty_text`]) and Enter does nothing.
//! With [`free_text`](ComboBox::free_text) the text is the value: nothing is
//! highlighted until an arrow key moves there, Enter takes the text unless an
//! option is highlighted, the focus leaving takes it too, and a list with no
//! match closes.
//!
//! The keyboard: Down opens the list on its first option, Up on its last,
//! Alt+Down opens it as it is; open, Up and Down move the highlight,
//! wrapping, Page Up and Page Down by a list's height, Alt+Up closes it.
//! Home, End and the rest edit the text. The pointer: the chevron opens and
//! closes the whole list; a press on the field while it lacks the focus
//! opens it too, selecting the text; a click on an option chooses it.
//!
//! Semantics: the field is an `editablecombobox` (WAI-ARIA `combobox`)
//! announcing whether its list is expanded and naming the highlighted option
//! as its active descendant; the list is a `listbox` of `option`s announcing
//! whether each is the chosen one and its place among those shown.
//!
//! Invalidation: the list builds once with the field. Filtering hides and
//! shows options and shapes again the labels whose matched letters moved;
//! a highlight is a `PAINT` of one option's look. The parts are
//! [`viso_ui::combo_box`]'s, shared with the view's `ComboBox` node.

use std::cell::RefCell;
use std::rc::Rc;

pub use viso_ui::combo_box::ComboBoxStyle;
use viso_ui::combo_box::{ComboOption, ComboParts, ComboSpec};
use viso_ui::{BuildCx, Component, Controls, EventCx, Sizing};

use super::select::SelectOption;

/// What runs when the user chooses an option.
type Pick = Rc<RefCell<dyn FnMut(&mut EventCx<'_>, usize)>>;

/// What runs when a free-text combo box takes its text.
type Commit = Rc<RefCell<dyn FnMut(&mut EventCx<'_>, &str)>>;

/// A text field choosing from a list it filters. Construct one with
/// [`combo_box`].
pub struct ComboBox {
    spec: ComboSpec,
    on_pick: Option<Pick>,
    on_text: Option<Commit>,
    /// The handle `build` binds, when the app minted one up front.
    handle: Option<ComboBoxHandle>,
}

/// A combo box over `options`, choosing among them, none chosen yet.
pub fn combo_box<I>(options: I) -> ComboBox
where
    I: IntoIterator,
    I::Item: Into<SelectOption>,
{
    let options = options
        .into_iter()
        .map(|option| {
            let option: SelectOption = option.into();
            ComboOption {
                label: option.label().to_owned(),
                enabled: option.is_enabled(),
            }
        })
        .collect();
    ComboBox {
        spec: ComboSpec {
            options,
            ..ComboSpec::default()
        },
        on_pick: None,
        on_text: None,
        handle: None,
    }
}

impl ComboBox {
    /// Choose option `index` to start with; the field shows its label.
    pub fn selected(mut self, index: usize) -> Self {
        self.spec.selected = Some(index);
        self
    }

    /// Take any text, the options only suggesting it.
    pub fn free_text(mut self, free: bool) -> Self {
        self.spec.free = free;
        self
    }

    /// Start a free-text combo box with `text`.
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.spec.text = text.into();
        self
    }

    /// Show `text` in the field while it is empty.
    pub fn placeholder(mut self, text: impl Into<String>) -> Self {
        self.spec.placeholder = text.into();
        self
    }

    /// Say `text` in the list when no option matches what was typed
    /// (default "No matches").
    pub fn empty_text(mut self, text: impl Into<String>) -> Self {
        self.spec.empty = text.into();
        self
    }

    /// Name the field for assistive technology.
    pub fn label(mut self, label: impl Into<String>) -> Self {
        self.spec.label = Some(label.into());
        self
    }

    /// Set the field's size within its parent (default: its content's).
    pub fn size(mut self, size: Sizing) -> Self {
        self.spec.size = size;
        self
    }

    /// Replace the whole [`ComboBoxStyle`].
    pub fn style(mut self, style: ComboBoxStyle) -> Self {
        self.spec.style = style;
        self
    }

    /// Run `pick` with the option's index when the user chooses one.
    pub fn on_pick(mut self, pick: impl FnMut(&mut EventCx<'_>, usize) + 'static) -> Self {
        self.on_pick = Some(Rc::new(RefCell::new(pick)));
        self
    }

    /// Run `commit` with the text a free-text combo box takes: on Enter, as
    /// the focus leaves, or when an option is chosen.
    pub fn on_text(mut self, commit: impl FnMut(&mut EventCx<'_>, &str) + 'static) -> Self {
        self.on_text = Some(Rc::new(RefCell::new(commit)));
        self
    }

    /// Bind `handle`, minted with [`BuildCx::handle`](viso_ui::BuildCx::handle)
    /// before the control is built, so closures declared earlier can drive it.
    pub fn handle(mut self, handle: ComboBoxHandle) -> Self {
        self.handle = Some(handle);
        self
    }

    /// Build the control into `cx` and return its handle.
    pub fn build(mut self, cx: &mut BuildCx<'_>) -> ComboBoxHandle {
        let handle = *self.handle.get_or_insert_with(|| cx.handle());
        Component::build(&self, cx);
        handle
    }
}

impl Component for ComboBox {
    fn build(&self, cx: &mut BuildCx<'_>) {
        let pick = self.on_pick.clone().map(|pick| {
            Box::new(move |ev: &mut EventCx<'_>, index: usize| (pick.borrow_mut())(ev, index))
                as viso_ui::combo_box::Pick
        });
        let commit = self.on_text.clone().map(|commit| {
            Box::new(move |ev: &mut EventCx<'_>, text: &str| (commit.borrow_mut())(ev, text))
                as viso_ui::combo_box::Commit
        });
        let parts = ComboParts::build(cx, &self.spec, pick, commit);
        if let Some(bound) = self.handle {
            cx.bind_control(bound.0, parts.root().id(), Rc::new(ComboBoxParts(parts)));
        }
    }
}

viso_ui::control_handle! {
    /// A handle an application captures to read and set a built combo box's
    /// value and open its list from its own handlers.
    ///
    /// `Copy`, an id alone: `build` returns it, or mint one first with
    /// [`BuildCx::handle`](viso_ui::BuildCx::handle) for closures declared
    /// before the control. Once the control is gone, every call is a no-op.
    pub struct ComboBoxHandle;
}

impl ComboBoxHandle {
    /// The chosen option's index: for free text, the option the text spells.
    pub fn selected(self, cx: &impl Controls) -> Option<usize> {
        match cx.control::<ComboBoxParts>(self.0) {
            Some(parts) => parts.selected(),
            None => None,
        }
    }

    /// The value: the chosen option's label, or a free-text box's text.
    pub fn text(self, cx: &impl Controls) -> String {
        match cx.control::<ComboBoxParts>(self.0) {
            Some(parts) => parts.text(),
            None => Default::default(),
        }
    }

    /// Choose option `index`, or none, without telling `on_pick`.
    pub fn select(self, ev: &mut EventCx<'_>, index: Option<usize>) {
        if let Some(parts) = ev.control::<ComboBoxParts>(self.0) {
            parts.select(ev, index);
        }
    }

    /// Set a free-text box's text, without telling `on_text`.
    pub fn set_text(self, ev: &mut EventCx<'_>, text: &str) {
        if let Some(parts) = ev.control::<ComboBoxParts>(self.0) {
            parts.set_text(ev, text);
        }
    }

    /// Open the whole list.
    pub fn open(self, ev: &mut EventCx<'_>) {
        if let Some(parts) = ev.control::<ComboBoxParts>(self.0) {
            parts.open(ev);
        }
    }

    /// Close the list, putting the value back in the field.
    pub fn close(self, ev: &mut EventCx<'_>) {
        if let Some(parts) = ev.control::<ComboBoxParts>(self.0) {
            parts.close(ev);
        }
    }

    /// Whether the list shows.
    pub fn is_open(self, cx: &impl Controls) -> bool {
        match cx.control::<ComboBoxParts>(self.0) {
            Some(parts) => parts.is_open(),
            None => false,
        }
    }
}

/// What a built control's handlers and its [`ComboBoxHandle`] share.
#[derive(Clone)]
struct ComboBoxParts(ComboParts);

impl ComboBoxParts {
    /// The chosen option's index: for free text, the option the text spells.
    pub fn selected(&self) -> Option<usize> {
        self.0.selected()
    }

    /// The value: the chosen option's label, or a free-text box's text.
    pub fn text(&self) -> String {
        self.0.text()
    }

    /// Choose option `index`, or none, without telling `on_pick`.
    pub fn select(&self, ev: &mut EventCx<'_>, index: Option<usize>) {
        self.0.select(ev, index);
    }

    /// Set a free-text box's text, without telling `on_text`.
    pub fn set_text(&self, ev: &mut EventCx<'_>, text: &str) {
        self.0.set_text(ev, text);
    }

    /// Open the whole list.
    pub fn open(&self, ev: &mut EventCx<'_>) {
        self.0.open(ev);
    }

    /// Close the list, putting the value back in the field.
    pub fn close(&self, ev: &mut EventCx<'_>) {
        self.0.close(ev);
    }

    /// Whether the list shows.
    pub fn is_open(&self) -> bool {
        self.0.is_open()
    }
}
