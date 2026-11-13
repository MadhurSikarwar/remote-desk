use std::net::TcpListener;
use std::sync::mpsc;

use rc_protocol::Permissions;

use super::*;
use crate::secure::test_support::{desktop, security};
use crate::secure::SecureReader;
use crate::share::test_support::{pending, settings, wait_until};
use crate::share::ShareController;

const WAIT: Duration = Duration::from_secs(10);

fn addr(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

#[test]
fn addresses_are_understood() {
    let ok = |text: &str| resolve_address(text).unwrap();
    assert_eq!(ok("51000"), SocketAddr::from((Ipv4Addr::LOCALHOST, 51000)));
    assert_eq!(ok("  8080 "), SocketAddr::from((Ipv4Addr::LOCALHOST, 8080)));
    assert_eq!(
        ok("192.168.1.20:51000"),
        SocketAddr::from(([192, 168, 1, 20], 51000))
    );
    assert_eq!(ok(" 10.0.0.1:1 "), SocketAddr::from(([10, 0, 0, 1], 1)));
    // A name that resolves on every system; IPv4 is preferred.
    assert!(ok("localhost:5000").is_ipv4());
    assert_eq!(ok("localhost:5000").port(), 5000);
}

#[test]
fn bad_addresses_are_refused_with_a_reason() {
    for bad in [
        "",
        "  ",
        "0",
        "65536",
        "99999999999",
        "host",
        "192.168.1.20",
        ":5000",
        "a b:5000",
        "10.0.0.1:0",
        "10.0.0.1:99999",
        "10.0.0.1:abc",
        "10.0.0.1:",
    ] {
        let error = resolve_address(bad).unwrap_err();
        assert!(!error.is_empty(), "{bad:?}");
    }
}

#[test]
fn packets_have_the_documented_layout() {
    let config = to_packet(Message::VideoConfig {
        width: 1920,
        height: 1080,
        fps: 30,
    });
    assert_eq!(
        config.unwrap(),
        [1, 0x80, 7, 0, 0, 0x38, 4, 0, 0, 30, 0, 0, 0]
    );

    let frame = to_packet(Message::VideoFrame {
        keyframe: true,
        timestamp_us: 0x0102_0304_0506_0708,
        data: vec![0xAA, 0xBB],
    });
    assert_eq!(frame.unwrap(), [2, 1, 8, 7, 6, 5, 4, 3, 2, 1, 0xAA, 0xBB]);

    let bye = to_packet(Message::Bye {
        reason: "ok".into(),
    });
    assert_eq!(bye.unwrap(), [3, b'o', b'k']);

    let allowed = Message::Permissions(Permissions { input: true });
    assert_eq!(to_packet(allowed).unwrap(), [4, 1]);
    assert_eq!(to_packet(Message::Accepted).unwrap(), [5]);

    assert_eq!(to_packet(Message::KeyframeRequest), None);
    let event = InputEvent::MouseMove { x: 1, y: 2 };
    assert_eq!(to_packet(Message::Input(event)), None);
}

#[test]
fn window_input_is_parsed_and_converted() {
    let parse = |json: &str| {
        serde_json::from_str::<ViewerInput>(json)
            .unwrap()
            .to_event()
    };

    assert_eq!(
        parse(r#"{"kind":"move","x":0.5,"y":1.0}"#),
        Some(InputEvent::MouseMove { x: 32768, y: 65535 })
    );
    // Positions outside the picture are pulled to its edge.
    assert_eq!(
        parse(r#"{"kind":"button","button":2,"down":true,"x":-3.0,"y":7.5}"#),
        Some(InputEvent::MouseButton {
            button: MouseButton::Right,
            down: true,
            x: 0,
            y: 65535
        })
    );
    assert_eq!(
        parse(r#"{"kind":"button","button":1,"down":false,"x":0,"y":0}"#),
        Some(InputEvent::MouseButton {
            button: MouseButton::Middle,
            down: false,
            x: 0,
            y: 0
        })
    );
    assert_eq!(
        parse(r#"{"kind":"wheel","dx":0,"dy":-1.5}"#),
        Some(InputEvent::MouseWheel { dx: 0, dy: -180 })
    );
    assert_eq!(
        parse(r#"{"kind":"key","code":"ArrowLeft","down":true}"#),
        Some(InputEvent::Key {
            scancode: 0x4B,
            extended: true,
            down: true
        })
    );
}

#[test]
fn unknown_keys_and_buttons_are_not_forwarded() {
    let unknown_key = ViewerInput::Key {
        code: "LaunchMail".into(),
        down: true,
    };
    let fourth_button = ViewerInput::Button {
        button: 3,
        down: true,
        x: 0.0,
        y: 0.0,
    };
    assert_eq!(unknown_key.to_event(), None);
    assert_eq!(fourth_button.to_event(), None);
    assert!(serde_json::from_str::<ViewerInput>(r#"{"kind":"teleport"}"#).is_err());
}

#[test]
fn connecting_fails_cleanly_without_a_key_or_a_listener() {
    let keyless = ViewerController::new(None);
    let error = keyless.connect(&addr(1), None, "Me", |_| {}).unwrap_err();
    assert!(error.contains("key"), "{error}");

    let (keys, _dir) = security();
    let viewer = ViewerController::new(Some(keys));
    // Port 1 on loopback has no listener.
    assert!(viewer.connect(&addr(1), None, "Me", |_| {}).is_err());
    assert!(viewer.request_keyframe().is_err());
    viewer.disconnect();
}

#[test]
fn a_host_with_a_different_key_than_expected_is_refused() {
    let (port, host_keys, _host_side, _dir) = fake_host(|_| {});
    let (viewer, sink, rx, _dir2) = collecting_viewer();
    let wrong: PublicKey = [0x42; 32];

    let error = viewer
        .connect(&addr(port), Some(wrong), "Me", sink)
        .unwrap_err();
    assert!(error.contains("not the one you picked"), "{error}");
    assert!(error.contains(&host_keys.fingerprint()), "{error}");
    assert!(error.contains(&fingerprint(&wrong)), "{error}");
    // Nothing reached the window.
    assert!(rx.try_recv().is_err());
    assert!(
        viewer.request_keyframe().is_err(),
        "left a connection behind"
    );
}

#[test]
fn the_expected_key_lets_the_right_host_through() {
    let (port, host_keys, _host_side, _dir) = fake_host(|_| {});
    let (viewer, sink, _rx, _dir2) = collecting_viewer();
    let right = *host_keys.identity().public();
    let host = viewer
        .connect(&addr(port), Some(right), "Me", sink)
        .unwrap();
    assert_eq!(host.fingerprint, host_keys.fingerprint());
    viewer.disconnect();
}

#[test]
fn a_host_that_does_not_encrypt_is_refused() {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    // Answers the way a host from before encryption would have.
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let hello = Message::Hello {
            version: rc_protocol::PROTOCOL_VERSION,
            role: Role::Host,
            name: "Old host".into(),
        };
        let _ = rc_protocol::write_message(&mut stream, &hello);
        let _ = std::io::Write::write_all(&mut stream, &[0u8; 64]);
        thread::sleep(Duration::from_secs(2));
    });

    let (keys, _dir) = security();
    let viewer = ViewerController::new(Some(keys));
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let error = viewer
        .connect(&addr(port), None, "Me", move |packet| {
            let _ = tx.send(packet);
        })
        .unwrap_err();
    assert!(error.contains("secure connection"), "{error}");
    assert!(rx.try_recv().is_err(), "packets from an unencrypted host");
}

/// A host made of the raw pieces: handshake and greeting done, then `after`
/// runs, then the connection is kept open.
fn fake_host(
    after: impl FnOnce(&SecureWriter) + Send + 'static,
) -> (
    u16,
    Arc<Security>,
    mpsc::Receiver<(SecureWriter, SecureReader)>,
    tempfile::TempDir,
) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (keys, dir) = security();
    let host_keys = Arc::clone(&keys);
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let (writer, mut reader, _) =
            establish(&stream, host_keys.identity(), Side::Responder).unwrap();
        // A viewer that turns away straight after the handshake is expected.
        if exchange_hello(&writer, &mut reader, Role::Host, "Fake\thost").is_err() {
            return;
        }
        after(&writer);
        let _ = tx.send((writer, reader));
    });
    (port, keys, rx, dir)
}

fn collecting_viewer() -> (
    ViewerController,
    impl Fn(Vec<u8>) + Send + 'static,
    mpsc::Receiver<Vec<u8>>,
    tempfile::TempDir,
) {
    let (keys, dir) = security();
    let (tx, rx) = mpsc::channel();
    let sink = move |packet| {
        let _ = tx.send(packet);
    };
    (ViewerController::new(Some(keys)), sink, rx, dir)
}

#[test]
fn connecting_reports_who_answered_and_waits_for_the_host() {
    let (port, host_keys, host_side, _dir) = fake_host(|_| {});
    let (viewer, sink, rx, _dir2) = collecting_viewer();

    let host = viewer.connect(&addr(port), None, "Me", sink).unwrap();
    assert_eq!(host.name, "Fakehost");
    assert_eq!(host.fingerprint, host_keys.fingerprint());
    assert_eq!(host.auth_code.len(), 7);
    assert!(
        viewer.connect(&addr(port), None, "Me", |_| {}).is_err(),
        "second connect"
    );

    // Nothing arrives until the host decides.
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    let (writer, _reader) = host_side.recv_timeout(WAIT).unwrap();
    writer.send(&Message::Accepted).unwrap();
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), [packet::ACCEPTED]);
    viewer.disconnect();
}

#[test]
fn disconnect_returns_promptly_from_a_silent_host() {
    let (port, _keys, host_side, _dir) = fake_host(|_| {});
    let (viewer, sink, rx, _dir2) = collecting_viewer();
    viewer.connect(&addr(port), None, "Me", sink).unwrap();
    let _host = host_side.recv_timeout(WAIT).unwrap();

    let before = std::time::Instant::now();
    viewer.disconnect();
    let took = before.elapsed();
    assert!(took < Duration::from_secs(2), "disconnect took {took:?}");
    assert_eq!(rx.iter().last().unwrap(), ended("disconnected"));
}

#[test]
fn the_viewer_closes_its_side_when_the_host_refuses() {
    let (port, _keys, host_side, _dir) = fake_host(|writer| {
        writer
            .send(&Message::Bye {
                reason: "the host declined the request".into(),
            })
            .unwrap();
    });
    let (viewer, sink, rx, _dir2) = collecting_viewer();
    viewer.connect(&addr(port), None, "Me", sink).unwrap();

    assert_eq!(
        rx.recv_timeout(WAIT).unwrap(),
        ended("the host declined the request")
    );
    // The host must see the connection close without the viewer being told
    // to disconnect.
    let (_writer, mut reader) = host_side.recv_timeout(WAIT).unwrap();
    assert!(reader.read_message().is_err());
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn a_viewer_receives_the_shared_screen_once_approved() {
    let _desktop = desktop();
    let (host_keys, _dir) = security();
    let host = ShareController::new(Some(host_keys));
    let port = host.start(settings()).unwrap().port.unwrap();
    let (viewer, sink, rx, _dir2) = collecting_viewer();

    let info = viewer
        .connect(&addr(port), None, "Viewer PC", sink)
        .unwrap();
    assert_eq!(info.name, "Test host");

    // The host sees the same code and the viewer's name, and until it
    // answers the viewer gets nothing.
    let request = pending(&host);
    assert_eq!(request.name, "Viewer PC");
    assert_eq!(request.auth_code, info.auth_code);
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err());
    host.answer(request.id, true, false).unwrap();

    assert_eq!(rx.recv_timeout(WAIT).unwrap(), [packet::ACCEPTED]);
    assert_eq!(rx.recv_timeout(WAIT).unwrap(), [packet::PERMISSIONS, 0]);
    let config = rx.recv_timeout(WAIT).expect("no config packet");
    assert_eq!(config[0], packet::CONFIG);
    let width = u32::from_le_bytes(config[1..5].try_into().unwrap());
    assert!(width > 0 && width % 2 == 0);

    let frame = rx.recv_timeout(WAIT).expect("no frame packet");
    assert_eq!(frame[0], packet::FRAME);
    assert_eq!(frame[1], 1, "the first frame must be a keyframe");
    assert!(frame.len() > 10 + 4);
    assert!(host.status().viewer_connected);
    assert!(host.status().frames >= 1);
    viewer.request_keyframe().unwrap();

    viewer.disconnect();
    let last = rx.iter().last().expect("no final packet");
    assert_eq!(last[0], packet::ENDED);

    // The host notices and goes back to waiting for a viewer.
    wait_until("the host to drop the viewer", || {
        !host.status().viewer_connected
    });
    assert!(host.status().active);
    host.stop();
}
