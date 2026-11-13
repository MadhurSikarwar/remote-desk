//! Viewer side: connects to a host and forwards its video to the window,
//! which decodes and draws it.
//!
//! Every connection starts with an encrypted handshake; the session itself
//! begins only once the person at the host has allowed it. The host may be
//! on this computer or any other that can be reached.

use std::net::{Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use rc_core::config::TransportPolicy;
use rc_crypto::channel::Side;
use rc_crypto::{fingerprint, fingerprint_bytes, parse_fingerprint, Identity, PublicKey};
use rc_input::keymap::scancode_for;
use rc_protocol::{InputEvent, Message, MouseButton, Role};
use rc_rendezvous::DeviceId;
use serde::{Deserialize, Serialize};

use crate::rendezvous::{reach, Route, ServerLink};
use crate::secure::{establish, exchange_hello, lock, Peer, SecureReader, SecureWriter, Security};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// Packets handed to the window. The first byte is the kind, the rest is
/// little endian:
///
/// - `CONFIG`: u32 width, u32 height, u32 fps
/// - `FRAME`: u8 keyframe, u64 capture time (µs since the Unix epoch), then
///   the H.264 access unit
/// - `ENDED`: UTF-8 reason
/// - `PERMISSIONS`: u8 flags, bit 0 = the viewer may control input
/// - `ACCEPTED`: nothing; the host allowed the session
pub mod packet {
    pub const ACCEPTED: u8 = 5;
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
        Message::Permissions(permissions) => Some(vec![packet::PERMISSIONS, permissions.bits()]),
        Message::Accepted => Some(vec![packet::ACCEPTED]),
        Message::Hello { .. }
        | Message::KeyframeRequest
        | Message::Input(_)
        | Message::ClipboardText(_)
        | Message::FileOffer { .. }
        | Message::FileAnswer { .. }
        | Message::FileChunk { .. }
        | Message::FileEnd { .. }
        | Message::FileDone { .. }
        | Message::FileCancel { .. } => None,
    }
}

/// Turns what the user typed into an address to connect to.
///
/// A bare port number means a host on this computer. Otherwise `host:port`,
/// where `host` is an IPv4 address or a name; names are looked up, and an
/// IPv4 result is preferred because that is what hosts listen on.
pub fn resolve_address(text: &str) -> Result<SocketAddr, String> {
    let text = text.trim();
    let bad_port = || "The port must be a number from 1 to 65535".to_owned();
    if text.is_empty() {
        return Err("Enter the address shown by the sharing device".to_owned());
    }
    if text.bytes().all(|b| b.is_ascii_digit()) {
        let port: u16 = text.parse().map_err(|_| bad_port())?;
        if port == 0 {
            return Err(bad_port());
        }
        return Ok(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
    }
    let (host, port) = text
        .rsplit_once(':')
        .ok_or("Enter the address as host:port, for example 192.168.1.20:51000")?;
    if host.is_empty() || host.chars().any(char::is_whitespace) {
        return Err("The host part of the address is not valid".to_owned());
    }
    match port.parse::<u16>() {
        Ok(port) if port != 0 => {}
        _ => return Err(bad_port()),
    }
    let mut found = text
        .to_socket_addrs()
        .map_err(|e| format!("Cannot find {host}: {e}"))?
        .collect::<Vec<_>>();
    found.sort_by_key(|address| !address.is_ipv4());
    found
        .into_iter()
        .next()
        .ok_or_else(|| format!("Cannot find {host}"))
}

/// The host a viewer has reached, for the person to check before the host
/// decides.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostInfo {
    /// What the host calls itself. Not proof of anything.
    pub name: String,
    /// Fingerprint of the host's key, which the handshake proved.
    pub fingerprint: String,
    /// Must match the code on the host's screen.
    pub auth_code: String,
    /// How the host was reached: directly, or through the server.
    pub route: String,
}

/// What to do besides connecting to a plain address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectOptions {
    /// The key a discovery reply announced for the device at the address.
    pub expected_key: Option<PublicKey>,
    /// The server to reach devices by ID through.
    pub server: Option<ServerLink>,
    /// Whether to connect directly, through the server, or either.
    pub policy: TransportPolicy,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            expected_key: None,
            server: None,
            policy: TransportPolicy::Auto,
        }
    }
}

/// What the user asked to connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `host:port`, or a bare port on this computer.
    Address(String),
    /// A device's ID: its 16-digit key fingerprint.
    Device(DeviceId),
}

/// Tells a device ID from an address. An ID is 16 hex digits, optionally
/// grouped; anything with a `:` is an address, and so is a bare port.
pub fn parse_target(text: &str) -> Target {
    let text = text.trim();
    if !text.contains(':') {
        if let Some(id) = parse_fingerprint(text) {
            return Target::Device(id);
        }
    }
    Target::Address(text.to_owned())
}

/// What a connection must prove about the device at the other end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pin {
    Nothing,
    Key(PublicKey),
    Id(DeviceId),
}

/// Completes the encrypted handshake on `stream` and checks the device is
/// the one asked for.
fn handshake_checked(
    stream: &TcpStream,
    identity: &Identity,
    pin: Pin,
    place: &str,
) -> Result<(SecureWriter, SecureReader, Peer), String> {
    let (writer, reader, peer) = establish(stream, identity, Side::Initiator)
        .map_err(|e| format!("could not set up a secure connection: {e}"))?;
    let mismatch = match pin {
        Pin::Nothing => None,
        Pin::Key(expected) => (expected != peer.public).then(|| {
            format!(
                "The device at {place} is not the one you picked: it has key {} but {} was expected",
                peer.fingerprint,
                fingerprint(&expected)
            )
        }),
        Pin::Id(expected) => (expected != fingerprint_bytes(&peer.public)).then(|| {
            format!(
                "The device reached {place} is not the one with that ID: it has key {}",
                peer.fingerprint
            )
        }),
    };
    if let Some(message) = mismatch {
        writer.shutdown();
        return Err(message);
    }
    Ok((writer, reader, peer))
}

