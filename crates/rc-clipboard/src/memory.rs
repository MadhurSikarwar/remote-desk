//! A clipboard that lives in memory, for tests.

use std::sync::{Arc, Mutex};

use crate::{locked, ClipboardBackend, ClipboardError};

#[derive(Default)]
struct State {
    text: Option<String>,
    private: bool,
    sequence: u32,
    /// How many times one write advances the sequence number; the real
    /// clipboard advances it more than once.
    bumps_per_write: u32,
}

/// A clipboard shared between clones, so a test can play the part of a user
/// copying while the code under test watches it.
#[derive(Clone, Default)]
pub struct MemoryClipboard {
    state: Arc<Mutex<State>>,
}

impl MemoryClipboard {
    pub fn new() -> Self {
        let clipboard = Self::default();
        locked(&clipboard.state).bumps_per_write = 1;
        clipboard
    }

    pub fn set_bumps_per_write(&self, bumps: u32) {
        locked(&self.state).bumps_per_write = bumps;
    }

    /// The user copies ordinary text.
    pub fn copy(&self, text: &str) {
        self.set(Some(text), false);
    }

    /// A password manager copies text and marks it private.
    pub fn copy_private(&self, text: &str) {
        self.set(Some(text), true);
    }

    /// The user copies something that is not text, such as an image.
    pub fn copy_non_text(&self) {
        self.set(None, false);
    }

    pub fn text(&self) -> Option<String> {
        locked(&self.state).text.clone()
    }

    fn set(&self, text: Option<&str>, private: bool) {
        let mut state = locked(&self.state);
        state.text = text.map(str::to_owned);
        state.private = private;
        state.sequence = state.sequence.wrapping_add(state.bumps_per_write.max(1));
    }
}

impl ClipboardBackend for MemoryClipboard {
    fn sequence(&self) -> u32 {
        locked(&self.state).sequence
    }

    fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
        let state = locked(&self.state);
        Ok(if state.private {
            None
        } else {
            state.text.clone()
        })
    }

    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        self.set(Some(text), false);
        Ok(())
    }
}
