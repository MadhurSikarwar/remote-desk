use std::net::{Ipv4Addr, TcpListener};
use std::sync::mpsc;

use rc_core::config::NetworkConfig;
use rc_crypto::{fingerprint, fingerprint_bytes, Identity};
use rc_server::{Config, Limits, Server};

use super::*;

/// A real server in a runtime of its own.
pub struct TestServer {
    runtime: tokio::runtime::Runtime,
    server: Option<Server>,
    pub address: SocketAddr,
    pub key: Identity,
}

impl TestServer {
    pub fn start() -> Self {
        Self::start_on(0, Identity::generate().unwrap())
    }

    pub fn start_on(port: u16, key: Identity) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server = runtime
            .block_on(Server::start(Config {
                listen: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                identity: key.clone(),
                limits: Limits::default(),
            }))
            .unwrap();
        let address = server.local_addr();
        Self {
            runtime,
            server: Some(server),
            address,
            key,
        }
    }

    pub fn link(&self) -> ServerLink {
        ServerLink {
            address: self.address.to_string(),
            fingerprint: fingerprint_bytes(self.key.public()),
        }
    }

    pub fn stats(&self) -> rc_server::Stats {
        self.server.as_ref().unwrap().stats()
    }

    pub fn stop(&mut self) {
        if let Some(server) = self.server.take() {
            self.runtime.block_on(server.shutdown());
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn wait_until(what: &str, condition: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn status_text(status: &Mutex<Option<String>>) -> String {
    lock(status).clone().unwrap_or_default()
}

/// Runs `body` while a device is registered with the server.
fn with_registered_host<R>(
    link: &ServerLink,
    identity: &Identity,
    port: u16,
    on_relay: &(dyn Fn(TcpStream) + Sync),
    body: impl FnOnce(&Mutex<Option<String>>) -> R,
) -> R {
    let stop = AtomicBool::new(false);
    let status = Mutex::new(None);
    thread::scope(|scope| {
        scope.spawn(|| {
            keep_registered(
                &Registration {
                    link,
                    identity,
                    port,
                    name: "Test host",
                },
                &stop,
                &status,
                on_relay,
            );
        });
        wait_until("registration", || {
            status_text(&status).starts_with("Registered")
        });
        let result = body(&status);
        stop.store(true, Ordering::Relaxed);
        result
    })
}

fn no_relay() -> impl Fn(TcpStream) + Sync {
    |_| panic!("an unexpected relay connection")
}

fn viewer_key() -> Identity {
    Identity::generate().unwrap()
}

/// A port with nothing listening on it.
fn closed_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn the_configured_server_is_read_from_the_settings() {
    let mut network = NetworkConfig::default();
    assert_eq!(ServerLink::from_config(&network), Ok(None));

    network.rendezvous_servers = vec!["rv.example.org:443".into()];
    network.server_fingerprint = "garbage".into();
    assert!(ServerLink::from_config(&network).is_err());

    network.server_fingerprint = "0754-E5CE-BDDB-DF24".into();
    let link = ServerLink::from_config(&network).unwrap().unwrap();
    assert_eq!(link.address, "rv.example.org:443");
    assert_eq!(
        link.fingerprint,
        [0x07, 0x54, 0xE5, 0xCE, 0xBD, 0xDB, 0xDF, 0x24]
    );
}

#[test]
fn a_server_with_another_key_than_configured_is_refused() {
    let server = TestServer::start();
    let mut link = server.link();
    link.fingerprint = [0xEE; 8];
    let error = Channel::open(&link, &viewer_key()).err().unwrap();
    assert!(error.contains("not the one in the settings"), "{error}");
    assert!(error.contains(&fingerprint(server.key.public())), "{error}");

    // The same check protects a registering host.
    let stop = AtomicBool::new(true);
    let status = Mutex::new(None);
    let host_key = viewer_key();
    let result = registered_session(
        &Registration {
            link: &link,
            identity: &host_key,
            port: 1,
            name: "x",
        },
        &stop,
        &status,
        &no_relay(),
    );
    assert!(result.unwrap_err().contains("not the one in the settings"));
    assert_eq!(server.stats().registered, 0);
}

#[test]
fn a_reachable_device_is_reached_directly_and_the_server_carries_no_traffic() {
    let server = TestServer::start();
    let link = server.link();
    let host_key = viewer_key();
    let host_id = fingerprint_bytes(host_key.public());
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();

    with_registered_host(&link, &host_key, port, &no_relay(), |_| {
        let (reached, route) = reach(
            &link,
            &viewer_key(),
            host_id,
            "Viewer",
            TransportPolicy::Auto,
            |stream, route| Ok((stream.peer_addr().unwrap(), route)),
        )
        .unwrap();
        assert_eq!(route, Route::Direct);
        assert_eq!(reached.0.port(), port);
        assert_eq!(reached.1, Route::Direct);
        // The host's listener got the connection.
        listener.set_nonblocking(false).unwrap();
        assert!(listener.accept().is_ok());
    });
    assert_eq!(server.stats().bytes_relayed, 0);
}

#[test]
fn an_unreachable_device_is_reached_through_the_relay() {
    let server = TestServer::start();
    let link = server.link();
    let host_key = viewer_key();
    let host_id = fingerprint_bytes(host_key.public());
    let (relay_tx, relay_rx) = mpsc::channel();
    let relay_tx = Mutex::new(relay_tx);
    let on_relay = move |stream: TcpStream| {
        let _ = lock(&relay_tx).send(stream);
    };

    with_registered_host(&link, &host_key, closed_port(), &on_relay, |_| {
        let routes = Mutex::new(Vec::new());
        let (mut viewer_side, route) = reach(
            &link,
            &viewer_key(),
            host_id,
            "Viewer",
            TransportPolicy::Auto,
            |stream, route| {
                lock(&routes).push(route);
                Ok(stream)
            },
        )
        .unwrap();
        assert_eq!(route, Route::Relayed);
        // The refused direct connection never reached the handshake step.
        assert_eq!(*lock(&routes), [Route::Relayed]);

        let mut host_side = relay_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        viewer_side.write_all(b"ping through").unwrap();
        let mut got = [0u8; 12];
        host_side
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        host_side.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"ping through");
        host_side.write_all(b"pong").unwrap();
        let mut back = [0u8; 4];
        viewer_side
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        viewer_side.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"pong");
    });
    assert!(server.stats().bytes_relayed > 0 || server.stats().relays_active == 1);
}

#[test]
fn relay_only_never_tries_a_direct_connection() {
    let server = TestServer::start();
    let link = server.link();
    let host_key = viewer_key();
    let host_id = fingerprint_bytes(host_key.public());
    // A reachable listener: Auto would use it.
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let (relay_tx, relay_rx) = mpsc::channel();
    let relay_tx = Mutex::new(relay_tx);
    let on_relay = move |stream: TcpStream| {
        let _ = lock(&relay_tx).send(stream);
    };

    with_registered_host(&link, &host_key, port, &on_relay, |_| {
        let (_stream, route) = reach(
            &link,
            &viewer_key(),
            host_id,
            "Viewer",
            TransportPolicy::RelayOnly,
            |stream, route| {
                assert_eq!(route, Route::Relayed, "tried a direct connection");
                Ok(stream)
            },
        )
        .unwrap();
        assert_eq!(route, Route::Relayed);
        relay_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            listener.accept().is_err(),
            "the host's listener was contacted"
        );
    });
}

