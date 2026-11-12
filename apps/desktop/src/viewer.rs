//! Viewer side: connects to a host and forwards its video to the window,
//! which decodes and draws it.
//!
//! Like the host side, this only ever connects to 127.0.0.1 until the
//! encrypted channel exists.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rc_input::keymap::scancode_for;
use rc_protocol::{
    read_message, write_message, InputEvent, Message, MouseButton, Role, PROTOCOL_VERSION,
};
use serde::Deserialize;

use crate::share::{lock, poll_messages, HANDSHAKE_TIMEOUT};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Packets handed to the window. The first byte is the kind, the rest is
/// little endian:
///
/// - `CONFIG`: u32 width, u32 height, u32 fps
/// - `FRAME`: u8 keyframe, u64 capture time (µs since the Unix epoch), then
///   the H.264 access unit
/// - `ENDED`: UTF-8 reason
/// - `PERMISSIONS`: u8 flags, bit 0 = the viewer may control input
pub mod packet {
    pub const CONFIG: u8 = 1;
    pub const FRAME: u8 = 2;
    pub const ENDED: u8 = 3;
    pub const PERMISSIONS: u8 = 4;
}

/// Mouse or keyboard input as the window reports it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ViewerInput {
    /// `x`/`y` are fractions of the picture, 0.0 to 1.0.
    Move { x: f64, y: f64 },
    /// `button` is the browser's numbering: 0 left, 1 middle, 2 right.
    Button {
        button: u8,
        down: bool,
        x: f64,
        y: f64,
    },
    /// Wheel movement in notches, positive away from the user / right.
    Wheel { dx: f64, dy: f64 },
    /// `code` is `KeyboardEvent.code`, the physical key.
    Key { code: String, down: bool },
}

fn fraction(value: f64) -> u16 {
    // A NaN stays NaN through `clamp` and becomes 0 in the cast.
    (value.clamp(0.0, 1.0) * 65_535.0).round() as u16
}

fn notches(value: f64) -> i16 {
    (value * 120.0)
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX))
        .round() as i16
}

impl ViewerInput {
    /// The wire event, or `None` for input that is not forwarded (an
    /// unknown key or mouse button).
    pub fn to_event(&self) -> Option<InputEvent> {
        Some(match self {
            Self::Move { x, y } => InputEvent::MouseMove {
                x: fraction(*x),
                y: fraction(*y),
            },
            Self::Button { button, down, x, y } => InputEvent::MouseButton {
                button: match button {
                    0 => MouseButton::Left,
                    1 => MouseButton::Middle,
                    2 => MouseButton::Right,
                    _ => return None,
                },
                down: *down,
                x: fraction(*x),
                y: fraction(*y),
            },
            Self::Wheel { dx, dy } => InputEvent::MouseWheel {
                dx: notches(*dx),
                dy: notches(*dy),
            },
            Self::Key { code, down } => {
                let (scancode, extended) = scancode_for(code)?;
                InputEvent::Key {
                    scancode,
                    extended,
                    down: *down,
                }
            }
        })
    }
}

fn ended(reason: &str) -> Vec<u8> {
    let mut out = vec![packet::ENDED];
    out.extend_from_slice(reason.as_bytes());
    out
}

/// Converts a message from the host into a packet for the window. `None`
/// for messages the window does not need.
fn to_packet(message: Message) -> Option<Vec<u8>> {
    match message {
        Message::VideoConfig { width, height, fps } => {
            let mut out = vec![packet::CONFIG];
            for value in [width, height, fps] {
                out.extend_from_slice(&value.to_le_bytes());
            }
            Some(out)
        }
        Message::VideoFrame {
            keyframe,
            timestamp_us,
            data,
        } => {
            let mut out = Vec::with_capacity(10 + data.len());
            out.push(packet::FRAME);
            out.push(u8::from(keyframe));
            out.extend_from_slice(&timestamp_us.to_le_bytes());
            out.extend_from_slice(&data);
            Some(out)
        }
        Message::Bye { reason } => Some(ended(&reason)),
        Message::Permissions(permissions) => {
            Some(vec![packet::PERMISSIONS, u8::from(permissions.input)])
        }
        Message::Hello { .. } | Message::KeyframeRequest | Message::Input(_) => None,
    }
}

/// Viewer half of the handshake: require a host speaking the same protocol
/// version, then announce ourselves.
pub fn viewer_handshake<S: Read + Write>(stream: &mut S) -> Result<(), String> {
    match read_message(stream).map_err(|e| e.to_string())? {
        Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Host,
        } => {}
        Message::Hello { version, role } => {
            return Err(format!(
                "unsupported peer: protocol version {version}, role {role:?}"
            ));
        }
        other => return Err(format!("expected a greeting, got {other:?}")),
    }
    let hello = Message::Hello {
        version: PROTOCOL_VERSION,
        role: Role::Viewer,
    };
    write_message(stream, &hello).map_err(|e| e.to_string())
}

