//! Conversions between the runtime, platform and UI tiers' types.

use viso_ui::Modifiers;

use crate::services;

/// The haptic service's pattern for a control's `haptic`.
pub(crate) fn play(haptic: viso_ui::Haptic) -> services::Haptic {
    use viso_ui::Haptic as H;
    match haptic {
        H::Selection => services::Haptic::Selection,
        H::Light => services::Haptic::Light,
        H::Medium => services::Haptic::Medium,
        H::Heavy => services::Haptic::Heavy,
        H::Success => services::Haptic::Success,
        H::Warning => services::Haptic::Warning,
        H::Error => services::Haptic::Error,
    }
}

/// What a drag carries out of the app: its text, link and files. A drop
/// elsewhere may copy or move its text, and copy or link to its files, which
/// stay where they are.
pub(crate) fn drag_out(data: &viso_ui::DragData) -> viso_platform::DragOut {
    let files = data.files().map(<[_]>::to_vec).unwrap_or_default();
    viso_platform::DragOut {
        text: data.as_text().map(str::to_owned),
        uri: data.uri().map(str::to_owned),
        actions: viso_platform::DropActions {
            moves: files.is_empty(),
            copies: true,
            links: !files.is_empty(),
        },
        files,
    }
}

/// What a drag from outside offers, as data a target may take: its files,
/// its link and its text, each empty until the OS gives its contents.
pub(crate) fn outside_data(offer: &viso_runtime::DropOffer) -> viso_ui::DragData {
    use viso_ui::{DragData, DragItem};
    let mut data = DragData::new();
    if offer.kinds.files {
        data = data.with(DragItem::Files(offer.files.clone().into()));
    }
    if offer.kinds.uri {
        data = data.with(DragItem::Uri(
            offer.uri.as_deref().unwrap_or_default().into(),
        ));
    }
    if offer.kinds.text {
        data = data.with(DragItem::Text(
            offer.text.as_deref().unwrap_or_default().into(),
        ));
    }
    data
}

/// Lower runtime-tier modifier state onto the UI-tier mirror (same fields, a
/// crate-boundary copy).
pub(crate) fn lower_modifiers(m: viso_runtime::Modifiers) -> Modifiers {
    Modifiers {
        shift: m.shift,
        control: m.control,
        alt: m.alt,
        logo: m.logo,
    }
}
