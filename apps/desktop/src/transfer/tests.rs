use std::collections::VecDeque;
use std::sync::Arc;

use super::*;

type Queue = Arc<Mutex<VecDeque<Message>>>;

/// Two ends of a session joined by queues, so a test decides when messages
/// are delivered.
struct Pair {
    a: Transfers,
    b: Transfers,
    to_b: Queue,
    to_a: Queue,
    _dir_a: tempfile::TempDir,
    dir_b: tempfile::TempDir,
}

impl Pair {
    fn new(a_allowed: bool, b_allowed: bool) -> Self {
        let (dir_a, dir_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (to_b, to_a): (Queue, Queue) = (Arc::default(), Arc::default());
        let sender = |queue: &Queue| -> Sender {
            let queue = Arc::clone(queue);
            Box::new(move |message| {
                lock(&queue).push_back(message);
                Ok(())
            })
        };
        Self {
            a: Transfers::new(dir_a.path().join("received"), a_allowed, sender(&to_b)),
            b: Transfers::new(dir_b.path().join("received"), b_allowed, sender(&to_a)),
            to_b,
            to_a,
            _dir_a: dir_a,
            dir_b,
        }
    }

    /// Delivers everything in flight, in both directions, until quiet.
    fn pump(&self) {
        loop {
            let to_b: Vec<_> = lock(&self.to_b).drain(..).collect();
            let to_a: Vec<_> = lock(&self.to_a).drain(..).collect();
            if to_b.is_empty() && to_a.is_empty() {
                return;
            }
            for message in to_b {
                self.b.handle(&message);
            }
            for message in to_a {
                self.a.handle(&message);
            }
        }
    }

    fn received_by_b(&self) -> PathBuf {
        self.dir_b.path().join("received")
    }

    /// Offers a file from A and has B accept it.
    fn offer_and_accept(&self, name: &str, size: u64) -> u32 {
        let id = self.a.offer(name, size).unwrap();
        self.pump();
        let waiting = self
            .b
            .status()
            .items
            .into_iter()
            .find(|i| i.state == State::WaitingForYou)
            .expect("no offer is waiting");
        self.b.answer(waiting.id, true).unwrap();
        self.pump();
        assert_eq!(self.a.outgoing_state(id), Some(State::Transferring));
        id
    }

    fn send_all(&self, id: u32, data: &[u8]) {
        for piece in data.chunks(200_000) {
            self.a.chunk(id, piece).unwrap();
        }
        self.a.finish(id).unwrap();
        self.pump();
    }
}

fn data(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i.wrapping_mul(31) % 251) as u8).collect()
}

fn item(transfers: &Transfers) -> TransferInfo {
    transfers.status().items.remove(0)
}

fn files_in(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn names_are_made_safe() {
    for (given, expected) in [
        ("report.pdf", "report.pdf"),
        ("C:\\Users\\x\\Desktop\\report.pdf", "report.pdf"),
        ("../../etc/passwd", "passwd"),
        ("..\\..\\evil.exe", "evil.exe"),
        ("a<b>c:d\"e|f?g*h.txt", "abcdefgh.txt"),
        ("tab\there\n.txt", "tabhere.txt"),
        ("trailing dots...", "trailing dots"),
        ("  spaced  ", "spaced"),
        ("", "file"),
        ("..", "file"),
        (".", "file"),
        ("...", "file"),
        ("dir/", "file"),
        ("CON", "_CON"),
        ("nul.txt", "_nul.txt"),
        ("com1.log", "_com1.log"),
        ("LPT9", "_LPT9"),
        ("COM10.txt", "COM10.txt"),
        ("console.txt", "console.txt"),
        ("ünïcode ✓.txt", "ünïcode ✓.txt"),
        (".hidden", ".hidden"),
    ] {
        assert_eq!(safe_file_name(given), expected, "{given:?}");
    }
}

#[test]
fn long_names_are_shortened_but_keep_their_extension() {
    let long = format!("{}.docx", "x".repeat(300));
    let name = safe_file_name(&long);
    assert_eq!(name.chars().count(), MAX_NAME_CHARS);
    assert!(name.ends_with(".docx"));
    let no_extension = safe_file_name(&"y".repeat(300));
    assert_eq!(no_extension.chars().count(), MAX_NAME_CHARS);
}

#[test]
fn a_name_already_taken_gets_a_number_and_nothing_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(unused_name(dir.path(), "a.txt"), dir.path().join("a.txt"));
    fs::write(dir.path().join("a.txt"), "first").unwrap();
    assert_eq!(
        unused_name(dir.path(), "a.txt"),
        dir.path().join("a (1).txt")
    );
    fs::write(dir.path().join("a (1).txt"), "second").unwrap();
    // A file still being written counts as taken too.
    fs::write(dir.path().join("a (2).txt.part"), "partial").unwrap();
    assert_eq!(
        unused_name(dir.path(), "a.txt"),
        dir.path().join("a (3).txt")
    );
    fs::write(dir.path().join("noext"), "x").unwrap();
    assert_eq!(
        unused_name(dir.path(), "noext"),
        dir.path().join("noext (1)")
    );
}

