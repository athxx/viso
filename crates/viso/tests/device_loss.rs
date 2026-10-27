//! A lost GPU device, driven through the facade: the window lets go of what
//! the device held and draws on a new one from the next frame — its text
//! shaped again, its image decoded and uploaded again — to the same pixels it
//! drew before, without a panic.

#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{DrivenApp, drive_scripted, lose_device_after, probe_frames};
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;
use viso::ui::{Asset, AssetFit, FlexStyle, Length};
use viso::widgets::image_asset;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const JPEG: &[u8] = include_bytes!("../../image/src/fixtures/two_tone.jpg");
const FONT: &[u8] = include_bytes!("../fixtures/DejaVuSans-subset.ttf");

/// A line of text over a photo.
struct Shown;

impl Application for Shown {
    fn new(cx: &mut AppCx) -> Self {
        cx.load_font(FONT);
        Shown
    }

    fn window_config(&self) -> viso::ui::WindowConfig {
        viso::ui::WindowConfig {
            caption: false,
            ..Default::default()
        }
    }

    fn build(&mut self, cx: &mut BuildCx<'_>) {
        cx.flex(
            FlexStyle {
                size: Sizing::fill(),
                axis: Axis::Column,
                ..FlexStyle::default()
            },
            |cx| {
                label("Lost and found").build(cx);
                image_asset(Asset::bytes(JPEG))
                    .fit(AssetFit::Fill)
                    .size(Sizing {
                        width: Length::Fixed(64.0),
                        height: Length::Fixed(32.0),
                    })
                    .build(cx);
            },
        );
    }
}

/// The window drawn over `frames` redraws, its device lost after the
/// `lost`th submit when one is given.
fn drive(frames: usize, lost: Option<u32>) -> DrivenApp<Shown> {
    lose_device_after(lost);
    let script = vec![RawEvent::RedrawRequested { window: WIN }; frames];
    let app = drive_scripted::<Shown>(script, STEP);
    lose_device_after(None);
    app
}

#[test]
fn a_lost_device_is_replaced_and_the_window_draws_as_before() {
    let submits = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let count = std::rc::Rc::clone(&submits);
    probe_frames(Some(Box::new(move |_, _| count.set(count.get() + 1))));
    let before = drive(8, None);
    probe_frames(None);
    assert_eq!(before.devices_replaced(), 0);
    assert!(
        before.store().assets().texture_bytes() > 0,
        "the photo is resident"
    );
    let expected = before.pixels().expect("the window draws");
    let mut colors: Vec<&[u8]> = expected.2.chunks(4).collect();
    colors.sort_unstable();
    colors.dedup();
    assert!(colors.len() > 20, "the text draws, antialiased");

    for lost in 1..=submits.get() {
        let after = drive(8, Some(lost));
        assert_eq!(
            after.devices_replaced(),
            1,
            "the device lost after submit {lost} is replaced"
        );
        assert_eq!(
            after.store().assets().texture_bytes(),
            before.store().assets().texture_bytes(),
            "the photo is resident on the new device"
        );
        let drawn = after.pixels().expect("the window draws on its new device");
        assert_eq!((drawn.0, drawn.1), (expected.0, expected.1));
        let differ = drawn
            .2
            .iter()
            .zip(&expected.2)
            .filter(|(a, b)| a != b)
            .count();
        assert_eq!(
            differ, 0,
            "lost after submit {lost}: {differ} bytes differ from the window never lost"
        );
    }
}
