//! The window's root view: GPU surface, first responder, and the app's
//! `UITextInput` client.
//!
//! - Touches, Pencil, and indirect-pointer (trackpad/mouse) clicks arrive as
//!   `touches…:withEvent:`; pointer hover comes from a hover recognizer and
//!   wheel/trackpad scrolling from a pan recognizer restricted to scroll
//!   input.
//! - Hardware keys arrive as presses. Keys that make no text (Enter, arrows,
//!   Backspace, …) are reported and repeated here and kept from the text
//!   system; the rest are reported and passed on so the text system turns
//!   them into `insertText:` or a composition. While a composition runs every
//!   press goes to the text system, which owns the keys then.
//! - The text system edits a mirror ([`ImeText`]) of the stretch of the
//!   focused field's text the app shows, the composition included; its
//!   edits become preedit, commit and ranged events. With the text shown
//!   the keyboard corrects and predicts from it.
//! - The soft keyboard is shown by the system whenever the first responder
//!   takes text; hiding it swaps in an empty input view, so the view stays
//!   first responder (and keeps hardware keys) either way.
//! - The edit menu is the system's: an edit-menu interaction on iOS 16 and
//!   later, the shared menu controller before. Its items are the view's
//!   `copy:`, `cut:`, `paste:` and `selectAll:` actions, offered as the app's
//!   menu allows.
//! - Drags from other apps arrive through a drop interaction ([`drop`]).

mod drop;

use std::cell::{Cell, RefCell};