#[test]
fn a_file_is_offered_accepted_sent_verified_and_saved() {
    let pair = Pair::new(true, true);
    let content = data(1_000_000);
    let id = pair
        .a
        .offer("report (final).pdf", content.len() as u64)
        .unwrap();
    pair.pump();

    // The receiver sees an offer and nothing has been written.
    let offer = item(&pair.b);
    assert_eq!(
        (
            offer.direction,
            offer.name.as_str(),
            offer.size,
            offer.state
        ),
        (
            Direction::Incoming,
            "report (final).pdf",
            1_000_000,
            State::WaitingForYou
        )
    );
    assert!(offer.seconds_left.unwrap() > 50);
    assert!(files_in(&pair.received_by_b()).is_empty());
    assert_eq!(pair.a.outgoing_state(id), Some(State::WaitingForPeer));

    pair.b.answer(offer.id, true).unwrap();
    pair.pump();
    assert_eq!(files_in(&pair.received_by_b()), ["report (final).pdf.part"]);
    pair.send_all(id, &content);

    let saved = pair.received_by_b().join("report (final).pdf");
    assert_eq!(fs::read(&saved).unwrap(), content);
    assert_eq!(files_in(&pair.received_by_b()), ["report (final).pdf"]);
    let received = item(&pair.b);
    assert_eq!(
        (received.state, received.done),
        (State::Finished, 1_000_000)
    );
    assert!(received.detail.unwrap().starts_with("saved as"));
    let sent = item(&pair.a);
    assert_eq!(sent.state, State::Finished);
    assert_eq!(sent.detail.as_deref(), Some("delivered"));

    // Marked as coming from another computer (NTFS alternate data stream).
    let mark = fs::read_to_string(format!("{}:Zone.Identifier", saved.display())).unwrap();
    assert!(mark.contains("ZoneId=3"), "{mark:?}");
}

#[test]
fn an_empty_file_can_be_sent() {
    let pair = Pair::new(true, true);
    let id = pair.offer_and_accept("empty.txt", 0);
    pair.send_all(id, b"");
    assert_eq!(
        fs::read(pair.received_by_b().join("empty.txt")).unwrap(),
        b""
    );
    assert_eq!(item(&pair.a).state, State::Finished);
}

#[test]
fn a_refused_offer_creates_nothing_and_tells_the_sender() {
    let pair = Pair::new(true, true);
    let id = pair.a.offer("unwanted.exe", 10).unwrap();
    pair.pump();
    pair.b.answer(item(&pair.b).id, false).unwrap();
    pair.pump();

    assert_eq!(pair.a.outgoing_state(id), Some(State::Refused));
    assert_eq!(item(&pair.b).state, State::Refused);
    assert!(files_in(&pair.received_by_b()).is_empty());
    // The sender cannot push data at a refused transfer.
    assert!(pair.a.chunk(id, b"data").is_err());
    // And an answer to it cannot be given twice.
    assert!(pair.b.answer(item(&pair.b).id, true).is_err());
}

