//! Keeping the clipboard text of two computers in step.
//!
//! The rules this crate enforces, because clipboards routinely hold
//! passwords and other secrets:
//!
//! - **Nothing is sent until sync is switched on, and switching it on never
//!   sends what is already on the clipboard.** Only text copied afterwards
//!   travels.
//! - **Content that its owner marked as private is never sent.** Password
//!   managers mark what they copy with the Windows format
//!   `ExcludeClipboardContentFromMonitorProcessing`; it is honoured.
//! - **Text only, and only up to a size limit.** Images and files on the
//!   clipboard are ignored.
//! - **Text that arrived from the other side is not sent back.**
//! - **Content is never logged.** Only counts are.
//!
//! The clipboard itself sits behind [`ClipboardBackend`], so the logic can
//! be tested without touching the real one.

#![deny(unsafe_op_in_unsafe_fn)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

mod memory;
#[cfg(windows)]
mod system;
#[cfg(not(windows))]
#[path = "system_other.rs"]
mod system;

pub use memory::MemoryClipboard;
pub use system::SystemClipboard;

#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    #[error("cannot use the clipboard: {0}")]
    Backend(String),
}

/// A clipboard that can be watched and written.
pub trait ClipboardBackend: Send {
    /// A number that changes whenever the clipboard's contents change.
    fn sequence(&self) -> u32;

    /// The text on the clipboard, or `None` when there is none, when it is
    /// not text, or when its owner marked it private.
    fn read_text(&mut self) -> Result<Option<String>, ClipboardError>;

    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError>;
}

fn locked<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

struct Inner {
    backend: Mutex<Box<dyn ClipboardBackend>>,
    enabled: AtomicBool,
    stop: AtomicBool,
    max_bytes: usize,
    /// The sequence number the watcher has already dealt with.
    seen: Mutex<u32>,
    /// The last text that crossed, in either direction, so the same text is
    /// not sent twice running.
    last_text: Mutex<Option<String>>,
    sent: AtomicU64,
    received: AtomicU64,
}

/// Watches the local clipboard and applies text from the other side.
pub struct ClipboardSync {
    inner: Arc<Inner>,
    thread: Option<JoinHandle<()>>,
}

impl ClipboardSync {
    /// Starts watching. Nothing is sent until [`set_enabled`](Self::set_enabled)
    /// turns sync on. `on_local` is called, on the watcher's thread, with
    /// each piece of text copied locally that should be sent.
    pub fn start(
        backend: Box<dyn ClipboardBackend>,
        max_bytes: usize,
        poll: Duration,
        on_local: impl Fn(String) + Send + 'static,
    ) -> Self {
        let seen = backend.sequence();
        let inner = Arc::new(Inner {
            backend: Mutex::new(backend),
            enabled: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            max_bytes,
            seen: Mutex::new(seen),
            last_text: Mutex::new(None),
            sent: AtomicU64::new(0),
            received: AtomicU64::new(0),
        });
        let watcher = Arc::clone(&inner);
        let thread = thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                while !watcher.stop.load(Ordering::Relaxed) {
                    if let Some(text) = watcher.poll_once() {
                        on_local(text);
                    }
                    thread::sleep(poll);
                }
            })
            .ok();
        Self { inner, thread }
    }

    /// Turns sync on or off. Turning it on starts from the clipboard as it
    /// is now: what is on it is not sent.
    pub fn set_enabled(&self, enabled: bool) {
        // Hold the clipboard so the starting point is read atomically.
        let backend = locked(&self.inner.backend);
        if enabled && !self.inner.enabled.load(Ordering::Relaxed) {
            *locked(&self.inner.seen) = backend.sequence();
            *locked(&self.inner.last_text) = None;
        }
        self.inner.enabled.store(enabled, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.inner.enabled.load(Ordering::Relaxed)
    }

    /// Puts text that came from the other side on the clipboard. Returns
    /// `Ok(false)` without touching the clipboard when sync is off or the
    /// text is over the size limit.
    pub fn apply_remote(&self, text: &str) -> Result<bool, ClipboardError> {
        if !self.is_enabled() || text.len() > self.inner.max_bytes {
            return Ok(false);
        }
        let mut backend = locked(&self.inner.backend);
        backend.write_text(text)?;
        // Everything the write did to the sequence number is ours.
        *locked(&self.inner.seen) = backend.sequence();
        *locked(&self.inner.last_text) = Some(text.to_owned());
        self.inner.received.fetch_add(1, Ordering::Relaxed);
        Ok(true)
    }

    /// Pieces of text sent to and received from the other side.
    pub fn counts(&self) -> (u64, u64) {
        (
            self.inner.sent.load(Ordering::Relaxed),
            self.inner.received.load(Ordering::Relaxed),
        )
    }
}

