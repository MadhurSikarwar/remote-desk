//! Host side: shares this screen with one viewer.
//!
//! Sharing exists only between an explicit start and stop from the user.
//! While it is on, a listener waits for a viewer. Each connection then goes
//! through, in order:
//!
//! 1. an encrypted handshake in which both devices prove their keys;
//! 2. greetings (protocol version, role, device name);
//! 3. **approval by the person at this computer** — until they allow it,
//!    nothing about the screen is captured or sent, and nothing the viewer
//!    sends is acted on;
//! 4. the session: screen out, and input in if separately allowed.
//!
//! The listener binds to 127.0.0.1 only, unless the user has allowed
//! connections from other computers, in which case it listens on all
//! interfaces. Either way every viewer is encrypted and needs approval.
//!
//! Connections are taken in parallel up to a small limit, so that one that
//! stalls cannot block the others, but only one viewer at a time is asked
//! for approval or shown the screen: anyone else is told the host is busy.

use std::net::{IpAddr, Ipv4Addr, TcpListener, TcpStream};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rc_capture::{CaptureError, Capturer, DisplayInfo};
use rc_codec::nv12::{bgra_to_nv12, even, nv12_len};
use rc_codec::{EncoderConfig, H264Encoder};
use rc_crypto::channel::Side;
use rc_input::mapping::Rect;
use rc_input::Injector;
use rc_net::{Announcement, Responder};
use rc_protocol::{InputEvent, Message, Permissions, Role, PROTOCOL_VERSION};
use serde::Serialize;

pub use crate::secure::lock;
use crate::secure::{establish, exchange_hello, Peer, SecureReader, SecureWriter, Security};

/// Wait for a screen change this long before checking the stop flag again.
const POLL_MS: u32 = 100;
/// Pause between attempts while the lock screen or a UAC prompt is showing.
const BLOCKED_RETRY: Duration = Duration::from_millis(500);
const ACCEPT_POLL: Duration = Duration::from_millis(50);
const APPROVAL_POLL: Duration = Duration::from_millis(100);
/// Connections being set up at once. Each is cut off after a few seconds
/// if it does not complete the handshake.
const MAX_CONNECTIONS: usize = 8;
/// After the user refuses a viewer (or does not answer), the next one is
/// asked to wait this long, so a device cannot keep the prompt on screen.
const REFUSAL_GAP: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShareSettings {
    pub display: usize,
    /// Loopback port to listen on; 0 lets the OS choose.
    pub port: u16,
    pub max_fps: u32,
    pub bitrate_kbps: u32,
    /// Whether a viewer may control mouse and keyboard when sharing starts.
    /// The user can change it while sharing is on.
    pub allow_input: bool,
    /// Accept connections from other computers, not only this one.
    pub allow_remote: bool,
    /// While `allow_remote` is on, answer discovery queries on the network.
    pub lan_discovery: bool,
    /// The name this device gives to viewers.
    pub device_name: String,
    /// How long an approval prompt waits before the answer is "no".
    pub approval_timeout: Duration,
}

/// A viewer waiting for the person at this computer to decide.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ApprovalRequest {
    pub id: u64,
    /// What the viewer calls itself. Not proof of anything.
    pub name: String,
    /// Fingerprint of the viewer's key, which the handshake did prove.
    pub fingerprint: String,
    /// Must match the code on the viewer's screen.
    pub auth_code: String,
    /// When this key was approved before (seconds since the Unix epoch),
    /// if the user chose to remember it.
    pub known_since: Option<u64>,
    pub seconds_left: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Allow { remember: bool },
    Deny,
}

struct Approval {
    request: ApprovalRequest,
    deadline: Instant,
    decision: Option<Decision>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Allowed { remember: bool },
    Denied,
    TimedOut,
    Stopped,
}

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    viewer_connected: AtomicBool,
    frames: AtomicU64,
    bytes: AtomicU64,
    width: AtomicU32,
    height: AtomicU32,
    /// True while the desktop cannot be captured (secure desktop).
    blocked: AtomicBool,
    error: Mutex<Option<String>>,
    /// The host's switch: may the viewer control mouse and keyboard?
    input_allowed: AtomicBool,
    /// Input events acted on, and events ignored because input was off.
    input_events: AtomicU64,
    input_ignored: AtomicU64,
    approval: Mutex<Option<Approval>>,
    approval_changed: Condvar,
    next_approval_id: AtomicU64,
    /// A viewer holds the one place for approval and viewing.
    busy: AtomicBool,
    /// When the user last refused a viewer or let a request time out.
    last_refusal: Mutex<Option<Instant>>,
}