use objc2::rc::{Retained, Weak};
use objc2::runtime::{AnyClass, AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{
    ClassType, DefinedClass, MainThreadMarker, MainThreadOnly, Message, define_class, msg_send, sel,
};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{
    NSArray, NSComparisonResult, NSDate, NSDictionary, NSNumber, NSRange, NSSet, NSString, NSTimer,
    NSValue,
};
use objc2_ui_kit::{
    UIDropInteraction, UIDropInteractionDelegate, UIDropProposal, UIDropSession,
    UIEditMenuConfiguration, UIEditMenuInteraction, UIEditMenuInteractionAnimating,
    UIEditMenuInteractionDelegate, UIEvent, UIEventButtonMask, UIGestureRecognizer,
    UIGestureRecognizerDelegate, UIGestureRecognizerState, UIHoverGestureRecognizer, UIKeyInput,
    UIKeyboardType, UIPanGestureRecognizer, UIPinchGestureRecognizer, UIPress, UIPressesEvent,
    UIResponder, UIReturnKeyType, UIRotationGestureRecognizer, UIScrollTypeMask,
    UITextAutocapitalizationType, UITextAutocorrectionType, UITextInput, UITextInputDelegate,
    UITextInputStringTokenizer, UITextInputTokenizer, UITextInputTraits, UITextPosition,
    UITextRange, UITextSelectionRect, UITextSmartDashesType, UITextSmartInsertDeleteType,
    UITextSmartQuotesType, UITextSpellCheckingType, UITouch, UITouchPhase, UITouchType,
    UITraitCollection, UITraitEnvironment, UIView,
};

use super::{clipboard, drive, push, report_appearance};
use crate::backend::ime_text::ImeText;
use crate::backend::ios_translate::{
    REPEAT_DELAY_SECS, REPEAT_INTERVAL_SECS, TouchIds, is_command_key, key_code, keyboard_overlap,
    modifiers, pressure, repeats,
};
use crate::clipboard::{ClipboardReply, PasteReply};
use crate::control::{
    EditMenu, EditMenuAction, InputKind, LogicalRect, Loupe, SurroundingText, TextEntry, WindowId,
};
use crate::event::{
    ClipboardShortcut, KeyCode, LogicalInsets, Modifiers, PointerButtons, PointerId, PointerKind,
    PointerPhase, RawEvent, RawKey, RawPointer, RawScroll, clipboard_shortcut,
};

/// `UITextLayoutDirection` values toward the document's start.
const DIRECTION_LEFT: isize = 3;
const DIRECTION_UP: isize = 4;

pub(super) struct ViewIvars {
    window: WindowId,
    /// The geometry last reported: scale factor and physical size.
    scale: Cell<f64>,
    size: Cell<(u32, u32)>,
    safe_area: Cell<LogicalInsets>,
    keyboard_height: Cell<f64>,
    touches: RefCell<TouchIds>,
    /// Indirect-pointer buttons held, as a [`PointerButtons`] mask.
    mouse_buttons: Cell<u8>,
    /// The text the keyboard edits: UTF-16 offsets into it are the
    /// document's positions.
    ime: RefCell<ImeText>,
    /// The focused field's caret, logical points in this view.
    ime_caret: Cell<Option<LogicalRect>>,
    /// The focused field takes a secret.
    secure: Cell<bool>,
    /// The focused field shows the keyboard its text.
    shows_text: Cell<bool>,
    /// What the focused field holds.
    kind: Cell<InputKind>,
    /// The focused field holds lines.
    multiline: Cell<bool>,
    soft_keyboard: Cell<bool>,
    /// The empty input view that stands in for the soft keyboard while it
    /// is hidden.
    no_keyboard: RefCell<Option<Retained<UIView>>>,
    input_delegate: RefCell<Weak<ProtocolObject<dyn UITextInputDelegate>>>,
    tokenizer: RefCell<Option<Retained<UITextInputStringTokenizer>>>,
    /// The held key being repeated, with its timer and modifiers.
    repeat: RefCell<Option<(KeyCode, Modifiers, Retained<NSTimer>)>>,
    /// The edit menu while it shows.
    edit_menu: Cell<Option<EditMenu>>,
    /// The edit-menu interaction, on iOS 16 and later.
    menu_interaction: RefCell<Option<Retained<UIEditMenuInteraction>>>,
    /// The menu is being closed or replaced at the app's word, so its
    /// closing is not the user's.
    menu_replacing: Cell<bool>,
    /// The loupe session while the magnifier shows, on iOS 17 and later.
    loupe: RefCell<Option<Retained<AnyObject>>>,
    /// What the drag from another app over the view offers, and the app's
    /// answer.
    arrival: RefCell<Option<(crate::DropKinds, crate::DropReply)>>,
    /// A drop whose contents are loading.
    landing: RefCell<Option<drop::Landing>>,
    /// The trackpad's gestures under way: a pinch, a rotation, as bits.
    gestures: Cell<u8>,
}

/// The pinch among a view's trackpad gestures.
const PINCH: u8 = 1;
/// The rotation among them.
const ROTATE: u8 = 2;

define_class!(
    // SAFETY: UIView has no subclassing requirements beyond calling its
    // designated initializer (done in `new`); `VisoView` has no `Drop` impl.
    #[unsafe(super(UIView, UIResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoView"]
    #[ivars = ViewIvars]
    pub(super) struct VisoView;

    unsafe impl NSObjectProtocol for VisoView {}

    impl VisoView {
        #[unsafe(method(canBecomeFirstResponder))]
        fn can_become_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method_id(accessibilityElements))]
        fn accessibility_elements(&self) -> Option<Retained<NSArray>> {
            let mut elements = None;
            super::guarded(|| elements = super::access::elements());
            elements
        }

        #[unsafe(method(layoutSubviews))]
        fn layout_subviews(&self) {
            // SAFETY: forwards to UIView's implementation.
            let _: () = unsafe { msg_send![super(self), layoutSubviews] };
            super::guarded(|| {
                self.report_geometry();
                drive();
            });
        }

        #[unsafe(method(safeAreaInsetsDidChange))]
        fn safe_area_insets_did_change(&self) {
            // SAFETY: forwards to UIView's implementation.
            let _: () = unsafe { msg_send![super(self), safeAreaInsetsDidChange] };
            super::guarded(|| {
                let e = self.safeAreaInsets();
                let insets = LogicalInsets {
                    top: e.top,
                    left: e.left,
                    bottom: e.bottom,
                    right: e.right,
                };
                if self.ivars().safe_area.replace(insets) != insets {
                    push(RawEvent::SafeAreaChanged {
                        window: self.ivars().window,
                        insets,
                    });
                    drive();
                }
            });
        }

        #[unsafe(method(traitCollectionDidChange:))]
        fn trait_collection_did_change(&self, previous: Option<&UITraitCollection>) {
            // SAFETY: forwards to UIView's implementation.
            let _: () = unsafe { msg_send![super(self), traitCollectionDidChange: previous] };
            super::guarded(|| {
                report_appearance(&self.traitCollection());
                drive();
            });
        }

        #[unsafe(method(touchesBegan:withEvent:))]
        fn touches_began(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            super::guarded(|| self.touches(touches, event, PointerPhase::Down));
        }

        #[unsafe(method(touchesMoved:withEvent:))]
        fn touches_moved(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            super::guarded(|| self.touches(touches, event, PointerPhase::Moved));
        }

        #[unsafe(method(touchesEnded:withEvent:))]
        fn touches_ended(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            super::guarded(|| self.touches(touches, event, PointerPhase::Up));
        }

        #[unsafe(method(touchesCancelled:withEvent:))]
        fn touches_cancelled(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>) {
            super::guarded(|| self.touches(touches, event, PointerPhase::Cancel));
        }

        #[unsafe(method(hover:))]
        fn hover(&self, recognizer: &UIHoverGestureRecognizer) {
            super::guarded(|| {
                let phase = match gesture_state(recognizer) {
                    UIGestureRecognizerState::Began | UIGestureRecognizerState::Changed => {
                        PointerPhase::Moved
                    }
                    // A pressed pointer leaves hover while it drags; the
                    // touch stream carries it until release.
                    _ if self.ivars().mouse_buttons.get() != 0 => return,
                    _ => PointerPhase::Left,
                };
                let p = recognizer.locationInView(Some(self));
                push(RawEvent::Pointer(RawPointer {
                    window: self.ivars().window,
                    pointer: PointerId::MOUSE,
                    kind: PointerKind::Mouse,
                    x: p.x,
                    y: p.y,
                    pressure: 0.0,
                    buttons: PointerButtons::NONE,
                    modifiers: modifiers(recognizer.modifierFlags().0),
                    phase,
                }));
                drive();
            });
        }

        #[unsafe(method(scroll:))]
        fn scroll(&self, recognizer: &UIPanGestureRecognizer) {
            super::guarded(|| {
                let t = recognizer.translationInView(Some(self));
                recognizer.setTranslation_inView(CGPoint::new(0.0, 0.0), Some(self));
                // The recognizer's states make the trackpad gesture; the
                // window coasts it after the lift.
                let phase = match gesture_state(recognizer) {
                    UIGestureRecognizerState::Began => crate::ScrollPhase::Began,
                    UIGestureRecognizerState::Changed => crate::ScrollPhase::Changed,
                    UIGestureRecognizerState::Ended | UIGestureRecognizerState::Cancelled => {
                        crate::ScrollPhase::Ended
                    }
                    _ => crate::ScrollPhase::None,
                };
                if t.x == 0.0 && t.y == 0.0 && phase != crate::ScrollPhase::Ended {
                    return;
                }
                let p = recognizer.locationInView(Some(self));
                push(RawEvent::Scroll(RawScroll {
                    window: self.ivars().window,
                    x: p.x,
                    y: p.y,
                    delta_x: -t.x,
                    delta_y: -t.y,
                    modifiers: modifiers(recognizer.modifierFlags().0),
                    phase,
                }));
                drive();
            });
        }

        #[unsafe(method(pinch:))]
        fn pinch(&self, recognizer: &UIPinchGestureRecognizer) {
            super::guarded(|| {
                let scale = recognizer.scale();
                recognizer.setScale(1.0);
                self.gesture(recognizer, PINCH, scale, 0.0);
            });
        }

        #[unsafe(method(rotate:))]
        fn rotate(&self, recognizer: &UIRotationGestureRecognizer) {
            super::guarded(|| {
                let turn = recognizer.rotation();
                recognizer.setRotation(0.0);
                self.gesture(recognizer, ROTATE, 1.0, turn);
            });
        }

        #[unsafe(method(pressesBegan:withEvent:))]
        fn presses_began(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            let mut forward = Vec::new();
            super::guarded(|| forward = self.presses(presses, true));
            self.forward_presses(sel!(pressesBegan:withEvent:), presses, forward, event);
        }

        #[unsafe(method(pressesEnded:withEvent:))]
        fn presses_ended(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            let mut forward = Vec::new();
            super::guarded(|| forward = self.presses(presses, false));
            self.forward_presses(sel!(pressesEnded:withEvent:), presses, forward, event);
        }

        #[unsafe(method(pressesCancelled:withEvent:))]
        fn presses_cancelled(&self, presses: &NSSet<UIPress>, event: Option<&UIPressesEvent>) {
            let mut forward = Vec::new();
            super::guarded(|| forward = self.presses(presses, false));
            self.forward_presses(sel!(pressesCancelled:withEvent:), presses, forward, event);
        }

        #[unsafe(method(keyRepeat:))]
        fn key_repeat(&self, _timer: &NSTimer) {
            super::guarded(|| {
                let Some((code, modifiers, _)) = &*self.ivars().repeat.borrow() else {
                    return;
                };
                push(RawEvent::Key(RawKey {
                    window: self.ivars().window,
                    code: *code,
                    pressed: true,
                    repeat: true,
                    modifiers: *modifiers,
                }));
            });
            super::guarded(drive);
        }

        #[unsafe(method(copy:))]
        fn copy(&self, _sender: Option<&AnyObject>) {
            super::guarded(|| self.clipboard(ClipboardShortcut::Copy));
        }

        #[unsafe(method(cut:))]
        fn cut(&self, _sender: Option<&AnyObject>) {
            super::guarded(|| self.clipboard(ClipboardShortcut::Cut));
        }

        #[unsafe(method(paste:))]
        fn paste(&self, _sender: Option<&AnyObject>) {
            super::guarded(|| self.clipboard(ClipboardShortcut::Paste));
        }

        #[unsafe(method(selectAll:))]
        fn select_all(&self, _sender: Option<&AnyObject>) {
            super::guarded(|| {
                self.menu_picked();
                push(RawEvent::EditMenu {
                    window: self.ivars().window,
                    action: EditMenuAction::SelectAll,
                });
                drive();
            });
        }

        #[unsafe(method(canPerformAction:withSender:))]
        fn can_perform_action(&self, action: Sel, sender: Option<&AnyObject>) -> bool {
            // While the edit menu shows, its items are what the app offers;
            // otherwise the clipboard keys always reach the app.
            let menu = self.ivars().edit_menu.get();
            if action == sel!(copy:) {
                menu.is_none_or(|m| m.copy)
            } else if action == sel!(cut:) {
                menu.is_none_or(|m| m.cut)
            } else if action == sel!(paste:) {
                menu.is_none_or(|m| m.paste && clipboard::has_content())
            } else if action == sel!(selectAll:) {
                menu.is_some_and(|m| m.select_all)
            } else {
                // SAFETY: forwards to UIResponder's implementation.
                unsafe { msg_send![super(self), canPerformAction: action, withSender: sender] }
            }
        }

        #[unsafe(method_id(inputView))]
        fn input_view(&self) -> Option<Retained<UIView>> {
            self.keyboard_stand_in()
        }
    }

    unsafe impl UIGestureRecognizerDelegate for VisoView {
        // A trackpad's pinch and rotation run together, as one gesture.
        #[unsafe(method(gestureRecognizer:shouldRecognizeSimultaneouslyWithGestureRecognizer:))]
        fn simultaneous(&self, a: &UIGestureRecognizer, b: &UIGestureRecognizer) -> bool {
            let transform = |r: &UIGestureRecognizer| {
                r.isKindOfClass(UIPinchGestureRecognizer::class())
                    || r.isKindOfClass(UIRotationGestureRecognizer::class())
            };
            transform(a) && transform(b)
        }
    }

    unsafe impl UIEditMenuInteractionDelegate for VisoView {
        #[unsafe(method(editMenuInteraction:targetRectForConfiguration:))]
        fn edit_menu_target(
            &self,
            _interaction: &UIEditMenuInteraction,
            _configuration: &UIEditMenuConfiguration,
        ) -> CGRect {
            self.ivars()
                .edit_menu
                .get()
                .map_or(CGRect::default(), |m| cg_rect(m.at))
        }

        #[unsafe(method(editMenuInteraction:willDismissMenuForConfiguration:animator:))]
        fn edit_menu_will_dismiss(
            &self,
            _interaction: &UIEditMenuInteraction,
            _configuration: &UIEditMenuConfiguration,
            _animator: &ProtocolObject<dyn UIEditMenuInteractionAnimating>,
        ) {
            if !self.ivars().menu_replacing.get() {
                super::guarded(|| self.menu_closed());
            }
        }
    }

    unsafe impl UIDropInteractionDelegate for VisoView {
        #[unsafe(method(dropInteraction:canHandleSession:))]
        fn drop_can_handle(
            &self,
            _interaction: &UIDropInteraction,
            _session: &ProtocolObject<dyn UIDropSession>,
        ) -> bool {
            true
        }

        #[unsafe(method_id(dropInteraction:sessionDidUpdate:))]
        fn drop_did_update(
            &self,
            _interaction: &UIDropInteraction,
            session: &ProtocolObject<dyn UIDropSession>,
        ) -> Retained<UIDropProposal> {
            let mut operation = objc2_ui_kit::UIDropOperation::Cancel;
            super::guarded(|| operation = self.drop_step(session, crate::DropPhase::Over));
            UIDropProposal::initWithDropOperation(UIDropProposal::alloc(self.mtm()), operation)
        }

        #[unsafe(method(dropInteraction:sessionDidExit:))]
        fn drop_did_exit(
            &self,
            _interaction: &UIDropInteraction,
            session: &ProtocolObject<dyn UIDropSession>,
        ) {
            super::guarded(|| {
                self.drop_step(session, crate::DropPhase::Leave);
            });
        }

        #[unsafe(method(dropInteraction:performDrop:))]
        fn drop_perform(
            &self,
            _interaction: &UIDropInteraction,
            session: &ProtocolObject<dyn UIDropSession>,
        ) {
            super::guarded(|| self.perform_drop(session));
        }
    }

    unsafe impl UITextInputTraits for VisoView {
        // The keyboard corrects and checks words only in text it can read.
        #[unsafe(method(autocorrectionType))]
        fn autocorrection_type(&self) -> UITextAutocorrectionType {
            if self.corrects() {
                UITextAutocorrectionType::Default
            } else {
                UITextAutocorrectionType::No
            }
        }

        #[unsafe(method(spellCheckingType))]
        fn spell_checking_type(&self) -> UITextSpellCheckingType {
            if self.corrects() {
                UITextSpellCheckingType::Default
            } else {
                UITextSpellCheckingType::No
            }
        }

        #[unsafe(method(smartQuotesType))]
        fn smart_quotes_type(&self) -> UITextSmartQuotesType {
            UITextSmartQuotesType::No
        }

        #[unsafe(method(smartDashesType))]
        fn smart_dashes_type(&self) -> UITextSmartDashesType {
            UITextSmartDashesType::No
        }

        #[unsafe(method(smartInsertDeleteType))]
        fn smart_insert_delete_type(&self) -> UITextSmartInsertDeleteType {
            UITextSmartInsertDeleteType::No
        }

        #[unsafe(method(autocapitalizationType))]
        fn autocapitalization_type(&self) -> UITextAutocapitalizationType {
            UITextAutocapitalizationType::None
        }

        #[unsafe(method(keyboardType))]
        fn keyboard_type(&self) -> UIKeyboardType {
            match self.ivars().kind.get() {
                InputKind::Text => UIKeyboardType::Default,
                InputKind::Digits => UIKeyboardType::ASCIICapableNumberPad,
                // The pads have no minus sign.
                InputKind::Integer | InputKind::Decimal => UIKeyboardType::NumbersAndPunctuation,
                InputKind::Phone => UIKeyboardType::PhonePad,
                InputKind::Email => UIKeyboardType::EmailAddress,
                InputKind::Url => UIKeyboardType::URL,
                InputKind::Search => UIKeyboardType::WebSearch,
            }
        }

        #[unsafe(method(returnKeyType))]
        fn return_key_type(&self) -> UIReturnKeyType {
            let ivars = self.ivars();
            if ivars.multiline.get() {
                UIReturnKeyType::Default
            } else if ivars.kind.get() == InputKind::Search {
                UIReturnKeyType::Search
            } else {
                UIReturnKeyType::Done
            }
        }

        // A secure field's keyboard composes, suggests and records nothing,
        // and the system keeps its keys from screen capture.
        #[unsafe(method(isSecureTextEntry))]
        fn is_secure_text_entry(&self) -> bool {
            self.ivars().secure.get()
        }
    }

    unsafe impl UIKeyInput for VisoView {
        // Always true, so the keyboard's delete key reaches the app's text.
        #[unsafe(method(hasText))]
        fn has_text(&self) -> bool {
            true
        }

        #[unsafe(method(insertText:))]
        fn insert_text(&self, text: &NSString) {
            super::guarded(|| {
                let text = text.to_string();
                let composing = self.ivars().ime.borrow().is_marked();
                if text == "\n" && !composing {
                    self.tap_key(KeyCode::Enter);
                } else {
                    let event = self.ime_edit(|ime, w| ime.insert(w, &text, None));
                    self.apply(event);
                }
                drive();
            });
        }

        #[unsafe(method(deleteBackward))]
        fn delete_backward(&self) {
            super::guarded(|| {
                let window = self.ivars().window;
                let mut ime = self.ivars().ime.borrow_mut();
                let (start, end) = ime.selection();
                let start = if start == end {
                    ime.text()[..start]
                        .char_indices()
                        .next_back()
                        .map_or(start, |(at, _)| at)
                } else {
                    start
                };
                if ime.is_marked() {
                    let event = ime.replace(window, start, end, "");
                    drop(ime);
                    self.apply(event);
                } else {
                    // The app deletes by its own rules; the mirror guesses
                    // a character until the app shows its text again.
                    if ime.is_shown() && start < end {
                        let _ = ime.replace(window, start, end, "");
                    }
                    drop(ime);
                    self.tap_key(KeyCode::Backspace);
                }
                drive();
            });
        }
    }

    unsafe impl UITextInput for VisoView {
        #[unsafe(method_id(textInRange:))]
        fn text_in_range(&self, range: &UITextRange) -> Option<Retained<NSString>> {
            range_of(range).map(|(start, end)| {
                let ime = self.ivars().ime.borrow();
                let (a, b) = ime.span(ime.byte(start), ime.byte(end));
                NSString::from_str(&ime.text()[a..b])
            })
        }

        #[unsafe(method(replaceRange:withText:))]
        fn replace_range(&self, range: &UITextRange, text: &NSString) {
            super::guarded(|| {
                let Some((start, end)) = range_of(range) else {
                    return;
                };
                let text = text.to_string();
                let event = self.ime_edit(|ime, w| {
                    let (a, b) = (ime.byte(start), ime.byte(end));
                    ime.replace(w, a, b, &text)
                });
                self.apply(event);
                drive();
            });
        }

        #[unsafe(method_id(selectedTextRange))]
        fn selected_text_range(&self) -> Option<Retained<UITextRange>> {
            let ime = self.ivars().ime.borrow();
            let (start, end) = ime.selection();
            Some(VisoTextRange::span(self.mtm(), ime.units(start), ime.units(end)))
        }

        #[unsafe(method(setSelectedTextRange:))]
        fn set_selected_text_range(&self, range: Option<&UITextRange>) {
            super::guarded(|| {
                let Some((start, end)) = range.and_then(range_of) else {
                    return;
                };
                // Inside the composition the caret moves in the preedit.
                let event = self.ime_edit(|ime, w| {
                    let (a, b) = (ime.byte(start), ime.byte(end));
                    ime.select(w, a, b).or_else(|| ime.preedit(w))
                });
                if event.is_some() {
                    self.apply(event);
                    drive();
                }
            });
        }

        #[unsafe(method_id(markedTextRange))]
        fn marked_text_range(&self) -> Option<Retained<UITextRange>> {
            let ime = self.ivars().ime.borrow();
            ime.marked()
                .map(|(start, end)| VisoTextRange::span(self.mtm(), ime.units(start), ime.units(end)))
        }

        #[unsafe(method_id(markedTextStyle))]
        fn marked_text_style(&self) -> Option<Retained<NSDictionary>> {
            None
        }

        #[unsafe(method(setMarkedTextStyle:))]
        fn set_marked_text_style(&self, _style: Option<&NSDictionary>) {}

        #[unsafe(method(setMarkedText:selectedRange:))]
        fn set_marked_text(&self, text: Option<&NSString>, selected: NSRange) {
            super::guarded(|| {
                let text = text.map(|t| t.to_string()).unwrap_or_default();
                let at = |units: usize| super::super::utf16::byte_offset(&text, units);
                let selected = (
                    at(selected.location),
                    at(selected.location.saturating_add(selected.length)),
                );
                // A ranged selection is the clause being converted.
                let clauses = crate::PreeditClauses {
                    clauses: Vec::new(),
                    selected: (selected.0 < selected.1).then_some(selected),
                };
                let event =
                    self.ime_edit(|ime, w| ime.set_marked(w, &text, selected, clauses, None));
                self.apply(event);
                drive();
            });
        }

        #[unsafe(method(unmarkText))]
        fn unmark_text(&self) {
            super::guarded(|| {
                let event = self.ime_edit(ImeText::unmark);
                self.apply(event);
                drive();
            });
        }

        #[unsafe(method_id(beginningOfDocument))]
        fn beginning_of_document(&self) -> Retained<UITextPosition> {
            VisoTextPosition::at(self.mtm(), 0)
        }

        #[unsafe(method_id(endOfDocument))]
        fn end_of_document(&self) -> Retained<UITextPosition> {
            VisoTextPosition::at(self.mtm(), self.ivars().ime.borrow().len_units())
        }

        #[unsafe(method_id(textRangeFromPosition:toPosition:))]
        fn text_range_from_position(
            &self,
            from: &UITextPosition,
            to: &UITextPosition,
        ) -> Option<Retained<UITextRange>> {
            match (offset_of(from), offset_of(to)) {
                (Some(a), Some(b)) => Some(VisoTextRange::span(self.mtm(), a.min(b), a.max(b))),
                _ => None,
            }
        }

        #[unsafe(method_id(positionFromPosition:offset:))]
        fn position_from_position(
            &self,
            position: &UITextPosition,
            offset: isize,
        ) -> Option<Retained<UITextPosition>> {
            self.moved(position, offset)
        }

        #[unsafe(method_id(positionFromPosition:inDirection:offset:))]
        fn position_from_position_in_direction(
            &self,
            position: &UITextPosition,
            direction: isize,
            offset: isize,
        ) -> Option<Retained<UITextPosition>> {
            let backward = direction == DIRECTION_LEFT || direction == DIRECTION_UP;
            self.moved(position, if backward { -offset } else { offset })
        }

        #[unsafe(method(comparePosition:toPosition:))]
        fn compare_position(
            &self,
            position: &UITextPosition,
            other: &UITextPosition,
        ) -> NSComparisonResult {
            match offset_of(position).cmp(&offset_of(other)) {
                std::cmp::Ordering::Less => NSComparisonResult::Ascending,
                std::cmp::Ordering::Equal => NSComparisonResult::Same,
                std::cmp::Ordering::Greater => NSComparisonResult::Descending,
            }
        }

        #[unsafe(method(offsetFromPosition:toPosition:))]
        fn offset_from_position(&self, from: &UITextPosition, to: &UITextPosition) -> isize {
            match (offset_of(from), offset_of(to)) {
                (Some(a), Some(b)) => b as isize - a as isize,
                _ => 0,
            }
        }

        #[unsafe(method_id(inputDelegate))]
        fn input_delegate(&self) -> Option<Retained<ProtocolObject<dyn UITextInputDelegate>>> {
            self.ivars().input_delegate.borrow().load()
        }

        #[unsafe(method(setInputDelegate:))]
        fn set_input_delegate(&self, delegate: Option<&ProtocolObject<dyn UITextInputDelegate>>) {
            *self.ivars().input_delegate.borrow_mut() = match delegate {
                Some(delegate) => Weak::new(delegate),
                None => Weak::default(),
            };
        }

        #[unsafe(method_id(tokenizer))]
        fn tokenizer(&self) -> Retained<ProtocolObject<dyn UITextInputTokenizer>> {
            let mut slot = self.ivars().tokenizer.borrow_mut();
            let tokenizer = slot.get_or_insert_with(|| {
                let responder: &UIResponder = self;
                // SAFETY: the tokenizer queries this view, which outlives it
                // (the view owns the only strong reference).
                unsafe {
                    UITextInputStringTokenizer::initWithTextInput(
                        UITextInputStringTokenizer::alloc(self.mtm()),
                        responder,
                    )
                }
            });
            ProtocolObject::from_retained(tokenizer.clone())
        }

        #[unsafe(method_id(positionWithinRange:farthestInDirection:))]
        fn position_within_range(
            &self,
            range: &UITextRange,
            direction: isize,
        ) -> Option<Retained<UITextPosition>> {
            let backward = direction == DIRECTION_LEFT || direction == DIRECTION_UP;
            range_of(range).map(|(start, end)| {
                VisoTextPosition::at(self.mtm(), if backward { start } else { end })
            })
        }

        #[unsafe(method_id(characterRangeByExtendingPosition:inDirection:))]
        fn character_range_by_extending(
            &self,
            position: &UITextPosition,
            direction: isize,
        ) -> Option<Retained<UITextRange>> {
            let len = self.ivars().ime.borrow().len_units();
            let backward = direction == DIRECTION_LEFT || direction == DIRECTION_UP;
            offset_of(position).map(|at| {
                let (start, end) = if backward { (0, at) } else { (at, len) };
                VisoTextRange::span(self.mtm(), start, end)
            })
        }

        // `NSWritingDirectionLeftToRight`: the preedit is laid out by the
        // app, so the system only needs a stable answer.
        #[unsafe(method(baseWritingDirectionForPosition:inDirection:))]
        fn base_writing_direction(&self, _position: &UITextPosition, _direction: isize) -> isize {
            0
        }

        #[unsafe(method(setBaseWritingDirection:forRange:))]
        fn set_base_writing_direction(&self, _direction: isize, _range: &UITextRange) {}

        #[unsafe(method(firstRectForRange:))]
        fn first_rect_for_range(&self, _range: &UITextRange) -> CGRect {
            self.caret_rect()
        }

        #[unsafe(method(caretRectForPosition:))]
        fn caret_rect_for_position(&self, _position: &UITextPosition) -> CGRect {
            self.caret_rect()
        }

        #[unsafe(method_id(selectionRectsForRange:))]
        fn selection_rects_for_range(
            &self,
            _range: &UITextRange,
        ) -> Retained<NSArray<UITextSelectionRect>> {
            NSArray::new()
        }

        #[unsafe(method_id(closestPositionToPoint:))]
        fn closest_position_to_point(&self, _point: CGPoint) -> Option<Retained<UITextPosition>> {
            let ime = self.ivars().ime.borrow();
            Some(VisoTextPosition::at(self.mtm(), ime.units(ime.selection().1)))
        }

        #[unsafe(method_id(closestPositionToPoint:withinRange:))]
        fn closest_position_within_range(
            &self,
            _point: CGPoint,
            range: &UITextRange,
        ) -> Option<Retained<UITextPosition>> {
            range_of(range).map(|(_, end)| VisoTextPosition::at(self.mtm(), end))
        }

        #[unsafe(method_id(characterRangeAtPoint:))]
        fn character_range_at_point(&self, _point: CGPoint) -> Option<Retained<UITextRange>> {
            None
        }
    }
);

