//! Switching a permission off must be complete when the switch returns, not
//! whenever the video loop next comes round (which can be a long time if the
//! connection is stalled). None of these tests needs a desktop.

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use rc_clipboard::{ClipboardBackend, MemoryClipboard};
use rc_input::mapping::Rect;

use super::*;
use crate::secure::test_support::secure_pair;
use crate::transfer::State;

fn capabilities(
    writer: Arc<SecureWriter>,
    folder: std::path::PathBuf,
    clipboard: Option<MemoryClipboard>,
) -> Arc<Capabilities> {
    Arc::new(Capabilities::new(writer, folder, true, clipboard))
}

#[test]
fn switching_the_clipboard_off_stops_writes_before_the_switch_returns() {
    let ((writer, _reader, _), _viewer) = secure_pair();
    let memory = MemoryClipboard::new();
    let caps = capabilities(
        Arc::new(writer),
        tempfile::tempdir().unwrap().keep(),
        Some(memory.clone()),
    );
    let shared = Shared::default();
    *lock(&shared.capabilities) = Some(Arc::clone(&caps));

    caps.set_clipboard(true);
    caps.handle(&Message::ClipboardText("first".into()));
    assert_eq!(
        memory.clone().read_text().unwrap().as_deref(),
        Some("first")
    );

    // The host switches it off; the session has not looked at the switch yet.
    shared.set_clipboard(false);
    assert!(!caps.clipboard_enabled());
    caps.handle(&Message::ClipboardText("second".into()));
    assert_eq!(
        memory.clone().read_text().unwrap().as_deref(),
        Some("first"),
        "text from the viewer was written after the switch was off"
    );
}

#[test]
fn switching_files_off_ends_transfers_at_once_even_if_the_connection_is_stalled() {
    let ((writer, _reader, _), (peer_writer, mut peer_reader, _)) = secure_pair();
    let writer = Arc::new(writer);
    let folder = tempfile::tempdir().unwrap();
    let caps = capabilities(Arc::clone(&writer), folder.path().to_path_buf(), None);
    let shared = Shared::default();
    *lock(&shared.capabilities) = Some(Arc::clone(&caps));

    // A file is arriving: accepted, and part of it is on disk.
    caps.transfers.handle(&Message::FileOffer {
        id: 1,
        name: "big.bin".into(),
        size: 1000,
    });
    caps.transfers.answer(1, true).unwrap();
    caps.transfers.handle(&Message::FileChunk {
        id: 1,
        data: vec![7; 100],
    });
    let part = folder.path().join("big.bin.part");
    assert!(part.exists(), "the partial file was not created");

    // The viewer stops reading, and the video the host is sending fills the
    // connection until a write is stuck holding the connection.
    let stop = Arc::new(AtomicBool::new(false));
    let stalled = {
        let writer = Arc::clone(&writer);
        let stop = Arc::clone(&stop);
        thread::spawn(move || {
            let frame = Message::VideoFrame {
                keyframe: false,
                timestamp_us: 0,
                data: vec![1; 200_000],
            };
            while !stop.load(Ordering::Relaxed) && writer.send(&frame).is_ok() {}
        })
    };
    thread::sleep(Duration::from_millis(500));

    let started = Instant::now();
    shared.set_files(false);
    let took = started.elapsed();
    assert!(
        took < Duration::from_secs(1),
        "switching files off waited {took:?} on a stalled connection"
    );

    // Everything local is already done.
    let item = caps
        .transfers
        .status()
        .items
        .into_iter()
        .find(|i| i.id == 1)
        .unwrap();
    assert_eq!(item.state, State::Cancelled);
    assert!(!part.exists(), "the half-written file is still there");
    assert!(!caps.transfers.is_allowed());

    // The other end is still told, once its connection moves again.
    stop.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut told = false;
    while Instant::now() < deadline {
        match peer_reader.read_message() {
            Ok(Message::FileCancel { id: 1, .. }) => {
                told = true;
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    drop(peer_reader);
    drop(peer_writer);
    stalled.join().unwrap();
    assert!(told, "the viewer was never told the transfer was cancelled");
}

#[test]
fn switching_control_off_waits_for_an_injection_in_progress() {
    let shared = Arc::new(Shared::default());
    let injector = Arc::new(Mutex::new(Injector::new(Rect {
        x: 0,
        y: 0,
        width: 100,
        height: 100,
    })));
    *lock(&shared.injector) = Some(Arc::clone(&injector));
    shared.input_allowed.store(true, Ordering::Relaxed);

    // The reader holds the injector while it injects one event.
    let injecting = {
        let injector = Arc::clone(&injector);
        thread::spawn(move || {
            let _held = lock(&injector);
            thread::sleep(Duration::from_millis(300));
        })
    };
    thread::sleep(Duration::from_millis(50));

    let started = Instant::now();
    shared.set_input(false);
    assert!(
        started.elapsed() >= Duration::from_millis(150),
        "the switch returned while an injection was still in progress"
    );
    // From here on the reader finds the switch off for every event it checks.
    assert!(!shared.input_allowed.load(Ordering::Relaxed));
    injecting.join().unwrap();
}
