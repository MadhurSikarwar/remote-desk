//! Talking to a rendezvous server.
//!
//! A device that is sharing keeps a control connection to the server open so
//! that viewers can ask for it. A viewer opens one briefly to ask for a
//! device by its ID, then tries a direct connection and, failing that, asks
//! the server to relay.
//!
//! Every control channel is encrypted, and the server must present the key
//! whose fingerprint the user configured: a device never talks to a server
//! it has not been told to trust. The server cannot read a session (the
//! devices run their own handshake through it), and the viewer checks the
//! key it reaches against the ID it asked for, so the server cannot
//! substitute another device either.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use rc_core::config::{NetworkConfig, TransportPolicy};
use rc_crypto::channel::{Established, Handshake, Opener, Sealer, Side};
use rc_crypto::{fingerprint, fingerprint_bytes, parse_fingerprint, Identity};
use rc_rendezvous::{
    relay_hello, ClientMessage, DeviceId, ServerMessage, Token, RENDEZVOUS_VERSION,
};

use crate::secure::lock;
use crate::viewer::resolve_address;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DIRECT_TIMEOUT: Duration = Duration::from_secs(3);
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
const PING_EVERY: Duration = Duration::from_secs(25);
const POLL: Duration = Duration::from_millis(500);
const FIRST_RETRY: Duration = Duration::from_secs(1);
const LONGEST_RETRY: Duration = Duration::from_secs(30);

/// The server a device is configured to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerLink {
    /// `host:port` as configured.
    pub address: String,
    /// The fingerprint the server must present.
    pub fingerprint: [u8; 8],
}

impl ServerLink {
    /// The configured server, or `None` when there is none.
    pub fn from_config(network: &NetworkConfig) -> Result<Option<Self>, String> {
        let Some(address) = network.rendezvous_servers.first() else {
            return Ok(None);
        };
        let fingerprint = parse_fingerprint(&network.server_fingerprint)
            .ok_or("The server's key fingerprint in the settings is not valid")?;
        Ok(Some(Self {
            address: address.clone(),
            fingerprint,
        }))
    }

    fn connect(&self) -> Result<TcpStream, String> {
        let address = resolve_address(&self.address)?;
        let stream = TcpStream::connect_timeout(&address, CONNECT_TIMEOUT)
            .map_err(|e| format!("cannot reach the server at {address}: {e}"))?;
        stream.set_nodelay(true).map_err(|e| e.to_string())?;
        Ok(stream)
    }
}

/// An encrypted control channel to the server.
struct Channel {
    stream: TcpStream,
    sealer: Sealer,
    opener: Opener,
}

impl Channel {
    fn open(link: &ServerLink, identity: &Identity) -> Result<Self, String> {
        let mut stream = link.connect()?;
        stream
            .set_read_timeout(Some(CONNECT_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(CONNECT_TIMEOUT)))
            .map_err(|e| e.to_string())?;
        let Established {
            remote_public,
            sealer,
            opener,
            ..
        } = Handshake::run(identity, Side::Initiator, &mut stream)
            .map_err(|e| format!("could not set up a secure connection to the server: {e}"))?;
        if fingerprint_bytes(&remote_public) != link.fingerprint {
            return Err(format!(
                "The server's key ({}) is not the one in the settings; refusing to use it",
                fingerprint(&remote_public)
            ));
        }
        Ok(Self {
            stream,
            sealer,
            opener,
        })
    }

    fn send(&mut self, message: &ClientMessage) -> Result<(), String> {
        let sealed = self
            .sealer
            .seal(&message.encode())
            .map_err(|e| e.to_string())?;
        self.stream
            .write_all(&sealed)
            .map_err(|e| format!("lost the server: {e}"))
    }