impl Shared {
    /// Status for a worker that is neither remote nor discoverable.
    #[cfg(test)]
    fn status(&self, port: u16, seconds: f64, active: bool) -> ShareStatus {
        self.status_with(port, seconds, active, (false, false, None))
    }

    fn status_with(
        &self,
        port: u16,
        seconds: f64,
        active: bool,
        (remote, discoverable, lan_address): (bool, bool, Option<String>),
    ) -> ShareStatus {
        let pending = lock(&self.approval)
            .as_ref()
            .filter(|a| active && a.decision.is_none())
            .map(|a| ApprovalRequest {
                seconds_left: a
                    .deadline
                    .saturating_duration_since(Instant::now())
                    .as_secs(),
                ..a.request.clone()
            });
        ShareStatus {
            active,
            port: Some(port),
            viewer_connected: active && self.viewer_connected.load(Ordering::Relaxed),
            width: self.width.load(Ordering::Relaxed),
            height: self.height.load(Ordering::Relaxed),
            frames: self.frames.load(Ordering::Relaxed),
            bytes: self.bytes.load(Ordering::Relaxed),
            seconds,
            blocked: self.blocked.load(Ordering::Relaxed),
            error: lock(&self.error).clone(),
            input_allowed: active && self.input_allowed.load(Ordering::Relaxed),
            input_events: self.input_events.load(Ordering::Relaxed),
            input_ignored: self.input_ignored.load(Ordering::Relaxed),
            pending,
            remote: active && remote,
            discoverable: active && discoverable,
            lan_address: lan_address.filter(|_| active && remote),
        }
    }

    /// Puts a request in front of the user and waits for the answer, the
    /// deadline, or the end of sharing — whichever comes first. The default
    /// is always "no".
    fn ask(&self, peer: &Peer, name: &str, known_since: Option<u64>, timeout: Duration) -> Verdict {
        let id = self.next_approval_id.fetch_add(1, Ordering::Relaxed) + 1;
        let deadline = Instant::now() + timeout;
        let mut slot = lock(&self.approval);
        *slot = Some(Approval {
            request: ApprovalRequest {
                id,
                name: name.to_owned(),
                fingerprint: peer.fingerprint.clone(),
                auth_code: peer.auth_code.clone(),
                known_since,
                seconds_left: timeout.as_secs(),
            },
            deadline,
            decision: None,
        });
        let verdict = loop {
            match slot.as_ref().and_then(|a| a.decision) {
                Some(Decision::Allow { remember }) => break Verdict::Allowed { remember },
                Some(Decision::Deny) => break Verdict::Denied,
                None => {}
            }
            if self.stop.load(Ordering::Relaxed) {
                break Verdict::Stopped;
            }
            if Instant::now() >= deadline {
                break Verdict::TimedOut;
            }
            slot = self
                .approval_changed
                .wait_timeout(slot, APPROVAL_POLL)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        };
        *slot = None;
        verdict
    }
}

