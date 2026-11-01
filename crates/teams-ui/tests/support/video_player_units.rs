//! Offline unit scenarios for the parent module; kept outside runtime coverage sources.

use super::*;
use gpui::AppContext as _;

#[test]
fn times_are_written_as_the_player_writes_them() {
    assert_eq!(clock(Duration::from_secs(5)), "0:05");
    assert_eq!(clock(Duration::from_secs(754)), "12:34");
    assert_eq!(clock(Duration::from_secs(3_723)), "1:02:03");
}

#[test]
fn a_picture_is_fitted_inside_its_box() {
    let bounds = Bounds {
        origin: point(px(10.0), px(20.0)),
        size: size(px(320.0), px(180.0)),
    };
    let fit = fitted(bounds, 640, 480);
    assert_eq!(fit.size, size(px(240.0), px(180.0)));
    assert_eq!(fit.origin, point(px(50.0), px(20.0)));
}

#[derive(Default)]
struct Files(Mutex<Vec<(String, u64, u64)>>);

impl crate::inline::PictureEngine for Files {
    fn me(&self) -> &'static str {
        "8:orgid:00000000-0000-4000-8000-00000000000a"
    }

    fn message_picture(&self, _: String) -> teams_core::Reply<Option<teams_core::PictureBytes>> {
        teams_core::Reply::ready(Ok(None))
    }

    fn reaction_animation(&self, _: String) -> teams_core::Reply<Option<teams_core::Strip>> {
        teams_core::Reply::ready(Ok(None))
    }

    fn file_stream(&self, share: String, offset: u64, len: u64) -> teams_core::Reply<FileStream> {
        self.0.lock().unwrap().push((share.clone(), offset, len));
        teams_core::Reply::ready(match share.as_str() {
            "missing" => Err(EngineError::FileUnavailable),
            "stopped" => Err(EngineError::Stopped),
            "failed" => Err(EngineError::PictureUnread),
            "https://files.example.test/synthetic.mp4" => {
                let start = usize::try_from(offset).unwrap().min(MOVIE.len());
                let end = start.saturating_add(usize::try_from(len).unwrap()).min(MOVIE.len());
                Ok(FileStream::of(offset, &MOVIE[start..end], MOVIE.len() as u64, 4096))
            }
            _ => Ok(FileStream::of(offset, b"synthetic bytes", 100, 4)),
        })
    }

    fn engine(&self) -> *const () {
        std::ptr::from_ref(self).cast()
    }
}

const MOVIE: &[u8] = include_bytes!("../../../teams-media/tests/fixtures/media/made-up-test-pattern.mp4");

fn movie_file() -> SharedFile {
    SharedFile {
        name: "synthetic.mp4".to_owned(),
        extension: "mp4".to_owned(),
        share_url: Some("https://files.example.test/synthetic.mp4".to_owned()),
        object_url: Some("https://files.example.test/synthetic.mp4".to_owned()),
        preview: None,
    }
}

