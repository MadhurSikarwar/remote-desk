//! This device's keys, and protocol messages over an encrypted connection.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rc_crypto::channel::{Handshake, Opener, Sealer, Side};
use rc_crypto::{fingerprint, Identity, PublicKey, TrustStore};
use rc_protocol::{write_message, Message, MessageDecoder, Role, PROTOCOL_VERSION};

pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// The most a new connection may take, from the first byte to the end of the
/// greeting, however slowly its peer sends. A socket timeout alone is not
/// this: it starts again with every byte that arrives, so a peer sending one
/// byte at a time could hold a connection (and one of the few places for
/// them) for hours.
pub const SETUP_DEADLINE: Duration = Duration::from_secs(10);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a reader wakes up to check whether it should stop.
const READ_POLL: Duration = Duration::from_millis(200);

const IDENTITY_FILE: &str = "identity.key";
const TRUST_FILE: &str = "trusted.toml";
const MAX_NAME_CHARS: usize = 64;

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A device name as it may be shown to a person: printable and bounded.
/// Names come from the other device and are never trusted for anything.
pub fn display_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_NAME_CHARS)
        .collect();
    match cleaned.trim() {
        "" => "Unnamed device".to_owned(),
        trimmed => trimmed.to_owned(),
    }
}

/// This device's identity and the devices its user has approved.
pub struct Security {
    identity: Identity,
    trust: Mutex<TrustStore>,
}

impl Security {
    /// Loads both from `config_dir`, creating the identity on first use.
    pub fn load(config_dir: &Path) -> Result<Self, String> {
        let identity =
            Identity::load_or_create(&config_dir.join(IDENTITY_FILE)).map_err(|e| e.to_string())?;
        let trust = TrustStore::load(&config_dir.join(TRUST_FILE)).map_err(|e| e.to_string())?;
        Ok(Self {
            identity,
            trust: Mutex::new(trust),
        })
    }

    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    pub fn fingerprint(&self) -> String {
        self.identity.fingerprint()
    }

    pub fn trust(&self) -> MutexGuard<'_, TrustStore> {
        lock(&self.trust)
    }
}

/// The device at the other end, as proven by the handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub public: PublicKey,
    pub fingerprint: String,
    /// The same six digits on both devices unless someone is in between.
    pub auth_code: String,
}

/// Sends messages, encrypted. Shareable between threads; messages go out
/// whole and in the order `send` was called.
pub struct SecureWriter {
    stream: TcpStream,
    sealer: Mutex<Sealer>,
}

impl SecureWriter {
    pub fn send(&self, message: &Message) -> Result<(), String> {
        let mut framed = Vec::new();
        write_message(&mut framed, message).map_err(|e| e.to_string())?;
        // Held across the write: records must reach the wire in the order
        // they were encrypted.
        let mut sealer = lock(&self.sealer);
        let sealed = sealer.seal(&framed).map_err(|e| e.to_string())?;
        (&self.stream).write_all(&sealed).map_err(|e| e.to_string())
    }

    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

/// Receives messages, decrypting and reassembling them.
pub struct SecureReader {
    stream: TcpStream,
    opener: Opener,
    decoder: MessageDecoder,
    chunk: Vec<u8>,
    /// While the greeting is still to come: when setup must be over.
    deadline: Option<Instant>,
}

/// A connection whose every read and write must finish by a fixed time.
struct Deadline<'a> {
    stream: &'a TcpStream,
    until: Instant,
}

impl Deadline<'_> {
    /// Time left, or the error that ends the attempt.
    fn left(&self) -> io::Result<Duration> {
        let left = self.until.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the other device took too long to set up the connection",
            ));
        }
        Ok(left)
    }
}

impl Read for Deadline<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.left()?))?;
        (&*self.stream).read(buf)
    }
}

impl Write for Deadline<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.left()?))?;
        (&*self.stream).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self.stream).flush()
    }
}