struct Worker {
    port: u16,
    remote: bool,
    discoverable: bool,
    lan_address: Option<String>,
    /// Answers discovery queries for as long as sharing is on.
    _responder: Option<Responder>,
    started: Instant,
    shared: Arc<Shared>,
    thread: JoinHandle<()>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShareStatus {
    pub active: bool,
    pub port: Option<u16>,
    /// A viewer has been approved and is being sent the screen.
    pub viewer_connected: bool,
    pub width: u32,
    pub height: u32,
    /// Encoded frames sent since sharing started.
    pub frames: u64,
    pub bytes: u64,
    pub seconds: f64,
    /// The desktop is temporarily not capturable (lock screen, UAC prompt).
    pub blocked: bool,
    pub error: Option<String>,
    /// The viewer may control mouse and keyboard.
    pub input_allowed: bool,
    /// Input events from the viewer that were carried out.
    pub input_events: u64,
    /// Input events discarded because control was not allowed.
    pub input_ignored: u64,
    /// A viewer is waiting for the user's decision.
    pub pending: Option<ApprovalRequest>,
    /// Other computers may connect (the listener is open to the network).
    pub remote: bool,
    /// Whether discovery queries are being answered.
    pub discoverable: bool,
    /// The address other computers most likely reach this one at.
    pub lan_address: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DisplaySummary {
    pub index: usize,
    pub label: String,
}

impl From<DisplayInfo> for DisplaySummary {
    fn from(d: DisplayInfo) -> Self {
        let primary = if d.primary { ", primary" } else { "" };
        Self {
            index: d.index,
            label: format!(
                "Display {} — {}×{}{primary}",
                d.index + 1,
                d.width,
                d.height
            ),
        }
    }
}

/// Limits capture to a frame rate by scheduling against fixed deadlines.
///
/// Sleeping "the rest of the interval" after each frame drifts: Windows
/// sleeps overshoot by up to a timer tick (about 15 ms), which turned a
/// 24 fps limit into a measured 17 fps. Deadlines advance by exactly one
/// interval per frame, so an overshoot shortens the next wait instead.
struct Pacer {
    interval: Duration,
    next: Instant,
}

impl Pacer {
    fn new(max_fps: u32, now: Instant) -> Self {
        Self {
            interval: Duration::from_secs(1) / max_fps.max(1),
            next: now,
        }
    }

    /// Call after each frame; returns how long to wait before the next one.
    fn after_frame(&mut self, now: Instant) -> Duration {
        self.next += self.interval;
        match self.next.checked_duration_since(now) {
            Some(wait) => wait,
            None => {
                // Behind schedule (the screen was idle): start a new schedule
                // rather than bursting to catch up.
                self.next = now;
                Duration::ZERO
            }
        }
    }
}

fn unix_time() -> Duration {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
}

/// A connection that has finished the handshake and the greeting and has
/// the host's single place for approval and viewing.
struct Welcomed {
    writer: SecureWriter,
    reader: SecureReader,
    peer: Peer,
    name: String,
}

/// Sets up one connection: handshake, greeting, then claims the place for
/// approval. Runs on a thread of its own so that a slow or silent peer holds
/// up nobody else. A viewer that arrives while another is being asked, or
/// is viewing, is told so and turned away.
fn welcome(
    stream: &TcpStream,
    settings: &ShareSettings,
    shared: &Shared,
    security: &Security,
) -> Result<Welcomed, String> {
    let (writer, mut reader, peer) = establish(stream, security.identity(), Side::Responder)?;
    let name = exchange_hello(&writer, &mut reader, Role::Host, &settings.device_name)?;

    let turn_away = |reason: &str| {
        let _ = writer.send(&Message::Bye {
            reason: reason.to_owned(),
        });
        writer.shutdown();
        Err(reason.to_owned())
    };
    let recently_refused = lock(&shared.last_refusal).is_some_and(|t| t.elapsed() < REFUSAL_GAP);
    if recently_refused {
        return turn_away("try again in a moment");
    }
    if shared
        .busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return turn_away("the host is busy with another viewer");
    }
    Ok(Welcomed {
        writer,
        reader,
        peer,
        name,
    })
}

/// Handles one welcomed connection from approval to goodbye.
fn run_session(
    welcomed: Welcomed,
    settings: &ShareSettings,
    shared: &Arc<Shared>,
    security: &Security,
) -> Result<&'static str, String> {
    let Welcomed {
        writer,
        mut reader,
        peer,
        name,
    } = welcomed;

    // Nothing is captured, sent or accepted until the user says yes.
    let known_since = security.trust().get(&peer.public).map(|d| d.approved_at);
    tracing::info!(
        name,
        fingerprint = peer.fingerprint,
        known = known_since.is_some(),
        "viewer is asking to view the screen"
    );
    let verdict = shared.ask(&peer, &name, known_since, settings.approval_timeout);
    tracing::info!(
        name,
        fingerprint = peer.fingerprint,
        ?verdict,
        "approval decided"
    );
    let refusal = match verdict {
        Verdict::Allowed { remember } => {
            if remember {
                let saved = security
                    .trust()
                    .remember(&peer.public, &name, unix_time().as_secs());
                if let Err(e) = saved {
                    tracing::warn!(error = %e, "could not remember the device");
                }
            }
            None
        }
        Verdict::Denied => Some("the host declined the request"),
        Verdict::TimedOut => Some("the host did not answer"),
        Verdict::Stopped => Some("sharing stopped"),
    };
    if matches!(verdict, Verdict::Denied | Verdict::TimedOut) {
        *lock(&shared.last_refusal) = Some(Instant::now());
    }
    if let Some(reason) = refusal {
        let _ = writer.send(&Message::Bye {
            reason: reason.to_owned(),
        });
        writer.shutdown();
        return Ok(reason);
    }
    writer.send(&Message::Accepted)?;
    shared.viewer_connected.store(true, Ordering::Relaxed);