#[test]
fn an_offer_nobody_answers_is_refused_after_the_time_limit() {
    let (dir, queue): (_, Queue) = (tempfile::tempdir().unwrap(), Arc::default());
    let sent = Arc::clone(&queue);
    let receiver = Transfers::new(
        dir.path().to_path_buf(),
        true,
        Box::new(move |m| {
            lock(&sent).push_back(m);
            Ok(())
        }),
    )
    .with_offer_timeout(Duration::from_millis(150));

    receiver.handle(&Message::FileOffer {
        id: 5,
        name: "late.zip".into(),
        size: 9,
    });
    assert_eq!(item(&receiver).state, State::WaitingForYou);
    std::thread::sleep(Duration::from_millis(300));
    let expired = item(&receiver);
    assert_eq!(expired.state, State::Refused);
    assert!(
        receiver.answer(5, true).is_err(),
        "answered an expired offer"
    );
    assert!(matches!(
        lock(&queue).pop_front(),
        Some(Message::FileAnswer {
            id: 5,
            accept: false,
            ..
        })
    ));
    assert!(files_in(dir.path()).is_empty());
}

#[test]
fn nothing_is_offered_or_accepted_while_transfers_are_not_allowed() {
    let pair = Pair::new(true, false);
    // B does not allow: A's offer is refused straight away.
    let id = pair.a.offer("x.txt", 5).unwrap();
    pair.pump();
    assert_eq!(pair.a.outgoing_state(id), Some(State::Refused));
    assert!(pair.b.status().items.is_empty(), "B was shown an offer");

    // A that does not allow cannot offer at all.
    let pair = Pair::new(false, true);
    assert!(pair
        .a
        .offer("x.txt", 5)
        .unwrap_err()
        .contains("not allowed"));
    assert!(pair.to_b.lock().unwrap().is_empty());
}

#[test]
fn offers_that_are_too_large_duplicated_or_too_many_are_refused() {
    let (dir, queue): (_, Queue) = (tempfile::tempdir().unwrap(), Arc::default());
    let sent = Arc::clone(&queue);
    let receiver = Transfers::new(
        dir.path().to_path_buf(),
        true,
        Box::new(move |m| {
            lock(&sent).push_back(m);
            Ok(())
        }),
    );
    let offer = |id: u32, size: u64| Message::FileOffer {
        id,
        name: format!("f{id}"),
        size,
    };
    let last_refusal = || match lock(&queue).pop_back() {
        Some(Message::FileAnswer {
            accept: false,
            reason,
            ..
        }) => reason,
        other => panic!("expected a refusal, got {other:?}"),
    };

    receiver.handle(&offer(1, MAX_FILE_BYTES + 1));
    assert!(last_refusal().contains("too large"));
    receiver.handle(&offer(2, 10));
    receiver.handle(&offer(2, 10));
    assert!(last_refusal().contains("already in use"));
    receiver.handle(&offer(3, 10));
    receiver.handle(&offer(4, 10));
    receiver.handle(&offer(5, 10));
    receiver.handle(&offer(6, 10));
    assert!(last_refusal().contains("too many"));
    assert_eq!(receiver.status().items.len(), MAX_INCOMING);
}

#[test]
fn a_file_name_cannot_escape_the_receiving_folder_or_replace_a_file() {
    let pair = Pair::new(true, true);
    fs::create_dir_all(pair.received_by_b()).unwrap();
    fs::write(pair.received_by_b().join("evil.exe"), "mine").unwrap();

    let id = pair.offer_and_accept("..\\..\\..\\Windows\\evil.exe", 3);
    pair.send_all(id, b"bad");

    assert_eq!(
        fs::read(pair.received_by_b().join("evil.exe")).unwrap(),
        b"mine"
    );
    assert_eq!(
        fs::read(pair.received_by_b().join("evil (1).exe")).unwrap(),
        b"bad"
    );
    // Nothing was created beside the folder.
    assert_eq!(files_in(pair.dir_b.path()), ["received"]);
}