impl VisoView {
    pub(super) fn new(
        mtm: MainThreadMarker,
        window: WindowId,
        frame: CGRect,
        scale: f64,
    ) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ViewIvars {
            window,
            scale: Cell::new(scale),
            size: Cell::new(physical_size(frame.size, scale)),
            safe_area: Cell::new(LogicalInsets::default()),
            keyboard_height: Cell::new(0.0),
            touches: RefCell::new(TouchIds::default()),
            mouse_buttons: Cell::new(0),
            ime: RefCell::new(ImeText::default()),
            ime_caret: Cell::new(None),
            secure: Cell::new(false),
            shows_text: Cell::new(false),
            kind: Cell::new(InputKind::Text),
            multiline: Cell::new(false),
            soft_keyboard: Cell::new(false),
            no_keyboard: RefCell::new(None),
            input_delegate: RefCell::new(Weak::default()),
            tokenizer: RefCell::new(None),
            repeat: RefCell::new(None),
            edit_menu: Cell::new(None),
            menu_interaction: RefCell::new(None),
            menu_replacing: Cell::new(false),
            loupe: RefCell::new(None),
            arrival: RefCell::new(None),
            landing: RefCell::new(None),
            gestures: Cell::new(0),
        });
        // SAFETY: `initWithFrame:` is UIView's designated initializer.
        let view: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        view.setContentScaleFactor(scale);
        view.setMultipleTouchEnabled(true);

        // SAFETY: the view implements each recognizer's action with the
        // `(&self, &Recognizer)` signature recognizers call, and owns the
        // recognizers, so the target outlives them.
        let hover = unsafe {
            UIHoverGestureRecognizer::initWithTarget_action(
                UIHoverGestureRecognizer::alloc(mtm),
                Some(&view),
                Some(sel!(hover:)),
            )
        };
        view.addGestureRecognizer(&hover);
        // SAFETY: as for `hover`.
        let scroll = unsafe {
            UIPanGestureRecognizer::initWithTarget_action(
                UIPanGestureRecognizer::alloc(mtm),
                Some(&view),
                Some(sel!(scroll:)),
            )
        };
        // Scroll input only: no touch type may drive it, and touches keep
        // flowing to the view while it tracks.
        scroll.setAllowedTouchTypes(&NSArray::new());
        scroll.setAllowedScrollTypesMask(UIScrollTypeMask::All);
        scroll.setCancelsTouchesInView(false);
        view.addGestureRecognizer(&scroll);
        // A trackpad's pinch and rotation: the indirect pointer only, since
        // fingers on the screen arrive as touches.
        let pointer_only =
            NSArray::from_retained_slice(&[NSNumber::new_isize(UITouchType::IndirectPointer.0)]);
        let delegate = ProtocolObject::<dyn UIGestureRecognizerDelegate>::from_ref(&*view);
        // SAFETY: as for `hover`.
        let pinch = unsafe {
            UIPinchGestureRecognizer::initWithTarget_action(
                UIPinchGestureRecognizer::alloc(mtm),
                Some(&view),
                Some(sel!(pinch:)),
            )
        };
        // SAFETY: as for `hover`.
        let rotate = unsafe {
            UIRotationGestureRecognizer::initWithTarget_action(
                UIRotationGestureRecognizer::alloc(mtm),
                Some(&view),
                Some(sel!(rotate:)),
            )
        };
        for recognizer in [&**pinch, &**rotate] {
            recognizer.setAllowedTouchTypes(&pointer_only);
            recognizer.setCancelsTouchesInView(false);
            recognizer.setDelegate(Some(delegate));
            view.addGestureRecognizer(recognizer);
        }
        if AnyClass::get(c"UIEditMenuInteraction").is_some() {
            let delegate = ProtocolObject::<dyn UIEditMenuInteractionDelegate>::from_ref(&*view);
            let interaction = UIEditMenuInteraction::initWithDelegate(
                UIEditMenuInteraction::alloc(mtm),
                Some(delegate),
            );
            view.addInteraction(ProtocolObject::from_ref(&*interaction));
            *view.ivars().menu_interaction.borrow_mut() = Some(interaction);
        }
        let delegate = ProtocolObject::<dyn UIDropInteractionDelegate>::from_ref(&*view);
        let drops = UIDropInteraction::initWithDelegate(UIDropInteraction::alloc(mtm), delegate);
        view.addInteraction(ProtocolObject::from_ref(&*drops));
        view
    }

    /// Show `menu` beside the selection, move it or change its items while
    /// it shows, or (`None`) close it.
    /// Show the magnifier over `loupe`'s caret at its finger, move it, or
    /// with `None` put it away. Nothing shows before iOS 17.
    pub(super) fn set_loupe(&self, loupe: Option<Loupe>) {
        let ivars = self.ivars();
        let Some(loupe) = loupe else {
            if let Some(session) = ivars.loupe.borrow_mut().take() {
                // SAFETY: `invalidate` is UITextLoupeSession's, taking no
                // argument, on the session this view began.
                unsafe {
                    let _: () = msg_send![&*session, invalidate];
                }
            }
            return;
        };
        let point = CGPoint {
            x: loupe.x,
            y: loupe.y,
        };
        let caret = cg_rect(loupe.caret);
        let mut held = ivars.loupe.borrow_mut();
        if held.is_none() {
            let Some(class) = AnyClass::get(c"UITextLoupeSession") else {
                return;
            };
            // SAFETY: the class method takes a point, an optional selection
            // widget view and the view the loupe magnifies, and returns a
            // session (or nil); this view is live on the main thread.
            *held = unsafe {
                msg_send![
                    class,
                    beginLoupeSessionAtPoint: point,
                    fromSelectionWidgetView: Option::<&UIView>::None,
                    inView: self
                ]
            };
        }
        if let Some(session) = held.as_ref() {
            // SAFETY: `moveToPoint:withCaretRect:trackingCaret:` is
            // UITextLoupeSession's, on the session this view began.
            unsafe {
                let _: () = msg_send![
                    &**session,
                    moveToPoint: point,
                    withCaretRect: caret,
                    trackingCaret: true
                ];
            }
        }
    }

    pub(super) fn set_edit_menu(&self, menu: Option<EditMenu>) {
        let ivars = self.ivars();
        let was = ivars.edit_menu.replace(menu);
        if was == menu {
            return;
        }
        let interaction = ivars.menu_interaction.borrow().clone();
        ivars.menu_replacing.set(true);
        match (menu, &interaction) {
            (None, Some(interaction)) => interaction.dismissMenu(),
            (Some(m), Some(interaction)) => match was {
                Some(w)
                    if (w.cut, w.copy, w.paste, w.select_all)
                        == (m.cut, m.copy, m.paste, m.select_all) =>
                {
                    interaction.updateVisibleMenuPositionAnimated(false);
                }
                Some(_) => {
                    interaction.reloadVisibleMenu();
                    interaction.updateVisibleMenuPositionAnimated(false);
                }
                None => {
                    if !self.isFirstResponder() {
                        self.becomeFirstResponder();
                    }
                    let at = cg_rect(m.at);
                    let point = CGPoint {
                        x: at.origin.x + at.size.width * 0.5,
                        y: at.origin.y,
                    };
                    // SAFETY: no identifier is a valid one.
                    let config = unsafe {
                        UIEditMenuConfiguration::configurationWithIdentifier_sourcePoint(
                            None,
                            point,
                            MainThreadMarker::from(self),
                        )
                    };
                    interaction.presentEditMenuWithConfiguration(&config);
                }
            },
            (None, None) => menu_controller(self).hideMenuFromView(self),
            (Some(m), None) => {
                if !self.isFirstResponder() {
                    self.becomeFirstResponder();
                }
                menu_controller(self).showMenuFromView_rect(self, cg_rect(m.at));
            }
        }
        ivars.menu_replacing.set(false);
    }

    /// The user picked an item of the edit menu, which closes it.
    fn menu_picked(&self) {
        if self.ivars().edit_menu.get().is_some()
            && self.ivars().menu_interaction.borrow().is_none()
        {
            // The menu controller says nothing of its closing.
            self.menu_closed();
        }
    }

    /// The edit menu closed on its own.
    fn menu_closed(&self) {
        if self.ivars().edit_menu.take().is_some() {
            push(RawEvent::EditMenu {
                window: self.ivars().window,
                action: EditMenuAction::Closed,
            });
            drive();
        }
    }

    /// The input view in place of the soft keyboard: none (the system
    /// keyboard) while it is shown, an empty view while it is hidden.
    fn keyboard_stand_in(&self) -> Option<Retained<UIView>> {
        if self.ivars().soft_keyboard.get() {
            return None;
        }
        let mut slot = self.ivars().no_keyboard.borrow_mut();
        let view = slot.get_or_insert_with(|| {
            UIView::initWithFrame(UIView::alloc(self.mtm()), CGRect::default())
        });
        Some(view.clone())
    }

    /// The drawable size in physical pixels.
    pub(super) fn pixel_size(&self) -> (u32, u32) {
        physical_size(self.bounds().size, self.contentScaleFactor())
    }

    /// Record the focused field's caret (`None`: no field has focus, so any
    /// composition is dropped), whether it takes a secret and what it holds,
    /// reloading the keyboard when those change.
    pub(super) fn set_text_entry(&self, entry: Option<TextEntry>) {
        let ivars = self.ivars();
        let caret = entry.map(|e| e.caret);
        let secure = entry.is_some_and(|e| e.secure);
        let kind = entry.map_or(InputKind::Text, |e| e.kind);
        let multiline = entry.is_some_and(|e| e.multiline);
        let traits = (ivars.secure.replace(secure) != secure)
            | (ivars.kind.replace(kind) != kind)
            | (ivars.multiline.replace(multiline) != multiline);
        if traits && self.isFirstResponder() {
            self.reloadInputViews();
        }
        let old = self.ivars().ime_caret.replace(caret);
        if caret.is_none() {
            let delegate = self.ivars().input_delegate.borrow().load();
            let me = ProtocolObject::<dyn UITextInput>::from_ref(self);
            if let Some(d) = &delegate {
                d.textWillChange(Some(me));
            }
            let event = {
                let mut ime = self.ivars().ime.borrow_mut();
                let event = ime.cancel(self.ivars().window);
                ime.show(None);
                event
            };
            if let Some(d) = &delegate {
                d.textDidChange(Some(me));
            }
            self.apply(event);
        } else if old != caret {
            // The candidate window follows the caret: have UIKit re-query it.
            if let Some(d) = self.ivars().input_delegate.borrow().load() {
                let me = ProtocolObject::<dyn UITextInput>::from_ref(self);
                d.selectionWillChange(Some(me));
                d.selectionDidChange(Some(me));
            }
        }
    }

    pub(super) fn show_soft_keyboard(&self, show: bool) {
        if self.ivars().soft_keyboard.replace(show) != show {
            if !self.isFirstResponder() {
                self.becomeFirstResponder();
            }
            self.reloadInputViews();
        }
    }

    /// Report the keyboard's end frame (screen coordinates) as the height it
    /// covers at the bottom of the view.
    pub(super) fn keyboard_frame_changed(&self, value: &NSValue) {
        // SAFETY: the keyboard frame is an `NSValue` boxing a `CGRect`.
        let frame: CGRect = unsafe { msg_send![value, CGRectValue] };
        let height = match self.window().map(|w| w.screen()) {
            Some(screen) if frame.size.height > 0.0 => {
                let space = screen.coordinateSpace();
                // SAFETY: both are live coordinate spaces; the conversion is
                // a pure geometry query.
                let local: CGRect =
                    unsafe { msg_send![self, convertRect: frame, fromCoordinateSpace: &*space] };
                let bounds = self.bounds();
                keyboard_overlap(bounds.origin.y + bounds.size.height, local.origin.y)
            }
            _ => 0.0,
        };
        if self.ivars().keyboard_height.replace(height) != height {
            push(RawEvent::KeyboardInsetChanged {
                window: self.ivars().window,
                height,
            });
            drive();
        }
    }

    fn report_geometry(&self) {
        let ivars = self.ivars();
        let scale = self.contentScaleFactor();
        let size = self.pixel_size();
        let window = ivars.window;
        if ivars.scale.replace(scale) != scale {
            ivars.size.set(size);
            push(RawEvent::ScaleFactorChanged {
                window,
                scale,
                width: size.0,
                height: size.1,
            });
        } else if ivars.size.replace(size) != size {
            push(RawEvent::Resized {
                window,
                width: size.0,
                height: size.1,
            });
        }
    }

    /// Report a sample of the trackpad gesture `which`: one gesture while
    /// either the pinch or the rotation runs.
    fn gesture(&self, recognizer: &UIGestureRecognizer, which: u8, scale: f64, rotation: f64) {
        use crate::GesturePhase;
        let ivars = self.ivars();
        let before = ivars.gestures.get();
        let now = match gesture_state(recognizer) {
            UIGestureRecognizerState::Began => before | which,
            UIGestureRecognizerState::Changed => before,
            _ => before & !which,
        };
        ivars.gestures.set(now);
        let phase = match (before, now) {
            (0, 0) => return,
            (0, _) => GesturePhase::Began,
            (_, 0) => GesturePhase::Ended,
            _ => GesturePhase::Changed,
        };
        let p = recognizer.locationInView(Some(self));
        push(RawEvent::Gesture(crate::RawGesture {
            window: ivars.window,
            x: p.x,
            y: p.y,
            scale,
            rotation,
            dx: 0.0,
            dy: 0.0,
            modifiers: modifiers(recognizer.modifierFlags().0),
            phase,
        }));
        drive();
    }

    fn touches(&self, touches: &NSSet<UITouch>, event: Option<&UIEvent>, phase: PointerPhase) {
        let ivars = self.ivars();
        let flags = event.map_or(0, |e| e.modifierFlags().0);
        for touch in touches.iter() {
            let kind = match touch.r#type() {
                UITouchType::Direct => PointerKind::Touch,
                UITouchType::Pencil => PointerKind::Pen,
                UITouchType::IndirectPointer => PointerKind::Mouse,
                _ => continue,
            };
            let key = Retained::as_ptr(&touch) as usize;
            let (pointer, buttons) = if kind == PointerKind::Mouse {
                let mask = event.map_or(UIEventButtonMask(0), |e| e.buttonMask());
                let mut buttons = 0;
                if phase != PointerPhase::Up && phase != PointerPhase::Cancel {
                    if mask.contains(UIEventButtonMask::Primary) || mask.0 == 0 {
                        buttons |= PointerButtons::PRIMARY.0;
                    }
                    if mask.contains(UIEventButtonMask::Secondary) {
                        buttons |= PointerButtons::SECONDARY.0;
                    }
                }
                ivars.mouse_buttons.set(buttons);
                (PointerId::MOUSE, PointerButtons(buttons))
            } else {
                let id = ivars.touches.borrow_mut().id(key);
                let down = phase == PointerPhase::Down || phase == PointerPhase::Moved;
                let buttons = if down {
                    PointerButtons::PRIMARY
                } else {
                    PointerButtons::NONE
                };
                (id, buttons)
            };
            let pressed = buttons.0 != 0;
            let sample = |t: &UITouch| {
                let p = t.locationInView(Some(self));
                RawEvent::Pointer(RawPointer {
                    window: ivars.window,
                    pointer,
                    kind,
                    x: p.x,
                    y: p.y,
                    pressure: pressure(t.force(), t.maximumPossibleForce(), pressed),
                    buttons,
                    modifiers: modifiers(flags),
                    phase,
                })
            };
            // Moves carry every sample since the last event, in order.
            let coalesced = (phase == PointerPhase::Moved)
                .then(|| event.and_then(|e| e.coalescedTouchesForTouch(&touch)))
                .flatten();
            match coalesced {
                Some(samples) if !samples.is_empty() => {
                    for t in samples.iter() {
                        push(sample(&t));
                    }
                }
                _ => push(sample(&touch)),
            }
            if matches!(touch.phase(), UITouchPhase::Ended | UITouchPhase::Cancelled) {
                ivars.touches.borrow_mut().release(key);
            }
        }
        drive();
    }

    /// Report each press; the presses to hand on to the text system.
    fn presses(&self, presses: &NSSet<UIPress>, pressed: bool) -> Vec<Retained<UIPress>> {
        let mtm = self.mtm();
        let window = self.ivars().window;
        if self.ivars().ime.borrow().is_marked() {
            return presses.iter().collect();
        }
        let mut forward = Vec::new();
        for press in presses.iter() {
            let Some(key) = press.key(mtm) else {
                forward.push(press);
                continue;
            };
            let code = key_code(key.keyCode().0);
            let mods = modifiers(key.modifierFlags().0);
            push(RawEvent::Key(RawKey {
                window,
                code,
                pressed,
                repeat: false,
                modifiers: mods,
            }));
            if pressed {
                self.stop_repeat(None);
                if let Some(shortcut) = clipboard_shortcut(code, mods) {
                    self.clipboard(shortcut);
                    continue;
                }
                if is_command_key(code) {
                    if repeats(code) {
                        self.start_repeat(code, mods);
                    }
                    continue;
                }
            } else {
                self.stop_repeat(Some(code));
                if is_command_key(code) || clipboard_shortcut(code, mods).is_some() {
                    continue;
                }
            }
            forward.push(press);
        }
        drive();
        forward
    }

    /// Pass `forward` (a subset of `presses`) on to UIView's handler.
    fn forward_presses(
        &self,
        selector: Sel,
        presses: &NSSet<UIPress>,
        forward: Vec<Retained<UIPress>>,
        event: Option<&UIPressesEvent>,
    ) {
        if forward.is_empty() {
            return;
        }
        let set = if forward.len() == presses.len() {
            presses.retain()
        } else {
            NSSet::from_retained_slice(&forward)
        };
        // SAFETY: `selector` is one of UIResponder's `presses…:withEvent:`
        // methods, all taking `(NSSet<UIPress>, UIPressesEvent?)`.
        unsafe {
            let _: () = match selector {
                s if s == sel!(pressesBegan:withEvent:) => {
                    msg_send![super(self), pressesBegan: &*set, withEvent: event]
                }
                s if s == sel!(pressesEnded:withEvent:) => {
                    msg_send![super(self), pressesEnded: &*set, withEvent: event]
                }
                _ => msg_send![super(self), pressesCancelled: &*set, withEvent: event],
            };
        }
    }

    fn start_repeat(&self, code: KeyCode, mods: Modifiers) {
        // SAFETY: the view implements `keyRepeat:` with the
        // `(&self, &NSTimer)` signature a timer calls; the timer is
        // invalidated when the key is released.
        let timer = unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                REPEAT_INTERVAL_SECS,
                self,
                sel!(keyRepeat:),
                None,
                true,
            )
        };
        timer.setFireDate(&NSDate::dateWithTimeIntervalSinceNow(REPEAT_DELAY_SECS));
        *self.ivars().repeat.borrow_mut() = Some((code, mods, timer));
    }

    /// Stop repeating: on `code`'s release, or on any press (`None`), as a
    /// new key supersedes the held one.
    fn stop_repeat(&self, code: Option<KeyCode>) {
        let mut slot = self.ivars().repeat.borrow_mut();
        if (code.is_none() || slot.as_ref().is_some_and(|(held, ..)| Some(*held) == code))
            && let Some((_, _, timer)) = slot.take()
        {
            timer.invalidate();
        }
    }

    fn clipboard(&self, shortcut: ClipboardShortcut) {
        self.menu_picked();
        let window = self.ivars().window;
        match shortcut {
            ClipboardShortcut::Copy | ClipboardShortcut::Cut => push(RawEvent::CopyRequested {
                window,
                cut: shortcut == ClipboardShortcut::Cut,
                reply: ClipboardReply::new(),
            }),
            ClipboardShortcut::Paste => push(RawEvent::PasteRequested {
                window,
                reply: PasteReply::new(),
            }),
        }
        drive();
    }

    /// A key press and release with no modifiers (a soft-keyboard key).
    fn tap_key(&self, code: KeyCode) {
        for pressed in [true, false] {
            push(RawEvent::Key(RawKey {
                window: self.ivars().window,
                code,
                pressed,
                repeat: false,
                modifiers: Modifiers::default(),
            }));
        }
    }

    /// Push the event an edit of the keyboard's text made, if any.
    fn apply(&self, event: Option<RawEvent>) {
        if let Some(event) = event {
            push(event);
        }
    }

    /// Make edit `f` to the keyboard's text.
    fn ime_edit(
        &self,
        f: impl FnOnce(&mut ImeText, WindowId) -> Option<RawEvent>,
    ) -> Option<RawEvent> {
        f(&mut self.ivars().ime.borrow_mut(), self.ivars().window)
    }

    /// Whether the keyboard may correct and check words: the field shows
    /// its text and holds words.
    fn corrects(&self) -> bool {
        let ivars = self.ivars();
        ivars.shows_text.get()
            && !ivars.secure.get()
            && matches!(ivars.kind.get(), InputKind::Text | InputKind::Search)
    }

    /// Show the keyboard the text around the focused field's caret. UIKit
    /// hears of a change the keyboard did not make itself, so it reads the
    /// text again.
    pub(super) fn set_surrounding_text(&self, text: Option<&SurroundingText>) {
        let ivars = self.ivars();
        let (text_changed, selection_changed) = ivars.ime.borrow().change(text);
        let delegate = ivars.input_delegate.borrow().load();
        let me = ProtocolObject::<dyn UITextInput>::from_ref(self);
        if let Some(d) = &delegate {
            if text_changed {
                d.textWillChange(Some(me));
            } else if selection_changed {
                d.selectionWillChange(Some(me));
            }
        }
        ivars.ime.borrow_mut().show(text);
        if let Some(d) = &delegate {
            if text_changed {
                d.textDidChange(Some(me));
            } else if selection_changed {
                d.selectionDidChange(Some(me));
            }
        }
        if ivars.shows_text.replace(text.is_some()) != text.is_some() && self.isFirstResponder() {
            self.reloadInputViews();
        }
    }

    fn moved(&self, position: &UITextPosition, by: isize) -> Option<Retained<UITextPosition>> {
        let at = offset_of(position)? as isize + by;
        let len = self.ivars().ime.borrow().len_units() as isize;
        (0..=len)
            .contains(&at)
            .then(|| VisoTextPosition::at(self.mtm(), at as usize))
    }

    fn caret_rect(&self) -> CGRect {
        match self.ivars().ime_caret.get() {
            Some(r) => CGRect::new(
                CGPoint::new(r.x, r.y),
                CGSize::new(r.width.max(1.0), r.height),
            ),
            None => CGRect::default(),
        }
    }
}