    let display = rc_capture::list_displays()
        .map_err(|e| e.to_string())?
        .into_iter()
        .nth(settings.display)
        .ok_or_else(|| format!("display {} does not exist", settings.display))?;
    // Shared by the reader, which injects, and this thread, which makes it
    // let go of everything when the host withdraws control.
    let injector = Arc::new(Mutex::new(Injector::new(Rect {
        x: display.x,
        y: display.y,
        width: display.width,
        height: display.height,
    })));

    // A second thread reads what the viewer sends while this one writes.
    let closed = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let keyframe_wanted = Arc::new(AtomicBool::new(false));
    let reader_thread = {
        let closed = Arc::clone(&closed);
        let finished = Arc::clone(&finished);
        let keyframe_wanted = Arc::clone(&keyframe_wanted);
        let injector = Arc::clone(&injector);
        let counters = Arc::clone(shared);
        // The permission is checked for every single event, under the same
        // lock the revocation takes, so nothing is injected after "off".
        let input = move |event: &InputEvent| {
            let mut injector = lock(&injector);
            if !counters.input_allowed.load(Ordering::Relaxed) {
                counters.input_ignored.fetch_add(1, Ordering::Relaxed);
            } else if injector.inject(event).is_ok() {
                counters.input_events.fetch_add(1, Ordering::Relaxed);
            }
        };
        thread::Builder::new()
            .name("share-reader".into())
            .spawn(move || {
                let _ = reader.poll(&finished, |message| match message {
                    Message::KeyframeRequest => {
                        keyframe_wanted.store(true, Ordering::Relaxed);
                        ControlFlow::Continue(())
                    }
                    Message::Input(event) => {
                        input(&event);
                        ControlFlow::Continue(())
                    }
                    Message::Bye { .. } => ControlFlow::Break(()),
                    _ => ControlFlow::Continue(()),
                });
                closed.store(true, Ordering::Relaxed);
            })
            .map_err(|e| e.to_string())?
    };

    let result = stream_frames(
        &writer,
        settings,
        shared,
        &closed,
        &keyframe_wanted,
        &injector,
    );
    tracing::debug!("frame loop ended");
    lock(&injector).release_all();

    let reason = match &result {
        Ok(reason) => (*reason).to_owned(),
        Err(e) => e.clone(),
    };
    let _ = writer.send(&Message::Bye { reason });
    finished.store(true, Ordering::Relaxed);
    writer.shutdown();
    tracing::debug!("waiting for the session reader");
    let _ = reader_thread.join();
    tracing::debug!("session reader finished");
    result
}

