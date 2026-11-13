//! A request for a fresh keyframe must be answered from the last picture when
//! the screen has not changed, not left waiting for it to change. These use
//! the real H.264 encoder (Windows).

use std::sync::atomic::AtomicBool;

use rc_codec::{EncoderConfig, H264Encoder};

use super::*;

const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

fn encoder(width: u32, height: u32) -> H264Encoder {
    H264Encoder::new(EncoderConfig {
        width,
        height,
        fps: 30,
        bitrate_kbps: 1000,
    })
    .unwrap()
}

/// A picture of the given size: not a flat colour, so the encoder has work to do.
fn fill(retained: &mut Retained, width: u32, height: u32) {
    let picture = retained.picture(width, height);
    for (i, byte) in picture.iter_mut().enumerate() {
        *byte = (i.wrapping_mul(7) % 251) as u8;
    }
}

/// NAL unit types in an Annex B stream.
fn nal_types(data: &[u8]) -> Vec<u8> {
    data.windows(4)
        .filter(|w| w[..3] == [0, 0, 1])
        .map(|w| w[3] & 0x1F)
        .collect()
}

#[test]
fn a_keyframe_is_made_from_the_last_picture_when_nothing_new_is_captured() {
    let mut encoder = encoder(WIDTH, HEIGHT);
    let mut retained = Retained::default();
    fill(&mut retained, WIDTH, HEIGHT);
    // The stream is under way: a keyframe, then frames that depend on it.
    let mut last = Vec::new();
    for i in 0..6 {
        last = encoder.encode(&retained.nv12, i * 33_333).unwrap();
    }
    assert!(
        !last.is_empty() && !last[0].keyframe,
        "the stream should be in delta frames"
    );

    let wanted = AtomicBool::new(true);
    let frames = retained
        .keyframe_if_wanted(&wanted, &mut encoder, 1_000_000)
        .unwrap()
        .expect("no keyframe was made from the retained picture");
    assert!(
        frames.iter().any(|f| f.keyframe),
        "the frame is not a keyframe"
    );
    let types = nal_types(&frames[0].data);
    assert!(
        types.contains(&5) && types.contains(&7) && types.contains(&8),
        "a decoder starting here needs the parameters and an IDR picture: {types:?}"
    );
    assert!(
        !wanted.load(Ordering::Relaxed),
        "the request was not cleared"
    );
    assert!(
        retained
            .keyframe_if_wanted(&wanted, &mut encoder, 1_033_333)
            .unwrap()
            .is_none(),
        "a second keyframe was made without a second request"
    );
}

#[test]
fn nothing_is_made_unless_a_keyframe_was_asked_for() {
    let mut encoder = encoder(WIDTH, HEIGHT);
    let mut retained = Retained::default();
    fill(&mut retained, WIDTH, HEIGHT);
    let wanted = AtomicBool::new(false);
    assert!(retained
        .keyframe_if_wanted(&wanted, &mut encoder, 0)
        .unwrap()
        .is_none());
}

#[test]
fn a_request_with_no_picture_yet_is_kept_for_the_next_captured_frame() {
    let mut encoder = encoder(WIDTH, HEIGHT);
    let retained = Retained::default();
    let wanted = AtomicBool::new(true);
    assert!(retained
        .keyframe_if_wanted(&wanted, &mut encoder, 0)
        .unwrap()
        .is_none());
    assert!(wanted.load(Ordering::Relaxed), "the request was lost");
}

#[test]
fn a_picture_of_another_size_than_the_encoder_is_not_used() {
    let mut encoder = encoder(WIDTH, HEIGHT);
    let mut retained = Retained::default();
    fill(&mut retained, 160, 120); // from before the screen changed size
    let wanted = AtomicBool::new(true);
    assert!(retained
        .keyframe_if_wanted(&wanted, &mut encoder, 0)
        .unwrap()
        .is_none());
    assert!(wanted.load(Ordering::Relaxed), "the request was lost");
}

/// The whole loop, with a real screen. It only tells the old behavior from
/// the new one while the screen is still (a quiet desktop): on a screen that
/// keeps changing, the next frame brings the keyframe either way.
#[test]
#[ignore = "needs an interactive desktop with a monitor; discriminating only while the screen is still"]
fn a_viewer_that_asks_for_a_keyframe_gets_one_without_the_screen_changing() {
    use super::test_support::{knock, pending, settings, wait_until};
    use crate::secure::test_support::{desktop, security};

    let _desktop = desktop();
    let (host_keys, _d1) = security();
    let (viewer_keys, _d2) = security();
    let host = ShareController::new(Some(host_keys));
    let port = host.start(settings()).unwrap().port.unwrap();
    let (writer, mut reader, _) = knock(port, &viewer_keys, "Viewer");
    host.answer(pending(&host).id, true, false).unwrap();
    wait_until("the viewer to be connected", || {
        host.status().viewer_connected
    });

    // The first picture arrives as a keyframe; then wait for the stream to go
    // quiet (a read that times out: nothing new on screen), for at most 10 s.
    let mut saw_first = false;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        match reader.read_message() {
            Ok(Message::VideoFrame { keyframe, .. }) => saw_first |= keyframe,
            Ok(_) => {}
            Err(_) => break,
        }
    }
    assert!(saw_first, "no first keyframe");

    // Ask, and time how long a keyframe takes.
    writer.send(&Message::KeyframeRequest).unwrap();
    let asked = Instant::now();
    let mut took = None;
    while asked.elapsed() < Duration::from_secs(2) {
        match reader.read_message() {
            Ok(Message::VideoFrame { keyframe: true, .. }) => {
                took = Some(asked.elapsed());
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    host.stop();
    let took = took.expect("no keyframe arrived within 2 s of asking");
    assert!(
        took < Duration::from_millis(1000),
        "the keyframe took {took:?}"
    );
}