impl Inner {
    /// Looks at the clipboard once. Returns text that should be sent.
    fn poll_once(&self) -> Option<String> {
        let mut backend = locked(&self.backend);
        let sequence = backend.sequence();
        {
            let mut seen = locked(&self.seen);
            if sequence == *seen {
                return None;
            }
            // Always move on, so that a change made while sync was off is
            // not sent later.
            *seen = sequence;
        }
        if !self.enabled.load(Ordering::Relaxed) {
            return None;
        }
        let text = match backend.read_text() {
            Ok(Some(text)) => text,
            Ok(None) => return None,
            Err(e) => {
                tracing::debug!(error = %e, "could not read the clipboard");
                return None;
            }
        };
        if text.is_empty() || text.len() > self.max_bytes {
            return None;
        }
        let mut last = locked(&self.last_text);
        if last.as_deref() == Some(text.as_str()) {
            return None;
        }
        *last = Some(text.clone());
        self.sent.fetch_add(1, Ordering::Relaxed);
        Some(text)
    }
}

impl Drop for ClipboardSync {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    const POLL: Duration = Duration::from_millis(10);

    fn sync(clipboard: &MemoryClipboard, max: usize) -> (ClipboardSync, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sync = ClipboardSync::start(Box::new(clipboard.clone()), max, POLL, move |text| {
            let _ = locked(&tx).send(text);
        });
        (sync, rx)
    }