fn stream_frames(
    writer: &SecureWriter,
    settings: &ShareSettings,
    shared: &Shared,
    closed: &AtomicBool,
    keyframe_wanted: &AtomicBool,
    injector: &Mutex<Injector>,
) -> Result<&'static str, String> {
    let mut capturer = Capturer::new(settings.display).map_err(|e| e.to_string())?;
    let mut encoder: Option<H264Encoder> = None;
    let mut nv12 = Vec::new();
    let mut pacer = Pacer::new(settings.max_fps, Instant::now());
    // What the viewer has been told; `None` until the first message.
    let mut announced: Option<bool> = None;

    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return Ok("sharing stopped");
        }
        if closed.load(Ordering::Relaxed) {
            return Ok("viewer disconnected");
        }

        // Tell the viewer whenever the host's control switch changes, and
        // drop whatever the viewer was holding when it is switched off.
        let input = shared.input_allowed.load(Ordering::Relaxed);
        if announced != Some(input) {
            if !input {
                lock(injector).release_all();
            }
            writer.send(&Message::Permissions(Permissions { input }))?;
            announced = Some(input);
        }

        let frame = match capturer.next_frame(POLL_MS) {
            Ok(Some(frame)) => frame,
            Ok(None) => continue,
            Err(CaptureError::AccessDenied) => {
                shared.blocked.store(true, Ordering::Relaxed);
                thread::sleep(BLOCKED_RETRY);
                continue;
            }
            Err(e) => return Err(e.to_string()),
        };
        shared.blocked.store(false, Ordering::Relaxed);
        let captured_at = unix_time().as_micros() as u64;
        let (width, height) = (even(frame.width), even(frame.height));
        if width == 0 || height == 0 {
            continue;
        }

        // A new encoder (first frame, or the screen size changed) starts with
        // a keyframe; tell the viewer the size first.
        let wanted = EncoderConfig {
            width,
            height,
            fps: settings.max_fps,
            bitrate_kbps: settings.bitrate_kbps,
        };
        if encoder.as_ref().map(H264Encoder::config) != Some(wanted) {
            encoder = Some(H264Encoder::new(wanted).map_err(|e| e.to_string())?);
            writer.send(&Message::VideoConfig {
                width,
                height,
                fps: settings.max_fps,
            })?;
            shared.width.store(width, Ordering::Relaxed);
            shared.height.store(height, Ordering::Relaxed);
        }
        let Some(encoder) = encoder.as_mut() else {
            continue;
        };

        nv12.resize(nv12_len(width as usize, height as usize), 0);
        bgra_to_nv12(
            frame.data,
            frame.stride(),
            width as usize,
            height as usize,
            &mut nv12,
        );
        if keyframe_wanted.swap(false, Ordering::Relaxed) {
            encoder.request_keyframe().map_err(|e| e.to_string())?;
        }
        for encoded in encoder
            .encode(&nv12, captured_at)
            .map_err(|e| e.to_string())?
        {
            let bytes = encoded.data.len() as u64;
            let message = Message::VideoFrame {
                keyframe: encoded.keyframe,
                timestamp_us: encoded.timestamp_us,
                data: encoded.data,
            };
            if writer.send(&message).is_err() {
                return Ok("viewer disconnected");
            }
            shared.frames.fetch_add(1, Ordering::Relaxed);
            shared.bytes.fetch_add(bytes, Ordering::Relaxed);
        }

        thread::sleep(pacer.after_frame(Instant::now()));
    }
}

/// Counts a connection thread while it lives.
struct ConnectionSlot(Arc<AtomicUsize>);

impl Drop for ConnectionSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Accepts connections until sharing stops, each on a thread of its own.
///
/// This runs apart from the thread that handles approval and viewing:
/// while one viewer is being asked, others must still be able to connect so
/// that they can be told the host is busy.
fn accept_loop(
    listener: &TcpListener,
    settings: &ShareSettings,
    shared: &Arc<Shared>,
    security: &Arc<Security>,
    welcomed_tx: &mpsc::Sender<(Welcomed, std::net::SocketAddr)>,
) {
    let connections = Arc::new(AtomicUsize::new(0));
    while !shared.stop.load(Ordering::Relaxed) {
        let (stream, peer) = match listener.accept() {
            Ok(accepted) => accepted,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
                continue;
            }
            Err(e) => {
                tracing::error!(error = %e, "sharing stopped: cannot accept connections");
                *lock(&shared.error) = Some(e.to_string());
                // Ends the other half of `serve` too.
                shared.stop.store(true, Ordering::Relaxed);
                return;
            }
        };
        // Without permission for other computers, only this one.
        if !(settings.allow_remote || peer.ip().is_loopback())
            || stream.set_nonblocking(false).is_err()
        {
            continue;
        }
        if connections.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            connections.fetch_sub(1, Ordering::AcqRel);
            tracing::warn!(%peer, "too many connections at once; refusing");
            continue;
        }
        let slot = ConnectionSlot(Arc::clone(&connections));
        let (settings, shared, security) =
            (settings.clone(), Arc::clone(shared), Arc::clone(security));
        let tx = welcomed_tx.clone();
        tracing::info!(%peer, "connection received");
        let spawned = thread::Builder::new()
            .name("connection".into())
            .spawn(move || {
                let _slot = slot;
                match welcome(&stream, &settings, &shared, &security) {
                    Ok(welcomed) => {
                        if tx.send((welcomed, peer)).is_err() {
                            shared.busy.store(false, Ordering::Release);
                        }
                    }
                    // Failing before approval is not the user's problem and
                    // is not shown as a sharing error.
                    Err(error) => tracing::warn!(%peer, %error, "connection not accepted"),
                }
            });
        if let Err(e) = spawned {
            tracing::error!(error = %e, "cannot start a connection thread");
        }
    }
}