#[test]
fn too_much_data_a_wrong_hash_and_an_early_end_delete_the_file() {
    // More data than offered.
    let pair = Pair::new(true, true);
    let id = pair.offer_and_accept("a.bin", 4);
    pair.b.handle(&Message::FileChunk {
        id,
        data: vec![1; 5],
    });
    pair.pump();
    assert_eq!(item(&pair.b).state, State::Failed);
    assert!(files_in(&pair.received_by_b()).is_empty());
    assert!(matches!(pair.a.outgoing_state(id), Some(State::Cancelled)));

    // Right size, wrong hash.
    let pair = Pair::new(true, true);
    let id = pair.offer_and_accept("b.bin", 4);
    pair.b.handle(&Message::FileChunk {
        id,
        data: vec![1; 4],
    });
    pair.b.handle(&Message::FileEnd {
        id,
        sha256: [0; 32],
    });
    pair.pump();
    let failed = item(&pair.b);
    assert_eq!(failed.state, State::Failed);
    assert!(failed.detail.unwrap().contains("damaged"));
    assert!(files_in(&pair.received_by_b()).is_empty());

    // Ended before all of it arrived.
    let pair = Pair::new(true, true);
    let id = pair.offer_and_accept("c.bin", 10);
    pair.b.handle(&Message::FileChunk {
        id,
        data: vec![1; 4],
    });
    pair.b.handle(&Message::FileEnd {
        id,
        sha256: [0; 32],
    });
    assert_eq!(item(&pair.b).state, State::Failed);
    assert!(files_in(&pair.received_by_b()).is_empty());
}

#[test]
fn data_for_an_unknown_unaccepted_or_finished_transfer_is_ignored() {
    let pair = Pair::new(true, true);
    pair.b.handle(&Message::FileChunk {
        id: 99,
        data: vec![1; 4],
    });
    pair.b.handle(&Message::FileEnd {
        id: 99,
        sha256: [0; 32],
    });
    pair.b.handle(&Message::FileCancel {
        id: 99,
        by_sender: true,
        reason: String::new(),
    });
    pair.b.handle(&Message::FileDone { id: 99 });

    // An offer that has not been accepted must not receive data.
    pair.a.offer("w.bin", 4).unwrap();
    pair.pump();
    let waiting = item(&pair.b).id;
    pair.b.handle(&Message::FileChunk {
        id: waiting,
        data: vec![1; 4],
    });
    assert_eq!(item(&pair.b).state, State::WaitingForYou);
    assert!(files_in(&pair.received_by_b()).is_empty());
}

#[test]
fn either_side_can_cancel_and_leaves_nothing_behind() {
    // The sender cancels part-way.
    let pair = Pair::new(true, true);
    let id = pair.offer_and_accept("big.bin", 1000);
    pair.a.chunk(id, &data(400)).unwrap();
    pair.pump();
    assert_eq!(files_in(&pair.received_by_b()), ["big.bin.part"]);
    pair.a.cancel(Direction::Outgoing, id);
    pair.pump();
    assert_eq!(item(&pair.b).state, State::Cancelled);
    assert!(files_in(&pair.received_by_b()).is_empty());

    // The receiver cancels part-way; the sender is stopped.
    let pair = Pair::new(true, true);
    let id = pair.offer_and_accept("big.bin", 1000);
    pair.a.chunk(id, &data(400)).unwrap();
    pair.pump();
    pair.b.cancel(Direction::Incoming, item(&pair.b).id);
    pair.pump();
    assert_eq!(pair.a.outgoing_state(id), Some(State::Cancelled));
    assert!(pair.a.chunk(id, &data(10)).is_err());
    assert!(files_in(&pair.received_by_b()).is_empty());

    // Cancelling an offer that is still waiting counts as refusing it.
    let pair = Pair::new(true, true);
    let id = pair.a.offer("w.bin", 5).unwrap();
    pair.pump();
    pair.b.cancel(Direction::Incoming, item(&pair.b).id);
    pair.pump();
    assert_eq!(pair.a.outgoing_state(id), Some(State::Refused));
}

