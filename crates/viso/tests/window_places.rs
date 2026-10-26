//! Window places through the facade: a window's size, position and
//! fullscreen state are stored as they change and when the session ends,
//! and the next launch opens the window as it was; a window that keeps
//! nothing stores nothing.
#![cfg(feature = "headless")]

use std::time::Duration;

use viso::__test_support::{drive_scripted, keep_window_places};
use viso::platform::{RawEvent, WindowId};
use viso::prelude::*;

const STEP: Duration = Duration::from_millis(16);
const WIN: WindowId = WindowId(1);
const KEY: &str = "window/main";

struct Remembered;

impl Application for Remembered {
    fn new(_cx: &mut AppCx) -> Self {
        Remembered
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            size: (420.0, 260.0),
            ..Default::default()
        }
    }
}

struct Forgetful;

impl Application for Forgetful {
    fn new(_cx: &mut AppCx) -> Self {
        Forgetful
    }

    fn window_config(&self) -> WindowConfig {
        WindowConfig {
            persist: None,
            ..Default::default()
        }
    }
}

fn stored(memory: &viso::__test_support::WindowPlaces) -> String {
    String::from_utf8(memory.get(KEY).expect("a place was stored")).unwrap()
}

#[test]
fn a_moved_and_resized_window_reopens_where_it_was() {
    let memory = keep_window_places();
    let first = drive_scripted::<Remembered>(
        vec![
            RawEvent::Moved {
                window: WIN,
                x: 300.0,
                y: 200.0,
            },
            RawEvent::Resized {
                window: WIN,
                width: 500,
                height: 400,
            },
        ],
        STEP,
    );
    drop(first);
    assert_eq!(stored(&memory), "1 500 400 300 200 normal");

    // The next launch opens at that size and place: a resize alone keeps
    // the position the window reopened at.
    let second = drive_scripted::<Remembered>(
        vec![RawEvent::Resized {
            window: WIN,
            width: 640,
            height: 480,
        }],
        STEP,
    );
    assert_eq!(second.surface_size_at(0), (640, 480));
    drop(second);
    assert_eq!(stored(&memory), "1 640 480 300 200 normal");
    let third = drive_scripted::<Remembered>(vec![], STEP);
    assert_eq!(
        third.surface_size_at(0),
        (640, 480),
        "opened at the kept size"
    );
}

#[test]
fn a_fullscreen_window_reopens_fullscreen_and_leaving_it_is_kept() {
    let memory = keep_window_places();
    drop(drive_scripted::<Remembered>(
        vec![RawEvent::FullscreenChanged {
            window: WIN,
            fullscreen: true,
        }],
        STEP,
    ));
    assert!(
        stored(&memory).ends_with(" fullscreen"),
        "{}",
        stored(&memory)
    );
    let again = drive_scripted::<Remembered>(vec![], STEP);
    assert!(again.store().window_fullscreen(), "reopened fullscreen");
    drop(again);
    drop(drive_scripted::<Remembered>(
        vec![RawEvent::FullscreenChanged {
            window: WIN,
            fullscreen: false,
        }],
        STEP,
    ));
    assert!(stored(&memory).ends_with(" normal"), "{}", stored(&memory));
}

#[test]
fn a_window_that_keeps_nothing_stores_nothing() {
    let memory = keep_window_places();
    drop(drive_scripted::<Forgetful>(
        vec![RawEvent::Moved {
            window: WIN,
            x: 10.0,
            y: 10.0,
        }],
        STEP,
    ));
    assert!(memory.keys().is_empty());
}