fn draw(cx: &mut gpui::VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn click(cx: &mut gpui::VisualTestContext, selector: &'static str) {
    let at = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"))
        .center();
    cx.simulate_click(at, gpui::Modifiers::none());
    draw(cx);
}

fn wait_state(view: &Entity<VideoPlayer>, cx: &mut gpui::VisualTestContext, wanted: State) {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        draw(cx);
        let state = cx.update(|_, cx| view.read(cx).state());
        if state == Some(wanted) {
            return;
        }
        assert!(Instant::now() < until, "wanted {wanted:?}, last state {state:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_synthetic_movie_plays_pauses_and_seeks_without_opening_any_devices() {
    let mut app = gpui::TestAppContext::single();
    app.update(crate::window::init);
    let files = Arc::new(Files::default());
    let source = PictureSource::of(files.clone());
    let (view, cx) = app.add_window_view(|_, cx| VideoPlayer::new("movie".to_owned(), movie_file(), source, cx));
    draw(cx);
    click(cx, "video-mute-movie");
    click(cx, "video-play-movie");
    wait_state(&view, cx, State::Playing);
    click(cx, "video-play-movie");
    wait_state(&view, cx, State::Paused);
    assert!(!files.0.lock().unwrap().is_empty());
    cx.update(|_, cx| {
        view.update(cx, |view, cx| {
            assert!(view.muted);
            assert!(view.frame().is_some(), "the synthetic movie produced a decoded frame");
            view.status = Some(Status {
                state: State::Paused,
                position: Duration::from_secs(5),
                duration: Duration::from_secs(30),
                sound: false,
            });
            view.skip(true, cx);
            assert_eq!(view.status.unwrap().position, Duration::ZERO);
            view.skip(false, cx);
            assert_eq!(view.status.unwrap().position, SKIP);
            view.seek(Command::Seek(Duration::from_secs(29)), cx);
            view.skip(false, cx);
            assert_eq!(view.status.unwrap().position, Duration::from_secs(30));
            view.seek(Command::Pause, cx);
            assert_eq!(view.status.unwrap().position, Duration::from_secs(30));
        });
    });
    click(cx, "video-fullscreen-movie");
    assert!(cx.update(|_, cx| view.read(cx).full_screen.on));
    cx.simulate_keystrokes("m left right escape");
    assert!(!cx.update(|_, cx| view.read(cx).full_screen.on));
    assert!(!cx.update(|_, cx| view.read(cx).muted));
    draw(cx);
    let bar = cx.debug_bounds("video-seek-movie").unwrap();
    let from = bar.origin + point(bar.size.width * 0.2, bar.size.height * 0.5);
    let to = bar.origin + point(bar.size.width * 0.8, bar.size.height * 0.5);
    cx.simulate_mouse_down(from, MouseButton::Left, gpui::Modifiers::none());
    cx.simulate_mouse_move(to, MouseButton::Left, gpui::Modifiers::none());
    assert!(cx.update(|_, cx| view.read(cx).dragging.is_some()));
    cx.simulate_mouse_up(to, MouseButton::Left, gpui::Modifiers::none());
    assert!(cx.update(|_, cx| view.read(cx).dragging.is_none()));
}

#[test]
fn video_actions_open_only_the_mock_browser_and_copy_the_original_link() {
    let mut app = gpui::TestAppContext::single();
    app.update(crate::window::init);
    let source = PictureSource::of(Arc::new(Files::default()));
    let (_, cx) = app.add_window_view(|_, cx| VideoPlayer::new("menu".to_owned(), movie_file(), source, cx));
    draw(cx);
    click(cx, "video-open-menu");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://files.example.test/synthetic.mp4")
    );
    click(cx, "video-more-menu");
    click(cx, "video-menu-copy");
    assert_eq!(
        cx.read_from_clipboard().and_then(|item| item.text()).as_deref(),
        Some("https://files.example.test/synthetic.mp4")
    );
    click(cx, "video-more-menu");
    click(cx, "video-menu-download");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://files.example.test/synthetic.mp4?download=1")
    );
    click(cx, "video-more-menu");
    click(cx, "video-menu-open");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://files.example.test/synthetic.mp4")
    );
}

#[test]
fn starting_a_second_movie_pauses_the_first() {
    let mut app = gpui::TestAppContext::single();
    app.update(crate::window::init);
    let source = PictureSource::of(Arc::new(Files::default()));
    let (first, cx) =
        app.add_window_view(|_, cx| VideoPlayer::new("first".to_owned(), movie_file(), source.clone(), cx));
    draw(cx);
    click(cx, "video-play-first");
    wait_state(&first, cx, State::Playing);
    let second = cx.update(|_, cx| cx.new(|cx| VideoPlayer::new("second".to_owned(), movie_file(), source, cx)));
    cx.update(|_, cx| second.update(cx, VideoPlayer::toggle));
    wait_state(&first, cx, State::Paused);
    assert!(cx.update(|_, cx| second.read(cx).player.is_some()));
    assert_eq!(cx.update(|_, cx| players(cx).len()), 2);
}