#[test]
fn switching_transfers_off_ends_everything_in_progress_on_both_sides() {
    let pair = Pair::new(true, true);
    let id = pair.offer_and_accept("big.bin", 1000);
    pair.a.chunk(id, &data(100)).unwrap();
    pair.pump();
    pair.a.offer("second.bin", 5).unwrap();
    pair.pump();

    pair.b.set_allowed(false);
    pair.pump();
    assert!(pair.a.status().items.iter().all(|i| !i.state.active()));
    assert!(pair.b.status().items.iter().all(|i| !i.state.active()));
    assert!(files_in(&pair.received_by_b()).is_empty());
    assert!(pair.a.chunk(id, &data(10)).is_err());
    // And nothing new is taken.
    pair.a.offer("third.bin", 5).unwrap();
    pair.pump();
    assert_eq!(pair.a.status().items.last().unwrap().state, State::Refused);
}

#[test]
fn the_sender_cannot_misuse_its_own_side() {
    let pair = Pair::new(true, true);
    let id = pair.a.offer("w.bin", 10).unwrap();
    // Before acceptance.
    assert!(pair.a.chunk(id, b"x").unwrap_err().contains("not accepted"));
    assert!(pair.a.finish(id).is_err());
    assert!(pair.a.chunk(999, b"x").is_err());
    pair.pump();
    pair.b.answer(item(&pair.b).id, true).unwrap();
    pair.pump();

    // Finishing early fails the transfer; so does sending too much.
    pair.a.chunk(id, &[1; 4]).unwrap();
    assert!(pair.a.finish(id).is_err());
    assert_eq!(pair.a.outgoing_state(id), Some(State::Failed));

    let pair = Pair::new(true, true);
    let id = pair.offer_and_accept("w.bin", 4);
    assert!(pair.a.chunk(id, &[1; 5]).unwrap_err().contains("More data"));
    assert_eq!(pair.a.outgoing_state(id), Some(State::Failed));
}

#[test]
fn no_more_than_two_files_are_sent_at_once_and_sizes_are_limited() {
    let pair = Pair::new(true, true);
    pair.a.offer("1", 1).unwrap();
    pair.a.offer("2", 1).unwrap();
    assert!(pair.a.offer("3", 1).unwrap_err().contains("Too many"));
    assert!(pair
        .a
        .offer("big", MAX_FILE_BYTES + 1)
        .unwrap_err()
        .contains("larger"));
}

#[test]
fn sending_is_held_to_the_rate_limit() {
    let pair = Pair::new(true, true);
    let size = 2 * 1024 * 1024;
    let id = pair.offer_and_accept("rate.bin", size as u64);
    let content = data(size);
    let started = Instant::now();
    pair.a.chunk(id, &content[..size / 2]).unwrap();
    pair.a.chunk(id, &content[size / 2..]).unwrap();
    // 2 MiB at 8 MiB/s is a quarter of a second.
    assert!(
        started.elapsed() >= Duration::from_millis(200),
        "{:?}",
        started.elapsed()
    );
    pair.a.finish(id).unwrap();
    pair.pump();
    assert_eq!(item(&pair.b).state, State::Finished);
}

#[test]
fn chunks_on_the_wire_respect_the_size_limit() {
    let pair = Pair::new(true, true);
    let id = pair.offer_and_accept("c.bin", 300_000);
    pair.a.chunk(id, &data(300_000)).unwrap();
    for message in lock(&pair.to_b).iter() {
        if let Message::FileChunk { data, .. } = message {
            assert!(data.len() <= MAX_FILE_CHUNK_BYTES);
        }
    }
}

#[test]
fn only_a_few_finished_transfers_are_kept() {
    let pair = Pair::new(true, true);
    for n in 0..(KEEP_FINISHED + 6) {
        let id = pair.offer_and_accept(&format!("f{n}.txt"), 1);
        pair.send_all(id, b"x");
    }
    assert!(pair.a.status().items.len() <= KEEP_FINISHED + 1);
    assert!(pair.b.status().items.len() <= KEEP_FINISHED + 1);
}

#[test]
fn dropping_the_transfers_removes_half_written_files() {
    let pair = Pair::new(true, true);
    let id = pair.offer_and_accept("half.bin", 100);
    pair.a.chunk(id, &data(50)).unwrap();
    pair.pump();
    let folder = pair.received_by_b();
    assert_eq!(files_in(&folder), ["half.bin.part"]);
    let Pair { b, dir_b, .. } = pair;
    drop(b);
    assert!(files_in(&dir_b.path().join("received")).is_empty());
}
