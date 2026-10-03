//! The adaptive environment as the platform reports it: each window's metrics,
//! safe area, keyboard and display features, the system appearance, text
//! scale and locale, and the input devices its input reveals. A field is written only when its value changes, so a steady
//! stream of input touches nothing.

use viso_platform::{Appearance, PointerKind};
use viso_render::Rect;
use viso_ui::StateStore;
use viso_ui::adaptive::{
    DisplayFeature, DisplayFeatureKind, InputCapabilities, LayoutDirection, PointerPrecision,
    WindowMetrics,
};
use viso_ui::layout::Inset;

use crate::WindowState;

impl WindowState {
    /// Seeds a new window's environment before its tree is built, so every
    /// region selects its first arm against the real window.
    pub(crate) fn seed_environment(
        &mut self,
        appearance: Appearance,
        locale: &str,
        features: &[viso_platform::DisplayFeature],
    ) {
        self.report_window();
        initial_input(&mut self.states);
        self.report_appearance(appearance);
        report_locale(&mut self.states, locale);
        report_display_features(&mut self.states, features);
    }

    /// Writes the parts of the system appearance the environment carries; the
    /// text scale also resizes the window's `sp` lengths.
    pub(crate) fn report_appearance(&mut self, appearance: Appearance) {
        let text_scale = if appearance.text_scale.is_finite() && appearance.text_scale > 0.0 {
            appearance.text_scale
        } else {
            1.0
        };
        let now = self.states.env().environment();
        if now.reduced_motion != appearance.reduce_motion || now.text_scale != text_scale {
            self.states.update_env(|env| {
                env.reduced_motion = appearance.reduce_motion;
                env.text_scale = text_scale;
            });
        }
        let lengths = self.store.length_env();
        if lengths.text_scale != text_scale {
            self.store.set_length_env(viso_ui::LengthEnv {
                text_scale,
                ..lengths
            });
        }
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

/// Writes the user's locale and the layout direction its script reads in.
pub(crate) fn report_locale(states: &mut StateStore, locale: &str) {
    let direction = LayoutDirection::of_locale(locale);
    let now = states.env().environment();
    if now.locale != locale || now.layout_direction != direction {
        states.update_env(|env| {
            locale.clone_into(&mut env.locale);
            env.layout_direction = direction;
        });
    }
}

/// Writes the hinges, folds and cutouts over a window.
pub(crate) fn report_display_features(
    states: &mut StateStore,
    features: &[viso_platform::DisplayFeature],
) {
    let features: Vec<_> = features
        .iter()
        .map(|feature| DisplayFeature {
            kind: match feature.kind {
                viso_platform::DisplayFeatureKind::Hinge => DisplayFeatureKind::Hinge,
                viso_platform::DisplayFeatureKind::Fold => DisplayFeatureKind::Fold,
                viso_platform::DisplayFeatureKind::Cutout => DisplayFeatureKind::Cutout,
            },
            bounds: Rect {
                x: feature.bounds.x as f32,
                y: feature.bounds.y as f32,
                w: feature.bounds.width as f32,
                h: feature.bounds.height as f32,
            },
        })
        .collect();
    if states.env().environment().display_features != features {
        states.update_env(|env| env.display_features = features);
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