#[test]
fn direct_only_fails_instead_of_relaying() {
    let server = TestServer::start();
    let link = server.link();
    let host_key = viewer_key();
    let host_id = fingerprint_bytes(host_key.public());

    with_registered_host(&link, &host_key, closed_port(), &no_relay(), |_| {
        let error = reach(
            &link,
            &viewer_key(),
            host_id,
            "Viewer",
            TransportPolicy::DirectOnly,
            |_stream, _route| -> Result<(), String> { panic!("no stream expected") },
        )
        .unwrap_err();
        assert!(error.contains("relaying is switched off"), "{error}");
    });
    assert_eq!(server.stats().bytes_relayed, 0);
}

#[test]
fn a_wrong_device_at_the_direct_address_is_turned_away_and_the_relay_is_used() {
    let server = TestServer::start();
    let link = server.link();
    let host_key = viewer_key();
    let host_id = fingerprint_bytes(host_key.public());
    // Something else answers at the address the host advertises.
    let imposter = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = imposter.local_addr().unwrap().port();
    let (relay_tx, relay_rx) = mpsc::channel();
    let relay_tx = Mutex::new(relay_tx);
    let on_relay = move |stream: TcpStream| {
        let _ = lock(&relay_tx).send(stream);
    };

    with_registered_host(&link, &host_key, port, &on_relay, |_| {
        let seen = Mutex::new(Vec::new());
        let (_stream, route) = reach(
            &link,
            &viewer_key(),
            host_id,
            "Viewer",
            TransportPolicy::Auto,
            |stream, route| {
                lock(&seen).push(route);
                // The step that checks the device's key: it is not the one.
                if route == Route::Direct {
                    Err("the device has a different key".to_owned())
                } else {
                    Ok(stream)
                }
            },
        )
        .unwrap();
        assert_eq!(route, Route::Relayed);
        assert_eq!(*lock(&seen), [Route::Direct, Route::Relayed]);
        relay_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    });
}

