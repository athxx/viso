//! A player plays the three-frame clip — red, green, blue, a tenth of a
//! second each, 64 × 48 — through the platform's player, on the main thread
//! with its run loop turning. Run without the test harness, which would run
//! it on a thread of its own.

#[cfg(any(
    target_vendor = "apple",
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
mod player {
    use std::time::{Duration, Instant};

    use viso_media::{VideoEvent, VideoPlayer, VideoSource};

    const CLIP: &[u8] = include_bytes!("fixtures/three.mp4");

    fn clip() -> VideoSource {
        VideoSource::File(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/three.mp4").into())
    }

    /// Turns the main run loop for `seconds`.
    #[cfg(target_vendor = "apple")]
    fn turn(seconds: f64) {
        use objc2_foundation::{NSDate, NSRunLoop};
        NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(seconds));
    }

    /// Waits `seconds`: the player needs no loop of this thread's.
    #[cfg(not(target_vendor = "apple"))]
    fn turn(seconds: f64) {
        std::thread::sleep(Duration::from_secs_f64(seconds));
    }

    /// Polls `player` until `want` holds of an event, or panics after
    /// `within`.
    fn until(
        player: &mut VideoPlayer,
        within: Duration,
        want: impl Fn(&VideoEvent) -> bool,
    ) -> VideoEvent {
        let start = Instant::now();
        loop {
            while let Some(event) = player.poll() {
                if want(&event) {
                    return event;
                }
            }
            assert!(start.elapsed() < within, "no such event within {within:?}");
            turn(0.005);
        }
    }

    /// The colour a frame shows at its centre: which of red, green and blue
    /// is brightest.
    fn shown(player: &mut VideoPlayer) -> Option<char> {
        let frame = player.frame()?;
        // It lies in an IOSurface of its size, which a GPU samples in place.
        #[cfg(target_vendor = "apple")]
        {
            let Some(viso_handle::ExternalImage::IoSurface {
                surface,
                width,
                height,
            }) = frame.native()
            else {
                panic!("a frame not in an IOSurface");
            };
            assert!(!surface.is_null());
            assert_eq!((width, height), (frame.width, frame.height));
        }
        let mut rgba = Vec::new();
        assert!(frame.copy_rgba(&mut rgba));
        let at =
            ((frame.height as usize / 2) * frame.width as usize + frame.width as usize / 2) * 4;
        let [r, g, b] = [rgba[at], rgba[at + 1], rgba[at + 2]];
        Some(if r > g && r > b {
            'r'
        } else if g > b {
            'g'
        } else {
            'b'
        })
    }

