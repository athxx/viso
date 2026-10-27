//! The kinds of node a `.vs` view builds, one marker per standard widget:
//! the type parameter of the [`Handle`](crate::Handle) a `view!` or
//! `component!` hands out for a named node (`Handle<kind::Button>`), so a
//! handle of one kind is not taken for another.

macro_rules! kinds {
    ($($name:ident),+ $(,)?) => {
        $(
            #[doc = concat!("A `", stringify!($name), "` node.")]
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
            pub enum $name {}
        )+

        /// The widget names the markers are for.
        #[cfg(test)]
        const NAMES: &[&str] = &[$(stringify!($name)),+];
    };
}

kinds! {
    Row, Column, Flex, AdaptiveScope, SafeArea, KeyboardAvoiding, BlurView, Grid, Stack,
    Absolute, InteractiveViewer, SwipeRow, SwipeActions, DragPreview, ReorderHandle, Scroll,
    Fragment, SlotOutlet,
    Text, LinkLabel, TextInput, NumberField, Button, Toggle, CheckBox, Chip, Stepper, Slider,
    Knob, ComboBox, SearchField, ColorPicker, RouterView, Image, Svg, Video, CameraView,
    ProgressBar, ProgressRing, Spinner, Badge, Avatar, ShaderView, GameViewport, Tabs,
    NavigationStack, Disclosure, Modal, Sheet, Toast, RadioGroup, SegmentedControl, FocusScope,
    KeyShortcut, VirtualList,
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_standard_widget_has_a_kind() {
        let widgets: Vec<&str> = viso_schema::widgets::STANDARD
            .iter()
            .map(|w| w.name)
            .filter(|&name| name != viso_schema::widgets::COMPONENT.name)
            .collect();
        assert_eq!(widgets, super::NAMES);
    }
}