#[test]
fn when_every_way_fails_the_error_lists_what_was_tried() {
    let server = TestServer::start();
    let link = server.link();
    let host_key = viewer_key();
    let host_id = fingerprint_bytes(host_key.public());
    let on_relay = |_stream: TcpStream| {};

    with_registered_host(&link, &host_key, closed_port(), &on_relay, |_| {
        let error = reach(
            &link,
            &viewer_key(),
            host_id,
            "Viewer",
            TransportPolicy::Auto,
            |_stream, route| -> Result<(), String> { Err(format!("refused {route:?}")) },
        )
        .unwrap_err();
        assert!(error.contains("directly to"), "{error}");
        assert!(
            error.contains("through the server: refused Relayed"),
            "{error}"
        );
    });
}

#[test]
fn asking_for_a_device_that_is_not_registered_says_so() {
    let server = TestServer::start();
    let error = reach(
        &server.link(),
        &viewer_key(),
        [7; 8],
        "Viewer",
        TransportPolicy::Auto,
        |_s, _r| -> Result<(), String> { panic!("no stream expected") },
    )
    .unwrap_err();
    assert!(error.contains("No device with that ID"), "{error}");
}

#[test]
fn an_unreachable_server_is_reported_and_a_returning_server_is_picked_up_again() {
    let mut server = TestServer::start();
    let key = server.key.clone();
    let port = server.address.port();
    let link = server.link();
    let host_key = viewer_key();

    with_registered_host(&link, &host_key, 1234, &no_relay(), |status| {
        assert_eq!(server.stats().registered, 1);

        // The server goes away: the device says so and keeps trying.
        server.stop();
        wait_until("the outage to be noticed", || {
            status_text(status).starts_with("Server unavailable")
        });

        // The server comes back on the same address with the same key.
        let returned = TestServer::start_on(port, key.clone());
        wait_until("the device to register again", || {
            status_text(status).starts_with("Registered") && returned.stats().registered == 1
        });
    });
}

// ---- whole sessions through the server -------------------------------------

mod sessions {
    use std::sync::Arc;

    use super::*;
    use crate::secure::test_support::{desktop, security};
    use crate::secure::Security;
    use crate::share::test_support::{pending, settings, wait_until as wait_share};
    use crate::share::{ShareController, ShareSettings};
    use crate::viewer::{packet, ConnectOptions, ViewerController};

    fn sharing(link: &ServerLink, extra: impl FnOnce(&mut ShareSettings)) -> ShareSettings {
        let mut settings = ShareSettings {
            allow_remote: true,
            server: Some(link.clone()),
            ..settings()
        };
        extra(&mut settings);
        settings
    }

    fn registered(host: &ShareController) {
        wait_share("the host to register", || {
            host.status()
                .rendezvous
                .is_some_and(|text| text.starts_with("Registered"))
        });
    }

