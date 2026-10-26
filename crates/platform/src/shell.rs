//! The app's presence in the desktop shell beyond its windows: an icon in
//! the system tray or menu bar with a menu, and a badge on its icon in the
//! dock or taskbar.
//!
//! A cold path: the app hands a whole value down when it changes, and
//! the backend compares it with what it shows to skip work it need not
//! redo (an unchanged image is not uploaded again).

use std::sync::Arc;

use crate::menu::Menu;

/// An image for the shell: straight (not premultiplied) RGBA8 pixels, row
/// by row from the top. Cheap to clone; the pixels are shared.
#[derive(Clone, PartialEq, Eq)]
pub struct IconImage {
    width: u32,
    height: u32,
    rgba: Arc<[u8]>,
}

impl IconImage {
    /// An icon of `width × height` pixels; `None` unless `rgba` holds
    /// exactly four bytes a pixel and the image has some.
    pub fn from_rgba(width: u32, height: u32, rgba: impl Into<Arc<[u8]>>) -> Option<IconImage> {
        let rgba = rgba.into();
        let len = (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)?;
        (len > 0 && rgba.len() == len).then_some(IconImage {
            width,
            height,
            rgba,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// The pixels, RGBA8, row by row from the top.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// The icon resampled to `width × height`: each pixel the area average
    /// of what it covers, weighted by alpha so transparent pixels lend no
    /// color. Itself when the size already matches.
    pub fn resized(&self, width: u32, height: u32) -> IconImage {
        if (width, height) == (self.width, self.height) || width == 0 || height == 0 {
            return self.clone();
        }
        let (sw, sh) = (self.width as usize, self.height as usize);
        let (dw, dh) = (width as usize, height as usize);
        let mut out = Vec::with_capacity(dw * dh * 4);
        // The source span each destination pixel covers, in source pixels.
        let span = |d: usize, n_dst: usize, n_src: usize| {
            let scale = n_src as f64 / n_dst as f64;
            (d as f64 * scale, (d + 1) as f64 * scale)
        };
        for dy in 0..dh {
            let (y0, y1) = span(dy, dh, sh);
            for dx in 0..dw {
                let (x0, x1) = span(dx, dw, sw);
                let mut acc = [0f64; 4];
                let mut area = 0f64;
                let mut sy = y0.floor() as usize;
                while (sy as f64) < y1 && sy < sh {
                    let wy = (y1.min(sy as f64 + 1.0) - y0.max(sy as f64)).max(0.0);
                    let mut sx = x0.floor() as usize;
                    while (sx as f64) < x1 && sx < sw {
                        let wx = (x1.min(sx as f64 + 1.0) - x0.max(sx as f64)).max(0.0);
                        let w = wx * wy;
                        let p = &self.rgba[(sy * sw + sx) * 4..][..4];
                        let a = f64::from(p[3]) * w;
                        acc[0] += f64::from(p[0]) * a;
                        acc[1] += f64::from(p[1]) * a;
                        acc[2] += f64::from(p[2]) * a;
                        acc[3] += a;
                        area += w;
                        sx += 1;
                    }
                    sy += 1;
                }
                let alpha = if area > 0.0 { acc[3] / area } else { 0.0 };
                let color = |c: f64| if acc[3] > 0.0 { c / acc[3] } else { 0.0 };
                out.extend_from_slice(&[
                    color(acc[0]).round().clamp(0.0, 255.0) as u8,
                    color(acc[1]).round().clamp(0.0, 255.0) as u8,
                    color(acc[2]).round().clamp(0.0, 255.0) as u8,
                    alpha.round().clamp(0.0, 255.0) as u8,
                ]);
            }
        }
        IconImage {
            width,
            height,
            rgba: out.into(),
        }
    }

    /// Whether `other` shows the same pixels, without comparing them when
    /// both share one buffer.
    pub fn same(&self, other: &IconImage) -> bool {
        (Arc::ptr_eq(&self.rgba, &other.rgba) && self.width == other.width) || self == other
    }
}

impl core::fmt::Debug for IconImage {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "IconImage({}×{})", self.width, self.height)
    }
}

/// The app's icon in the system tray (Windows, Linux) or the menu bar
/// (macOS), with its tooltip and menu.
///
/// Picking an item of [`menu`](Self::menu) delivers
/// [`RawEvent::MenuCommand`](crate::RawEvent::MenuCommand), the same as the
/// application menu; a click on the icon itself delivers
/// [`RawEvent::Tray`](crate::RawEvent::Tray).
#[derive(Debug, Clone, PartialEq)]
pub struct Tray {
    /// The icon, drawn as it is. Scaled to the shell's icon size (16 to 22
    /// points): an image twice that size stays sharp on a high-density
    /// display.
    pub icon: IconImage,
    /// A monochrome variant whose alpha alone counts: the system tints it to
    /// match the menu bar, light or dark (macOS). Where there is no such
    /// tinting, `icon` shows.
    pub template: Option<IconImage>,
    /// The text shown while the pointer rests on the icon.
    pub tooltip: String,
    /// The menu: items, submenus and separators. Empty for none.
    pub menu: Vec<Menu>,
    /// Whether a primary click opens the menu, as most menu-bar items do on
    /// macOS, rather than delivering [`TrayEvent::Click`]. A secondary click
    /// opens it either way.
    pub menu_on_click: bool,
}

impl Tray {
    /// A tray icon showing `icon`, with no tooltip and no menu.
    pub fn new(icon: IconImage) -> Tray {
        Tray {
            icon,
            template: None,
            tooltip: String::new(),
            menu: Vec::new(),
            menu_on_click: false,
        }
    }
}

/// What the user did to the tray icon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayEvent {
    /// A primary click (or the keyboard's select) on the icon. Not sent when
    /// [`Tray::menu_on_click`] opens the menu instead.
    Click,
    /// A primary double click, after the click of its first press. Linux's
    /// tray hosts report single clicks alone.
    DoubleClick,
}