/// The shared menu controller: the edit menu before iOS 16, which has no
/// edit-menu interaction.
#[allow(deprecated)]
fn menu_controller(view: &VisoView) -> Retained<objc2_ui_kit::UIMenuController> {
    objc2_ui_kit::UIMenuController::sharedMenuController(MainThreadMarker::from(view))
}

/// `r` as a rectangle of the view.
fn cg_rect(r: LogicalRect) -> CGRect {
    CGRect {
        origin: CGPoint { x: r.x, y: r.y },
        size: CGSize {
            width: r.width.max(1.0),
            height: r.height.max(1.0),
        },
    }
}

fn physical_size(size: CGSize, scale: f64) -> (u32, u32) {
    (
        (size.width * scale).round().max(1.0) as u32,
        (size.height * scale).round().max(1.0) as u32,
    )
}

fn gesture_state(recognizer: &UIGestureRecognizer) -> UIGestureRecognizerState {
    recognizer.state()
}

fn offset_of(position: &UITextPosition) -> Option<usize> {
    position
        .downcast_ref::<VisoTextPosition>()
        .map(|p| *p.ivars())
}

fn range_of(range: &UITextRange) -> Option<(usize, usize)> {
    range.downcast_ref::<VisoTextRange>().map(|r| *r.ivars())
}