    struct Viewing {
        viewer: ViewerController,
        packets: mpsc::Receiver<Vec<u8>>,
        _keys: Arc<Security>,
        _dir: tempfile::TempDir,
    }

    /// A viewer with keys of its own; `packets` gets what the connection
    /// delivers once one is made.
    fn viewer() -> (Viewing, mpsc::Sender<Vec<u8>>) {
        let (keys, dir) = security();
        let (tx, packets) = mpsc::channel();
        (
            Viewing {
                viewer: ViewerController::new(Some(Arc::clone(&keys))),
                packets,
                _keys: keys,
                _dir: dir,
            },
            tx,
        )
    }

    /// Connects, has the host approve, and waits for the first picture.
    fn view_to_first_frame(
        host: &ShareController,
        link: &ServerLink,
        policy: TransportPolicy,
        target: &str,
    ) -> (Viewing, crate::viewer::HostInfo) {
        let (viewing, tx) = viewer();
        let options = ConnectOptions {
            server: Some(link.clone()),
            policy,
            ..ConnectOptions::default()
        };
        let info = viewing
            .viewer
            .connect_with(target, &options, "Viewer PC", move |packet| {
                let _ = tx.send(packet);
            })
            .expect("connecting failed");

        let request = pending(host);
        assert_eq!(request.name, "Viewer PC");
        assert_eq!(
            request.auth_code, info.auth_code,
            "the two screens show different codes"
        );
        host.answer(request.id, true, false).unwrap();
        loop {
            let packet = viewing
                .packets
                .recv_timeout(Duration::from_secs(10))
                .expect("no picture arrived");
            if packet[0] == packet::FRAME {
                break;
            }
        }
        (viewing, info)
    }

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn a_device_is_reached_by_its_id_directly_when_it_can_be() {
        let _desktop = desktop();
        let server = TestServer::start();
        let link = server.link();
        let (host_keys, _d) = security();
        let host = ShareController::new(Some(Arc::clone(&host_keys)));
        host.start(sharing(&link, |_| {})).unwrap();
        registered(&host);

        let (viewing, info) = view_to_first_frame(
            &host,
            &link,
            TransportPolicy::Auto,
            &host_keys.fingerprint(),
        );
        assert_eq!(info.route, "directly");
        assert_eq!(info.fingerprint, host_keys.fingerprint());
        viewing.viewer.disconnect();
        host.stop();
        assert_eq!(
            server.stats().bytes_relayed,
            0,
            "traffic went through the server"
        );
    }

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn an_unreachable_device_is_viewed_through_the_relay_with_the_same_approval() {
        let _desktop = desktop();
        let server = TestServer::start();
        let link = server.link();
        let (host_keys, _d) = security();
        let host = ShareController::new(Some(Arc::clone(&host_keys)));
        // The address given to viewers leads nowhere.
        host.start(sharing(&link, |s| s.advertised_port = Some(closed_port())))
            .unwrap();
        registered(&host);

        // Typed the way people paste it: lower case, no dashes.
        let typed = host_keys.fingerprint().replace('-', "").to_lowercase();
        let (viewing, info) = view_to_first_frame(&host, &link, TransportPolicy::Auto, &typed);
        assert_eq!(info.route, "through the server");
        assert_eq!(info.fingerprint, host_keys.fingerprint());
        assert!(host.status().viewer_connected);

        viewing.viewer.disconnect();
        wait_share("the session to end", || !host.status().viewer_connected);
        // Real picture data went through, and it was ciphertext: the server
        // holds no session key, so there is nothing else it could have seen.
        assert!(
            server.stats().bytes_relayed > 10_000,
            "{:?}",
            server.stats()
        );
        host.stop();
    }

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn relay_only_uses_the_server_even_when_the_device_is_reachable() {
        let _desktop = desktop();
        let server = TestServer::start();
        let link = server.link();
        let (host_keys, _d) = security();
        let host = ShareController::new(Some(Arc::clone(&host_keys)));
        host.start(sharing(&link, |_| {})).unwrap();
        registered(&host);

        let (viewing, info) = view_to_first_frame(
            &host,
            &link,
            TransportPolicy::RelayOnly,
            &host_keys.fingerprint(),
        );
        assert_eq!(info.route, "through the server");
        viewing.viewer.disconnect();
        wait_share("the session to end", || !host.status().viewer_connected);
        assert!(server.stats().bytes_relayed > 0);
        host.stop();
    }

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn a_different_device_at_the_advertised_address_is_never_approved_or_prompted() {
        let _desktop = desktop();
        let server = TestServer::start();
        let link = server.link();

        // Device B listens where device A tells viewers to connect.
        let (b_keys, _db) = security();
        let device_b = ShareController::new(Some(Arc::clone(&b_keys)));
        let b_port = device_b.start(settings()).unwrap().port.unwrap();

        let (a_keys, _da) = security();
        let device_a = ShareController::new(Some(Arc::clone(&a_keys)));
        device_a
            .start(sharing(&link, |s| s.advertised_port = Some(b_port)))
            .unwrap();
        registered(&device_a);

        let (viewing, info) = view_to_first_frame(
            &device_a,
            &link,
            TransportPolicy::Auto,
            &a_keys.fingerprint(),
        );
        // The viewer reached A, by way of the relay, after turning B away.
        assert_eq!(info.fingerprint, a_keys.fingerprint());
        assert_eq!(info.route, "through the server");
        assert!(device_b.status().pending.is_none(), "B was shown a request");
        assert!(!device_b.status().viewer_connected);
        assert_eq!(device_b.status().frames, 0);
        viewing.viewer.disconnect();
        device_a.stop();
        device_b.stop();
    }

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn direct_only_fails_when_the_device_is_unreachable_and_nothing_is_relayed() {
        let _desktop = desktop();
        let server = TestServer::start();
        let link = server.link();
        let (host_keys, _d) = security();
        let host = ShareController::new(Some(Arc::clone(&host_keys)));
        host.start(sharing(&link, |s| s.advertised_port = Some(closed_port())))
            .unwrap();
        registered(&host);

        let (viewing, _tx) = viewer();
        let options = ConnectOptions {
            server: Some(link.clone()),
            policy: TransportPolicy::DirectOnly,
            ..ConnectOptions::default()
        };
        let error = viewing
            .viewer
            .connect_with(&host_keys.fingerprint(), &options, "Viewer", |_| {})
            .unwrap_err();
        assert!(error.contains("relaying is switched off"), "{error}");
        assert!(host.status().pending.is_none());
        assert_eq!(server.stats().bytes_relayed, 0);
        host.stop();
    }

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn an_id_nobody_has_registered_and_a_missing_server_both_say_so() {
        let _desktop = desktop();
        let server = TestServer::start();
        let (viewing, _tx) = viewer();
        let options = ConnectOptions {
            server: Some(server.link()),
            ..ConnectOptions::default()
        };
        let error = viewing
            .viewer
            .connect_with("0000-1111-2222-3333", &options, "Viewer", |_| {})
            .unwrap_err();
        assert!(error.contains("No device with that ID"), "{error}");

        let error = viewing
            .viewer
            .connect_with(
                "0000-1111-2222-3333",
                &ConnectOptions::default(),
                "Viewer",
                |_| {},
            )
            .unwrap_err();
        assert!(error.contains("needs a server"), "{error}");
    }

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn a_device_registers_only_while_sharing_with_other_computers_allowed() {
        let _desktop = desktop();
        let server = TestServer::start();
        let link = server.link();
        let (host_keys, _d) = security();
        let host = ShareController::new(Some(host_keys));

        // A server is configured, but other computers are not allowed.
        host.start(sharing(&link, |s| s.allow_remote = false))
            .unwrap();
        thread::sleep(Duration::from_millis(1200));
        assert_eq!(server.stats().registered, 0);
        assert!(host.status().rendezvous.is_none());
        host.stop();

        // Allowed: registered, and gone again the moment sharing stops.
        host.start(sharing(&link, |_| {})).unwrap();
        registered(&host);
        wait_until("the server to know the device", || {
            server.stats().registered == 1
        });
        host.stop();
        wait_until("the device to be unregistered", || {
            server.stats().registered == 0
        });
    }
}