struct Connection {
    stream: TcpStream,
    closing: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

#[derive(Default)]
pub struct ViewerController {
    connection: Mutex<Option<Connection>>,
}

impl ViewerController {
    /// Connects to a host on this computer and starts delivering packets to
    /// `sink` from a background thread. The last packet is always `ENDED`.
    pub fn connect<F>(&self, port: u16, sink: F) -> Result<(), String>
    where
        F: Fn(Vec<u8>) + Send + 'static,
    {
        let mut slot = lock(&self.connection);
        if slot.as_ref().is_some_and(|c| !c.thread.is_finished()) {
            return Err("Already viewing a screen".to_owned());
        }

        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
            .map_err(|e| format!("cannot connect to port {port}: {e}"))?;
        stream
            .set_nodelay(true)
            .and_then(|()| stream.set_read_timeout(Some(HANDSHAKE_TIMEOUT)))
            .map_err(|e| e.to_string())?;
        viewer_handshake(&mut &stream)?;
        let closing = Arc::new(AtomicBool::new(false));
        let thread = {
            let stream = stream.try_clone().map_err(|e| e.to_string())?;
            let closing = Arc::clone(&closing);
            thread::Builder::new()
                .name("viewer".into())
                .spawn(move || {
                    let mut goodbye = None;
                    let outcome = poll_messages(&stream, &closing, |message| match message {
                        Message::Bye { reason } => {
                            goodbye = Some(reason);
                            ControlFlow::Break(())
                        }
                        message => {
                            if let Some(packet) = to_packet(message) {
                                sink(packet);
                            }
                            ControlFlow::Continue(())
                        }
                    });
                    let reason = match (goodbye, outcome) {
                        (Some(reason), _) => reason,
                        _ if closing.load(Ordering::Relaxed) => "disconnected".to_owned(),
                        (None, Ok(())) => "the host closed the connection".to_owned(),
                        (None, Err(e)) => format!("connection lost: {e}"),
                    };
                    // Close our side so the host is not left waiting on it.
                    let _ = stream.shutdown(Shutdown::Both);
                    tracing::info!(reason, "viewing ended");
                    sink(ended(&reason));
                })
                .map_err(|e| e.to_string())?
        };

        tracing::info!(port, "viewing started");
        *slot = Some(Connection {
            stream,
            closing,
            thread,
        });
        Ok(())
    }

    /// Asks the host for a fresh keyframe after a decoding problem.
    pub fn request_keyframe(&self) -> Result<(), String> {
        match lock(&self.connection).as_ref() {
            Some(connection) => write_message(&mut &connection.stream, &Message::KeyframeRequest)
                .map_err(|e| e.to_string()),
            None => Err("Not viewing a screen".to_owned()),
        }
    }

    /// Sends mouse or keyboard input to the host. The host acts on it only
    /// if it has allowed control. Input that is not forwarded is dropped
    /// silently.
    pub fn send_input(&self, input: &ViewerInput) -> Result<(), String> {
        let Some(event) = input.to_event() else {
            return Ok(());
        };
        match lock(&self.connection).as_ref() {
            Some(connection) => write_message(&mut &connection.stream, &Message::Input(event))
                .map_err(|e| e.to_string()),
            None => Err("Not viewing a screen".to_owned()),
        }
    }

    /// Ends the session and waits for the reader thread, so the final
    /// `ENDED` packet has been delivered when this returns. Safe to call when
    /// not connected. Must not be called from the UI thread: delivering a
    /// packet may need that thread.
    pub fn disconnect(&self) {
        if let Some(thread) = self.close() {
            if thread.join().is_err() {
                tracing::error!("viewer thread panicked");
            }
        }
    }

    /// Ends the session without waiting for the reader thread. For
    /// application exit, which runs on the UI thread.
    pub fn abandon(&self) {
        drop(self.close());
    }

    fn close(&self) -> Option<JoinHandle<()>> {
        let connection = lock(&self.connection).take()?;
        connection.closing.store(true, Ordering::Relaxed);
        let bye = Message::Bye {
            reason: "viewer disconnected".to_owned(),
        };
        let _ = write_message(&mut &connection.stream, &bye);
        let _ = connection.stream.shutdown(Shutdown::Both);
        Some(connection.thread)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::share::test_support::{desktop, Duplex};
    use crate::share::{ShareController, ShareSettings};

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
        assert_eq!(to_packet(Message::KeyframeRequest), None);

        let allowed = Message::Permissions(rc_protocol::Permissions { input: true });
        assert_eq!(to_packet(allowed).unwrap(), [4, 1]);
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
    fn viewer_handshake_accepts_a_matching_host() {
        let mut peer = Duplex::with_incoming(&[Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Host,
        }]);
        viewer_handshake(&mut peer).unwrap();
        assert_eq!(
            peer.sent(),
            [Message::Hello {
                version: PROTOCOL_VERSION,
                role: Role::Viewer
            }]
        );
    }

