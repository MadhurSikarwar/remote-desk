//! Rendezvous and relay server.
//!
//! **What it does.** Devices that are sharing register by their key. A
//! viewer asks for one by its ID; the server tells the viewer where to try a
//! direct connection and tells the device someone is coming. If direct
//! connection fails, the viewer asks for a relay and the server joins two
//! connections, one from each side, and copies bytes between them.
//!
//! **What it cannot do.** Devices run their own end-to-end handshake through
//! the relay, so the server sees only ciphertext. It cannot read or alter a
//! session, and cannot make a viewer believe it reached a device it did not:
//! the viewer checks the key it reaches against the ID it asked for. What the
//! server *can* do is refuse service, and see who talks to whom and when.
//!
//! **One port.** Registration and relaying share a TCP port, so that one
//! rule through a firewall (typically 443) serves both. A connection that
//! begins with the relay marker is a relay; anything else is a device
//! opening an encrypted control channel.
//!
//! **Limits.** Connections in total and per address, devices registered,
//! relays at once, requests per device per minute, and time allowed for
//! every stage. Nothing is stored on disk except the server's own key.

#![forbid(unsafe_code)]

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use rc_crypto::channel::{Established, Handshake, Side};
use rc_crypto::{fingerprint, fingerprint_bytes, CryptoError, Identity};
use rc_rendezvous::{
    ClientMessage, DeviceId, ServerMessage, Token, RELAY_HELLO_LEN, RELAY_MAGIC,
    RENDEZVOUS_VERSION, TOKEN_LEN,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{sleep, timeout};

/// Limits on what one server will carry.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_connections: usize,
    pub max_connections_per_address: usize,
    pub max_registered: usize,
    /// Relays waiting to be joined plus relays running.
    pub max_relays: usize,
    /// Time for a new connection to finish the handshake or send its relay
    /// hello.
    pub handshake_timeout: Duration,
    /// A device that sends nothing for this long is dropped. Devices ping
    /// well inside it.
    pub control_idle_timeout: Duration,
    /// How long the first side of a relay waits for the second.
    pub relay_pair_timeout: Duration,
    /// A relay with no traffic in either direction for this long is closed.
    pub relay_idle_timeout: Duration,
    /// Connect requests one device may make per minute.
    pub connects_per_minute: usize,
    /// How long a connect request stays valid for a later relay request.
    pub request_lifetime: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_connections: 4096,
            max_connections_per_address: 32,
            max_registered: 10_000,
            max_relays: 512,
            handshake_timeout: Duration::from_secs(5),
            control_idle_timeout: Duration::from_secs(90),
            relay_pair_timeout: Duration::from_secs(30),
            relay_idle_timeout: Duration::from_secs(15 * 60),
            connects_per_minute: 10,
            request_lifetime: Duration::from_secs(120),
        }
    }
}

pub struct Config {
    pub listen: SocketAddr,
    pub identity: Identity,
    pub limits: Limits,
}

/// Counters for monitoring and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub connections: usize,
    pub registered: usize,
    pub pending_requests: usize,
    pub relays_waiting: usize,
    pub relays_active: usize,
    pub bytes_relayed: u64,
}

struct Registration {
    conn_id: u64,
    tx: mpsc::Sender<ServerMessage>,
    address: Ipv4Addr,
    port: u16,
    lan: Vec<Ipv4Addr>,
}

struct Request {
    viewer_conn: u64,
    host_tx: mpsc::Sender<ServerMessage>,
    host_conn: u64,
    created: Instant,
    relay_requested: bool,
}

struct RelaySlot {
    created: Instant,
    waiting: Option<oneshot::Sender<TcpStream>>,
}

