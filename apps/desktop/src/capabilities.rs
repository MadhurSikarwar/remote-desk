//! What a session can do besides showing the screen: clipboard text and
//! files. The same code serves both ends.
//!
//! Each capability has a switch that is off until someone turns it on, and
//! messages for a capability that is off are ignored, whatever the other end
//! sends.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rc_clipboard::{ClipboardBackend, ClipboardSync, MemoryClipboard, SystemClipboard};
use rc_protocol::{Message, MAX_CLIPBOARD_BYTES};

use crate::secure::SecureWriter;
use crate::transfer::Transfers;

/// How often the clipboard is looked at for changes.
const CLIPBOARD_POLL: Duration = Duration::from_millis(250);

pub struct Capabilities {
    /// `None` when the clipboard could not be opened; sync is then simply
    /// unavailable.
    clipboard: Option<ClipboardSync>,
    pub transfers: Arc<Transfers>,
}

impl Capabilities {
    /// `folder` is where received files are saved. `clipboard` replaces the
    /// system clipboard (for tests).
    pub fn new(
        writer: Arc<SecureWriter>,
        folder: PathBuf,
        files_allowed: bool,
        clipboard: Option<MemoryClipboard>,
    ) -> Self {
        let backend: Result<Box<dyn ClipboardBackend>, String> = match clipboard {
            Some(memory) => Ok(Box::new(memory)),
            None => SystemClipboard::new()
                .map(|c| Box::new(c) as Box<dyn ClipboardBackend>)
                .map_err(|e| e.to_string()),
        };
        let clipboard = backend
            .map_err(|error| tracing::warn!(%error, "clipboard sync is unavailable"))
            .ok()
            .map(|backend| {
                let writer = Arc::clone(&writer);
                ClipboardSync::start(backend, MAX_CLIPBOARD_BYTES, CLIPBOARD_POLL, move |text| {
                    let _ = writer.send(&Message::ClipboardText(text));
                })
            });
        let transfers = Arc::new(Transfers::new(
            folder,
            files_allowed,
            Box::new(move |message| writer.send(&message)),
        ));
        Self {
            clipboard,
            transfers,
        }
    }

    /// Acts on a message that belongs to a capability. Returns whether it
    /// was one.
    pub fn handle(&self, message: &Message) -> bool {
        match message {
            Message::ClipboardText(text) => {
                if let Some(clipboard) = &self.clipboard {
                    if let Err(error) = clipboard.apply_remote(text) {
                        tracing::debug!(%error, "could not apply clipboard text");
                    }
                }
                true
            }
            Message::FileOffer { .. }
            | Message::FileAnswer { .. }
            | Message::FileChunk { .. }
            | Message::FileEnd { .. }
            | Message::FileDone { .. }
            | Message::FileCancel { .. } => {
                self.transfers.handle(message);
                true
            }
            _ => false,
        }
    }

    /// Turns clipboard sync on or off at this end.
    pub fn set_clipboard(&self, on: bool) {
        if let Some(clipboard) = &self.clipboard {
            if clipboard.is_enabled() != on {
                tracing::info!(on, "clipboard sync switched");
            }
            clipboard.set_enabled(on);
        }
    }

    pub fn clipboard_available(&self) -> bool {
        self.clipboard.is_some()
    }

    pub fn clipboard_enabled(&self) -> bool {
        self.clipboard
            .as_ref()
            .is_some_and(ClipboardSync::is_enabled)
    }

    /// Pieces of text sent and received.
    pub fn clipboard_counts(&self) -> (u64, u64) {
        self.clipboard
            .as_ref()
            .map_or((0, 0), ClipboardSync::counts)
    }

    /// Forbids file transfers at once, for the person switching them off:
    /// see [`Transfers::revoke`].
    pub fn revoke_files(&self) {
        if self.transfers.is_allowed() {
            tracing::info!(on = false, "file transfer switched");
        }
        self.transfers.revoke();
    }

    /// Allows or forbids file transfers at this end.
    pub fn set_files(&self, on: bool) {
        if self.transfers.is_allowed() != on {
            tracing::info!(on, "file transfer switched");
        }
        self.transfers.set_allowed(on);
    }

    /// Ends everything in progress; called when the session ends.
    pub fn end(&self) {
        self.set_clipboard(false);
        self.transfers.cancel_all("the session ended");
    }
}