    #[test]
    fn viewer_handshake_refuses_other_versions_roles_and_messages() {
        for incoming in [
            Message::Hello {
                version: PROTOCOL_VERSION + 1,
                role: Role::Host,
            },
            Message::Hello {
                version: PROTOCOL_VERSION,
                role: Role::Viewer,
            },
            Message::VideoConfig {
                width: 2,
                height: 2,
                fps: 1,
            },
        ] {
            let mut peer = Duplex::with_incoming(std::slice::from_ref(&incoming));
            assert!(viewer_handshake(&mut peer).is_err(), "{incoming:?}");
            assert!(peer.sent().is_empty(), "replied to {incoming:?}");
        }
    }

    #[test]
    fn connecting_to_a_closed_port_fails_and_disconnect_is_harmless() {
        let viewer = ViewerController::default();
        // Port 1 on loopback has no listener.
        assert!(viewer.connect(1, |_| {}).is_err());
        assert!(viewer.request_keyframe().is_err());
        viewer.disconnect();
    }

    /// A host that completes the handshake, runs `after`, and then keeps the
    /// connection open without doing anything else.
    fn fake_host(
        after: impl FnOnce(&mut TcpStream) + Send + 'static,
    ) -> (u16, mpsc::Receiver<TcpStream>) {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let hello = Message::Hello {
                version: PROTOCOL_VERSION,
                role: Role::Host,
            };
            write_message(&mut stream, &hello).unwrap();
            read_message(&mut stream).unwrap();
            after(&mut stream);
            let _ = tx.send(stream);
        });
        (port, rx)
    }

    #[test]
    fn disconnect_returns_promptly_from_a_silent_host() {
        let (port, host_side) = fake_host(|_| {});
        let (tx, rx) = mpsc::channel();
        let viewer = ViewerController::default();
        viewer
            .connect(port, move |packet| {
                let _ = tx.send(packet);
            })
            .unwrap();
        let _host_stream = host_side.recv_timeout(Duration::from_secs(5)).unwrap();

        let before = std::time::Instant::now();
        viewer.disconnect();
        let took = before.elapsed();
        assert!(took < Duration::from_secs(2), "disconnect took {took:?}");
        assert_eq!(rx.iter().last().unwrap(), ended("disconnected"));
    }

    #[test]
    fn the_viewer_closes_its_socket_when_the_host_says_goodbye() {
        let (port, host_side) = fake_host(|stream| {
            let bye = Message::Bye {
                reason: "sharing stopped".into(),
            };
            write_message(stream, &bye).unwrap();
        });
        let (tx, rx) = mpsc::channel();
        let viewer = ViewerController::default();
        viewer
            .connect(port, move |packet| {
                let _ = tx.send(packet);
            })
            .unwrap();

        let wait = Duration::from_secs(5);
        assert_eq!(rx.recv_timeout(wait).unwrap(), ended("sharing stopped"));
        // The host must see the connection close without the viewer being
        // told to disconnect.
        let mut host_stream = host_side.recv_timeout(wait).unwrap();
        host_stream.set_read_timeout(Some(wait)).unwrap();
        assert_eq!(host_stream.read(&mut [0u8; 16]).unwrap(), 0);
    }

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn a_viewer_receives_the_shared_screen() {
        let _desktop = desktop();
        let host = ShareController::default();
        let port = host
            .start(ShareSettings {
                display: 0,
                port: 0,
                max_fps: 30,
                bitrate_kbps: 4000,
                allow_input: false,
            })
            .unwrap()
            .port
            .unwrap();

        let (tx, rx) = mpsc::channel();
        let viewer = ViewerController::default();
        viewer
            .connect(port, move |packet| {
                let _ = tx.send(packet);
            })
            .unwrap();
        assert!(viewer.connect(port, |_| {}).is_err(), "second connect");

        let wait = Duration::from_secs(10);
        // The host says what the viewer may do before it sends any picture.
        let permissions = rx.recv_timeout(wait).expect("no permissions packet");
        assert_eq!(permissions, [packet::PERMISSIONS, 0]);
        let config = rx.recv_timeout(wait).expect("no config packet");
        assert_eq!(config[0], packet::CONFIG);
        let width = u32::from_le_bytes(config[1..5].try_into().unwrap());
        assert!(width > 0 && width % 2 == 0);

        let frame = rx.recv_timeout(wait).expect("no frame packet");
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
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while host.status().viewer_connected {
            assert!(
                std::time::Instant::now() < deadline,
                "host still shows a viewer"
            );
            thread::sleep(Duration::from_millis(50));
        }
        assert!(host.status().active);
        host.stop();
    }
}