define_class!(
    /// An offset into the text document, UTF-16 units.
    // SAFETY: UITextPosition has no subclassing requirements; no `Drop`.
    #[unsafe(super(UITextPosition, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoTextPosition"]
    #[ivars = usize]
    struct VisoTextPosition;
);

impl VisoTextPosition {
    fn at(mtm: MainThreadMarker, offset: usize) -> Retained<UITextPosition> {
        let this = Self::alloc(mtm).set_ivars(offset);
        // SAFETY: `init` is NSObject's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        Retained::into_super(this)
    }
}

define_class!(
    /// A span of the text document, UTF-16 units, start ≤ end.
    // SAFETY: UITextRange's subclass contract is overriding `start`, `end`
    // and `isEmpty`, done here; no `Drop`.
    #[unsafe(super(UITextRange, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "VisoTextRange"]
    #[ivars = (usize, usize)]
    struct VisoTextRange;

    impl VisoTextRange {
        #[unsafe(method_id(start))]
        fn start(&self) -> Retained<UITextPosition> {
            VisoTextPosition::at(self.mtm(), self.ivars().0)
        }

        #[unsafe(method_id(end))]
        fn end(&self) -> Retained<UITextPosition> {
            VisoTextPosition::at(self.mtm(), self.ivars().1)
        }

        #[unsafe(method(isEmpty))]
        fn is_empty(&self) -> bool {
            self.ivars().0 == self.ivars().1
        }
    }
);

impl VisoTextRange {
    fn span(mtm: MainThreadMarker, start: usize, end: usize) -> Retained<UITextRange> {
        let this = Self::alloc(mtm).set_ivars((start, end));
        // SAFETY: `init` is NSObject's designated initializer.
        let this: Retained<Self> = unsafe { msg_send![super(this), init] };
        Retained::into_super(this)
    }
}