struct State {
    identity: Identity,
    limits: Limits,
    registry: Mutex<HashMap<DeviceId, Registration>>,
    requests: Mutex<HashMap<u64, Request>>,
    relays: Mutex<HashMap<Token, RelaySlot>>,
    per_address: Mutex<HashMap<IpAddr, usize>>,
    connections: AtomicUsize,
    relays_active: AtomicUsize,
    bytes_relayed: AtomicU64,
    next_id: AtomicU64,
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn id_text(id: &DeviceId) -> String {
    id.iter().map(|b| format!("{b:02X}")).collect()
}

/// Counts a connection, in total and for its address, until dropped.
struct Admission {
    state: Arc<State>,
    ip: IpAddr,
}

impl Admission {
    fn try_new(state: &Arc<State>, ip: IpAddr) -> Option<Self> {
        if state.connections.fetch_add(1, Ordering::AcqRel) >= state.limits.max_connections {
            state.connections.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        let mut per_address = locked(&state.per_address);
        let count = per_address.entry(ip).or_insert(0);
        if *count >= state.limits.max_connections_per_address {
            drop(per_address);
            state.connections.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        *count += 1;
        Some(Self {
            state: Arc::clone(state),
            ip,
        })
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        self.state.connections.fetch_sub(1, Ordering::AcqRel);
        let mut per_address = locked(&self.state.per_address);
        if let Some(count) = per_address.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                per_address.remove(&self.ip);
            }
        }
    }
}

// ---- encrypted channel, asynchronously --------------------------------------

async fn read_record<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    reader.read_exact(&mut len).await?;
    let mut record = vec![0u8; usize::from(u16::from_le_bytes(len))];
    reader.read_exact(&mut record).await?;
    Ok(record)
}

async fn write_record<W: AsyncWrite + Unpin>(writer: &mut W, record: &[u8]) -> io::Result<()> {
    let len = u16::try_from(record.len()).map_err(io::Error::other)?;
    let mut framed = Vec::with_capacity(2 + record.len());
    framed.extend_from_slice(&len.to_le_bytes());
    framed.extend_from_slice(record);
    writer.write_all(&framed).await
}