struct Connection {
    writer: Arc<SecureWriter>,
    closing: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

pub struct ViewerController {
    security: Option<Arc<Security>>,
    connection: Mutex<Option<Connection>>,
}

impl ViewerController {
    /// `security` is `None` when this device's keys could not be loaded;
    /// connecting is then refused.
    pub fn new(security: Option<Arc<Security>>) -> Self {
        Self {
            security,
            connection: Mutex::new(None),
        }
    }

    /// Connects to `address` (see [`resolve_address`]), completes the
    /// encrypted handshake and returns who answered. Packets then arrive on
    /// `sink` from a background thread: `ACCEPTED` once the host allows the
    /// session, and always `ENDED` last.
    ///
    /// With `expected_key` — the key a discovery reply announced — the
    /// connection is dropped if the device that answers holds a different
    /// one, so a viewer does not end up with whoever happens to be at an
    /// address.
    #[cfg(test)]
    pub fn connect<F>(
        &self,
        address: &str,
        expected_key: Option<PublicKey>,
        device_name: &str,
        sink: F,
    ) -> Result<HostInfo, String>
    where
        F: Fn(Vec<u8>) + Send + 'static,
    {
        let options = ConnectOptions {
            expected_key,
            ..ConnectOptions::default()
        };
        self.connect_with(address, &options, device_name, sink)
    }

    /// As [`connect`](Self::connect), and `target` may also be a device ID
    /// (see [`parse_target`]), which is reached through the server in
    /// `options`: directly if possible, relayed if not, as the policy
    /// allows.
    pub fn connect_with<F>(
        &self,
        target: &str,
        options: &ConnectOptions,
        device_name: &str,
        sink: F,
    ) -> Result<HostInfo, String>
    where
        F: Fn(Vec<u8>) + Send + 'static,
    {
        let security = self
            .security
            .clone()
            .ok_or("This device's key could not be loaded, so it cannot connect")?;
        let mut slot = lock(&self.connection);
        if slot.as_ref().is_some_and(|c| !c.thread.is_finished()) {
            return Err("Already viewing a screen".to_owned());
        }

        let (label, (writer, mut reader, peer), route) = match parse_target(target) {
            Target::Address(text) => {
                let address = resolve_address(&text)?;
                let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
                    .map_err(|e| format!("cannot connect to {address}: {e}"))?;
                let pin = options.expected_key.map_or(Pin::Nothing, Pin::Key);
                let session =
                    handshake_checked(&stream, security.identity(), pin, &address.to_string())?;
                (address.to_string(), session, Route::Direct)
            }
            Target::Device(id) => {
                let link = options.server.as_ref().ok_or(
                    "A device ID needs a server. Set one in the settings (Network), with its key fingerprint",
                )?;
                let (session, route) = reach(
                    link,
                    security.identity(),
                    id,
                    device_name,
                    options.policy,
                    |stream, route| {
                        handshake_checked(
                            &stream,
                            security.identity(),
                            Pin::Id(id),
                            route.describe(),
                        )
                    },
                )?;
                (format!("device {}", target.trim()), session, route)
            }
        };
        let name = exchange_hello(&writer, &mut reader, Role::Viewer, device_name)?;
        let host = HostInfo {
            name,
            fingerprint: peer.fingerprint,
            auth_code: peer.auth_code,
            route: route.describe().to_owned(),
        };

        let writer = Arc::new(writer);
        let closing = Arc::new(AtomicBool::new(false));
        let thread = {
            let writer = Arc::clone(&writer);
            let closing = Arc::clone(&closing);
            thread::Builder::new()
                .name("viewer".into())
                .spawn(move || {
                    let mut goodbye = None;
                    let outcome = reader.poll(&closing, |message| match message {
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
                    writer.shutdown();
                    tracing::info!(reason, "viewing ended");
                    sink(ended(&reason));
                })
                .map_err(|e| e.to_string())?
        };

        tracing::info!(
            target = label,
            route = host.route,
            host = host.name,
            fingerprint = host.fingerprint,
            "connected; waiting for the host to allow viewing"
        );
        *slot = Some(Connection {
            writer,
            closing,
            thread,
        });
        Ok(host)
    }

    fn send(&self, message: &Message) -> Result<(), String> {
        match lock(&self.connection).as_ref() {
            Some(connection) => connection.writer.send(message),
            None => Err("Not viewing a screen".to_owned()),
        }
    }

    /// Asks the host for a fresh keyframe after a decoding problem.
    pub fn request_keyframe(&self) -> Result<(), String> {
        self.send(&Message::KeyframeRequest)
    }

    /// Sends mouse or keyboard input to the host. The host acts on it only
    /// if it has allowed control. Input that is not forwarded is dropped
    /// silently.
    pub fn send_input(&self, input: &ViewerInput) -> Result<(), String> {
        match input.to_event() {
            Some(event) => self.send(&Message::Input(event)),
            None => Ok(()),
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
        let _ = connection.writer.send(&Message::Bye {
            reason: "viewer disconnected".to_owned(),
        });
        connection.writer.shutdown();
        Some(connection.thread)
    }
}

#[cfg(test)]
mod tests;