    fn opens_paused_on_its_first_picture_then_plays_each_to_its_end() {
        let mut player = VideoPlayer::open(&clip()).unwrap();
        let VideoEvent::Ready(info) = until(&mut player, Duration::from_secs(5), |e| {
            matches!(e, VideoEvent::Ready(_))
        }) else {
            unreachable!()
        };
        assert_eq!((info.width, info.height), (64, 48));
        assert_eq!(info.frame_rate, Some(10.0));
        assert!(!info.audio);
        let length = info.duration.expect("a file has a length");
        assert!((length.as_secs_f64() - 0.3).abs() < 0.02, "{length:?}");

        // Paused, it shows its first picture.
        let start = Instant::now();
        let first = loop {
            if let Some(colour) = shown(&mut player) {
                break colour;
            }
            assert!(start.elapsed() < Duration::from_secs(3), "no first picture");
            turn(0.005);
        };
        assert_eq!(first, 'r');
        assert!(!player.is_playing());

        player.play();
        let mut seen = vec![first];
        let start = Instant::now();
        loop {
            if let Some(colour) = shown(&mut player)
                && seen.last() != Some(&colour)
            {
                seen.push(colour);
            }
            match player.poll() {
                Some(VideoEvent::Ended) => break,
                Some(VideoEvent::Waiting(_)) | None => {}
                Some(other) => panic!("{other:?}"),
            }
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "never ended: {seen:?}"
            );
            turn(0.002);
        }
        assert_eq!(seen, ['r', 'g', 'b']);
        assert!(!player.is_playing());
        assert!(player.position() >= Duration::from_millis(290));
    }

    fn a_looping_player_starts_over_rather_than_end() {
        let mut player = VideoPlayer::open(&clip()).unwrap();
        player.set_looping(true);
        player.set_muted(true);
        player.play();
        until(&mut player, Duration::from_secs(5), |e| {
            matches!(e, VideoEvent::Ready(_))
        });
        let start = Instant::now();
        let mut wrapped = false;
        let mut last = Duration::ZERO;
        while start.elapsed() < Duration::from_millis(900) {
            let event = player.poll();
            assert!(
                matches!(event, None | Some(VideoEvent::Waiting(_))),
                "a looping player does not end: {event:?}"
            );
            let now = player.position();
            wrapped |= now + Duration::from_millis(50) < last;
            last = now;
            turn(0.005);
        }
        assert!(wrapped, "it went round");
        assert!(player.is_playing());
    }

    fn bytes_play_as_their_file_would_and_a_seek_shows_its_picture_while_paused() {
        let mut player = VideoPlayer::open(&VideoSource::Bytes(CLIP.into())).unwrap();
        until(&mut player, Duration::from_secs(5), |e| {
            matches!(e, VideoEvent::Ready(_))
        });
        player.seek(Duration::from_millis(250));
        let start = Instant::now();
        loop {
            if shown(&mut player) == Some('b') {
                break;
            }
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "the sought picture never showed"
            );
            turn(0.005);
        }
        assert!(!player.is_playing());
    }

    fn what_is_not_a_video_fails() {
        let mut player =
            VideoPlayer::open(&VideoSource::Bytes(b"not a video at all".as_slice().into()))
                .unwrap();
        let event = until(&mut player, Duration::from_secs(5), |e| {
            matches!(e, VideoEvent::Failed(_))
        });
        assert!(matches!(event, VideoEvent::Failed(_)), "{event:?}");
        assert!(player.frame().is_none());
    }

    /// Pictures come as NV12 planes, which a GPU converts, until one is
    /// copied out as rows; then as BGRA rows, which GStreamer converts.
    #[cfg(not(any(target_vendor = "apple", target_os = "windows")))]
    fn pictures_come_as_planes_until_one_is_copied_then_as_rows() {
        let mut player = VideoPlayer::open(&clip()).unwrap();
        player.set_looping(true);
        until(&mut player, Duration::from_secs(5), |e| {
            matches!(e, VideoEvent::Ready(_))
        });
        player.play();
        let mut planes = 0;
        let start = Instant::now();
        while planes < 3 {
            if let Some(frame) = player.frame() {
                let picture = frame.planes().expect("a picture in planes");
                assert_eq!((picture.width, picture.height), (64, 48));
                // Rows as the buffer's metadata lays them, padded or not.
                assert!(picture.y_stride >= 64 && picture.uv_stride >= 64);
                assert!(picture.y.len() >= picture.y_stride * 47 + 64);
                assert!(picture.uv.len() >= picture.uv_stride * 23 + 64);
                planes += 1;
            }
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "{planes} pictures"
            );
            turn(0.005);
        }
        // Copied once, the pictures come as rows from a few on.
        let start = Instant::now();
        while shown(&mut player).is_none() {
            while player.poll().is_some() {}
            assert!(start.elapsed() < Duration::from_secs(3), "no picture");
            turn(0.005);
        }
        let start = Instant::now();
        loop {
            if let Some(frame) = player.frame()
                && frame.planes().is_none()
            {
                let mut rgba = Vec::new();
                assert!(frame.copy_rgba(&mut rgba));
                assert_eq!(rgba.len(), 64 * 48 * 4);
                break;
            }
            while player.poll().is_some() {}
            assert!(
                start.elapsed() < Duration::from_secs(3),
                "the pictures never came as rows"
            );
            turn(0.005);
        }
    }

    /// The default camera, where there is one the process may use: ready
    /// at its first picture, with no length; paused, it lets the camera go,
    /// and plays on when asked again. Skipped where there is none, or the
    /// user has not let the process capture.
    fn a_camera_plays_live_where_one_is_allowed() {
        let cameras = viso_media::cameras();
        if cameras.is_empty() {
            println!("no camera; skipped");
            return;
        }
        assert!(cameras.iter().all(|c| !c.id.is_empty()));
        let request = viso_media::CameraRequest {
            width: 640,
            height: 480,
            ..Default::default()
        };
        let mut player = match VideoPlayer::open(&VideoSource::Camera(request)) {
            Ok(player) => player,
            Err(viso_media::VideoError::Denied) => {
                println!("the camera is refused; skipped");
                return;
            }
            Err(error) => panic!("{error}"),
        };
        let start = Instant::now();
        let info = loop {
            match player.poll() {
                Some(VideoEvent::Ready(info)) => break info,
                Some(VideoEvent::Failed(viso_media::VideoError::Denied)) => {
                    println!("the camera is refused; skipped");
                    return;
                }
                Some(other) => panic!("{other:?}"),
                None => {}
            }
            if start.elapsed() > Duration::from_secs(10) {
                println!("the camera never answered (access not given?); skipped");
                return;
            }
            turn(0.01);
        };
        println!(
            "camera: {}x{} at {:?}, facing {:?}",
            info.width, info.height, info.frame_rate, info.facing
        );
        assert!(info.width > 0 && info.height > 0);
        assert_eq!(
            (info.duration, info.seekable, info.audio),
            (None, false, false)
        );
        assert!(info.facing.is_some());
        let picture = |player: &mut VideoPlayer| {
            let start = Instant::now();
            loop {
                if let Some(frame) = player.frame() {
                    let mut rgba = Vec::new();
                    assert!(frame.copy_rgba(&mut rgba));
                    assert_eq!(rgba.len(), (frame.width * frame.height * 4) as usize);
                    return;
                }
                while player.poll().is_some() {}
                assert!(start.elapsed() < Duration::from_secs(5), "no picture");
                turn(0.01);
            }
        };
        picture(&mut player);
        player.play();
        assert!(player.is_playing());
        picture(&mut player);
        player.pause();
        turn(0.3);
        while player.frame().is_some() {}
        player.play();
        picture(&mut player);
        assert_eq!(player.position(), Duration::ZERO);
    }

    pub fn run() {
        #[cfg(not(any(target_vendor = "apple", target_os = "windows")))]
        {
            pictures_come_as_planes_until_one_is_copied_then_as_rows();
            println!("test pictures_come_as_planes_until_one_is_copied_then_as_rows ... ok");
        }
        for (name, test) in [
            (
                "opens_paused_on_its_first_picture_then_plays_each_to_its_end",
                opens_paused_on_its_first_picture_then_plays_each_to_its_end as fn(),
            ),
            (
                "a_looping_player_starts_over_rather_than_end",
                a_looping_player_starts_over_rather_than_end,
            ),
            (
                "bytes_play_as_their_file_would_and_a_seek_shows_its_picture_while_paused",
                bytes_play_as_their_file_would_and_a_seek_shows_its_picture_while_paused,
            ),
            ("what_is_not_a_video_fails", what_is_not_a_video_fails),
            (
                "a_camera_plays_live_where_one_is_allowed",
                a_camera_plays_live_where_one_is_allowed,
            ),
        ] {
            test();
            println!("test {name} ... ok");
        }
    }
}

fn main() {
    #[cfg(any(
        target_vendor = "apple",
        target_os = "windows",
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    player::run();
}