#[derive(Debug, thiserror::Error)]
enum HandshakeError {
    #[error("{0}")]
    Crypto(#[from] CryptoError),
    #[error("connection: {0}")]
    Io(#[from] io::Error),
}

async fn handshake(
    stream: &mut TcpStream,
    identity: &Identity,
) -> Result<Established, HandshakeError> {
    let mut handshake = Handshake::new(identity, Side::Responder)?;
    while !handshake.is_finished() {
        if handshake.is_my_turn() {
            write_record(stream, &handshake.write_message()?).await?;
        } else {
            handshake.read_message(&read_record(stream).await?)?;
        }
    }
    Ok(handshake.finish()?)
}

// ---- the server ----------------------------------------------------------------

pub struct Server {
    state: Arc<State>,
    address: SocketAddr,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl Server {
    pub async fn start(config: Config) -> io::Result<Self> {
        let listener = TcpListener::bind(config.listen).await?;
        let address = listener.local_addr()?;
        let state = Arc::new(State {
            identity: config.identity,
            limits: config.limits,
            registry: Mutex::default(),
            requests: Mutex::default(),
            relays: Mutex::default(),
            per_address: Mutex::default(),
            connections: AtomicUsize::new(0),
            relays_active: AtomicUsize::new(0),
            bytes_relayed: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
        });
        let (shutdown, shutdown_rx) = watch::channel(false);
        tracing::info!(
            %address,
            key = fingerprint(state.identity.public()),
            "server listening"
        );
        let task = tokio::spawn(run(Arc::clone(&state), listener, shutdown_rx));
        Ok(Self {
            state,
            address,
            shutdown,
            task,
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// The server's key fingerprint, which devices are configured with.
    pub fn fingerprint(&self) -> String {
        fingerprint(self.state.identity.public())
    }

    pub fn stats(&self) -> Stats {
        Stats {
            connections: self.state.connections.load(Ordering::Acquire),
            registered: locked(&self.state.registry).len(),
            pending_requests: locked(&self.state.requests).len(),
            relays_waiting: locked(&self.state.relays).len(),
            relays_active: self.state.relays_active.load(Ordering::Acquire),
            bytes_relayed: self.state.bytes_relayed.load(Ordering::Acquire),
        }
    }

    /// Stops accepting and tells every connection to end.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        let _ = self.task.await;
    }
}

async fn run(state: Arc<State>, listener: TcpListener, mut shutdown: watch::Receiver<bool>) {
    tokio::spawn(housekeeping(Arc::clone(&state), shutdown.clone()));
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(accepted) => accepted,
                    Err(e) => {
                        // Usually a transient resource shortage.
                        tracing::warn!(error = %e, "accept failed");
                        sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let Some(admission) = Admission::try_new(&state, peer.ip()) else {
                    tracing::warn!(%peer, "connection limit reached; refusing");
                    continue;
                };
                let (state, shutdown) = (Arc::clone(&state), shutdown.clone());
                tokio::spawn(async move {
                    let _admission = admission;
                    handle_connection(state, stream, peer, shutdown).await;
                });
            }
        }
    }
}

/// Drops requests and relay slots that nobody used in time.
async fn housekeeping(state: Arc<State>, mut shutdown: watch::Receiver<bool>) {
    let interval = (state.limits.request_lifetime / 4).max(Duration::from_millis(100));
    loop {
        tokio::select! {
            _ = shutdown.changed() => return,
            () = sleep(interval) => {}
        }
        let now = Instant::now();
        locked(&state.requests)
            .retain(|_, r| now.duration_since(r.created) < state.limits.request_lifetime);
        // A slot nobody has connected to is useless once its request has
        // gone; a waiting slot removes itself when its wait times out.
        locked(&state.relays).retain(|_, slot| {
            slot.waiting.is_some()
                || now.duration_since(slot.created) < state.limits.request_lifetime
        });
    }
}

async fn handle_connection(
    state: Arc<State>,
    stream: TcpStream,
    peer: SocketAddr,
    shutdown: watch::Receiver<bool>,
) {
    let _ = stream.set_nodelay(true);
    // Decide what this is from its first two bytes, which are left in the
    // stream for whoever handles it.
    let mut first = [0u8; 2];
    let sniffed = timeout(state.limits.handshake_timeout, async {
        loop {
            let n = stream.peek(&mut first).await?;
            if n >= 2 {
                return Ok::<_, io::Error>(());
            }
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    if !matches!(sniffed, Ok(Ok(()))) {
        return;
    }
    if first == RELAY_MAGIC[..2] {
        relay(state, stream, peer, shutdown).await;
    } else {
        control(state, stream, peer, shutdown).await;
    }
}

// ---- control channel -------------------------------------------------------------

/// Allows at most `limit` events per minute.
struct RateLimit {
    limit: usize,
    recent: VecDeque<Instant>,
}

impl RateLimit {
    fn allow(&mut self, now: Instant) -> bool {
        while self
            .recent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60))
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= self.limit {
            return false;
        }
        self.recent.push_back(now);
        true
    }
}

fn random_token() -> Token {
    let mut token = [0u8; TOKEN_LEN];
    // A failure here means the operating system has no randomness, which
    // makes continuing unsafe.
    getrandom::getrandom(&mut token).expect("the operating system has no random numbers");
    token
}

fn to_ipv4(ip: IpAddr) -> Option<Ipv4Addr> {
    match ip {
        IpAddr::V4(v4) => Some(v4),
        IpAddr::V6(v6) => v6.to_ipv4_mapped(),
    }
}

/// What one control connection knows about itself.
struct Session {
    conn_id: u64,
    device: DeviceId,
    address: Option<Ipv4Addr>,
    registered: bool,
    connects: RateLimit,
}

/// The outcome of one client message.
enum Outcome {
    Continue,
    /// Close after sending whatever was queued.
    Close,
}

fn handle_message(
    state: &State,
    session: &mut Session,
    me: &mpsc::Sender<ServerMessage>,
    message: ClientMessage,
) -> Outcome {
    let reply = |message: ServerMessage| {
        let _ = me.try_send(message);
    };
    let fail = |reason: &str| {
        let _ = me.try_send(ServerMessage::Error {
            reason: reason.to_owned(),
        });
        Outcome::Continue
    };

    match message {
        ClientMessage::Ping => reply(ServerMessage::Pong),

        ClientMessage::Register {
            version,
            port,
            lan,
            name,
        } => {
            if version != RENDEZVOUS_VERSION {
                reply(ServerMessage::Error {
                    reason: "unsupported protocol version".to_owned(),
                });
                return Outcome::Close;
            }
            let Some(address) = session.address else {
                reply(ServerMessage::Error {
                    reason: "only IPv4 devices can register".to_owned(),
                });
                return Outcome::Close;
            };
            if port == 0 {
                return fail("port must not be 0");
            }
            let mut registry = locked(&state.registry);
            if !registry.contains_key(&session.device)
                && registry.len() >= state.limits.max_registered
            {
                drop(registry);
                reply(ServerMessage::Error {
                    reason: "the server is full".to_owned(),
                });
                return Outcome::Close;
            }
            let previous = registry.insert(
                session.device,
                Registration {
                    conn_id: session.conn_id,
                    tx: me.clone(),
                    address,
                    port,
                    lan,
                },
            );
            drop(registry);
            if let Some(previous) = previous.filter(|p| p.conn_id != session.conn_id) {
                let _ = previous.tx.try_send(ServerMessage::Replaced);
            }
            session.registered = true;
            tracing::info!(device = id_text(&session.device), %address, port, name, "device registered");
            reply(ServerMessage::Registered { address });
        }

        ClientMessage::Connect { target, name } => {
            let Some(viewer_address) = session.address else {
                return fail("only IPv4 devices can connect");
            };
            if !session.connects.allow(Instant::now()) {
                return fail("too many requests; wait a minute");
            }
            let found = locked(&state.registry)
                .get(&target)
                .map(|r| (r.tx.clone(), r.conn_id, r.address, r.port, r.lan.clone()));
            let Some((host_tx, host_conn, host_address, port, lan)) = found else {
                return {
                    reply(ServerMessage::NotFound { target });
                    Outcome::Continue
                };
            };
            let request_id = state.next_id.fetch_add(1, Ordering::Relaxed);
            let mut requests = locked(&state.requests);
            if requests.len() >= state.limits.max_registered {
                drop(requests);
                return fail("the server is busy");
            }
            requests.insert(
                request_id,
                Request {
                    viewer_conn: session.conn_id,
                    host_tx: host_tx.clone(),
                    host_conn,
                    created: Instant::now(),
                    relay_requested: false,
                },
            );
            drop(requests);
            let told = host_tx.try_send(ServerMessage::Incoming {
                request_id,
                viewer: session.device,
                name,
            });
            if told.is_err() {
                locked(&state.requests).remove(&request_id);
                return fail("that device is not answering");
            }
            tracing::info!(
                viewer = id_text(&session.device),
                host = id_text(&target),
                request_id,
                "connect requested"
            );
            reply(ServerMessage::Peer {
                request_id,
                address: host_address,
                port,
                lan,
                same_network: host_address == viewer_address,
            });
        }

        ClientMessage::UseRelay { request_id } => {
            let mut requests = locked(&state.requests);
            let Some(request) = requests
                .get_mut(&request_id)
                .filter(|r| r.viewer_conn == session.conn_id)
            else {
                drop(requests);
                return fail("unknown request");
            };
            if request.relay_requested {
                drop(requests);
                return fail("a relay was already requested");
            }
            let mut relays = locked(&state.relays);
            if relays.len() + state.relays_active.load(Ordering::Acquire) >= state.limits.max_relays
            {
                drop(relays);
                drop(requests);
                return fail("relay capacity reached");
            }
            let token = random_token();
            let told_host = request
                .host_tx
                .try_send(ServerMessage::RelayRequest { request_id, token });
            if told_host.is_err() {
                drop(relays);
                drop(requests);
                return fail("that device is not answering");
            }
            relays.insert(
                token,
                RelaySlot {
                    created: Instant::now(),
                    waiting: None,
                },
            );
            request.relay_requested = true;
            drop(relays);
            drop(requests);
            tracing::info!(request_id, "relay granted");
            reply(ServerMessage::RelayToken { request_id, token });
        }
    }
    Outcome::Continue
}

fn forget(state: &State, session: &Session) {
    if session.registered {
        let mut registry = locked(&state.registry);
        if registry
            .get(&session.device)
            .is_some_and(|r| r.conn_id == session.conn_id)
        {
            registry.remove(&session.device);
            tracing::info!(device = id_text(&session.device), "device unregistered");
        }
    }
    locked(&state.requests)
        .retain(|_, r| r.viewer_conn != session.conn_id && r.host_conn != session.conn_id);
}

async fn control(
    state: Arc<State>,
    mut stream: TcpStream,
    peer: SocketAddr,
    mut shutdown: watch::Receiver<bool>,
) {
    let established = match timeout(
        state.limits.handshake_timeout,
        handshake(&mut stream, &state.identity),
    )
    .await
    {
        Ok(Ok(established)) => established,
        Ok(Err(e)) => {
            tracing::debug!(%peer, error = %e, "handshake failed");
            return;
        }
        Err(_) => {
            tracing::debug!(%peer, "handshake timed out");
            return;
        }
    };
    let Established {
        remote_public,
        mut sealer,
        mut opener,
        ..
    } = established;

    let mut session = Session {
        conn_id: state.next_id.fetch_add(1, Ordering::Relaxed),
        device: fingerprint_bytes(&remote_public),
        address: to_ipv4(peer.ip()),
        registered: false,
        connects: RateLimit {
            limit: state.limits.connects_per_minute,
            recent: VecDeque::new(),
        },
    };
    let (outbox, mut inbox) = mpsc::channel::<ServerMessage>(32);
    let (mut reader, mut writer) = stream.into_split();
    let mut buffer = vec![0u8; 16 * 1024];
    let mut closing = false;

    'connection: loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            () = sleep(state.limits.control_idle_timeout) => {
                tracing::debug!(%peer, "idle; closing");
                break;
            }
            read = reader.read(&mut buffer) => {
                let n = match read {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                opener.push(&buffer[..n]);
                loop {
                    let plain = match opener.next_plaintext() {
                        Ok(Some(plain)) => plain,
                        Ok(None) => break,
                        Err(e) => {
                            tracing::debug!(%peer, error = %e, "bad record; closing");
                            break 'connection;
                        }
                    };
                    let message = match ClientMessage::decode(&plain) {
                        Ok(message) => message,
                        Err(e) => {
                            tracing::debug!(%peer, error = %e, "bad message; closing");
                            break 'connection;
                        }
                    };
                    if matches!(
                        handle_message(&state, &mut session, &outbox, message),
                        Outcome::Close
                    ) {
                        closing = true;
                    }
                }
                // Send what the handlers queued, so replies keep their order.
                while let Ok(message) = inbox.try_recv() {
                    if !send(&mut writer, &mut sealer, &message).await {
                        break 'connection;
                    }
                }
                if closing {
                    break;
                }
            }
            Some(message) = inbox.recv() => {
                let replaced = message == ServerMessage::Replaced;
                if !send(&mut writer, &mut sealer, &message).await || replaced {
                    break;
                }
            }
        }
    }
    forget(&state, &session);
}

async fn send(
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    sealer: &mut rc_crypto::channel::Sealer,
    message: &ServerMessage,
) -> bool {
    let Ok(sealed) = sealer.seal(&message.encode()) else {
        return false;
    };
    matches!(
        timeout(Duration::from_secs(10), writer.write_all(&sealed)).await,
        Ok(Ok(()))
    )
}

// ---- relay ---------------------------------------------------------------------------

/// A stream that records when it was last used and how much came through.
struct Metered {
    inner: TcpStream,
    last_activity: Arc<AtomicU64>,
    started: Instant,
    bytes: Arc<AtomicU64>,
}

impl Metered {
    fn touch(&self) {
        let ms = self.started.elapsed().as_millis() as u64;
        self.last_activity.store(ms, Ordering::Relaxed);
    }
}

impl AsyncRead for Metered {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        if let Poll::Ready(Ok(())) = &result {
            let read = buf.filled().len() - before;
            if read > 0 {
                self.bytes.fetch_add(read as u64, Ordering::Relaxed);
                self.touch();
            }
        }
        result
    }
}

impl AsyncWrite for Metered {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(n)) = &result {
            if *n > 0 {
                self.touch();
            }
        }
        result
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

async fn relay(
    state: Arc<State>,
    mut stream: TcpStream,
    peer: SocketAddr,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut hello = [0u8; RELAY_HELLO_LEN];
    let read = timeout(
        state.limits.handshake_timeout,
        stream.read_exact(&mut hello),
    )
    .await;
    if !matches!(read, Ok(Ok(_))) || !hello.starts_with(RELAY_MAGIC) {
        return;
    }
    let mut token = [0u8; TOKEN_LEN];
    token.copy_from_slice(&hello[RELAY_MAGIC.len()..]);

    // The first side to arrive waits; the second hands over its stream.
    let waiting = {
        let mut relays = locked(&state.relays);
        match relays.get_mut(&token) {
            None => {
                tracing::debug!(%peer, "relay connection with an unknown token");
                return;
            }
            Some(slot) if slot.waiting.is_some() => {
                let Some(first) = relays.remove(&token).and_then(|s| s.waiting) else {
                    return;
                };
                drop(relays);
                let _ = first.send(stream);
                return;
            }
            Some(slot) => {
                let (tx, rx) = oneshot::channel();
                slot.waiting = Some(tx);
                rx
            }
        }
    };

    let other = tokio::select! {
        _ = shutdown.changed() => None,
        joined = timeout(state.limits.relay_pair_timeout, waiting) => joined.ok().and_then(Result::ok),
    };
    let Some(other) = other else {
        locked(&state.relays).remove(&token);
        tracing::debug!(%peer, "relay was never joined");
        return;
    };

    state.relays_active.fetch_add(1, Ordering::AcqRel);
    let started = Instant::now();
    let last_activity = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let meter = |inner| Metered {
        inner,
        last_activity: Arc::clone(&last_activity),
        started,
        bytes: Arc::clone(&bytes),
    };
    let (mut a, mut b) = (meter(stream), meter(other));
    tracing::info!("relay started");

    let idle_limit = state.limits.relay_idle_timeout.as_millis() as u64;
    let check_every = (state.limits.relay_idle_timeout / 4)
        .clamp(Duration::from_millis(50), Duration::from_secs(5));
    let copying = tokio::io::copy_bidirectional(&mut a, &mut b);
    tokio::pin!(copying);
    let reason = loop {
        tokio::select! {
            _ = shutdown.changed() => break "server shutting down",
            result = &mut copying => break if result.is_ok() { "closed" } else { "connection error" },
            () = sleep(check_every) => {
                let idle = (started.elapsed().as_millis() as u64).saturating_sub(last_activity.load(Ordering::Relaxed));
                if idle >= idle_limit {
                    break "idle";
                }
            }
        }
    };
    state.relays_active.fetch_sub(1, Ordering::AcqRel);
    let total = bytes.load(Ordering::Relaxed);
    state.bytes_relayed.fetch_add(total, Ordering::AcqRel);
    tracing::info!(
        reason,
        bytes = total,
        seconds = started.elapsed().as_secs(),
        "relay ended"
    );
}