/// What the app's icon in the dock or taskbar shows over its corner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppBadge {
    /// A count, such as unread messages; zero shows nothing.
    Count(u32),
    /// A short label, a few characters. Where the shell shows counts alone
    /// (Linux docks, iOS), a label holding a number shows as that count and
    /// any other shows nothing; the web shows it as a plain mark.
    Label(String),
}

impl AppBadge {
    /// The count this badge shows where only counts can: its count, or the
    /// number its label holds.
    pub fn count(&self) -> Option<u32> {
        match self {
            AppBadge::Count(0) => None,
            AppBadge::Count(n) => Some(*n),
            AppBadge::Label(text) => text.trim().parse().ok().filter(|&n| n > 0),
        }
    }

    /// The text this badge shows: its count (`99+` past ninety-nine where
    /// `short`), or its label; `None` for nothing.
    pub fn text(&self, short: bool) -> Option<String> {
        match self {
            AppBadge::Count(0) => None,
            AppBadge::Count(n) if short && *n > 99 => Some("99+".to_owned()),
            AppBadge::Count(n) => Some(n.to_string()),
            AppBadge::Label(text) if text.trim().is_empty() => None,
            AppBadge::Label(text) => Some(text.trim().to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_badge_shows_its_count_or_label() {
        assert_eq!(AppBadge::Count(0).text(true), None);
        assert_eq!(AppBadge::Count(7).text(true).as_deref(), Some("7"));
        assert_eq!(AppBadge::Count(120).text(true).as_deref(), Some("99+"));
        assert_eq!(AppBadge::Count(120).text(false).as_deref(), Some("120"));
        assert_eq!(
            AppBadge::Label(" new ".into()).text(true).as_deref(),
            Some("new")
        );
        assert_eq!(AppBadge::Label(" ".into()).text(true), None);
        assert_eq!(AppBadge::Count(3).count(), Some(3));
        assert_eq!(AppBadge::Count(0).count(), None);
        assert_eq!(AppBadge::Label("12".into()).count(), Some(12));
        assert_eq!(AppBadge::Label("new".into()).count(), None);
    }

    #[test]
    fn an_icon_takes_four_bytes_a_pixel() {
        assert!(IconImage::from_rgba(2, 2, vec![0u8; 16]).is_some());
        assert!(IconImage::from_rgba(2, 2, vec![0u8; 15]).is_none());
        assert!(IconImage::from_rgba(0, 0, Vec::<u8>::new()).is_none());
        assert!(IconImage::from_rgba(u32::MAX, u32::MAX, vec![0u8; 4]).is_none());
    }

    #[test]
    fn resizing_averages_what_each_pixel_covers() {
        // A 2×2 of one opaque red and three transparent pixels halves to one
        // pixel: red, a quarter opaque.
        let mut rgba = vec![0u8; 16];
        rgba[..4].copy_from_slice(&[255, 0, 0, 255]);
        rgba[4..8].copy_from_slice(&[0, 255, 0, 0]);
        let icon = IconImage::from_rgba(2, 2, rgba).unwrap();
        let one = icon.resized(1, 1);
        assert_eq!(one.rgba(), &[255, 0, 0, 64]);
        // Growing repeats pixels; the same size is the same icon.
        let big = icon.resized(4, 4);
        assert_eq!(&big.rgba()[..4], &[255, 0, 0, 255]);
        assert_eq!(big.rgba()[4 * 4 * 2 + 3], 0);
        assert!(icon.resized(2, 2).same(&icon));
    }

    #[test]
    fn icons_compare_by_pixels() {
        let a = IconImage::from_rgba(1, 1, vec![1u8, 2, 3, 4]).unwrap();
        let b = IconImage::from_rgba(1, 1, vec![1u8, 2, 3, 4]).unwrap();
        let c = IconImage::from_rgba(1, 1, vec![9u8, 2, 3, 4]).unwrap();
        assert!(a.same(&a.clone()));
        assert!(a.same(&b));
        assert!(!a.same(&c));
    }
}