    fn nothing_within(rx: &mpsc::Receiver<String>) {
        assert_eq!(
            rx.recv_timeout(Duration::from_millis(150)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );
    }

    fn next(rx: &mpsc::Receiver<String>) -> String {
        rx.recv_timeout(Duration::from_secs(2))
            .expect("nothing was sent")
    }

    #[test]
    fn nothing_is_sent_while_sync_is_off() {
        let clipboard = MemoryClipboard::new();
        let (_sync, rx) = sync(&clipboard, 1000);
        clipboard.copy("a password");
        nothing_within(&rx);
    }

    #[test]
    fn switching_sync_on_does_not_send_what_is_already_there_but_sends_later_copies() {
        let clipboard = MemoryClipboard::new();
        clipboard.copy("already on the clipboard");
        let (sync, rx) = sync(&clipboard, 1000);
        // Copied while sync was off, then sync is switched on.
        clipboard.copy("copied before switching on");
        sync.set_enabled(true);
        nothing_within(&rx);

        clipboard.copy("copied afterwards");
        assert_eq!(next(&rx), "copied afterwards");
        assert_eq!(sync.counts(), (1, 0));
    }

    #[test]
    fn text_from_the_other_side_is_applied_and_not_sent_back() {
        let clipboard = MemoryClipboard::new();
        let (sync, rx) = sync(&clipboard, 1000);
        sync.set_enabled(true);

        assert_eq!(sync.apply_remote("from afar"), Ok_(true));
        assert_eq!(clipboard.text().as_deref(), Some("from afar"));
        nothing_within(&rx);
        assert_eq!(sync.counts(), (0, 1));

        // A later local copy still goes out.
        clipboard.copy("local again");
        assert_eq!(next(&rx), "local again");
    }

    #[test]
    fn a_remote_write_that_changes_the_sequence_several_times_is_still_not_echoed() {
        let clipboard = MemoryClipboard::new();
        clipboard.set_bumps_per_write(3);
        let (sync, rx) = sync(&clipboard, 1000);
        sync.set_enabled(true);
        sync.apply_remote("x").unwrap();
        nothing_within(&rx);
    }

    #[test]
    fn remote_text_is_refused_while_sync_is_off_or_when_too_large() {
        let clipboard = MemoryClipboard::new();
        clipboard.copy("mine");
        let (sync, _rx) = sync(&clipboard, 10);
        assert_eq!(sync.apply_remote("hello"), Ok_(false));
        assert_eq!(clipboard.text().as_deref(), Some("mine"));

        sync.set_enabled(true);
        assert_eq!(sync.apply_remote("far too long for the limit"), Ok_(false));
        assert_eq!(clipboard.text().as_deref(), Some("mine"));
        assert_eq!(sync.apply_remote("fits"), Ok_(true));
    }

    #[test]
    fn private_content_oversized_text_and_non_text_are_never_sent() {
        let clipboard = MemoryClipboard::new();
        let (sync, rx) = sync(&clipboard, 20);
        sync.set_enabled(true);

        clipboard.copy_private("hunter2");
        nothing_within(&rx);
        clipboard.copy("this text is longer than twenty bytes");
        nothing_within(&rx);
        clipboard.copy_non_text();
        nothing_within(&rx);
        clipboard.copy("");
        nothing_within(&rx);

        // Ordinary text afterwards still works.
        clipboard.copy("ok");
        assert_eq!(next(&rx), "ok");
    }

    #[test]
    fn the_same_text_is_not_sent_twice_in_a_row() {
        let clipboard = MemoryClipboard::new();
        let (sync, rx) = sync(&clipboard, 1000);
        sync.set_enabled(true);
        clipboard.copy("one");
        assert_eq!(next(&rx), "one");
        clipboard.copy("one");
        nothing_within(&rx);
        clipboard.copy("two");
        assert_eq!(next(&rx), "two");
        clipboard.copy("one");
        assert_eq!(next(&rx), "one");
    }

    #[test]
    fn switching_off_and_on_again_does_not_send_what_was_copied_in_between() {
        let clipboard = MemoryClipboard::new();
        let (sync, rx) = sync(&clipboard, 1000);
        sync.set_enabled(true);
        sync.set_enabled(false);
        clipboard.copy("copied while off");
        thread::sleep(Duration::from_millis(80));
        sync.set_enabled(true);
        nothing_within(&rx);
        clipboard.copy("copied after");
        assert_eq!(next(&rx), "copied after");
    }

    #[test]
    fn dropping_the_sync_stops_the_watcher() {
        let clipboard = MemoryClipboard::new();
        let (sync, rx) = sync(&clipboard, 1000);
        sync.set_enabled(true);
        drop(sync);
        clipboard.copy("too late");
        // Nothing arrives; the watcher's channel is closed as well.
        assert!(rx.recv_timeout(Duration::from_millis(150)).is_err());
    }

    /// `assert_eq!` on `Result<bool, ClipboardError>`, which is not
    /// comparable, reads better with this.
    #[allow(non_snake_case)]
    fn Ok_(value: bool) -> OkBool {
        OkBool(value)
    }

    struct OkBool(bool);

    impl PartialEq<OkBool> for Result<bool, ClipboardError> {
        fn eq(&self, other: &OkBool) -> bool {
            matches!(self, Ok(v) if *v == other.0)
        }
    }

    impl std::fmt::Debug for OkBool {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Ok({})", self.0)
        }
    }
}