    /// The next message from the server, or `None` if nothing came within
    /// `wait`.
    fn receive(&mut self, wait: Duration) -> Result<Option<ServerMessage>, String> {
        self.stream
            .set_read_timeout(Some(wait))
            .map_err(|e| e.to_string())?;
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(plain) = self.opener.next_plaintext().map_err(|e| e.to_string())? {
                return ServerMessage::decode(&plain)
                    .map(Some)
                    .map_err(|e| format!("the server sent something unexpected: {e}"));
            }
            match self.stream.read(&mut chunk) {
                Ok(0) => return Err("the server closed the connection".to_owned()),
                Ok(n) => self.opener.push(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    return Ok(None)
                }
                Err(e) => return Err(format!("lost the server: {e}")),
            }
        }
    }

    /// Waits for a message that `pick` accepts, up to [`REPLY_TIMEOUT`].
    /// A server error is returned as the error.
    fn expect<T>(
        &mut self,
        what: &str,
        mut pick: impl FnMut(ServerMessage) -> Option<T>,
    ) -> Result<T, String> {
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(format!("the server did not answer ({what})"));
            }
            match self.receive(left)? {
                Some(ServerMessage::Error { reason }) => {
                    return Err(format!("the server: {reason}"))
                }
                Some(message) => {
                    if let Some(found) = pick(message) {
                        return Ok(found);
                    }
                }
                None => {}
            }
        }
    }
}

/// Opens a relay connection: a new connection to the server that begins
/// with the relay marker and the token.
fn open_relay(link: &ServerLink, token: &Token) -> Result<TcpStream, String> {
    let mut stream = link.connect()?;
    stream
        .write_all(&relay_hello(token))
        .map_err(|e| format!("lost the server: {e}"))?;
    Ok(stream)
}

// ---- the sharing device ------------------------------------------------------

/// What a sharing device registers with.
pub struct Registration<'a> {
    pub link: &'a ServerLink,
    pub identity: &'a Identity,
    /// Port viewers should try directly.
    pub port: u16,
    pub name: &'a str,
}

/// Keeps a sharing device registered with the server until `stop`, and
/// hands relayed connections to `on_relay`. Reconnects with a growing pause
/// when the server goes away. `status` always says how it is going.
pub fn keep_registered(
    registration: &Registration<'_>,
    stop: &AtomicBool,
    status: &Mutex<Option<String>>,
    on_relay: &(dyn Fn(TcpStream) + Sync),
) {
    let say = |text: String| *lock(status) = Some(text);
    say("Connecting to the server…".to_owned());
    let mut pause = FIRST_RETRY;
    while !stop.load(Ordering::Relaxed) {
        match registered_session(registration, stop, status, on_relay) {
            Ok(()) => pause = FIRST_RETRY,
            Err(error) => {
                tracing::warn!(%error, "server connection ended");
                say(format!("Server unavailable: {error}. Trying again…"));
            }
        }
        let until = Instant::now() + pause;
        while Instant::now() < until && !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(100));
        }
        pause = (pause * 2).min(LONGEST_RETRY);
    }
    *lock(status) = None;
}

/// One connection's worth of registration. `Ok` when stopped on request.
fn registered_session(
    registration: &Registration<'_>,
    stop: &AtomicBool,
    status: &Mutex<Option<String>>,
    on_relay: &(dyn Fn(TcpStream) + Sync),
) -> Result<(), String> {
    let mut channel = Channel::open(registration.link, registration.identity)?;
    let lan = match rc_net::local_address() {
        Some(std::net::IpAddr::V4(v4)) => vec![v4],
        _ => Vec::<Ipv4Addr>::new(),
    };
    channel.send(&ClientMessage::Register {
        version: RENDEZVOUS_VERSION,
        port: registration.port,
        lan,
        name: registration.name.to_owned(),
    })?;
    let address = channel.expect("registering", |m| match m {
        ServerMessage::Registered { address } => Some(address),
        _ => None,
    })?;
    tracing::info!(%address, server = registration.link.address, "registered with the server");
    *lock(status) = Some(format!(
        "Registered with {} (it sees this device as {address})",
        registration.link.address
    ));

    let mut last_ping = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        if last_ping.elapsed() >= PING_EVERY {
            channel.send(&ClientMessage::Ping)?;
            last_ping = Instant::now();
        }
        match channel.receive(POLL)? {
            None | Some(ServerMessage::Pong) => {}
            Some(ServerMessage::Incoming {
                request_id,
                viewer,
                name,
            }) => tracing::info!(
                request_id,
                viewer = fingerprint_text(&viewer),
                name,
                "a viewer asked for this device through the server"
            ),
            Some(ServerMessage::RelayRequest { request_id, token }) => {
                tracing::info!(request_id, "opening a relay connection for a viewer");
                match open_relay(registration.link, &token) {
                    Ok(stream) => on_relay(stream),
                    Err(error) => tracing::warn!(%error, "relay connection failed"),
                }
            }
            Some(ServerMessage::Replaced) => {
                return Err("another device is registered with this device's key".to_owned())
            }
            Some(ServerMessage::Error { reason }) => return Err(format!("the server: {reason}")),
            Some(other) => tracing::debug!(?other, "ignoring an unexpected server message"),
        }
    }
    Ok(())
}

