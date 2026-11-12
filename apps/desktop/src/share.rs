//! Host side: shares this screen with one viewer.
//!
//! Sharing exists only between an explicit start and stop from the user.
//! While it is on, a listener waits on the loopback interface; when a viewer
//! connects, the screen is captured, encoded to H.264 and streamed until the
//! viewer leaves or sharing stops.
//!
//! The listener binds to 127.0.0.1 only. The stream is not encrypted yet, so
//! it must not be reachable from another machine.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rc_capture::{CaptureError, Capturer, DisplayInfo};
use rc_codec::nv12::{bgra_to_nv12, even, nv12_len};
use rc_codec::{EncoderConfig, H264Encoder};
use rc_input::mapping::Rect;
use rc_input::Injector;
use rc_protocol::{
    read_message, write_message, Message, MessageDecoder, Permissions, Role, PROTOCOL_VERSION,
};
use serde::Serialize;

/// Wait for a screen change this long before checking the stop flag again.
const POLL_MS: u32 = 100;
/// Pause between attempts while the lock screen or a UAC prompt is showing.
const BLOCKED_RETRY: Duration = Duration::from_millis(500);
const ACCEPT_POLL: Duration = Duration::from_millis(50);
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
/// How often a reader thread wakes up to check whether it should stop.
const READ_POLL: Duration = Duration::from_millis(200);

pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareSettings {
    pub display: usize,
    /// Loopback port to listen on; 0 lets the OS choose.
    pub port: u16,
    pub max_fps: u32,
    pub bitrate_kbps: u32,
    /// Whether a viewer may control mouse and keyboard when sharing starts.
    /// The user can change it while sharing is on.
    pub allow_input: bool,
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
}

struct Worker {
    port: u16,
    started: Instant,
    shared: Arc<Shared>,
    thread: JoinHandle<()>,
}

impl Shared {
    fn status(&self, port: u16, seconds: f64, active: bool) -> ShareStatus {
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
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShareStatus {
    pub active: bool,
    pub port: Option<u16>,
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

/// Host half of the handshake: announce ourselves, then require a viewer
/// speaking the same protocol version.
pub fn host_handshake<S: Read + Write>(stream: &mut S) -> Result<(), String> {
    let hello = Message::Hello {
        version: PROTOCOL_VERSION,
        role: Role::Host,
    };
    write_message(stream, &hello).map_err(|e| e.to_string())?;
    match read_message(stream).map_err(|e| e.to_string())? {
        Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Viewer,
        } => Ok(()),
        Message::Hello { version, role } => {
            let reason = format!("unsupported peer: protocol version {version}, role {role:?}");
            let _ = write_message(
                stream,
                &Message::Bye {
                    reason: reason.clone(),
                },
            );
            Err(reason)
        }
        other => Err(format!("expected a greeting, got {other:?}")),
    }
}

/// Reads messages from `stream` and passes them to `handle` until it breaks,
/// the peer closes the connection, or `stop` is set.
///
/// The read wakes every [`READ_POLL`] to look at `stop`. A blocking read
/// cannot be relied on to end when the socket is shut down from another
/// thread: on Windows it stays blocked until the peer closes its side, which
/// once left "Stop sharing" hanging for as long as the viewer stayed open.
pub fn poll_messages(
    stream: &TcpStream,
    stop: &AtomicBool,
    mut handle: impl FnMut(Message) -> ControlFlow<()>,
) -> Result<(), String> {
    stream
        .set_read_timeout(Some(READ_POLL))
        .map_err(|e| e.to_string())?;
    let mut decoder = MessageDecoder::default();
    let mut chunk = vec![0u8; 64 * 1024];
    let mut reader = stream;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                decoder.push(&chunk[..n]);
                while let Some(message) = decoder.next_message().map_err(|e| e.to_string())? {
                    if handle(message).is_break() {
                        return Ok(());
                    }
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn now_micros() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_micros() as u64)
}

/// Streams the screen to one connected viewer until it leaves, sharing is
/// stopped, or something fails.
fn run_session(
    stream: &TcpStream,
    settings: ShareSettings,
    shared: &Arc<Shared>,
) -> Result<&'static str, String> {
    stream.set_nodelay(true).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
        .and_then(|()| stream.set_write_timeout(Some(WRITE_TIMEOUT)))
        .map_err(|e| e.to_string())?;
    host_handshake(&mut &*stream)?;
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

    // A second handle reads what the viewer sends while this thread writes.
    let closed = Arc::new(AtomicBool::new(false));
    let finished = Arc::new(AtomicBool::new(false));
    let keyframe_wanted = Arc::new(AtomicBool::new(false));
    let reader = {
        let stream = stream.try_clone().map_err(|e| e.to_string())?;
        let closed = Arc::clone(&closed);
        let finished = Arc::clone(&finished);
        let keyframe_wanted = Arc::clone(&keyframe_wanted);
        let injector = Arc::clone(&injector);
        let counters = Arc::clone(shared);
        // The permission is checked for every single event, under the same
        // lock the revocation takes, so nothing is injected after "off".
        let input = move |event: &rc_protocol::InputEvent| {
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
                let _ = poll_messages(&stream, &finished, |message| match message {
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
        stream,
        settings,
        shared,
        &closed,
        &keyframe_wanted,
        &injector,
    );
    lock(&injector).release_all();

    let reason = match &result {
        Ok(reason) => (*reason).to_owned(),
        Err(e) => e.clone(),
    };
    let _ = write_message(&mut &*stream, &Message::Bye { reason });
    finished.store(true, Ordering::Relaxed);
    let _ = stream.shutdown(Shutdown::Both);
    let _ = reader.join();
    result
}

fn stream_frames(
    stream: &TcpStream,
    settings: ShareSettings,
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
            let message = Message::Permissions(Permissions { input });
            write_message(&mut &*stream, &message).map_err(|e| e.to_string())?;
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
        let captured_at = now_micros();
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
            let config = Message::VideoConfig {
                width,
                height,
                fps: settings.max_fps,
            };
            write_message(&mut &*stream, &config).map_err(|e| e.to_string())?;
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
            if write_message(&mut &*stream, &message).is_err() {
                return Ok("viewer disconnected");
            }
            shared.frames.fetch_add(1, Ordering::Relaxed);
            shared.bytes.fetch_add(bytes, Ordering::Relaxed);
        }

        thread::sleep(pacer.after_frame(Instant::now()));
    }
}

fn serve(listener: &TcpListener, settings: ShareSettings, shared: &Arc<Shared>) {
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
                return;
            }
        };
        // The listener is bound to loopback; this guards against that ever
        // changing without encryption being in place.
        if !peer.ip().is_loopback() || stream.set_nonblocking(false).is_err() {
            continue;
        }