#[test]
fn playback_reads_through_the_engine_in_pieces_and_cancellation_stops_a_wait() {
    let files = Arc::new(Files::default());
    let source = PictureSource::of(files.clone());
    let bytes = EngineBytes {
        source: source.clone(),
        share_url: "synthetic-file".to_owned(),
    };
    let abandon = Abandon::new();
    let mut stream = bytes.open(10, 15, &abandon).unwrap();
    assert_eq!(stream.total(), 100);
    let mut read = Vec::new();
    while let Some(piece) = stream.next(&abandon).unwrap() {
        read.extend(piece);
    }
    assert_eq!(read, b"synthetic bytes");
    assert_eq!(
        files.0.lock().unwrap().as_slice(),
        [("synthetic-file".to_owned(), 10, 15)]
    );
    abandon.set();
    assert_eq!(stream.next(&abandon).unwrap_err(), SourceError::Stopped);
    assert!(matches!(bytes.open(0, 1, &abandon), Err(SourceError::Stopped)));
    for (share_url, expected) in [
        ("missing", SourceError::Unavailable),
        ("stopped", SourceError::Stopped),
        ("failed", SourceError::Failed),
    ] {
        let bytes = EngineBytes {
            source: source.clone(),
            share_url: share_url.to_owned(),
        };
        assert!(matches!(bytes.open(0, 1, &Abandon::new()), Err(error) if error == expected));
    }
}

#[test]
fn the_frame_sink_accepts_only_packed_bgra_and_keeps_the_latest_frame_and_status() {
    let (wake, woken) = async_channel::bounded(1);
    let screen = Screen {
        picture: Mutex::new(None),
        status: Mutex::new(None),
        serial: AtomicU64::new(0),
        wake,
    };
    let frame = VideoFrame {
        width: 2,
        height: 1,
        stride: 8,
        format: PixelFormat::Bgra,
        data: vec![1; 8],
    };
    screen.frame(VideoFrame {
        format: PixelFormat::Rgba,
        ..frame.clone()
    });
    screen.frame(VideoFrame {
        stride: 12,
        ..frame.clone()
    });
    assert!(screen.picture.lock().unwrap().is_none());
    assert!(woken.try_recv().is_err());
    screen.frame(frame.clone());
    screen.frame(VideoFrame {
        data: vec![2; 8],
        ..frame
    });
    let picture = screen.picture.lock().unwrap().clone().unwrap();
    assert_eq!((picture.width, picture.height, picture.serial), (2, 1, 2));
    assert_eq!(*picture.bgra, vec![2; 8]);
    let status = Status {
        state: State::Buffering,
        position: Duration::from_secs(1),
        duration: Duration::from_secs(10),
        sound: false,
    };
    screen.status(status);
    assert_eq!(*screen.status.lock().unwrap(), Some(status));
    assert!(woken.try_recv().is_ok());
    assert!(woken.try_recv().is_err(), "a pending wake coalesces frames and status");
}

#[test]
fn player_statuses_render_their_loading_and_failure_states_and_fullscreen_is_reversible() {
    use gpui::TestAppContext;

    let mut app = TestAppContext::single();
    app.update(crate::window::init);
    let source = PictureSource::of(Arc::new(Files::default()));
    let file = SharedFile {
        name: "synthetic.mp4".to_owned(),
        extension: "mp4".to_owned(),
        share_url: None,
        object_url: None,
        preview: None,
    };
    let (view, cx) = app.add_window_view(|_, cx| VideoPlayer::new("status".to_owned(), file, source, cx));
    let output = cx.update(|_, cx| view.read(cx).screen.clone());
    for state in [
        State::Loading,
        State::Buffering,
        State::Paused,
        State::Playing,
        State::Ended,
        State::Failed(Failure::Unavailable),
        State::Failed(Failure::Network),
        State::Failed(Failure::Unsupported),
        State::Failed(Failure::NoCodec),
        State::Failed(Failure::Internal),
    ] {
        output.status(Status {
            state,
            position: Duration::from_secs(5),
            duration: Duration::from_secs(30),
            sound: false,
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert_eq!(cx.update(|_, cx| view.read(cx).state()), Some(state));
        assert_eq!(
            cx.debug_bounds("video-loading-status").is_some(),
            matches!(state, State::Loading | State::Buffering)
        );
        assert_eq!(
            cx.debug_bounds("video-failed-status").is_some(),
            matches!(state, State::Failed(_))
        );
    }
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.toggle(cx); // no sharing link: never starts a media thread
            assert!(view.player.is_none());
            view.toggle_mute(cx);
            assert!(view.muted);
            view.toggle_fullscreen(window, cx);
            assert!(view.full_screen.on);
            view.toggle_fullscreen(window, cx);
            assert!(!view.full_screen.on);
        });
    });
}