fn serve(
    listener: &TcpListener,
    settings: &ShareSettings,
    shared: &Arc<Shared>,
    security: &Arc<Security>,
) {
    let (welcomed_tx, welcomed_rx) = mpsc::channel::<(Welcomed, std::net::SocketAddr)>();
    thread::scope(|scope| {
        scope.spawn(|| accept_loop(listener, settings, shared, security, &welcomed_tx));

        // Approval and the session itself take place here, one at a time.
        while !shared.stop.load(Ordering::Relaxed) {
            match welcomed_rx.recv_timeout(ACCEPT_POLL) {
                Ok((welcomed, peer)) => {
                    let outcome = run_session(welcomed, settings, shared, security);
                    shared.viewer_connected.store(false, Ordering::Relaxed);
                    shared.blocked.store(false, Ordering::Relaxed);
                    shared.busy.store(false, Ordering::Release);
                    let (input_events, input_ignored) = (
                        shared.input_events.load(Ordering::Relaxed),
                        shared.input_ignored.load(Ordering::Relaxed),
                    );
                    match outcome {
                        Ok(reason) => {
                            tracing::info!(%peer, reason, input_events, input_ignored, "connection ended");
                        }
                        Err(error) => tracing::warn!(%peer, %error, "connection failed"),
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
    });
}

pub struct ShareController {
    security: Option<Arc<Security>>,
    worker: Mutex<Option<Worker>>,
}

impl ShareController {
    /// `security` is `None` when this device's keys could not be loaded;
    /// sharing is then refused.
    pub fn new(security: Option<Arc<Security>>) -> Self {
        Self {
            security,
            worker: Mutex::new(None),
        }
    }

    pub fn displays(&self) -> Result<Vec<DisplaySummary>, String> {
        rc_capture::list_displays()
            .map(|all| all.into_iter().map(Into::into).collect())
            .map_err(|e| e.to_string())
    }

    /// Starts sharing. Fails if sharing is already on, the display does not
    /// exist, the port cannot be opened, or this device has no usable key.
    pub fn start(&self, settings: ShareSettings) -> Result<ShareStatus, String> {
        let security = self
            .security
            .clone()
            .ok_or("This device's key could not be loaded, so it cannot share")?;
        let mut slot = lock(&self.worker);
        if slot.as_ref().is_some_and(|w| !w.thread.is_finished()) {
            return Err("Screen sharing is already on".to_owned());
        }
        let displays = rc_capture::list_displays().map_err(|e| e.to_string())?;
        if settings.display >= displays.len() {
            return Err(format!("display {} does not exist", settings.display));
        }

        let bind_address = if settings.allow_remote {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        let listener = TcpListener::bind((bind_address, settings.port))
            .map_err(|e| format!("cannot listen on port {}: {e}", settings.port))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();

        let shared = Arc::new(Shared::default());
        shared
            .input_allowed
            .store(settings.allow_input, Ordering::Relaxed);

        // Answer "who is sharing?" only when other computers may connect.
        let discoverable = settings.allow_remote && settings.lan_discovery;
        let responder = discoverable
            .then(|| {
                Responder::start(Announcement {
                    name: settings.device_name.clone(),
                    public_key: *security.identity().public(),
                    port,
                    protocol_version: PROTOCOL_VERSION,
                })
                .map_err(|e| tracing::warn!(error = %e, "discovery is not available"))
                .ok()
            })
            .flatten();
        let lan_address = settings
            .allow_remote
            .then(rc_net::local_address)
            .flatten()
            .map(|ip: IpAddr| ip.to_string());
        if settings.allow_remote {
            tracing::warn!(
                port,
                discoverable = responder.is_some(),
                "sharing is open to other computers on the network"
            );
        }

        let remote = settings.allow_remote;
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("share".into())
            .spawn(move || serve(&listener, &settings, &thread_shared, &security))
            .map_err(|e| e.to_string())?;

        *slot = Some(Worker {
            remote,
            discoverable: responder.is_some(),
            lan_address,
            _responder: responder,
            port,
            started: Instant::now(),
            shared,
            thread,
        });
        drop(slot);
        Ok(self.status())
    }

    /// Stops sharing, disconnecting any viewer and refusing any pending
    /// request. Safe to call when sharing is off.
    pub fn stop(&self) -> ShareStatus {
        let Some(worker) = lock(&self.worker).take() else {
            return self.status();
        };
        worker.shared.stop.store(true, Ordering::Relaxed);
        worker.shared.approval_changed.notify_all();
        let seconds = worker.started.elapsed().as_secs_f64();
        tracing::debug!("waiting for the share thread");
        let extras = Self::extras(&worker);
        let Worker {
            port,
            shared,
            thread,
            ..
        } = worker;
        if thread.join().is_err() {
            tracing::error!("share thread panicked");
        }
        // Report the totals of the run that just ended.
        shared.status_with(port, seconds, false, extras)
    }

    /// Answers the approval request with this `id`. An answer for a request
    /// that is no longer waiting (timed out, withdrawn, already answered)
    /// is an error and changes nothing.
    pub fn answer(&self, id: u64, allow: bool, remember: bool) -> Result<ShareStatus, String> {
        if let Some(worker) = lock(&self.worker).as_ref() {
            let mut slot = lock(&worker.shared.approval);
            if let Some(approval) = slot
                .as_mut()
                .filter(|a| a.request.id == id && a.decision.is_none())
            {
                approval.decision = Some(if allow {
                    Decision::Allow { remember }
                } else {
                    Decision::Deny
                });
                drop(slot);
                worker.shared.approval_changed.notify_all();
                return Ok(Self::status_of(worker));
            }
        }
        Err("That request is no longer waiting for an answer".to_owned())
    }

    /// Turns the viewer's mouse and keyboard control on or off. Takes
    /// effect immediately, also for a viewer that is already connected.
    pub fn set_input_allowed(&self, allowed: bool) -> ShareStatus {
        if let Some(worker) = lock(&self.worker).as_ref() {
            worker
                .shared
                .input_allowed
                .store(allowed, Ordering::Relaxed);
        }
        self.status()
    }

    fn extras(worker: &Worker) -> (bool, bool, Option<String>) {
        (
            worker.remote,
            worker.discoverable,
            worker.lan_address.clone(),
        )
    }

    fn status_of(worker: &Worker) -> ShareStatus {
        worker.shared.status_with(
            worker.port,
            worker.started.elapsed().as_secs_f64(),
            !worker.thread.is_finished(),
            Self::extras(worker),
        )
    }

    pub fn status(&self) -> ShareStatus {
        match lock(&self.worker).as_ref() {
            Some(worker) => Self::status_of(worker),
            None => ShareStatus {
                active: false,
                port: None,
                viewer_connected: false,
                width: 0,
                height: 0,
                frames: 0,
                bytes: 0,
                seconds: 0.0,
                blocked: false,
                error: None,
                input_allowed: false,
                input_events: 0,
                input_ignored: 0,
                pending: None,
                remote: false,
                discoverable: false,
                lan_address: None,
            },
        }
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use crate::secure::SecureReader;

    pub fn settings() -> ShareSettings {
        ShareSettings {
            display: 0,
            port: 0,
            max_fps: 30,
            bitrate_kbps: 4000,
            allow_input: false,
            allow_remote: false,
            lan_discovery: false,
            device_name: "Test host".to_owned(),
            approval_timeout: Duration::from_secs(10),
        }
    }

    pub fn wait_until(what: &str, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// Waits for the host to show an approval request and returns it.
    pub fn pending(host: &ShareController) -> ApprovalRequest {
        wait_until("an approval request", || host.status().pending.is_some());
        host.status().pending.expect("request disappeared")
    }

    /// A viewer made of the raw pieces: connected, handshake and greeting
    /// done, waiting for the host's decision.
    pub fn knock(port: u16, security: &Security, name: &str) -> (SecureWriter, SecureReader, Peer) {
        knock_at((Ipv4Addr::LOCALHOST, port).into(), security, name)
    }

    /// As [`knock`], at any address.
    pub fn knock_at(
        address: std::net::SocketAddr,
        security: &Security,
        name: &str,
    ) -> (SecureWriter, SecureReader, Peer) {
        let stream = TcpStream::connect(address).unwrap();
        let (writer, mut reader, peer) =
            establish(&stream, security.identity(), Side::Initiator).unwrap();
        exchange_hello(&writer, &mut reader, Role::Viewer, name).unwrap();
        (writer, reader, peer)
    }
}

#[cfg(test)]
mod tests;
