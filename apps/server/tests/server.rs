use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use rc_crypto::channel::{Established, Handshake, Opener, Sealer, Side};
use rc_crypto::{fingerprint, fingerprint_bytes, Identity};
use rc_rendezvous::{
    relay_hello, ClientMessage, DeviceId, ServerMessage, Token, RENDEZVOUS_VERSION,
};
use rc_server::{Config, Limits, Server, Stats};

/// A server running in a background runtime of its own.
struct TestServer {
    runtime: tokio::runtime::Runtime,
    server: Option<Server>,
    address: SocketAddr,
}

impl TestServer {
    fn start() -> Self {
        Self::with_limits(Limits::default())
    }

    fn with_limits(limits: Limits) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime
            .block_on(Server::start(Config {
                listen: SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
                identity: Identity::generate().unwrap(),
                limits,
            }))
            .unwrap();
        let address = server.local_addr();
        Self {
            runtime,
            server: Some(server),
            address,
        }
    }

    fn stats(&self) -> Stats {
        self.server.as_ref().unwrap().stats()
    }

    fn fingerprint(&self) -> String {
        self.server.as_ref().unwrap().fingerprint()
    }

    fn wait_for(&self, what: &str, condition: impl Fn(Stats) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition(self.stats()) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {:?}",
                self.stats()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn shutdown(&mut self) {
        if let Some(server) = self.server.take() {
            self.runtime.block_on(server.shutdown());
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A device talking to the server over the encrypted control channel.
struct Client {
    stream: TcpStream,
    sealer: Sealer,
    opener: Opener,
    server_key: [u8; 32],
    id: DeviceId,
}

impl Client {
    fn connect(server: &TestServer, identity: &Identity) -> Self {
        Self::try_connect(server.address, identity).expect("handshake with the server failed")
    }

    fn try_connect(address: SocketAddr, identity: &Identity) -> Option<Self> {
        let mut stream = TcpStream::connect(address).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        let Established {
            remote_public,
            sealer,
            opener,
            ..
        } = Handshake::run(identity, Side::Initiator, &mut stream).ok()?;
        Some(Self {
            stream,
            sealer,
            opener,
            server_key: remote_public,
            id: fingerprint_bytes(identity.public()),
        })
    }

    fn send(&mut self, message: &ClientMessage) {
        let sealed = self.sealer.seal(&message.encode()).unwrap();
        self.stream.write_all(&sealed).unwrap();
    }

    /// The next message, or `None` if nothing arrives within `wait` or the
    /// server closed the connection.
    fn receive_within(&mut self, wait: Duration) -> Option<ServerMessage> {
        self.stream.set_read_timeout(Some(wait)).unwrap();
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(plain) = self.opener.next_plaintext().ok()? {
                return Some(ServerMessage::decode(&plain).unwrap());
            }
            match self.stream.read(&mut chunk) {
                Ok(0) | Err(_) => return None,
                Ok(n) => self.opener.push(&chunk[..n]),
            }
        }
    }

    fn receive(&mut self) -> ServerMessage {
        self.receive_within(Duration::from_secs(5))
            .expect("no message from the server")
    }

    fn register(&mut self, port: u16) -> ServerMessage {
        self.send(&ClientMessage::Register {
            version: RENDEZVOUS_VERSION,
            port,
            lan: vec![Ipv4Addr::new(192, 168, 1, 7)],
            name: "Host".into(),
        });
        self.receive()
    }
}

fn identity() -> Identity {
    Identity::generate().unwrap()
}

fn relay_stream(server: &TestServer, token: &Token) -> TcpStream {
    let mut stream = TcpStream::connect(server.address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(&relay_hello(token)).unwrap();
    stream
}

/// Reads exactly `n` bytes.
fn read_n(stream: &mut TcpStream, n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    stream.read_exact(&mut out).unwrap();
    out
}

/// True once the other end has closed the connection.
fn is_closed(stream: &mut TcpStream) -> bool {
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    matches!(stream.read(&mut [0u8; 16]), Ok(0) | Err(_))
}

/// Sets up a viewer and a host that has registered, a connect request, and
/// a relay grant; returns the token.
fn granted_relay(server: &TestServer) -> (Client, Client, Token) {
    let (host_key, viewer_key) = (identity(), identity());
    let mut host = Client::connect(server, &host_key);
    host.register(40_000);
    let mut viewer = Client::connect(server, &viewer_key);
    viewer.send(&ClientMessage::Connect {
        target: host.id,
        name: "Viewer".into(),
    });
    let ServerMessage::Peer { request_id, .. } = viewer.receive() else {
        panic!("no Peer");
    };
    assert!(matches!(host.receive(), ServerMessage::Incoming { .. }));
    viewer.send(&ClientMessage::UseRelay { request_id });
    let ServerMessage::RelayToken { token, .. } = viewer.receive() else {
        panic!("no token");
    };
    assert_eq!(
        host.receive(),
        ServerMessage::RelayRequest { request_id, token }
    );
    (host, viewer, token)
}

#[test]
fn the_server_proves_its_key_to_every_device() {
    let server = TestServer::start();
    let client = Client::connect(&server, &identity());
    assert_eq!(fingerprint(&client.server_key), server.fingerprint());
}

#[test]
fn registering_and_connecting_tells_both_sides_what_they_need() {
    let server = TestServer::start();
    let (host_key, viewer_key) = (identity(), identity());
    let mut host = Client::connect(&server, &host_key);
    let mut viewer = Client::connect(&server, &viewer_key);

    assert_eq!(
        host.register(41_000),
        ServerMessage::Registered {
            address: Ipv4Addr::LOCALHOST
        }
    );
    assert_eq!(server.stats().registered, 1);

    viewer.send(&ClientMessage::Connect {
        target: host.id,
        name: "Laptop".into(),
    });
    let ServerMessage::Peer {
        request_id,
        address,
        port,
        lan,
        same_network,
    } = viewer.receive()
    else {
        panic!("expected Peer");
    };
    assert_eq!((address, port), (Ipv4Addr::LOCALHOST, 41_000));
    assert_eq!(lan, [Ipv4Addr::new(192, 168, 1, 7)]);
    assert!(same_network, "both come from 127.0.0.1");

    assert_eq!(
        host.receive(),
        ServerMessage::Incoming {
            request_id,
            viewer: viewer.id,
            name: "Laptop".into()
        }
    );
}

#[test]
fn an_unknown_device_is_not_found_and_pings_are_answered() {
    let server = TestServer::start();
    let mut viewer = Client::connect(&server, &identity());
    viewer.send(&ClientMessage::Connect {
        target: [9; 8],
        name: "x".into(),
    });
    assert_eq!(viewer.receive(), ServerMessage::NotFound { target: [9; 8] });
    viewer.send(&ClientMessage::Ping);
    assert_eq!(viewer.receive(), ServerMessage::Pong);
}

#[test]
fn a_second_registration_replaces_the_first_and_leaving_unregisters() {
    let server = TestServer::start();
    let key = identity();
    let mut first = Client::connect(&server, &key);
    first.register(1000);
    let mut second = Client::connect(&server, &key);
    second.register(2000);

    assert_eq!(first.receive(), ServerMessage::Replaced);
    assert_eq!(server.stats().registered, 1);

    // The old connection ending must not remove the new registration.
    drop(first);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(server.stats().registered, 1);

    drop(second);
    server.wait_for("the device to be unregistered", |s| s.registered == 0);
}

#[test]
fn a_viewer_leaving_cancels_its_requests() {
    let server = TestServer::start();
    let mut host = Client::connect(&server, &identity());
    host.register(1);
    let mut viewer = Client::connect(&server, &identity());
    viewer.send(&ClientMessage::Connect {
        target: host.id,
        name: "v".into(),
    });
    viewer.receive();
    assert_eq!(server.stats().pending_requests, 1);
    drop(viewer);
    server.wait_for("the request to be dropped", |s| s.pending_requests == 0);
}

#[test]
fn only_the_requesting_viewer_may_ask_for_a_relay_and_only_once() {
    let server = TestServer::start();
    let mut host = Client::connect(&server, &identity());
    host.register(40_000);
    let mut viewer = Client::connect(&server, &identity());
    let mut stranger = Client::connect(&server, &identity());
    viewer.send(&ClientMessage::Connect {
        target: host.id,
        name: "v".into(),
    });
    let ServerMessage::Peer { request_id, .. } = viewer.receive() else {
        panic!("no Peer");
    };

    stranger.send(&ClientMessage::UseRelay { request_id });
    assert!(matches!(stranger.receive(), ServerMessage::Error { .. }));
    // Nothing was told to the host.
    host.receive(); // Incoming
    assert!(host.receive_within(Duration::from_millis(300)).is_none());

    viewer.send(&ClientMessage::UseRelay { request_id });
    assert!(matches!(viewer.receive(), ServerMessage::RelayToken { .. }));
    assert!(matches!(host.receive(), ServerMessage::RelayRequest { .. }));
    viewer.send(&ClientMessage::UseRelay { request_id });
    assert!(matches!(viewer.receive(), ServerMessage::Error { .. }));
    viewer.send(&ClientMessage::UseRelay {
        request_id: 999_999,
    });
    assert!(matches!(viewer.receive(), ServerMessage::Error { .. }));
}

#[test]
fn a_relay_carries_bytes_both_ways_unchanged_and_the_token_works_once() {
    let server = TestServer::start();
    let (_host, _viewer, token) = granted_relay(&server);

    let mut a = relay_stream(&server, &token);
    let mut b = relay_stream(&server, &token);
    server.wait_for("the relay to start", |s| s.relays_active == 1);

    let outgoing: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
    a.write_all(&outgoing).unwrap();
    assert_eq!(read_n(&mut b, outgoing.len()), outgoing);
    b.write_all(b"and back").unwrap();
    assert_eq!(read_n(&mut a, 8), b"and back");

    // A third connection with the same token gets nothing.
    let mut late = relay_stream(&server, &token);
    assert!(is_closed(&mut late));

    drop(a);
    drop(b);
    server.wait_for("the relay to end", |s| s.relays_active == 0);
    assert!(server.stats().bytes_relayed >= 200_008);
}

#[test]
fn two_devices_can_run_their_own_handshake_through_a_relay() {
    let server = TestServer::start();
    let (_host_c, _viewer_c, token) = granted_relay(&server);
    let (host_key, viewer_key) = (identity(), identity());
    let host_public = *host_key.public();
    let viewer_public = *viewer_key.public();

    let mut host_stream = relay_stream(&server, &token);
    let mut viewer_stream = relay_stream(&server, &token);
    let host = std::thread::spawn(move || {
        let mut session = Handshake::run(&host_key, Side::Responder, &mut host_stream).unwrap();
        host_stream
            .write_all(&session.sealer.seal(b"through the relay").unwrap())
            .unwrap();
        (session.remote_public, session.auth_code)
    });
    let mut session = Handshake::run(&viewer_key, Side::Initiator, &mut viewer_stream).unwrap();

    // They see each other's keys, not the server's.
    assert_eq!(session.remote_public, host_public);
    let mut chunk = [0u8; 256];
    let mut received = Vec::new();
    while received.len() < 17 {
        let n = viewer_stream.read(&mut chunk).unwrap();
        session.opener.push(&chunk[..n]);
        while let Some(plain) = session.opener.next_plaintext().unwrap() {
            received.extend_from_slice(&plain);
        }
    }
    assert_eq!(received, b"through the relay");
    let (host_saw, host_code) = host.join().unwrap();
    assert_eq!(host_saw, viewer_public);
    assert_eq!(host_code, session.auth_code);
}

#[test]
fn a_relay_with_an_unknown_token_is_closed() {
    let server = TestServer::start();
    let mut stream = relay_stream(&server, &[0x11; 32]);
    assert!(is_closed(&mut stream));
}

#[test]
fn a_relay_nobody_joins_is_dropped_after_the_pairing_time() {
    let server = TestServer::with_limits(Limits {
        relay_pair_timeout: Duration::from_millis(300),
        ..Limits::default()
    });
    let (_host, _viewer, token) = granted_relay(&server);
    let mut lonely = relay_stream(&server, &token);
    assert!(is_closed(&mut lonely));
    server.wait_for("the slot to be removed", |s| s.relays_waiting == 0);
}

#[test]
fn an_idle_relay_is_closed() {
    let server = TestServer::with_limits(Limits {
        relay_idle_timeout: Duration::from_millis(400),
        ..Limits::default()
    });
    let (_host, _viewer, token) = granted_relay(&server);
    let mut a = relay_stream(&server, &token);
    let mut b = relay_stream(&server, &token);
    server.wait_for("the relay to start", |s| s.relays_active == 1);
    // Silence from both sides: the server ends it.
    assert!(is_closed(&mut a));
    assert!(is_closed(&mut b));
    server.wait_for("the relay to end", |s| s.relays_active == 0);
}

#[test]
fn relay_capacity_is_limited() {
    let server = TestServer::with_limits(Limits {
        max_relays: 1,
        ..Limits::default()
    });
    let (_host, _viewer, _token) = granted_relay(&server);

    let mut host = Client::connect(&server, &identity());
    host.register(1);
    let mut viewer = Client::connect(&server, &identity());
    viewer.send(&ClientMessage::Connect {
        target: host.id,
        name: "v".into(),
    });
    let ServerMessage::Peer { request_id, .. } = viewer.receive() else {
        panic!("no Peer");
    };
    viewer.send(&ClientMessage::UseRelay { request_id });
    match viewer.receive() {
        ServerMessage::Error { reason } => assert!(reason.contains("capacity"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn too_many_connections_from_one_address_are_refused() {
    let server = TestServer::with_limits(Limits {
        max_connections_per_address: 2,
        ..Limits::default()
    });
    let _a = TcpStream::connect(server.address).unwrap();
    let _b = TcpStream::connect(server.address).unwrap();
    server.wait_for("two connections", |s| s.connections == 2);
    let mut third = TcpStream::connect(server.address).unwrap();
    assert!(is_closed(&mut third));
    // A proper client is also refused while both are held…
    assert!(Client::try_connect(server.address, &identity()).is_none());
    // …and accepted again once one goes.
    drop(_a);
    server.wait_for("a free slot", |s| s.connections < 2);
    assert!(Client::try_connect(server.address, &identity()).is_some());
}

#[test]
fn connect_requests_are_rate_limited() {
    let server = TestServer::with_limits(Limits {
        connects_per_minute: 2,
        ..Limits::default()
    });
    let mut viewer = Client::connect(&server, &identity());
    for _ in 0..2 {
        viewer.send(&ClientMessage::Connect {
            target: [1; 8],
            name: "v".into(),
        });
        assert!(matches!(viewer.receive(), ServerMessage::NotFound { .. }));
    }
    viewer.send(&ClientMessage::Connect {
        target: [1; 8],
        name: "v".into(),
    });
    assert!(matches!(viewer.receive(), ServerMessage::Error { .. }));
}

#[test]
fn a_different_protocol_version_is_refused_and_disconnected() {
    let server = TestServer::start();
    let mut client = Client::connect(&server, &identity());
    client.send(&ClientMessage::Register {
        version: RENDEZVOUS_VERSION + 1,
        port: 1,
        lan: vec![],
        name: "x".into(),
    });
    assert!(matches!(client.receive(), ServerMessage::Error { .. }));
    assert!(client.receive_within(Duration::from_secs(2)).is_none());
    assert_eq!(server.stats().registered, 0);
}

#[test]
fn the_registry_has_a_size_limit() {
    let server = TestServer::with_limits(Limits {
        max_registered: 1,
        ..Limits::default()
    });
    let mut first = Client::connect(&server, &identity());
    assert!(matches!(
        first.register(1),
        ServerMessage::Registered { .. }
    ));
    let mut second = Client::connect(&server, &identity());
    match second.register(1) {
        ServerMessage::Error { reason } => assert!(reason.contains("full"), "{reason}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(server.stats().registered, 1);
}

#[test]
fn garbage_and_unencrypted_peers_do_no_harm() {
    let server = TestServer::start();
    for payload in [
        &b"GET / HTTP/1.1\r\n\r\n"[..],
        &[0xFF; 300][..],
        b"R",
        b"RC",
        b"RCRELAY1",
        &[0u8; 2][..],
    ] {
        let mut stream = TcpStream::connect(server.address).unwrap();
        let _ = stream.write_all(payload);
        let _ = stream.shutdown(std::net::Shutdown::Write);
        assert!(is_closed(&mut stream), "{payload:?}");
    }
    // Still serving.
    let mut client = Client::connect(&server, &identity());
    assert!(matches!(
        client.register(5),
        ServerMessage::Registered { .. }
    ));
    server.wait_for("old connections to be gone", |s| s.connections <= 1);
}

#[test]
fn a_device_that_goes_quiet_is_dropped() {
    let server = TestServer::with_limits(Limits {
        control_idle_timeout: Duration::from_millis(400),
        ..Limits::default()
    });
    let mut client = Client::connect(&server, &identity());
    client.register(7);
    assert!(client.receive_within(Duration::from_secs(3)).is_none());
    server.wait_for("the registration to go", |s| s.registered == 0);
}

#[test]
fn a_device_that_keeps_pinging_stays_registered() {
    let server = TestServer::with_limits(Limits {
        control_idle_timeout: Duration::from_millis(500),
        ..Limits::default()
    });
    let mut client = Client::connect(&server, &identity());
    client.register(7);
    for _ in 0..6 {
        std::thread::sleep(Duration::from_millis(200));
        client.send(&ClientMessage::Ping);
        assert_eq!(client.receive(), ServerMessage::Pong);
    }
    assert_eq!(server.stats().registered, 1);
}

#[test]
fn shutting_down_closes_every_connection() {
    let mut server = TestServer::start();
    let mut client = Client::connect(&server, &identity());
    client.register(7);
    let (_host, _viewer, token) = granted_relay(&server);
    let mut a = relay_stream(&server, &token);
    let mut b = relay_stream(&server, &token);
    server.wait_for("the relay to start", |s| s.relays_active == 1);

    server.shutdown();
    assert!(client.receive_within(Duration::from_secs(3)).is_none());
    assert!(is_closed(&mut a));
    assert!(is_closed(&mut b));
}
