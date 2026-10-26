//! Windows that reopen where they were: a window opened with a
//! [`WindowConfig::persist`](viso_ui::WindowConfig::persist) name has its
//! size, position and maximized or fullscreen state stored as they change,
//! and the next window opened under that name — in this launch or a later
//! one — starts from them. The platform keeps the frame on the screens there
//! are then.
//!
//! A window's frame settles before it is kept: the frames a maximize or a
//! fullscreen animation passes through, and the stream a drag sends, are not
//! stored one by one, only the frame the window rests at
//! ([`SETTLE`] after the last change). Closing the window, the app going to
//! the background and the loop ending store what is waiting at once.

use std::cell::RefCell;
use std::time::Duration;

use viso_platform::{WindowId, WindowMode};
use viso_runtime::{Instant, RuntimeCx};
use viso_view::persist::{PersistStore, SharedStore};

/// How long a window's frame must rest before it is kept.
const SETTLE: Duration = Duration::from_millis(400);

thread_local! {
    static STORE: RefCell<Option<SharedStore>> = const { RefCell::new(None) };
}

/// Installs the store windows' places are kept in on this thread; `None`
/// keeps nothing.
pub(crate) fn install(store: Option<SharedStore>) {
    STORE.with(|s| *s.borrow_mut() = store);
}

fn store() -> Option<SharedStore> {
    STORE.with(|s| s.borrow().clone())
}

/// Where a window was and how it showed.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Place {
    /// The content size in logical points.
    size: (f64, f64),
    /// The outer top-left, as the platform reports it; `None` where it does
    /// not.
    position: Option<(f64, f64)>,
    mode: Shown,
}

/// The states a window reopens in: minimized is not one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shown {
    Normal,
    Maximized,
    Fullscreen,
}

impl Place {
    /// The place as stored: `1 width height x y mode`, `-` for no position.
    fn encode(&self) -> Vec<u8> {
        let (w, h) = self.size;
        let (x, y) = match self.position {
            Some((x, y)) => (x.to_string(), y.to_string()),
            None => ("-".to_owned(), "-".to_owned()),
        };
        let mode = match self.mode {
            Shown::Normal => "normal",
            Shown::Maximized => "maximized",
            Shown::Fullscreen => "fullscreen",
        };
        format!("1 {w} {h} {x} {y} {mode}").into_bytes()
    }

    fn decode(blob: &[u8]) -> Option<Place> {
        let text = std::str::from_utf8(blob).ok()?;
        let mut words = text.split_ascii_whitespace();
        if words.next()? != "1" {
            return None;
        }
        let mut number = || {
            words
                .next()
                .and_then(|w| w.parse::<f64>().ok())
                .filter(|n| n.is_finite())
        };
        let size = (number()?, number()?);
        if size.0 < 1.0 || size.1 < 1.0 {
            return None;
        }
        let position = match (number(), number()) {
            (Some(x), Some(y)) => Some((x, y)),
            _ => None,
        };
        let mode = match words.next()? {
            "normal" => Shown::Normal,
            "maximized" => Shown::Maximized,
            "fullscreen" => Shown::Fullscreen,
            _ => return None,
        };
        Some(Place {
            size,
            position,
            mode,
        })
    }
}

/// A window whose place is kept.
struct Kept {
    window: WindowId,
    key: String,
    /// What is stored (or was opened from).
    place: Place,
    /// A frame the window reached, at its own size, and when: kept once it
    /// rests.
    resting: Option<(Place, Instant)>,
    /// Entered or left fullscreen, as reported (ahead of the animation).
    fullscreen: bool,
    /// `place` changed since it was stored.
    dirty: bool,
}

/// The windows whose places are kept.
#[derive(Default)]
pub(crate) struct Places {
    kept: Vec<Kept>,
}

/// What opening a window under a name took from its stored place.
pub(crate) struct Opening {
    key: String,
    stored: Option<Place>,
}

impl Opening {
    /// Whether the window reopens fullscreen, which is asked for once it
    /// exists.
    pub(crate) fn fullscreen(&self) -> bool {
        self.stored.is_some_and(|p| p.mode == Shown::Fullscreen)
    }
}

impl Places {
    /// `config` for a window kept under `name`, from its stored place;
    /// `None` when nothing is kept: no name, no store, a platform whose
    /// windows are not framed, or another open window holding the name.
    pub(crate) fn open(
        &self,
        name: Option<&str>,
        framed: bool,
        config: &mut viso_platform::WindowConfig,
    ) -> Option<Opening> {
        let name = name.filter(|n| !n.is_empty() && framed)?;
        let key = format!("window/{name}");
        if self.kept.iter().any(|k| k.key == key) {
            return None;
        }
        let mut store = store()?;
        let stored = store
            .load(&key)
            .ok()
            .flatten()
            .and_then(|blob| Place::decode(&blob));
        if let Some(place) = stored {
            config.logical_size = place.size;
            config.position = place.position;
            config.maximized = place.mode == Shown::Maximized;
        }
        Some(Opening { key, stored })
    }