        tracing::info!(%peer, "viewer connected");
        shared.viewer_connected.store(true, Ordering::Relaxed);
        let outcome = run_session(&stream, settings, shared);
        shared.viewer_connected.store(false, Ordering::Relaxed);
        shared.blocked.store(false, Ordering::Relaxed);
        let (input_events, input_ignored) = (
            shared.input_events.load(Ordering::Relaxed),
            shared.input_ignored.load(Ordering::Relaxed),
        );
        match outcome {
            Ok(reason) => {
                tracing::info!(%peer, reason, input_events, input_ignored, "viewer session ended");
            }
            Err(error) => {
                tracing::warn!(%peer, %error, "viewer session failed");
                *lock(&shared.error) = Some(error);
            }
        }
    }
}

#[derive(Default)]
pub struct ShareController {
    worker: Mutex<Option<Worker>>,
}

impl ShareController {
    pub fn displays(&self) -> Result<Vec<DisplaySummary>, String> {
        rc_capture::list_displays()
            .map(|all| all.into_iter().map(Into::into).collect())
            .map_err(|e| e.to_string())
    }

    /// Starts sharing. Fails if sharing is already on, the display does not
    /// exist, or the port cannot be opened.
    pub fn start(&self, settings: ShareSettings) -> Result<ShareStatus, String> {
        let mut slot = lock(&self.worker);
        if slot.as_ref().is_some_and(|w| !w.thread.is_finished()) {
            return Err("Screen sharing is already on".to_owned());
        }
        let displays = rc_capture::list_displays().map_err(|e| e.to_string())?;
        if settings.display >= displays.len() {
            return Err(format!("display {} does not exist", settings.display));
        }

        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, settings.port))
            .map_err(|e| format!("cannot listen on port {}: {e}", settings.port))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();

        let shared = Arc::new(Shared::default());
        shared
            .input_allowed
            .store(settings.allow_input, Ordering::Relaxed);
        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("share".into())
            .spawn(move || serve(&listener, settings, &thread_shared))
            .map_err(|e| e.to_string())?;

        *slot = Some(Worker {
            port,
            started: Instant::now(),
            shared,
            thread,
        });
        drop(slot);
        Ok(self.status())
    }

    /// Stops sharing, disconnecting any viewer. Safe to call when sharing is
    /// off.
    pub fn stop(&self) -> ShareStatus {
        let Some(worker) = lock(&self.worker).take() else {
            return self.status();
        };
        worker.shared.stop.store(true, Ordering::Relaxed);
        let seconds = worker.started.elapsed().as_secs_f64();
        if worker.thread.join().is_err() {
            tracing::error!("share thread panicked");
        }
        // Report the totals of the run that just ended.
        worker.shared.status(worker.port, seconds, false)
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

    pub fn status(&self) -> ShareStatus {
        match lock(&self.worker).as_ref() {
            Some(worker) => worker.shared.status(
                worker.port,
                worker.started.elapsed().as_secs_f64(),
                !worker.thread.is_finished(),
            ),
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
            },
        }
    }
}

