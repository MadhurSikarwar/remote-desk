//! The clipboard on macOS and Linux (through `arboard`).
//!
//! Unlike Windows there is no change counter to ask for, so one is made by
//! looking at the text. Content marked private by the program that copied it
//! (what password managers do on Windows) cannot be told apart here, so it is
//! treated like any other text; the clipboard switch stays off by default and
//! needs both ends to agree.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use crate::{ClipboardBackend, ClipboardError};

pub struct SystemClipboard {
    inner: Mutex<arboard::Clipboard>,
    /// What the text hashed to when last looked at.
    last: Mutex<Option<u64>>,
    counter: AtomicU32,
}

impl SystemClipboard {
    pub fn new() -> Result<Self, ClipboardError> {
        let inner =
            arboard::Clipboard::new().map_err(|e| ClipboardError::Backend(e.to_string()))?;
        Ok(Self {
            inner: Mutex::new(inner),
            last: Mutex::new(None),
            counter: AtomicU32::new(0),
        })
    }

    fn text(&self) -> Result<Option<String>, ClipboardError> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match inner.get_text() {
            Ok(text) => Ok(Some(text)),
            // Not text, or empty: nothing to sync.
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(ClipboardError::Backend(e.to_string())),
        }
    }
}

fn hash_of(text: &Option<String>) -> Option<u64> {
    text.as_ref().map(|t| {
        let mut hasher = DefaultHasher::new();
        t.hash(&mut hasher);
        hasher.finish()
    })
}

impl ClipboardBackend for SystemClipboard {
    fn sequence(&self) -> u32 {
        // A clipboard that cannot be read counts as unchanged.
        if let Ok(text) = self.text() {
            let now = hash_of(&text);
            let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
            if *last != now {
                *last = now;
                self.counter.fetch_add(1, Ordering::Relaxed);
            }
        }
        self.counter.load(Ordering::Relaxed)
    }

    fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
        self.text()
    }

    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        self.inner
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .set_text(text)
            .map_err(|e| ClipboardError::Backend(e.to_string()))
    }
}
