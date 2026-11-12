#![cfg(windows)]

use std::time::{Duration, Instant};

use rc_capture::{list_displays, CaptureError, Capturer, Rotation};

#[test]
fn unknown_display_is_rejected() {
    assert!(matches!(
        Capturer::new(usize::MAX),
        Err(CaptureError::NoSuchDisplay(usize::MAX))
    ));
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn lists_displays_with_one_primary() {
    let displays = list_displays().unwrap();
    assert!(!displays.is_empty());
    assert_eq!(displays.iter().filter(|d| d.primary).count(), 1);
    for (position, display) in displays.iter().enumerate() {
        assert_eq!(display.index, position);
        assert!(display.width > 0 && display.height > 0, "{display:?}");
        assert!(!display.name.is_empty());
    }
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn first_frame_matches_the_display() {
    let mut capturer = Capturer::new(0).unwrap();
    let info = capturer.display().clone();

    // The first frame is the whole desktop and arrives without any change
    // on screen; allow a few empty polls while duplication settles.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(frame) = capturer.next_frame(500).unwrap() {
            assert_eq!(frame.data.len(), frame.stride() * frame.height as usize);
            if info.rotation == Rotation::None {
                assert_eq!((frame.width, frame.height), (info.width, info.height));
            }
            assert!(
                frame.data.iter().any(|&b| b != 0),
                "frame is entirely black"
            );
            return;
        }
        assert!(Instant::now() < deadline, "no frame within 5 s");
    }
}