#[cfg(test)]
pub mod test_support {
    use std::io::{Cursor, Read, Write};

    use rc_protocol::{read_message, write_message, Message};

    /// An in-memory connection: reads come from `input`, writes are kept.
    pub struct Duplex {
        pub input: Cursor<Vec<u8>>,
        pub output: Vec<u8>,
    }

    impl Duplex {
        pub fn with_incoming(messages: &[Message]) -> Self {
            let mut input = Vec::new();
            for message in messages {
                write_message(&mut input, message).unwrap();
            }
            Self {
                input: Cursor::new(input),
                output: Vec::new(),
            }
        }

        pub fn sent(&self) -> Vec<Message> {
            let mut reader = self.output.as_slice();
            std::iter::from_fn(|| read_message(&mut reader).ok()).collect()
        }
    }

    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.input.read(buf)
        }
    }

    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.output.write(buf)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::Duplex;
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn settings() -> ShareSettings {
        ShareSettings {
            display: 0,
            port: 0,
            max_fps: 30,
            bitrate_kbps: 4000,
            allow_input: false,
        }
    }

    #[test]
    fn pacer_waits_for_the_rest_of_the_interval() {
        let start = Instant::now();
        let mut pacer = Pacer::new(10, start);
        assert_eq!(pacer.after_frame(start + 30 * MS), 70 * MS);
    }

    #[test]
    fn pacer_makes_up_for_an_overslept_wait() {
        let start = Instant::now();
        let mut pacer = Pacer::new(10, start);
        assert_eq!(pacer.after_frame(start), 100 * MS);
        // The 100 ms sleep took 115 ms; the next wait is 15 ms shorter.
        assert_eq!(pacer.after_frame(start + 115 * MS), 85 * MS);
    }

    #[test]
    fn pacer_restarts_after_an_idle_gap_without_bursting() {
        let start = Instant::now();
        let mut pacer = Pacer::new(10, start);
        assert_eq!(pacer.after_frame(start + 5000 * MS), Duration::ZERO);
        assert_eq!(pacer.after_frame(start + 5000 * MS), 100 * MS);
    }

    #[test]
    fn pacer_tolerates_zero_fps() {
        let start = Instant::now();
        assert_eq!(
            Pacer::new(0, start).after_frame(start),
            Duration::from_secs(1)
        );
    }

    #[test]
    fn host_handshake_accepts_a_matching_viewer() {
        let mut peer = Duplex::with_incoming(&[Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Viewer,
        }]);
        host_handshake(&mut peer).unwrap();
        assert_eq!(
            peer.sent(),
            [Message::Hello {
                version: PROTOCOL_VERSION,
                role: Role::Host
            }]
        );
    }

    #[test]
    fn host_handshake_refuses_other_versions_roles_and_messages() {
        for incoming in [
            Message::Hello {
                version: PROTOCOL_VERSION + 1,
                role: Role::Viewer,
            },
            Message::Hello {
                version: PROTOCOL_VERSION,
                role: Role::Host,
            },
            Message::KeyframeRequest,
        ] {
            let mut peer = Duplex::with_incoming(std::slice::from_ref(&incoming));
            assert!(host_handshake(&mut peer).is_err(), "{incoming:?}");
        }
        let mut silent = Duplex::with_incoming(&[]);
        assert!(host_handshake(&mut silent).is_err());
    }

    #[test]
    fn idle_controller_reports_inactive_and_stop_is_harmless() {
        let controller = ShareController::default();
        assert!(!controller.status().active);
        assert!(!controller.stop().active);
    }

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn unknown_display_fails_to_start() {
        let controller = ShareController::default();
        let error = controller
            .start(ShareSettings {
                display: usize::MAX,
                ..settings()
            })
            .unwrap_err();
        assert!(error.contains("does not exist"), "{error}");
        assert!(!controller.status().active);
    }

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn stop_returns_promptly_while_a_silent_viewer_stays_connected() {
        let controller = ShareController::default();
        let port = controller.start(settings()).unwrap().port.unwrap();

        // A viewer that greets and then neither reads, writes nor closes.
        let mut viewer = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        read_message(&mut viewer).unwrap();
        let hello = Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Viewer,
        };
        write_message(&mut viewer, &hello).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !controller.status().viewer_connected {
            assert!(Instant::now() < deadline, "viewer never registered");
            thread::sleep(Duration::from_millis(20));
        }

        let before = Instant::now();
        assert!(!controller.stop().active);
        let took = before.elapsed();
        assert!(took < Duration::from_secs(2), "stop took {took:?}");
        drop(viewer);
    }

    /// Pointer position in physical pixels.
    fn pointer() -> (i32, i32) {
        use windows::Win32::Foundation::POINT;
        use windows::Win32::UI::HiDpi::{
            SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        };
        use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
        let mut point = POINT::default();
        // SAFETY: the out-pointer references a live local; the DPI context
        // change affects this test thread only.
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            GetCursorPos(&mut point).unwrap();
        }
        (point.x, point.y)
    }

    fn shift_is_down() -> bool {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
        // SAFETY: plain call without pointers.
        unsafe { GetAsyncKeyState(i32::from(VK_SHIFT.0)) < 0 }
    }

    /// Reads from the host until it announces permissions.
    fn next_permissions(viewer: &mut TcpStream) -> bool {
        loop {
            if let Message::Permissions(permissions) = read_message(viewer).unwrap() {
                return permissions.input;
            }
        }
    }

    fn wait_until(what: &str, condition: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    #[ignore = "moves the real mouse pointer and presses Shift briefly"]
    fn input_works_only_while_the_host_allows_it() {
        use rc_protocol::InputEvent;

        let display = rc_capture::list_displays().unwrap().remove(0);
        let centre = (
            display.x + display.width as i32 / 2,
            display.y + display.height as i32 / 2,
        );
        let to_centre = Message::Input(InputEvent::MouseMove { x: 32768, y: 32768 });
        let to_corner = Message::Input(InputEvent::MouseMove { x: 0, y: 0 });
        let shift = |down| {
            Message::Input(InputEvent::Key {
                scancode: 0x2A,
                extended: false,
                down,
            })
        };
        assert!(
            !shift_is_down(),
            "Shift is already held; cannot run this test"
        );

        let controller = ShareController::default();
        let port = controller.start(settings()).unwrap().port.unwrap();
        let mut viewer = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        viewer
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        read_message(&mut viewer).unwrap();
        let hello = Message::Hello {
            version: PROTOCOL_VERSION,
            role: Role::Viewer,
        };
        write_message(&mut viewer, &hello).unwrap();
        let original = pointer();
        // Park the pointer away from both targets so a move is detectable.
        let parked = (centre.0 / 2, centre.1 / 2);
        // SAFETY: plain call; the pointer is restored at the end.
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::SetCursorPos(parked.0, parked.1).unwrap();
        }
        let parked = pointer();

        // 1. Off by default: the viewer is told, and its input does nothing.
        assert!(!next_permissions(&mut viewer));
        write_message(&mut viewer, &to_centre).unwrap();
        write_message(&mut viewer, &shift(true)).unwrap();
        wait_until("ignored input", || controller.status().input_ignored == 2);
        let while_off = (pointer(), shift_is_down(), controller.status().input_events);

        // 2. The host allows control: the viewer is told, and input works.
        assert!(controller.set_input_allowed(true).input_allowed);
        assert!(next_permissions(&mut viewer));
        write_message(&mut viewer, &to_centre).unwrap();
        write_message(&mut viewer, &shift(true)).unwrap();
        wait_until("accepted input", || controller.status().input_events == 2);
        thread::sleep(Duration::from_millis(30));
        let while_on = (pointer(), shift_is_down());

        // 3. The host withdraws control: the held key is let go, and
        //    further input does nothing.
        assert!(!controller.set_input_allowed(false).input_allowed);
        assert!(!next_permissions(&mut viewer));
        thread::sleep(Duration::from_millis(30));
        let shift_after_revoke = shift_is_down();
        write_message(&mut viewer, &to_corner).unwrap();
        wait_until("ignored input", || controller.status().input_ignored == 3);
        let after_revoke = pointer();

        controller.stop();
        // SAFETY: plain call; puts the pointer back where the user had it.
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::SetCursorPos(original.0, original.1).unwrap();
        }

        assert_eq!(while_off, (parked, false, 0), "input acted on while off");
        let near = |a: (i32, i32), b: (i32, i32)| (a.0 - b.0).abs() <= 1 && (a.1 - b.1).abs() <= 1;
        assert!(near(while_on.0, centre), "pointer at {:?}", while_on.0);
        assert!(while_on.1, "Shift did not register while control was on");
        assert!(
            !shift_after_revoke,
            "Shift stayed down after control was withdrawn"
        );
        assert!(
            near(after_revoke, centre),
            "pointer moved after control was withdrawn"
        );
    }

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn sharing_listens_on_loopback_only_and_refuses_a_second_start() {
        let controller = ShareController::default();
        let status = controller.start(settings()).unwrap();
        let port = status.port.unwrap();
        assert!(status.active && port != 0 && !status.viewer_connected);
        assert!(controller.start(settings()).is_err());

        // Reachable on loopback…
        TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        // …and the port is released once sharing stops.
        assert!(!controller.stop().active);
        assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
    }
}