impl SecureReader {
    /// Reads once from the connection. `Ok(false)` means the read timed out.
    fn fill(&mut self) -> Result<bool, String> {
        match (&self.stream).read(&mut self.chunk) {
            Ok(0) => Err("the connection was closed".to_owned()),
            Ok(n) => {
                self.opener.push(&self.chunk[..n]);
                while let Some(plain) = self.opener.next_plaintext().map_err(|e| e.to_string())? {
                    self.decoder.push(&plain);
                }
                Ok(true)
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                Ok(false)
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// Waits for one message; running out of time is an error. For the
    /// opening exchange, where the peer must answer promptly: the message
    /// must be complete within [`HANDSHAKE_TIMEOUT`] of this call and within
    /// the setup deadline, however slowly it is sent.
    pub fn read_message(&mut self) -> Result<Message, String> {
        let mut until = Instant::now() + HANDSHAKE_TIMEOUT;
        if let Some(deadline) = self.deadline {
            until = until.min(deadline);
        }
        loop {
            if let Some(message) = self.decoder.next_message().map_err(|e| e.to_string())? {
                return Ok(message);
            }
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err("the other device did not answer in time".to_owned());
            }
            self.stream
                .set_read_timeout(Some(left))
                .map_err(|e| e.to_string())?;
            if !self.fill()? && Instant::now() >= until {
                return Err("the other device did not answer in time".to_owned());
            }
        }
    }

    /// Passes messages to `handle` until it breaks, `stop` is set, or the
    /// connection ends (an error).
    ///
    /// The read wakes every [`READ_POLL`] to look at `stop`: on Windows a
    /// blocking read is not ended by shutting the socket down from another
    /// thread.
    pub fn poll(
        &mut self,
        stop: &AtomicBool,
        mut handle: impl FnMut(Message) -> ControlFlow<()>,
    ) -> Result<(), String> {
        self.stream
            .set_read_timeout(Some(READ_POLL))
            .map_err(|e| e.to_string())?;
        loop {
            while let Some(message) = self.decoder.next_message().map_err(|e| e.to_string())? {
                if handle(message).is_break() {
                    return Ok(());
                }
            }
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            self.fill()?;
        }
    }
}

/// Performs the handshake on a fresh connection. From here on nothing is
/// sent in the clear. Together with the greeting that [`exchange_hello`]
/// carries out next, it must be over within [`SETUP_DEADLINE`].
pub fn establish(
    stream: &TcpStream,
    identity: &Identity,
    side: Side,
) -> Result<(SecureWriter, SecureReader, Peer), String> {
    establish_within(stream, identity, side, SETUP_DEADLINE)
}

/// As [`establish`], with the whole of the setup given `limit`.
pub fn establish_within(
    stream: &TcpStream,
    identity: &Identity,
    side: Side,
    limit: Duration,
) -> Result<(SecureWriter, SecureReader, Peer), String> {
    let until = Instant::now() + limit;
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    let mut timed = Deadline { stream, until };
    let session = Handshake::run(identity, side, &mut timed).map_err(|e| e.to_string())?;
    // Writes after setup (the greeting, then the session) use the ordinary
    // write timeout again.
    stream
        .set_write_timeout(Some(WRITE_TIMEOUT))
        .map_err(|e| e.to_string())?;
    let clone = || stream.try_clone().map_err(|e| e.to_string());

    let peer = Peer {
        public: session.remote_public,
        fingerprint: fingerprint(&session.remote_public),
        auth_code: session.auth_code,
    };
    let writer = SecureWriter {
        stream: clone()?,
        sealer: Mutex::new(session.sealer),
    };
    let reader = SecureReader {
        stream: clone()?,
        opener: session.opener,
        decoder: MessageDecoder::default(),
        chunk: vec![0u8; 64 * 1024],
        deadline: Some(until),
    };
    Ok((writer, reader, peer))
}

/// Exchanges greetings and returns the peer's name. Fails, telling the peer
/// why, unless it speaks the same protocol version in the opposite role.
pub fn exchange_hello(
    writer: &SecureWriter,
    reader: &mut SecureReader,
    my_role: Role,
    my_name: &str,
) -> Result<String, String> {
    let result = greet(writer, reader, my_role, my_name);
    // Setup is over, one way or the other: the session reads as it likes.
    reader.deadline = None;
    result
}

fn greet(
    writer: &SecureWriter,
    reader: &mut SecureReader,
    my_role: Role,
    my_name: &str,
) -> Result<String, String> {
    writer.send(&Message::Hello {
        version: PROTOCOL_VERSION,
        role: my_role,
        name: my_name.to_owned(),
    })?;
    let expected = match my_role {
        Role::Host => Role::Viewer,
        Role::Viewer => Role::Host,
    };
    match reader.read_message()? {
        Message::Hello {
            version: PROTOCOL_VERSION,
            role,
            name,
        } if role == expected => Ok(display_name(&name)),
        Message::Hello { version, role, .. } => {
            let reason = format!("unsupported peer: protocol version {version}, role {role:?}");
            let _ = writer.send(&Message::Bye {
                reason: reason.clone(),
            });
            Err(reason)
        }
        Message::Bye { reason } => Err(reason),
        other => Err(format!("expected a greeting, got {other:?}")),
    }
}

#[cfg(test)]
pub mod test_support {
    use std::net::{Ipv4Addr, TcpListener};
    use std::sync::{Arc, Mutex, MutexGuard};

    use super::*;

    /// Tests that capture the screen or drive the real mouse and keyboard
    /// share one desktop, so they take turns.
    pub fn desktop() -> MutexGuard<'static, ()> {
        static DESKTOP: Mutex<()> = Mutex::new(());
        DESKTOP.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Keys and an empty trust store in a temporary directory.
    pub fn security() -> (Arc<Security>, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (Arc::new(Security::load(dir.path()).unwrap()), dir)
    }

    /// Two ends of a loopback connection.
    pub fn socket_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    /// A secure channel between two fresh identities, with the handshake
    /// done: `(initiator, responder)`.
    #[allow(clippy::type_complexity)]
    pub fn secure_pair() -> (
        (SecureWriter, SecureReader, Peer),
        (SecureWriter, SecureReader, Peer),
    ) {
        let (client, server) = socket_pair();
        let responder = std::thread::spawn(move || {
            establish(&server, &Identity::generate().unwrap(), Side::Responder).unwrap()
        });
        let initiator =
            establish(&client, &Identity::generate().unwrap(), Side::Initiator).unwrap();
        (initiator, responder.join().unwrap())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use std::time::Instant;

    use super::test_support::{secure_pair, socket_pair};
    use super::*;

    /// Sends `first`, then one byte every `gap` until the other end goes
    /// away: a peer that is slow on purpose.
    fn trickle(mut stream: TcpStream, first: Vec<u8>, gap: Duration) {
        std::thread::spawn(move || {
            if stream.write_all(&first).is_err() {
                return;
            }
            while stream.write_all(&[0u8]).is_ok() {
                std::thread::sleep(gap);
            }
        });
    }

    #[test]
    fn a_peer_that_trickles_the_handshake_cannot_stretch_it() {
        let (client, server) = socket_pair();
        // A record header claiming 65535 bytes, then one byte at a time: the
        // socket timeout alone would start again with every byte.
        trickle(server, vec![0xFF, 0xFF], Duration::from_millis(100));
        let started = Instant::now();
        let result = establish_within(
            &client,
            &Identity::generate().unwrap(),
            Side::Initiator,
            Duration::from_secs(1),
        );
        let took = started.elapsed();
        assert!(result.is_err(), "the handshake succeeded");
        assert!(
            took >= Duration::from_millis(900) && took < Duration::from_millis(2500),
            "the deadline was 1 s but the handshake took {took:?}"
        );
    }

    #[test]
    fn a_peer_that_trickles_the_greeting_cannot_stretch_it() {
        let (client, server) = socket_pair();
        let responder = std::thread::spawn(move || {
            let (_writer, _reader, _) = establish_within(
                &server,
                &Identity::generate().unwrap(),
                Side::Responder,
                Duration::from_secs(5),
            )
            .unwrap();
            // The handshake is done; the greeting never completes: a record
            // header claiming a full-size record, then one byte at a time.
            trickle(
                server.try_clone().unwrap(),
                vec![0xFF, 0xFF],
                Duration::from_millis(100),
            );
            std::thread::sleep(Duration::from_secs(4));
            drop(server);
        });
        let started = Instant::now();
        let (writer, mut reader, _) = establish_within(
            &client,
            &Identity::generate().unwrap(),
            Side::Initiator,
            Duration::from_secs(1),
        )
        .unwrap();
        let result = exchange_hello(&writer, &mut reader, Role::Viewer, "Me");
        let took = started.elapsed();
        assert!(result.is_err(), "the greeting succeeded");
        assert!(
            took < Duration::from_millis(2500),
            "setup was given 1 s but the greeting ran to {took:?}"
        );
        drop((writer, reader, client));
        responder.join().unwrap();
    }

    #[test]
    fn names_are_made_printable_and_bounded() {
        assert_eq!(display_name("  Lab\tPC\n "), "LabPC");
        assert_eq!(display_name("\u{7}\n"), "Unnamed device");
        assert_eq!(display_name(&"x".repeat(200)).len(), MAX_NAME_CHARS);
    }

    #[test]
    fn security_keeps_its_identity_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let first = Security::load(dir.path()).unwrap().fingerprint();
        let second = Security::load(dir.path()).unwrap().fingerprint();
        assert_eq!(first, second);
        assert_eq!(first.len(), 19);
    }

    #[test]
    fn both_ends_agree_on_keys_and_code_and_exchange_messages() {
        let ((a_writer, mut a_reader, a_peer), (b_writer, mut b_reader, b_peer)) = secure_pair();
        assert_eq!(a_peer.auth_code, b_peer.auth_code);
        assert_ne!(a_peer.public, b_peer.public);
        assert_eq!(a_peer.fingerprint, fingerprint(&a_peer.public));

        let big = Message::VideoFrame {
            keyframe: true,
            timestamp_us: 7,
            data: vec![0x5A; 300_000],
        };
        a_writer.send(&Message::KeyframeRequest).unwrap();
        a_writer.send(&big).unwrap();
        assert_eq!(b_reader.read_message().unwrap(), Message::KeyframeRequest);
        assert_eq!(b_reader.read_message().unwrap(), big);

        b_writer.send(&Message::Accepted).unwrap();
        assert_eq!(a_reader.read_message().unwrap(), Message::Accepted);
    }

    #[test]
    fn nothing_readable_crosses_the_wire() {
        let (client, server) = socket_pair();
        let tap = server.try_clone().unwrap();
        let responder = std::thread::spawn(move || {
            establish(&server, &Identity::generate().unwrap(), Side::Responder).unwrap()
        });
        let (writer, _reader, _peer) =
            establish(&client, &Identity::generate().unwrap(), Side::Initiator).unwrap();
        // Keep the responder from consuming what is sent next.
        let _responder = responder.join().unwrap();

        writer
            .send(&Message::Bye {
                reason: "a recognisable sentence".into(),
            })
            .unwrap();
        let mut raw = vec![0u8; 4096];
        tap.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let n = (&tap).read(&mut raw).unwrap();
        assert!(n > 0);
        assert!(!raw[..n].windows(12).any(|w| w == b"recognisable"));
    }

    #[test]
    fn greetings_succeed_between_a_host_and_a_viewer() {
        let ((v_writer, mut v_reader, _), (h_writer, mut h_reader, _)) = secure_pair();
        let host = std::thread::spawn(move || {
            exchange_hello(&h_writer, &mut h_reader, Role::Host, "Host PC")
        });
        let seen_by_viewer = exchange_hello(&v_writer, &mut v_reader, Role::Viewer, "Viewer\nPC");
        assert_eq!(seen_by_viewer.unwrap(), "Host PC");
        assert_eq!(host.join().unwrap().unwrap(), "ViewerPC");
    }

    #[test]
    fn greetings_fail_for_the_same_role_or_another_version() {
        // Two hosts.
        let ((a_writer, mut a_reader, _), (b_writer, mut b_reader, _)) = secure_pair();
        let other =
            std::thread::spawn(move || exchange_hello(&b_writer, &mut b_reader, Role::Host, "B"));
        assert!(exchange_hello(&a_writer, &mut a_reader, Role::Host, "A").is_err());
        assert!(other.join().unwrap().is_err());

        // A viewer announcing a different protocol version.
        let ((v_writer, _v_reader, _), (h_writer, mut h_reader, _)) = secure_pair();
        v_writer
            .send(&Message::Hello {
                version: PROTOCOL_VERSION + 1,
                role: Role::Viewer,
                name: "Future".into(),
            })
            .unwrap();
        let error = exchange_hello(&h_writer, &mut h_reader, Role::Host, "Host").unwrap_err();
        assert!(error.contains("unsupported peer"), "{error}");
    }

    #[test]
    fn a_peer_that_does_not_do_the_handshake_is_rejected() {
        // Plain protocol bytes instead of a handshake.
        let (mut client, server) = socket_pair();
        let mut plain = Vec::new();
        write_message(&mut plain, &Message::KeyframeRequest).unwrap();
        client.write_all(&plain).unwrap();
        client.write_all(&[0xFF; 64]).unwrap();
        assert!(establish(&server, &Identity::generate().unwrap(), Side::Responder).is_err());

        // A peer that connects and says nothing.
        let (_silent, server) = socket_pair();
        server
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();
        let mut stream = &server;
        let quiet = Handshake::run(&Identity::generate().unwrap(), Side::Responder, &mut stream);
        assert!(quiet.is_err());
    }

    #[test]
    fn polling_stops_on_request_on_break_and_on_a_closed_connection() {
        let ((a_writer, _a_reader, _), (_b_writer, mut b_reader, _)) = secure_pair();

        // Break from the handler.
        a_writer.send(&Message::KeyframeRequest).unwrap();
        a_writer.send(&Message::Accepted).unwrap();
        let mut seen = Vec::new();
        let stop = Arc::new(AtomicBool::new(false));
        b_reader
            .poll(&stop, |message| {
                seen.push(message);
                ControlFlow::Break(())
            })
            .unwrap();
        assert_eq!(seen, [Message::KeyframeRequest]);

        // Stop flag: the second message is delivered first, then it stops.
        let flag = Arc::clone(&stop);
        let before = std::time::Instant::now();
        b_reader
            .poll(&stop, |message| {
                seen.push(message);
                flag.store(true, Ordering::Relaxed);
                ControlFlow::Continue(())
            })
            .unwrap();
        assert_eq!(seen.len(), 2);
        assert!(before.elapsed() < Duration::from_secs(2));

        // The peer goes away.
        stop.store(false, Ordering::Relaxed);
        a_writer.shutdown();
        assert!(b_reader.poll(&stop, |_| ControlFlow::Continue(())).is_err());
    }
}
