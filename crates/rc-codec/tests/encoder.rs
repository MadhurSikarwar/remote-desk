#![cfg(windows)]

use rc_codec::nv12::{bgra_to_nv12, nv12_len};
use rc_codec::{CodecError, EncodedFrame, EncoderConfig, H264Encoder};

const WIDTH: usize = 320;
const HEIGHT: usize = 240;

fn config() -> EncoderConfig {
    EncoderConfig {
        width: WIDTH as u32,
        height: HEIGHT as u32,
        fps: 30,
        bitrate_kbps: 1000,
    }
}

/// A gradient with a bright square that moves a little each frame.
fn test_frame(index: usize) -> Vec<u8> {
    let mut bgra = vec![0u8; WIDTH * HEIGHT * 4];
    let square = (index * 4) % (WIDTH - 40);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let inside = (square..square + 40).contains(&x) && (100..140).contains(&y);
            let px = if inside {
                [40, 220, 250, 255]
            } else {
                [(x * 255 / WIDTH) as u8, (y * 255 / HEIGHT) as u8, 90, 255]
            };
            bgra[(y * WIDTH + x) * 4..][..4].copy_from_slice(&px);
        }
    }
    let mut nv12 = vec![0u8; nv12_len(WIDTH, HEIGHT)];
    bgra_to_nv12(&bgra, WIDTH * 4, WIDTH, HEIGHT, &mut nv12);
    nv12
}

/// NAL unit types found in an Annex B stream.
fn nal_types(data: &[u8]) -> Vec<u8> {
    data.windows(4)
        .filter(|w| w[..3] == [0, 0, 1])
        .map(|w| w[3] & 0x1F)
        .collect()
}

fn encode_frames(encoder: &mut H264Encoder, range: std::ops::Range<usize>) -> Vec<EncodedFrame> {
    range
        .flat_map(|i| {
            encoder
                .encode(&test_frame(i), i as u64 * 33_333)
                .expect("encode")
        })
        .collect()
}

#[test]
fn rejects_invalid_settings() {
    for bad in [
        EncoderConfig {
            width: 0,
            ..config()
        },
        EncoderConfig {
            width: 321,
            ..config()
        },
        EncoderConfig {
            height: 241,
            ..config()
        },
        EncoderConfig { fps: 0, ..config() },
        EncoderConfig {
            bitrate_kbps: 0,
            ..config()
        },
    ] {
        assert!(
            matches!(H264Encoder::new(bad), Err(CodecError::InvalidConfig(_))),
            "{bad:?}"
        );
    }
}

#[test]
fn rejects_a_frame_of_the_wrong_size() {
    let mut encoder = H264Encoder::new(config()).unwrap();
    let error = encoder.encode(&[0u8; 100], 0).unwrap_err();
    assert!(matches!(
        error,
        CodecError::WrongFrameSize { actual: 100, .. }
    ));
}

#[test]
fn emits_one_frame_per_input_starting_with_a_keyframe() {
    let mut encoder = H264Encoder::new(config()).unwrap();
    let frames = encode_frames(&mut encoder, 0..30);

    assert_eq!(frames.len(), 30, "low-latency mode must not buffer frames");
    let first = &frames[0];
    assert!(first.keyframe);
    assert!(first.data.starts_with(&[0, 0, 0, 1]) || first.data.starts_with(&[0, 0, 1]));
    let types = nal_types(&first.data);
    assert!(types.contains(&7), "no SPS in the first frame: {types:?}");
    assert!(types.contains(&8), "no PPS in the first frame: {types:?}");
    assert!(
        types.contains(&5),
        "no IDR slice in the first frame: {types:?}"
    );

    assert!(
        frames[1..].iter().all(|f| !f.keyframe),
        "unexpected extra keyframe"
    );
    let timestamps: Vec<u64> = frames.iter().map(|f| f.timestamp_us).collect();
    assert!(timestamps.windows(2).all(|w| w[0] < w[1]), "{timestamps:?}");
}

#[test]
fn a_requested_keyframe_arrives_on_the_next_frame() {
    let mut encoder = H264Encoder::new(config()).unwrap();
    encode_frames(&mut encoder, 0..5);

    encoder.request_keyframe().unwrap();
    let frames = encode_frames(&mut encoder, 5..6);
    assert_eq!(frames.len(), 1);
    assert!(frames[0].keyframe);
    assert!(nal_types(&frames[0].data).contains(&5));
}
