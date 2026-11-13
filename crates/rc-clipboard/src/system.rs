//! The Windows clipboard.

use std::sync::OnceLock;

use windows::core::w;
use windows::Win32::System::DataExchange::{
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, RegisterClipboardFormatW,
};

use crate::{ClipboardBackend, ClipboardError};

/// The format password managers and some other programs add to what they
/// copy, to say it must not be recorded by clipboard history or monitors.
fn exclude_format() -> u32 {
    static FORMAT: OnceLock<u32> = OnceLock::new();
    *FORMAT.get_or_init(|| {
        // SAFETY: the argument is a constant, NUL-terminated wide string.
        unsafe { RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")) }
    })
}

/// True when the clipboard's current content is marked private.
pub fn content_is_private() -> bool {
    let format = exclude_format();
    // SAFETY: plain call with a format number; no clipboard handle needed.
    format != 0 && unsafe { IsClipboardFormatAvailable(format) }.is_ok()
}

pub struct SystemClipboard {
    inner: arboard::Clipboard,
}

impl SystemClipboard {
    pub fn new() -> Result<Self, ClipboardError> {
        let inner =
            arboard::Clipboard::new().map_err(|e| ClipboardError::Backend(e.to_string()))?;
        Ok(Self { inner })
    }
}

impl ClipboardBackend for SystemClipboard {
    fn sequence(&self) -> u32 {
        // SAFETY: plain call without arguments.
        unsafe { GetClipboardSequenceNumber() }
    }

    fn read_text(&mut self) -> Result<Option<String>, ClipboardError> {
        if content_is_private() {
            return Ok(None);
        }
        match self.inner.get_text() {
            Ok(text) => Ok(Some(text)),
            // Not text, or empty: nothing to sync.
            Err(arboard::Error::ContentNotAvailable) => Ok(None),
            Err(e) => Err(ClipboardError::Backend(e.to_string())),
        }
    }

    fn write_text(&mut self, text: &str) -> Result<(), ClipboardError> {
        self.inner
            .set_text(text)
            .map_err(|e| ClipboardError::Backend(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::ptr;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};

    use super::*;

    /// Puts text on the clipboard marked private, the way a password
    /// manager does.
    fn copy_private(text: &str) {
        // Unicode text format.
        const CF_UNICODETEXT: u32 = 13;
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: standard clipboard sequence. Each allocation is sized for
        // what is copied into it and is handed to the clipboard, which owns
        // it afterwards.
        unsafe {
            OpenClipboard(None).expect("open the clipboard");
            EmptyClipboard().expect("empty the clipboard");

            let text_memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2).expect("allocate");
            let target = GlobalLock(text_memory).cast::<u16>();
            ptr::copy_nonoverlapping(wide.as_ptr(), target, wide.len());
            let _ = GlobalUnlock(text_memory);
            SetClipboardData(CF_UNICODETEXT, Some(HANDLE(text_memory.0))).expect("set text");

            let flag_memory = GlobalAlloc(GMEM_MOVEABLE, 4).expect("allocate");
            let flag = GlobalLock(flag_memory).cast::<u32>();
            flag.write(1);
            let _ = GlobalUnlock(flag_memory);
            SetClipboardData(exclude_format(), Some(HANDLE(flag_memory.0))).expect("set flag");

            CloseClipboard().expect("close the clipboard");
        }
    }

    #[test]
    #[ignore = "uses the real clipboard; restores its text afterwards"]
    fn the_real_clipboard_round_trips_changes_its_sequence_and_respects_the_private_flag() {
        let mut clipboard = SystemClipboard::new().unwrap();
        let original = clipboard.read_text().ok().flatten();

        let before = clipboard.sequence();
        clipboard.write_text("round trip ✓ ünïcode").unwrap();
        assert_ne!(clipboard.sequence(), before, "the sequence did not change");
        assert_eq!(
            clipboard.read_text().unwrap().as_deref(),
            Some("round trip ✓ ünïcode")
        );
        assert!(!content_is_private());

        copy_private("a secret");
        assert!(content_is_private());
        assert_eq!(
            clipboard.read_text().unwrap(),
            None,
            "private text was readable"
        );

        // Ordinary copying clears the flag again.
        clipboard.write_text("ordinary").unwrap();
        assert!(!content_is_private());
        assert_eq!(clipboard.read_text().unwrap().as_deref(), Some("ordinary"));

        if let Some(text) = original {
            clipboard.write_text(&text).unwrap();
        }
    }
}
