//! Clipboard text and file transfer in real sessions, host and viewer
//! connected over loopback. Each end has a clipboard in memory, so the
//! user's own clipboard is never touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use rc_clipboard::MemoryClipboard;

use super::test_support::{pending, settings, wait_until};
use super::*;
use crate::secure::test_support::{desktop, security};
use crate::secure::Security;
use crate::transfer::{Direction, State};
use crate::viewer::{packet, ConnectOptions, ViewerController};

const WAIT: Duration = Duration::from_secs(5);

struct Rig {
    host: ShareController,
    viewer: ViewerController,
    host_clipboard: MemoryClipboard,
    viewer_clipboard: MemoryClipboard,
    host_files: PathBuf,
    viewer_files: PathBuf,
    packets: mpsc::Receiver<Vec<u8>>,
    _dirs: (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir),
    _keys: (Arc<Security>, Arc<Security>),
}

/// Starts sharing with the given switches, connects a viewer and has the
/// host approve it. Returns when the host's permissions have arrived.
fn rig(clipboard: bool, files: bool) -> Rig {
    let (host_keys, host_dir) = security();
    let (viewer_keys, viewer_dir) = security();
    let files_dir = tempfile::tempdir().unwrap();
    let host_files = files_dir.path().join("host");
    let viewer_files = files_dir.path().join("viewer");
    let (host_clipboard, viewer_clipboard) = (MemoryClipboard::new(), MemoryClipboard::new());

    let host = ShareController::new(Some(Arc::clone(&host_keys)));
    let port = host
        .start(ShareSettings {
            allow_clipboard: clipboard,
            allow_files: files,
            received_folder: host_files.clone(),
            clipboard: Some(host_clipboard.clone()),
            ..settings()
        })
        .unwrap()
        .port
        .unwrap();

    let viewer = ViewerController::new(Some(Arc::clone(&viewer_keys)));
    let (tx, packets) = mpsc::channel();
    viewer
        .connect_with(
            &format!("127.0.0.1:{port}"),
            &ConnectOptions {
                received_folder: viewer_files.clone(),
                clipboard: Some(viewer_clipboard.clone()),
                ..ConnectOptions::default()
            },
            "Viewer PC",
            move |packet| {
                let _ = tx.send(packet);
            },
        )
        .unwrap();
    host.answer(pending(&host).id, true, false).unwrap();

    let rig = Rig {
        host,
        viewer,
        host_clipboard,
        viewer_clipboard,
        host_files,
        viewer_files,
        packets,
        _dirs: (host_dir, viewer_dir, files_dir),
        _keys: (host_keys, viewer_keys),
    };
    // Wait for the host's first announcement of what it allows.
    let bits = rig.next_permissions();
    assert_eq!(bits & 2 != 0, clipboard, "clipboard bit");
    assert_eq!(bits & 4 != 0, files, "files bit");
    rig
}

impl Rig {
    /// The next PERMISSIONS packet the window would have received.
    fn next_permissions(&self) -> u8 {
        loop {
            let packet = self
                .packets
                .recv_timeout(WAIT)
                .expect("no permissions packet");
            if packet[0] == packet::PERMISSIONS {
                return packet[1];
            }
        }
    }

    fn host_caps(&self) -> Arc<Capabilities> {
        self.host.capabilities().expect("no host session")
    }

    fn viewer_caps(&self) -> Arc<Capabilities> {
        self.viewer.capabilities().expect("no viewer session")
    }

    fn settle(&self) {
        thread::sleep(Duration::from_millis(600));
    }

    /// Ends the session. Also happens when the rig is dropped, so a test
    /// that fails part-way does not leave a host capturing the screen for
    /// the tests after it.
    fn finish(self) {
        drop(self);
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.viewer.disconnect();
        self.host.stop();
    }
}

