//! Each raster format decodes to premultiplied RGBA8: a 16 × 16 image red
//! above blue — opaque on the left, clear on the right where it has alpha —
//! and an animation of three frames: red, red with a green square at 4, 4,
//! then blue, composited and timed as its file says. A JPEG turns upright as
//! its EXIF orientation says.

use std::num::NonZeroU32;
use std::time::Duration;

use viso_image::{Animation, Loops, RasterImage, decode_raster, is_animated};
use viso_schema::resource::AssetBytes;

macro_rules! fixture {
    ($name:literal) => {
        include_bytes!(concat!("fixtures/", $name)).as_slice()
    };
}

/// The texel at `x, y` of a 16-wide image.
fn at(pixels: &[u8], x: usize, y: usize) -> [u8; 4] {
    let i = (y * 16 + x) * 4;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

/// Whether `got` is within `tolerance` of `want` on each channel.
fn near(got: [u8; 4], want: [u8; 4], tolerance: u8) -> bool {
    got.iter()
        .zip(want)
        .all(|(&g, w)| g.abs_diff(w) <= tolerance)
}

/// Asserts `image` is the 16 × 16 red-over-blue image, to `tolerance`.
fn red_over_blue(image: &RasterImage, tolerance: u8) {
    assert_eq!((image.width, image.height), (16, 16));
    let texels = &image.texels;
    for (x, y, want) in [
        (2, 2, [255, 0, 0, 255]),
        (13, 5, [255, 0, 0, 255]),
        (2, 12, [0, 0, 255, 255]),
        (13, 14, [0, 0, 255, 255]),
    ] {
        let got = at(texels, x, y);
        assert!(near(got, want, tolerance), "{x}, {y}: {got:?}");
    }
}

#[test]
fn bmp_qoi_and_webp_decode() {
    red_over_blue(&decode_raster(fixture!("two.bmp")).unwrap(), 0);
    red_over_blue(&decode_raster(fixture!("two_lossy.webp")).unwrap(), 24);
    // Clear on the right: premultiplied to nothing.
    for name in ["two.qoi", "two_alpha.webp"] {
        let bytes = match name {
            "two.qoi" => fixture!("two.qoi"),
            _ => fixture!("two_alpha.webp"),
        };
        let image = decode_raster(bytes).unwrap();
        assert_eq!(at(&image.texels, 2, 2), [255, 0, 0, 255], "{name}");
        assert_eq!(at(&image.texels, 12, 12), [0, 0, 0, 0], "{name}");
    }
}

#[test]
fn an_icon_shows_its_image_with_its_alpha() {
    let image = decode_raster(fixture!("two_ico.ico")).unwrap();
    assert_eq!((image.width, image.height), (16, 16));
    assert_eq!(at(&image.texels, 2, 2), [255, 0, 0, 255]);
    assert_eq!(at(&image.texels, 2, 12), [0, 0, 255, 255]);
    assert_eq!(at(&image.texels, 12, 2)[3], 0);
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn avif_decodes_at_eight_and_ten_bits() {
    red_over_blue(&decode_raster(fixture!("two.avif")).unwrap(), 40);
    red_over_blue(&decode_raster(fixture!("two10.avif")).unwrap(), 40);
}

/// The canvas of each frame `name` plays, with its delay.
fn frames(bytes: &'static [u8]) -> (Animation, Vec<(Vec<u8>, Duration)>) {
    let mut animation = Animation::open(&AssetBytes::Static(bytes))
        .unwrap()
        .expect("animated");
    let mut out = Vec::new();
    while let Some(frame) = animation.next_frame().unwrap() {
        out.push((frame.pixels.to_vec(), frame.delay));
    }
    (animation, out)
}

/// Asserts `frames` are red, red with the green square, then blue.
fn three(frames: &[(Vec<u8>, Duration)], tolerance: u8) {
    assert_eq!(frames.len(), 3);
    let red = [255, 0, 0, 255];
    let (first, second, third) = (&frames[0].0, &frames[1].0, &frames[2].0);
    assert!(
        near(at(first, 5, 5), red, tolerance),
        "{:?}",
        at(first, 5, 5)
    );
    assert!(
        near(at(second, 5, 5), [0, 255, 0, 255], tolerance),
        "{:?}",
        at(second, 5, 5)
    );
    assert!(near(at(second, 12, 12), red, tolerance), "kept under it");
    assert!(
        near(at(third, 5, 5), [0, 0, 255, 255], tolerance),
        "{:?}",
        at(third, 5, 5)
    );
}

#[test]
fn a_gif_composites_its_frames_and_plays_forever() {
    let bytes = fixture!("anim.gif");
    assert!(is_animated(&AssetBytes::Static(bytes)));
    let (mut animation, played) = frames(bytes);
    // The palette rounds the colours a little.
    three(&played, 4);
    assert_eq!(animation.loops(), Loops::Forever);
    assert_eq!(animation.size(), (16, 16));
    assert!(played.iter().all(|(_, d)| *d == Duration::from_millis(100)));
    // A rewind plays it again from the first frame.
    animation.rewind().unwrap();
    let again = animation.next_frame().unwrap().unwrap();
    assert_eq!(again.pixels, played[0].0.as_slice());
    // As a still, its first frame.
    let still = decode_raster(bytes).unwrap();
    assert_eq!(*still.texels, *played[0].0);
}

#[test]
fn an_apng_plays_its_frames_twice() {
    let bytes = fixture!("anim.png");
    let (animation, played) = frames(bytes);
    three(&played, 0);
    assert_eq!(animation.loops(), Loops::Times(NonZeroU32::new(2).unwrap()));
    assert_eq!(animation.frame_count(), Some(3));
    assert_eq!(played[0].1, Duration::from_millis(100));
}

#[test]
fn an_animated_webp_plays_three_times() {
    let (animation, played) = frames(fixture!("anim.webp"));
    three(&played, 2);
    assert_eq!(animation.loops(), Loops::Times(NonZeroU32::new(3).unwrap()));
    assert_eq!(played[1].1, Duration::from_millis(100));
}

#[cfg(not(target_arch = "wasm32"))]
#[test]
fn an_avif_sequence_plays_its_samples() {
    let (_, played) = frames(fixture!("anim.avif"));
    three(&played, 48);
    assert_eq!(played[0].1, Duration::from_millis(100));
}

#[test]
fn a_still_image_is_not_an_animation() {
    for bytes in [fixture!("two.bmp"), fixture!("two_lossy.webp")] {
        assert!(!is_animated(&AssetBytes::Static(bytes)));
        assert!(
            Animation::open(&AssetBytes::Static(bytes))
                .unwrap()
                .is_none()
        );
    }
}

/// `jpeg` with an EXIF block recording `orientation` after its application
/// segments, where it is the one a reader keeps.
fn with_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
    let mut exif = b"Exif\0\0II*\0\x08\0\0\0\x01\0".to_vec();
    exif.extend_from_slice(&0x0112u16.to_le_bytes());
    exif.extend_from_slice(&3u16.to_le_bytes());
    exif.extend_from_slice(&1u32.to_le_bytes());
    exif.extend_from_slice(&orientation.to_le_bytes());
    exif.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    let mut at = 2;
    while jpeg[at] == 0xff && (0xe0..=0xef).contains(&jpeg[at + 1]) {
        at += 2 + usize::from(u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]));
    }
    let mut out = jpeg[..at].to_vec();
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&exif);
    out.extend_from_slice(&jpeg[at..]);
    out
}

#[test]
fn a_jpeg_turns_upright_as_its_exif_says() {
    let jpeg = include_bytes!("../src/fixtures/two_tone.jpg");
    let upright = decode_raster(jpeg).unwrap();
    assert_eq!((upright.width, upright.height), (8, 4));
    let turned = decode_raster(&with_orientation(jpeg, 6)).unwrap();
    assert_eq!((turned.width, turned.height), (4, 8));
    // Red on the left, turned clockwise, is red at the top.
    let texel = |image: &RasterImage, x: usize, y: usize| {
        let i = (y * image.width as usize + x) * 4;
        [image.texels[i], image.texels[i + 2]]
    };
    let [r, b] = texel(&turned, 2, 1);
    assert!(r > 150 && b < 100, "{r} {b}");
    let [r, b] = texel(&turned, 2, 6);
    assert!(b > 150 && r < 100, "{r} {b}");
}