    /// Starts keeping the place of `window`, just opened as `opening` said.
    pub(crate) fn opened(&mut self, cx: &mut RuntimeCx<'_>, window: WindowId, opening: Opening) {
        let fullscreen = opening.fullscreen();
        let place = opening
            .stored
            .or_else(|| Some(read(cx, window)?.0))
            .unwrap_or(Place {
                size: (0.0, 0.0),
                position: None,
                mode: Shown::Normal,
            });
        self.kept.push(Kept {
            window,
            key: opening.key,
            place,
            resting: None,
            fullscreen: false,
            dirty: false,
        });
        if fullscreen {
            cx.set_fullscreen(window, true);
        }
    }

    /// `window` moved, resized or changed mode.
    pub(crate) fn placed(&mut self, cx: &mut RuntimeCx<'_>, window: WindowId) {
        let now = cx.frame_now();
        let Some(kept) = self.kept.iter_mut().find(|k| k.window == window) else {
            return;
        };
        let Some((now_at, mode)) = read(cx, window) else {
            return;
        };
        match mode {
            WindowMode::Minimized => {}
            WindowMode::Normal if !kept.fullscreen => kept.resting = Some((now_at, now)),
            WindowMode::Normal | WindowMode::Maximized | WindowMode::Fullscreen => {
                let shown = if kept.fullscreen || mode == WindowMode::Fullscreen {
                    Shown::Fullscreen
                } else {
                    Shown::Maximized
                };
                // The frames just passed through led into this state; the
                // frame to come back to is the one kept before them.
                if kept.resting.is_some_and(|(_, at)| now < at + SETTLE) {
                    kept.resting = None;
                }
                kept.settle(now, true);
                if kept.place.mode != shown {
                    kept.place.mode = shown;
                    kept.dirty = true;
                }
            }
        }
    }

    /// `window` entered or left fullscreen, as its first event says.
    pub(crate) fn fullscreen_changed(&mut self, window: WindowId, fullscreen: bool) {
        if let Some(kept) = self.kept.iter_mut().find(|k| k.window == window) {
            kept.fullscreen = fullscreen;
            if !fullscreen && kept.place.mode == Shown::Fullscreen {
                kept.place.mode = Shown::Normal;
                kept.dirty = true;
            }
        }
    }

    /// Stores the places that settled by `now`.
    pub(crate) fn store_due(&mut self, now: Instant) {
        let Some(mut store) = store() else { return };
        for kept in &mut self.kept {
            kept.settle(now, false);
            kept.store(&mut store);
        }
    }

    /// Stores `window`'s place at once and stops keeping it.
    pub(crate) fn closed(&mut self, window: WindowId) {
        let Some(i) = self.kept.iter().position(|k| k.window == window) else {
            return;
        };
        let mut kept = self.kept.remove(i);
        if let Some(mut store) = store() {
            kept.settle(Instant::now(), true);
            kept.store(&mut store);
            let _ = store.flush();
        }
    }

    /// Stores every place waiting, settled or not, and makes them durable.
    pub(crate) fn flush(&mut self) {
        let Some(mut store) = store() else { return };
        let mut wrote = false;
        for kept in &mut self.kept {
            kept.settle(Instant::now(), true);
            wrote |= kept.dirty;
            kept.store(&mut store);
        }
        if wrote {
            let _ = store.flush();
        }
    }

    /// When the next place settles.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.kept
            .iter()
            .filter_map(|k| k.resting.map(|(_, at)| at + SETTLE))
            .min()
    }
}

impl Kept {
    /// Takes the resting frame as the place when it rested `SETTLE` by
    /// `now`, or at once with `force`.
    fn settle(&mut self, now: Instant, force: bool) {
        if let Some((place, at)) = self.resting
            && (force || now >= at + SETTLE)
        {
            self.resting = None;
            if place != self.place {
                self.place = place;
                self.dirty = true;
            }
        }
    }

    fn store(&mut self, store: &mut SharedStore) {
        if self.dirty {
            store.store(&self.key, self.place.encode());
            self.dirty = false;
        }
    }
}

/// `window`'s frame now, at its own size, and its mode.
fn read(cx: &RuntimeCx<'_>, window: WindowId) -> Option<(Place, WindowMode)> {
    let scale = cx.scale_factor(window).filter(|s| *s > 0.0)?;
    let (w, h) = cx.inner_size(window)?;
    let mode = cx.window_mode(window)?;
    let place = Place {
        size: (f64::from(w) / scale, f64::from(h) / scale),
        position: cx.window_position(window),
        mode: Shown::Normal,
    };
    Some((place, mode))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_place_reads_back_as_stored() {
        let place = Place {
            size: (800.0, 600.5),
            position: Some((-1200.0, 40.0)),
            mode: Shown::Maximized,
        };
        assert_eq!(Place::decode(&place.encode()), Some(place));
        let nowhere = Place {
            position: None,
            mode: Shown::Fullscreen,
            ..place
        };
        assert_eq!(Place::decode(&nowhere.encode()), Some(nowhere));
        assert_eq!(
            Place::decode(b"2 800 600 0 0 normal"),
            None,
            "a later format"
        );
        assert_eq!(Place::decode(b"1 0 600 0 0 normal"), None, "no size");
        assert_eq!(Place::decode(b"1 800 600 0 0 sideways"), None);
        assert_eq!(Place::decode(b"\xff"), None);
    }
}
