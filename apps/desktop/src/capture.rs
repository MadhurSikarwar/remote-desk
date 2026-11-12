//! Runs screen capture on a worker thread, only between an explicit start
//! and stop from the user.
//!
//! Frames are counted and dropped for now: nothing is encoded, stored or
//! sent until the video pipeline exists.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rc_capture::{CaptureError, Capturer, DisplayInfo};
use serde::Serialize;

/// Wait for a screen change this long before checking the stop flag again.
const POLL_MS: u32 = 100;
/// Pause between attempts while the lock screen or a UAC prompt is showing.
const BLOCKED_RETRY: Duration = Duration::from_millis(500);

#[derive(Default)]
struct Shared {
    stop: AtomicBool,
    frames: AtomicU64,
    width: AtomicU32,
    height: AtomicU32,
    /// True while the desktop cannot be captured (secure desktop).
    blocked: AtomicBool,
    error: Mutex<Option<String>>,
}

struct Worker {
    display: usize,
    started: Instant,
    shared: Arc<Shared>,
    thread: JoinHandle<()>,
}

impl Worker {
    fn snapshot(&self) -> CaptureStatus {
        CaptureStatus {
            active: !self.thread.is_finished(),
            display: Some(self.display),
            width: self.shared.width.load(Ordering::Relaxed),
            height: self.shared.height.load(Ordering::Relaxed),
            frames: self.shared.frames.load(Ordering::Relaxed),
            seconds: self.started.elapsed().as_secs_f64(),
            blocked: self.shared.blocked.load(Ordering::Relaxed),
            error: lock(&self.shared.error).clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CaptureStatus {
    pub active: bool,
    pub display: Option<usize>,
    pub width: u32,
    pub height: u32,
    pub frames: u64,
    pub seconds: f64,
    /// The desktop is temporarily not capturable (lock screen, UAC prompt).
    pub blocked: bool,
    pub error: Option<String>,
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

#[derive(Default)]
pub struct CaptureController {
    worker: Mutex<Option<Worker>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
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

fn run(mut capturer: Capturer, shared: &Shared, max_fps: u32) {
    let mut pacer = Pacer::new(max_fps, Instant::now());
    while !shared.stop.load(Ordering::Relaxed) {
        match capturer.next_frame(POLL_MS) {
            Ok(Some(frame)) => {
                shared.width.store(frame.width, Ordering::Relaxed);
                shared.height.store(frame.height, Ordering::Relaxed);
                shared.frames.fetch_add(1, Ordering::Relaxed);
                shared.blocked.store(false, Ordering::Relaxed);
                thread::sleep(pacer.after_frame(Instant::now()));
            }
            Ok(None) => {}
            Err(CaptureError::AccessDenied) => {
                shared.blocked.store(true, Ordering::Relaxed);
                thread::sleep(BLOCKED_RETRY);
            }
            Err(e) => {
                tracing::error!(error = %e, "capture stopped by an error");
                *lock(&shared.error) = Some(e.to_string());
                break;
            }
        }
    }
}

impl CaptureController {
    pub fn displays(&self) -> Result<Vec<DisplaySummary>, String> {
        rc_capture::list_displays()
            .map(|all| all.into_iter().map(Into::into).collect())
            .map_err(|e| e.to_string())
    }

    /// Starts capturing `display`. Fails if capture is already running or the
    /// display cannot be opened.
    pub fn start(&self, display: usize, max_fps: u32) -> Result<CaptureStatus, String> {
        let mut slot = lock(&self.worker);
        if slot.as_ref().is_some_and(|w| !w.thread.is_finished()) {
            return Err("Screen capture is already running".to_owned());
        }

        let shared = Arc::new(Shared::default());
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread_shared = Arc::clone(&shared);
        // The capturer is created on the thread that uses it: its Direct3D
        // objects must not move between threads.
        let thread = thread::Builder::new()
            .name("capture".into())
            .spawn(move || match Capturer::new(display) {
                Ok(capturer) => {
                    let _ = ready_tx.send(Ok(()));
                    run(capturer, &thread_shared, max_fps);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                }
            })
            .map_err(|e| e.to_string())?;

        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err("The capture thread stopped unexpectedly".to_owned()),
        }

        *slot = Some(Worker {
            display,
            started: Instant::now(),
            shared,
            thread,
        });
        drop(slot);
        Ok(self.status())
    }

    /// Stops capture and waits for the worker to finish. Safe to call when
    /// nothing is running.
    pub fn stop(&self) -> CaptureStatus {
        let Some(worker) = lock(&self.worker).take() else {
            return self.status();
        };
        worker.shared.stop.store(true, Ordering::Relaxed);
        let seconds = worker.started.elapsed().as_secs_f64();
        let mut last = worker.snapshot();
        if worker.thread.join().is_err() {
            tracing::error!("capture thread panicked");
        }
        // Report the totals of the run that just ended.
        last.frames = worker.shared.frames.load(Ordering::Relaxed);
        last.seconds = seconds;
        last.active = false;
        last
    }

    pub fn status(&self) -> CaptureStatus {
        match lock(&self.worker).as_ref() {
            Some(worker) => worker.snapshot(),
            None => CaptureStatus {
                active: false,
                display: None,
                width: 0,
                height: 0,
                frames: 0,
                seconds: 0.0,
                blocked: false,
                error: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

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
    fn idle_controller_reports_inactive_and_stop_is_harmless() {
        let controller = CaptureController::default();
        assert!(!controller.status().active);
        assert!(!controller.stop().active);
    }

    #[test]
    fn unknown_display_fails_to_start() {
        let controller = CaptureController::default();
        let error = controller.start(usize::MAX, 30).unwrap_err();
        assert!(error.contains("does not exist"), "{error}");
        assert!(!controller.status().active);
    }

    #[test]
    #[ignore = "needs an interactive desktop with a monitor"]
    fn start_capture_stop_on_the_real_desktop() {
        let controller = CaptureController::default();
        let started = controller.start(0, 30).unwrap();
        assert!(started.active);
        assert!(controller.start(0, 30).is_err(), "second start must fail");

        thread::sleep(Duration::from_millis(600));
        let running = controller.status();
        assert!(running.frames >= 1, "{running:?}");
        assert!(running.width > 0 && running.height > 0);

        let stopped = controller.stop();
        assert!(!stopped.active);
        assert!(stopped.frames >= running.frames, "{stopped:?}");
        controller.start(0, 30).unwrap();
        controller.stop();
    }
}
