//! Viewer side: connects to a host and forwards its video to the window,
//! which decodes and draws it.
//!
//! Like the host side, this only ever connects to 127.0.0.1 until the
//! encrypted channel exists.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rc_protocol::{read_message, write_message, Message, Role, PROTOCOL_VERSION};

use crate::share::{lock, HANDSHAKE_TIMEOUT};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Packets handed to the window. The first byte is the kind, the rest is
/// little endian:
///
/// - `CONFIG`: u32 width, u32 height, u32 fps
/// - `FRAME`: u8 keyframe, u64 capture time (µs since the Unix epoch), then
///   the H.264 access unit
/// - `ENDED`: UTF-8 reason
pub mod packet {
    pub const CONFIG: u8 = 1;
    pub const FRAME: u8 = 2;
    pub const ENDED: u8 = 3;
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
        Message::Hello { .. } | Message::KeyframeRequest => None,
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
        stream.set_read_timeout(None).map_err(|e| e.to_string())?;

        let closing = Arc::new(AtomicBool::new(false));
        let thread = {
            let stream = stream.try_clone().map_err(|e| e.to_string())?;
            let closing = Arc::clone(&closing);
            thread::Builder::new()
                .name("viewer".into())
                .spawn(move || {
                    let reason = loop {
                        match read_message(&mut &stream) {
                            Ok(Message::Bye { reason }) => break reason,
                            Ok(message) => {
                                if let Some(packet) = to_packet(message) {
                                    sink(packet);
                                }
                            }
                            Err(_) if closing.load(Ordering::Relaxed) => {
                                break "disconnected".to_owned()
                            }
                            Err(e) => break format!("connection lost: {e}"),
                        }
                    };
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

    /// Ends the session. Safe to call when not connected.
    pub fn disconnect(&self) {
        let Some(connection) = lock(&self.connection).take() else {
            return;
        };
        connection.closing.store(true, Ordering::Relaxed);
        let bye = Message::Bye {
            reason: "viewer disconnected".to_owned(),
        };
        let _ = write_message(&mut &connection.stream, &bye);
        let _ = connection.stream.shutdown(Shutdown::Both);
        if connection.thread.join().is_err() {
            tracing::error!("viewer thread panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;
    use crate::share::test_support::Duplex;
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

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn a_viewer_receives_the_shared_screen() {
        let host = ShareController::default();
        let port = host
            .start(ShareSettings {
                display: 0,
                port: 0,
                max_fps: 30,
                bitrate_kbps: 4000,
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