fn fingerprint_text(id: &DeviceId) -> String {
    id.iter().map(|b| format!("{b:02X}")).collect()
}

// ---- the viewer ------------------------------------------------------------------

/// How a viewer reached a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Direct,
    Relayed,
}

impl Route {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Direct => "directly",
            Self::Relayed => "through the server",
        }
    }
}

/// Reaches the device `target` by way of the server.
///
/// Direct connections are tried first (addresses on the viewer's own network
/// only when the server says both are on one network), then a relay. Each
/// way that produces a connection is handed to `attempt`, which completes
/// the handshake and checks that the device is the one asked for; if it
/// refuses, the next way is tried. Only when nothing works is there an
/// error, which lists what was tried.
pub fn reach<T>(
    link: &ServerLink,
    identity: &Identity,
    target: DeviceId,
    viewer_name: &str,
    policy: TransportPolicy,
    mut attempt: impl FnMut(TcpStream, Route) -> Result<T, String>,
) -> Result<(T, Route), String> {
    let mut channel = Channel::open(link, identity)?;
    channel.send(&ClientMessage::Connect {
        target,
        name: viewer_name.to_owned(),
    })?;
    let peer = channel.expect("looking up the device", |m| match m {
        ServerMessage::Peer {
            request_id,
            address,
            port,
            lan,
            same_network,
        } => Some(Ok((request_id, address, port, lan, same_network))),
        ServerMessage::NotFound { .. } => Some(Err(
            "No device with that ID is registered with the server. It must be sharing, with connections from other computers allowed and this server set"
                .to_owned(),
        )),
        _ => None,
    })??;
    let (request_id, address, port, lan, same_network) = peer;

    let mut tried: Vec<String> = Vec::new();
    if policy != TransportPolicy::RelayOnly {
        let mut candidates: Vec<SocketAddr> = Vec::new();
        if same_network {
            candidates.extend(lan.iter().map(|ip| SocketAddr::from((*ip, port))));
        }
        candidates.push(SocketAddr::from((address, port)));
        candidates.dedup();
        for candidate in candidates {
            let outcome = TcpStream::connect_timeout(&candidate, DIRECT_TIMEOUT)
                .map_err(|e| e.to_string())
                .and_then(|stream| {
                    stream.set_nodelay(true).map_err(|e| e.to_string())?;
                    attempt(stream, Route::Direct)
                });
            match outcome {
                Ok(done) => return Ok((done, Route::Direct)),
                Err(error) => tried.push(format!("directly to {candidate}: {error}")),
            }
        }
    }
    if policy == TransportPolicy::DirectOnly {
        return Err(format!(
            "No direct connection was possible ({}), and relaying is switched off in the settings",
            tried.join("; ")
        ));
    }

    channel.send(&ClientMessage::UseRelay { request_id })?;
    let token = channel.expect("asking for a relay", |m| match m {
        ServerMessage::RelayToken { token, .. } => Some(token),
        _ => None,
    })?;
    let stream = open_relay(link, &token)?;
    match attempt(stream, Route::Relayed) {
        Ok(done) => Ok((done, Route::Relayed)),
        Err(error) => {
            tried.push(format!("through the server: {error}"));
            Err(format!("Could not reach the device ({})", tried.join("; ")))
        }
    }
}

#[cfg(test)]
mod tests;
