//! The adaptive environment as the platform reports it: each window's metrics,
//! safe area and keyboard, the system appearance, and the input devices its
//! input reveals. A field is written only when its value changes, so a steady
//! stream of input touches nothing.

use viso_platform::{Appearance, PointerKind};
use viso_ui::StateStore;
use viso_ui::adaptive::{InputCapabilities, PointerPrecision, WindowMetrics};
use viso_ui::layout::Inset;

use crate::WindowState;

impl WindowState {
    /// Seeds a new window's environment before its tree is built, so every
    /// region selects its first arm against the real window.
    pub(crate) fn seed_environment(&mut self, appearance: Appearance) {
        self.report_window();
        initial_input(&mut self.states);
        report_appearance(&mut self.states, appearance);
    }

    /// Writes the window's logical size and scale factor, safe area and
    /// keyboard inset into its environment.
    pub(crate) fn report_window(&mut self) {
        let (width, height) = self.logical_surface();
        let window = WindowMetrics {
            width,
            height,
            scale_factor: self.dpi.max(1.0),
        };
        let a = self.safe_area;
        let safe_area = Inset {
            left: a.left as f32,
            top: a.top as f32,
            right: a.right as f32,
            bottom: a.bottom as f32,
        };
        report_metrics(
            &mut self.states,
            window,
            safe_area,
            self.keyboard_inset as f32,
        );
    }
}

fn report_metrics(
    states: &mut StateStore,
    window: WindowMetrics,
    safe_area: Inset,
    keyboard_inset: f32,
) {
    let now = states.env().environment();
    if now.window == window && now.safe_area == safe_area && now.keyboard_inset == keyboard_inset {
        return;
    }
    states.update_env(|env| {
        env.window = window;
        env.safe_area = safe_area;
        env.keyboard_inset = keyboard_inset;
    });
}

/// Writes the parts of the system appearance the environment carries.
pub(crate) fn report_appearance(states: &mut StateStore, appearance: Appearance) {
    if states.env().environment().reduced_motion != appearance.reduce_motion {
        states.update_env(|env| env.reduced_motion = appearance.reduce_motion);
    }
}

/// The input devices a window assumes before its input reveals any: a phone
/// or tablet is touched, everything else pointed at and typed on.
fn initial_input(states: &mut StateStore) {
    if cfg!(any(target_os = "ios", target_os = "android")) {
        update_input(states, |input| {
            input.primary_pointer_precision = PointerPrecision::Coarse;
            input.hover_available = false;
            input.keyboard_available = false;
            input.touch_available = true;
        });
    }
}

/// Notes that a pointer of `kind` was used: it becomes the primary pointer
/// and its device available.
pub(crate) fn note_pointer(states: &mut StateStore, kind: PointerKind) {
    update_input(states, |input| match kind {
        PointerKind::Mouse => {
            input.primary_pointer_precision = PointerPrecision::Fine;
            input.hover_available = true;
        }
        PointerKind::Touch => {
            input.primary_pointer_precision = PointerPrecision::Coarse;
            input.touch_available = true;
        }
        PointerKind::Pen => {
            input.primary_pointer_precision = PointerPrecision::Fine;
            input.pen_available = true;
        }
    });
}

/// Notes that a hardware key was pressed.
pub(crate) fn note_key(states: &mut StateStore) {
    update_input(states, |input| input.keyboard_available = true);
}

fn update_input(states: &mut StateStore, change: impl FnOnce(&mut InputCapabilities)) {
    let mut input = states.env().environment().input;
    change(&mut input);
    if input != states.env().environment().input {
        states.update_env(|env| env.input = input);
    }
}