fn dir_names(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(path)
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Offers `content` from one end, has the other end accept, and sends it.
fn send_file(from: &Arc<Capabilities>, to: &Arc<Capabilities>, name: &str, content: &[u8]) {
    let id = from.transfers.offer(name, content.len() as u64).unwrap();
    wait_until("the offer to arrive", || {
        to.transfers
            .status()
            .items
            .iter()
            .any(|i| i.state == State::WaitingForYou)
    });
    let incoming = to
        .transfers
        .status()
        .items
        .into_iter()
        .find(|i| i.state == State::WaitingForYou)
        .unwrap();
    to.transfers.answer(incoming.id, true).unwrap();
    wait_until("the transfer to be accepted", || {
        from.transfers.status().items.iter().any(|i| {
            i.id == id && i.direction == Direction::Outgoing && i.state == State::Transferring
        })
    });
    for piece in content.chunks(100_000) {
        from.transfers.chunk(id, piece).unwrap();
    }
    from.transfers.finish(id).unwrap();
    wait_until("the file to be confirmed", || {
        from.transfers
            .status()
            .items
            .iter()
            .any(|i| i.id == id && i.direction == Direction::Outgoing && i.state == State::Finished)
    });
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn the_viewer_is_told_what_the_host_allows_and_changes_follow_at_once() {
    let _desktop = desktop();
    let rig = rig(true, false);
    assert!(rig.viewer_caps().clipboard_available());
    assert!(!rig.viewer_caps().transfers.is_allowed());

    rig.host.set_files_allowed(true);
    assert_eq!(rig.next_permissions(), 2 | 4);
    wait_until("files to be allowed at the viewer", || {
        rig.viewer_caps().transfers.is_allowed()
    });
    rig.host.set_clipboard_allowed(false);
    assert_eq!(rig.next_permissions(), 4);
    rig.finish();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn clipboard_text_syncs_both_ways_only_when_the_host_allows_and_the_viewer_wants_it() {
    let _desktop = desktop();
    // Something already on the host's clipboard before anything is on.
    let before = {
        // Created inside the rig, so seed it through a clone afterwards.
        "text copied before sync was switched on"
    };
    let rig = rig(true, false);
    rig.host_clipboard.copy(before);

    // The host allows it but the viewer has not asked: nothing moves.
    rig.host_clipboard.copy("host copy 1");
    rig.viewer_clipboard.copy("viewer copy 1");
    rig.settle();
    assert_eq!(rig.host_clipboard.text().as_deref(), Some("host copy 1"));
    assert_eq!(
        rig.viewer_clipboard.text().as_deref(),
        Some("viewer copy 1")
    );
    assert!(!rig.viewer.status().unwrap().clipboard_active);

    // The viewer switches it on: only later copies travel.
    rig.viewer.set_clipboard_wanted(true).unwrap();
    wait_until("sync to start", || {
        rig.viewer.status().unwrap().clipboard_active
    });
    rig.settle();
    assert_eq!(rig.host_clipboard.text().as_deref(), Some("host copy 1"));
    assert_eq!(
        rig.viewer_clipboard.text().as_deref(),
        Some("viewer copy 1")
    );

    rig.viewer_clipboard.copy("from the viewer");
    wait_until("viewer text to reach the host", || {
        rig.host_clipboard.text().as_deref() == Some("from the viewer")
    });
    rig.host_clipboard.copy("from the host");
    wait_until("host text to reach the viewer", || {
        rig.viewer_clipboard.text().as_deref() == Some("from the host")
    });
    // Nothing bounced back and forth.
    rig.settle();
    assert_eq!(rig.host_clipboard.text().as_deref(), Some("from the host"));
    assert_eq!(
        rig.viewer_clipboard.text().as_deref(),
        Some("from the host")
    );
    let status = rig.viewer.status().unwrap();
    assert_eq!((status.clipboard_sent, status.clipboard_received), (1, 1));
    // The host sends whatever it copies while it allows sync (its first copy
    // too, which the viewer dropped); it received the viewer's one text.
    let host = rig.host_caps().clipboard_counts();
    assert_eq!(host, (2, 1));

    // Private content never travels.
    rig.host_clipboard.copy_private("a password");
    rig.settle();
    assert_eq!(
        rig.viewer_clipboard.text().as_deref(),
        Some("from the host")
    );

    // The host switches it off: it stops at once, and the viewer is told.
    rig.host.set_clipboard_allowed(false);
    wait_until("the viewer to see sync end", || {
        !rig.viewer.status().unwrap().clipboard_active
    });
    assert!(!rig.viewer.status().unwrap().clipboard_host_allows);
    rig.viewer_clipboard.copy("after the host said no");
    rig.settle();
    assert_ne!(
        rig.host_clipboard.text().as_deref(),
        Some("after the host said no")
    );
    rig.finish();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn clipboard_text_is_not_applied_by_a_viewer_that_has_not_asked_for_it() {
    let _desktop = desktop();
    let rig = rig(true, false);
    // The host sends its copies, but the viewer has sync off and drops them.
    rig.host_clipboard.copy("host secret");
    rig.settle();
    assert_ne!(rig.viewer_clipboard.text().as_deref(), Some("host secret"));
    rig.finish();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn clipboard_sync_is_impossible_when_the_host_has_not_allowed_it() {
    let _desktop = desktop();
    let rig = rig(false, false);
    rig.viewer.set_clipboard_wanted(true).unwrap();
    rig.viewer_clipboard.copy("viewer text");
    rig.host_clipboard.copy("host text");
    rig.settle();
    assert_eq!(rig.host_clipboard.text().as_deref(), Some("host text"));
    assert_eq!(rig.viewer_clipboard.text().as_deref(), Some("viewer text"));
    let status = rig.viewer.status().unwrap();
    assert!(status.clipboard_wanted && !status.clipboard_host_allows && !status.clipboard_active);
    rig.finish();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn files_go_both_ways_after_the_receiving_person_accepts() {
    let _desktop = desktop();
    let rig = rig(false, true);
    let (host, viewer) = (rig.host_caps(), rig.viewer_caps());
    let upload: Vec<u8> = (0..700_000u32).map(|i| (i % 249) as u8).collect();
    let download = b"from the host".to_vec();

    send_file(&viewer, &host, "C:\\Users\\viewer\\notes.txt", &upload);
    assert_eq!(fs::read(rig.host_files.join("notes.txt")).unwrap(), upload);
    send_file(&host, &viewer, "../escape.txt", &download);
    assert_eq!(
        fs::read(rig.viewer_files.join("escape.txt")).unwrap(),
        download
    );
    // Nothing outside the receiving folders, and no half-written files.
    assert_eq!(dir_names(&rig.host_files), ["notes.txt"]);
    assert_eq!(dir_names(&rig.viewer_files), ["escape.txt"]);
    rig.finish();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn a_file_is_refused_with_no_prompt_when_the_host_has_not_allowed_files() {
    let _desktop = desktop();
    let rig = rig(false, false);
    // The viewer is told files are off and cannot even offer.
    let error = rig.viewer_caps().transfers.offer("x.txt", 5).unwrap_err();
    assert!(error.contains("not allowed"), "{error}");
    // The host cannot send either.
    assert!(rig.host_caps().transfers.offer("y.txt", 5).is_err());
    assert!(rig.host_caps().transfers.status().items.is_empty());
    rig.finish();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn switching_files_off_mid_transfer_stops_it_and_removes_the_partial_file() {
    let _desktop = desktop();
    let rig = rig(false, true);
    let (host, viewer) = (rig.host_caps(), rig.viewer_caps());
    let id = viewer.transfers.offer("big.bin", 1_000_000).unwrap();
    wait_until("the offer", || {
        host.transfers
            .status()
            .items
            .iter()
            .any(|i| i.state == State::WaitingForYou)
    });
    let incoming = host.transfers.status().items[0].id;
    host.transfers.answer(incoming, true).unwrap();
    wait_until("acceptance", || {
        viewer
            .transfers
            .status()
            .items
            .iter()
            .any(|i| i.state == State::Transferring)
    });
    viewer.transfers.chunk(id, &[7u8; 300_000]).unwrap();
    wait_until("the partial file", || {
        dir_names(&rig.host_files) == ["big.bin.part"]
    });

    rig.host.set_files_allowed(false);
    wait_until("the viewer's transfer to stop", || {
        viewer
            .transfers
            .status()
            .items
            .iter()
            .all(|i| !matches!(i.state, State::Transferring))
    });
    assert!(viewer.transfers.chunk(id, &[7u8; 10]).is_err());
    assert!(
        dir_names(&rig.host_files).is_empty(),
        "{:?}",
        dir_names(&rig.host_files)
    );
    rig.finish();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn ending_the_session_mid_transfer_leaves_no_partial_file() {
    let _desktop = desktop();
    let rig = rig(false, true);
    let (host, viewer) = (rig.host_caps(), rig.viewer_caps());
    let id = viewer.transfers.offer("half.bin", 1_000_000).unwrap();
    wait_until("the offer", || {
        host.transfers
            .status()
            .items
            .iter()
            .any(|i| i.state == State::WaitingForYou)
    });
    host.transfers
        .answer(host.transfers.status().items[0].id, true)
        .unwrap();
    wait_until("acceptance", || {
        viewer
            .transfers
            .status()
            .items
            .iter()
            .any(|i| i.state == State::Transferring)
    });
    viewer.transfers.chunk(id, &[1u8; 200_000]).unwrap();
    wait_until("the partial file", || {
        dir_names(&rig.host_files) == ["half.bin.part"]
    });

    let host_files = rig.host_files.clone();
    rig.viewer.disconnect();
    wait_until("the partial file to go", || {
        dir_names(&host_files).is_empty()
    });
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn the_receiving_person_can_refuse_and_the_sender_is_told() {
    let _desktop = desktop();
    let rig = rig(false, true);
    let (host, viewer) = (rig.host_caps(), rig.viewer_caps());
    let id = viewer.transfers.offer("unwanted.exe", 10).unwrap();
    wait_until("the offer", || {
        host.transfers
            .status()
            .items
            .iter()
            .any(|i| i.state == State::WaitingForYou)
    });
    host.transfers
        .answer(host.transfers.status().items[0].id, false)
        .unwrap();
    wait_until("the refusal", || {
        viewer
            .transfers
            .status()
            .items
            .iter()
            .any(|i| i.id == id && i.state == State::Refused)
    });
    assert!(dir_names(&rig.host_files).is_empty());
    host.transfers.cancel(Direction::Incoming, 999);
    rig.finish();
}
