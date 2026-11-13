//! File transfer between the two ends of a session.
//!
//! Rules, because the other end is another computer and another person:
//!
//! - **Off unless allowed.** While transfers are not allowed every offer is
//!   refused and nothing can be offered.
//! - **The receiver decides about each file.** An offer sits waiting for the
//!   receiving person to accept; no answer within a minute is a refusal.
//! - **The sender never names a place.** Only a file *name* travels. It is
//!   cleaned (no folders, no characters Windows forbids, no reserved device
//!   names), and the file is saved into one folder chosen by this program,
//!   under a name that does not replace anything already there.
//! - **The sender only sends what its user picked.** This module never
//!   opens a path from the network or from a message; the bytes of an
//!   outgoing file are handed to it by the caller.
//! - **Checked at the end.** The file must be exactly the size offered and
//!   match the SHA-256 the sender computed, or it is deleted.
//! - **Marked as downloaded.** Saved files get the Windows "from another
//!   computer" mark, so SmartScreen and Office treat programs and documents
//!   from here with the usual caution.
//! - **Bounded.** A size limit, one file being received at a time, a few
//!   waiting offers, and a data rate limit for sending so a transfer cannot
//!   starve the screen it shares a connection with.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rc_protocol::{Message, MAX_FILE_CHUNK_BYTES};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::secure::lock;

/// Largest file accepted or offered.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// How long an offer waits for the receiving person.
pub const OFFER_TIMEOUT: Duration = Duration::from_secs(60);
/// Offers waiting for a decision, plus files being received.
const MAX_INCOMING: usize = 4;
/// Files being sent at once.
const MAX_OUTGOING: usize = 2;
/// Finished, refused and failed transfers kept for display.
const KEEP_FINISHED: usize = 12;
/// Sending is held to this many bytes a second on average.
const SEND_RATE: u64 = 8 * 1024 * 1024;
const MAX_NAME_CHARS: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Incoming,
    Outgoing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// An incoming offer waiting for this person to accept or refuse.
    WaitingForYou,
    /// An outgoing offer waiting for the other person.
    WaitingForPeer,
    Transferring,
    Finished,
    Refused,
    Cancelled,
    Failed,
}

impl State {
    fn active(self) -> bool {
        matches!(
            self,
            Self::WaitingForYou | Self::WaitingForPeer | Self::Transferring
        )
    }
}

/// What the window shows about one transfer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TransferInfo {
    pub id: u32,
    pub direction: Direction,
    pub name: String,
    pub size: u64,
    pub done: u64,
    pub state: State,
    /// Why it ended badly, or where it was saved.
    pub detail: Option<String>,
    /// Seconds left to answer an incoming offer.
    pub seconds_left: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TransferStatus {
    /// Transfers are currently allowed.
    pub allowed: bool,
    /// Where received files are saved.
    pub folder: String,
    pub items: Vec<TransferInfo>,
}

/// Turns what the sender calls a file into a name that is safe to create.
pub fn safe_file_name(name: &str) -> String {
    // Only the last part of anything that looks like a path.
    let last = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut cleaned: String = last
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*'))
        .collect();
    cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_owned();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        return "file".to_owned();
    }
    // Reserved device names (with or without an extension) cannot be files.
    let stem = cleaned.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix)
                .is_some_and(|n| n.len() == 1 && n != "0" && n.chars().all(|c| c.is_ascii_digit()))
        });
    if reserved {
        cleaned.insert(0, '_');
    }
    // Keep the extension when shortening.
    if cleaned.chars().count() > MAX_NAME_CHARS {
        let (stem, ext) = match cleaned.rfind('.') {
            Some(dot) if cleaned.chars().count() - cleaned[..dot].chars().count() <= 20 => {
                (&cleaned[..dot], &cleaned[dot..])
            }
            _ => (cleaned.as_str(), ""),
        };
        let keep = MAX_NAME_CHARS.saturating_sub(ext.chars().count());
        cleaned = stem.chars().take(keep).collect::<String>() + ext;
    }
    cleaned
}

/// A file name split before its extension: `report.pdf` is `report` and
/// `.pdf`; a name that only starts with a dot has no extension.
fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    }
}

/// `name`, or `name (1)`, `name (2)`… before the extension, whichever does
/// not exist yet in `dir` (also counting a `.part` file being written).
///
/// This only chooses a name for the file to be written under. Whether the
/// name is still free when the file is finished is decided by
/// [`finalize_without_replacing`], not by this look.
fn unused_name(dir: &Path, name: &str) -> PathBuf {
    let taken = |candidate: &str| {
        dir.join(candidate).exists() || dir.join(format!("{candidate}.part")).exists()
    };
    if !taken(name) {
        return dir.join(name);
    }
    let (stem, ext) = split_name(name);
    for n in 1..10_000 {
        let candidate = format!("{stem} ({n}){ext}");
        if !taken(&candidate) {
            return dir.join(candidate);
        }
    }
    dir.join(format!("{stem} (copy){ext}"))
}

/// Moves the finished file `part` to `wanted`, or to `wanted (1)`, `(2)`…
/// when that name is taken, and never replaces a file that exists.
///
/// Looking for a free name and then renaming is not safe: a rename replaces
/// an existing file (on Windows and on Unix), and another program may create
/// the file between the look and the rename. So the name is claimed first
/// with `create_new`, which only one caller can win, and the finished file is
/// then moved over that empty placeholder of ours.
fn finalize_without_replacing(part: &Path, wanted: &Path) -> io::Result<PathBuf> {
    let dir = wanted.parent().unwrap_or_else(|| Path::new("."));
    let name = wanted
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = split_name(&name);
    for n in 0..10_000 {
        let candidate = if n == 0 {
            wanted.to_path_buf()
        } else {
            dir.join(format!("{stem} ({n}){ext}"))
        };
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(placeholder) => {
                drop(placeholder);
                return match fs::rename(part, &candidate) {
                    Ok(()) => Ok(candidate),
                    Err(e) => {
                        let _ = fs::remove_file(&candidate);
                        Err(e)
                    }
                };
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            // Windows says "access denied", not "exists", when the name
            // belongs to a folder. Taken either way; a folder that cannot be
            // written to at all is still an error (nothing is there).
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && candidate.exists() => {}
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free name was found for the file",
    ))
}

/// Marks a saved file as having come from another computer, so Windows
/// warns before it is run (an NTFS stream). Other systems have no mark that
/// is set this way (macOS has a quarantine attribute, not done yet), and the
/// stream name must not be used there: it would make a stray file.
#[cfg(windows)]
fn mark_as_downloaded(path: &Path) {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    let _ = fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n");
}

#[cfg(not(windows))]
fn mark_as_downloaded(_path: &Path) {}

enum Work {
    None,
    Receiving {
        file: File,
        part: PathBuf,
        target: PathBuf,
        hasher: Sha256,
    },
    Sending {
        hasher: Sha256,
        started: Instant,
    },
}

struct Entry {
    direction: Direction,
    id: u32,
    name: String,
    size: u64,
    done: u64,
    state: State,
    detail: Option<String>,
    deadline: Option<Instant>,
    work: Work,
}

impl Entry {
    fn info(&self) -> TransferInfo {
        TransferInfo {
            id: self.id,
            direction: self.direction,
            name: self.name.clone(),
            size: self.size,
            done: self.done,
            state: self.state,
            detail: self.detail.clone(),
            seconds_left: self
                .deadline
                .filter(|_| self.state == State::WaitingForYou)
                .map(|d| d.saturating_duration_since(Instant::now()).as_secs()),
        }
    }

    /// Ends the transfer in `state`, removing a half-written file.
    fn end(&mut self, state: State, detail: impl Into<Option<String>>) {
        if let Work::Receiving { part, .. } = std::mem::replace(&mut self.work, Work::None) {
            let _ = fs::remove_file(part);
        }
        self.work = Work::None;
        self.state = state;
        self.detail = detail.into();
        self.deadline = None;
    }
}

struct Inner {
    next_id: u32,
    entries: Vec<Entry>,
}

impl Inner {
    fn find(&mut self, direction: Direction, id: u32) -> Option<&mut Entry> {
        self.entries
            .iter_mut()
            .find(|e| e.direction == direction && e.id == id)
    }

    fn active(&self, direction: Direction) -> usize {
        self.entries
            .iter()
            .filter(|e| e.direction == direction && e.state.active())
            .count()
    }

    /// Drops the oldest finished entries beyond the number kept.
    fn trim(&mut self) {
        let finished = self.entries.iter().filter(|e| !e.state.active()).count();
        let mut excess = finished.saturating_sub(KEEP_FINISHED);
        self.entries.retain(|e| {
            if excess > 0 && !e.state.active() {
                excess -= 1;
                false
            } else {
                true
            }
        });
    }
}

type Sender = Box<dyn Fn(Message) -> Result<(), String> + Send + Sync>;

pub struct Transfers {
    folder: PathBuf,
    offer_timeout: Duration,
    allowed: AtomicBool,
    inner: Mutex<Inner>,
    send: Arc<dyn Fn(Message) -> Result<(), String> + Send + Sync>,
}

impl Transfers {
    /// `folder` is where received files go; `send` delivers a message to the
    /// other end.
    pub fn new(folder: PathBuf, allowed: bool, send: Sender) -> Self {
        Self {
            folder,
            offer_timeout: OFFER_TIMEOUT,
            allowed: AtomicBool::new(allowed),
            inner: Mutex::new(Inner {
                next_id: 1,
                entries: Vec::new(),
            }),
            send: Arc::from(send),
        }
    }

    /// How long an incoming offer waits for the person (tests shorten it).
    #[cfg(test)]
    fn with_offer_timeout(mut self, timeout: Duration) -> Self {
        self.offer_timeout = timeout;
        self
    }

    fn tell(&self, messages: Vec<Message>) {
        for message in messages {
            // The connection may be closing; nothing to do about it here.
            let _ = (self.send)(message);
        }
    }

    pub fn is_allowed(&self) -> bool {
        self.allowed.load(Ordering::Relaxed)
    }

    /// Allows or forbids transfers. Forbidding ends every active one.
    pub fn set_allowed(&self, allowed: bool) {
        self.allowed.store(allowed, Ordering::Relaxed);
        if !allowed {
            self.cancel_all("file transfer was switched off");
        }
    }

    /// Forbids transfers and ends what is in progress, for the person
    /// switching them off. Everything local is done before this returns:
    /// nothing more is accepted or written and half-written files are gone.
    /// The notices to the other end go out on a thread of their own, so a
    /// stalled connection cannot hold up the caller (which is the window).
    pub fn revoke(&self) {
        self.allowed.store(false, Ordering::Relaxed);
        let messages = self.end_all("file transfer was switched off");
        if messages.is_empty() {
            return;
        }
        let send = Arc::clone(&self.send);
        let _ = std::thread::Builder::new()
            .name("transfer-cancel".into())
            .spawn(move || {
                for message in messages {
                    let _ = send(message);
                }
            });
    }

    /// Ends everything in progress, telling the other end. Used when
    /// transfers are forbidden and when the session ends.
    pub fn cancel_all(&self, reason: &str) {
        let messages = self.end_all(reason);
        self.tell(messages);
    }

    /// Ends everything in progress and returns the notices to send.
    fn end_all(&self, reason: &str) -> Vec<Message> {
        let mut messages = Vec::new();
        let mut inner = lock(&self.inner);
        for entry in inner.entries.iter_mut().filter(|e| e.state.active()) {
            messages.push(Message::FileCancel {
                id: entry.id,
                by_sender: entry.direction == Direction::Outgoing,
                reason: reason.to_owned(),
            });
            entry.end(State::Cancelled, reason.to_owned());
        }
        messages
    }

    /// Refuses offers nobody answered in time.
    fn expire(&self, inner: &mut Inner, messages: &mut Vec<Message>) {
        let now = Instant::now();
        for entry in &mut inner.entries {
            if entry.state == State::WaitingForYou && entry.deadline.is_some_and(|d| now >= d) {
                messages.push(Message::FileAnswer {
                    id: entry.id,
                    accept: false,
                    reason: "nobody answered in time".to_owned(),
                });
                entry.end(State::Refused, "nobody answered in time".to_owned());
            }
        }
    }

    pub fn status(&self) -> TransferStatus {
        let mut messages = Vec::new();
        let items = {
            let mut inner = lock(&self.inner);
            self.expire(&mut inner, &mut messages);
            inner.entries.iter().map(Entry::info).collect()
        };
        self.tell(messages);
        TransferStatus {
            allowed: self.is_allowed(),
            folder: self.folder.display().to_string(),
            items,
        }
    }

    // ---- messages from the other end -------------------------------------

    /// Acts on a transfer message from the other end. Other messages are
    /// ignored.
    pub fn handle(&self, message: &Message) {
        let mut replies = Vec::new();
        {
            let mut inner = lock(&self.inner);
            self.expire(&mut inner, &mut replies);
            match message {
                Message::FileOffer { id, name, size } => {
                    self.on_offer(&mut inner, *id, name, *size, &mut replies);
                }
                Message::FileChunk { id, data } => {
                    Self::on_chunk(&mut inner, *id, data, &mut replies)
                }
                Message::FileEnd { id, sha256 } => {
                    Self::on_end(&mut inner, *id, sha256, &mut replies)
                }
                Message::FileAnswer { id, accept, reason } => {
                    if let Some(entry) = inner
                        .find(Direction::Outgoing, *id)
                        .filter(|e| e.state == State::WaitingForPeer)
                    {
                        if *accept {
                            entry.state = State::Transferring;
                            entry.work = Work::Sending {
                                hasher: Sha256::new(),
                                started: Instant::now(),
                            };
                        } else {
                            let why = if reason.is_empty() { "refused" } else { reason };
                            entry.end(State::Refused, why.to_owned());
                        }
                    }
                }
                Message::FileDone { id } => {
                    if let Some(entry) = inner
                        .find(Direction::Outgoing, *id)
                        .filter(|e| e.state == State::Transferring)
                    {
                        entry.state = State::Finished;
                        entry.detail = Some("delivered".to_owned());
                    }
                }
                Message::FileCancel {
                    id,
                    by_sender,
                    reason,
                } => {
                    // `by_sender` is the sender's numbering: from the other
                    // end it names one of our incoming transfers; otherwise
                    // one of our outgoing ones.
                    let direction = if *by_sender {
                        Direction::Incoming
                    } else {
                        Direction::Outgoing
                    };
                    if let Some(entry) = inner.find(direction, *id).filter(|e| e.state.active()) {
                        let why = if reason.is_empty() {
                            "cancelled by the other side".to_owned()
                        } else {
                            reason.clone()
                        };
                        entry.end(State::Cancelled, why);
                    }
                }
                _ => {}
            }
            inner.trim();
        }
        self.tell(replies);
    }

    fn on_offer(
        &self,
        inner: &mut Inner,
        id: u32,
        name: &str,
        size: u64,
        replies: &mut Vec<Message>,
    ) {
        let refuse = |reason: &str, replies: &mut Vec<Message>| {
            replies.push(Message::FileAnswer {
                id,
                accept: false,
                reason: reason.to_owned(),
            });
        };
        if !self.is_allowed() {
            return refuse("file transfer is not allowed", replies);
        }
        if size > MAX_FILE_BYTES {
            return refuse("the file is too large", replies);
        }
        if inner
            .entries
            .iter()
            .any(|e| e.direction == Direction::Incoming && e.id == id && e.state.active())
        {
            return refuse("that transfer number is already in use", replies);
        }
        if inner.active(Direction::Incoming) >= MAX_INCOMING {
            return refuse("too many files are waiting", replies);
        }
        inner.entries.push(Entry {
            direction: Direction::Incoming,
            id,
            name: safe_file_name(name),
            size,
            done: 0,
            state: State::WaitingForYou,
            detail: None,
            deadline: Some(Instant::now() + self.offer_timeout),
            work: Work::None,
        });
        tracing::info!(id, size, "a file was offered");
    }

    fn on_chunk(inner: &mut Inner, id: u32, data: &[u8], replies: &mut Vec<Message>) {
        let Some(entry) = inner.find(Direction::Incoming, id) else {
            return;
        };
        if entry.state != State::Transferring {
            return;
        }
        let mut fail = |entry: &mut Entry, reason: String| {
            replies.push(Message::FileCancel {
                id,
                by_sender: false,
                reason: reason.clone(),
            });
            entry.end(State::Failed, reason);
        };
        if entry.done + data.len() as u64 > entry.size {
            return fail(entry, "more data than was offered".to_owned());
        }
        let Work::Receiving { file, hasher, .. } = &mut entry.work else {
            return;
        };
        if let Err(e) = file.write_all(data) {
            return fail(entry, format!("cannot write the file: {e}"));
        }
        hasher.update(data);
        entry.done += data.len() as u64;
    }

    fn on_end(inner: &mut Inner, id: u32, sha256: &[u8; 32], replies: &mut Vec<Message>) {
        let Some(entry) = inner.find(Direction::Incoming, id) else {
            return;
        };
        if entry.state != State::Transferring {
            return;
        }
        let mut fail = |entry: &mut Entry, reason: &str| {
            replies.push(Message::FileCancel {
                id,
                by_sender: false,
                reason: reason.to_owned(),
            });
            entry.end(State::Failed, reason.to_owned());
        };
        if entry.done != entry.size {
            return fail(entry, "the file is incomplete");
        }
        let Work::Receiving {
            mut file,
            part,
            target,
            hasher,
        } = std::mem::replace(&mut entry.work, Work::None)
        else {
            return;
        };
        let matches = hasher.finalize().as_slice() == sha256;
        if !matches || file.flush().and_then(|()| file.sync_all()).is_err() {
            let _ = fs::remove_file(&part);
            return fail(entry, "the file arrived damaged and was deleted");
        }
        drop(file);
        // The name may have been taken while the file was coming, even a
        // moment ago: it is claimed here, never assumed.
        let final_path = match finalize_without_replacing(&part, &target) {
            Ok(path) => path,
            Err(e) => {
                let _ = fs::remove_file(&part);
                return fail(entry, &format!("cannot save the file: {e}"));
            }
        };
        mark_as_downloaded(&final_path);
        entry.state = State::Finished;
        entry.detail = Some(format!("saved as {}", final_path.display()));
        replies.push(Message::FileDone { id });
        tracing::info!(id, size = entry.size, "a file was received and checked");
    }

    // ---- decisions and actions by this end's person -----------------------

    /// The person's answer to an incoming offer.
    pub fn answer(&self, id: u32, accept: bool) -> Result<(), String> {
        let mut replies = Vec::new();
        let result = {
            let mut inner = lock(&self.inner);
            self.expire(&mut inner, &mut replies);
            match inner
                .find(Direction::Incoming, id)
                .filter(|e| e.state == State::WaitingForYou)
            {
                None => Err("That offer is no longer waiting".to_owned()),
                Some(entry) if !accept => {
                    replies.push(Message::FileAnswer {
                        id,
                        accept: false,
                        reason: "refused".to_owned(),
                    });
                    entry.end(State::Refused, "you refused it".to_owned());
                    Ok(())
                }
                Some(_) if !self.is_allowed() => {
                    Err("File transfer is not allowed right now".to_owned())
                }
                Some(entry) => self.start_receiving(entry, &mut replies),
            }
        };
        self.tell(replies);
        result
    }

    fn start_receiving(&self, entry: &mut Entry, replies: &mut Vec<Message>) -> Result<(), String> {
        let prepared = fs::create_dir_all(&self.folder)
            .map_err(|e| format!("cannot create {}: {e}", self.folder.display()))
            .and_then(|()| {
                let target = unused_name(&self.folder, &entry.name);
                let mut part = target.clone().into_os_string();
                part.push(".part");
                let part = PathBuf::from(part);
                let file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&part)
                    .map_err(|e| format!("cannot create {}: {e}", part.display()))?;
                Ok((file, part, target))
            });
        match prepared {
            Ok((file, part, target)) => {
                entry.state = State::Transferring;
                entry.deadline = None;
                entry.work = Work::Receiving {
                    file,
                    part,
                    target,
                    hasher: Sha256::new(),
                };
                replies.push(Message::FileAnswer {
                    id: entry.id,
                    accept: true,
                    reason: String::new(),
                });
                Ok(())
            }
            Err(error) => {
                replies.push(Message::FileAnswer {
                    id: entry.id,
                    accept: false,
                    reason: "the receiver could not save the file".to_owned(),
                });
                entry.end(State::Failed, error.clone());
                Err(error)
            }
        }
    }

    /// Offers a file to the other end. Returns the transfer's number. The
    /// bytes follow through [`chunk`](Self::chunk) once it is accepted.
    pub fn offer(&self, name: &str, size: u64) -> Result<u32, String> {
        if !self.is_allowed() {
            return Err("File transfer is not allowed".to_owned());
        }
        if size > MAX_FILE_BYTES {
            return Err(format!(
                "That file is larger than the {} GiB limit",
                MAX_FILE_BYTES >> 30
            ));
        }
        let id = {
            let mut inner = lock(&self.inner);
            if inner.active(Direction::Outgoing) >= MAX_OUTGOING {
                return Err("Too many files are being sent; wait for one to finish".to_owned());
            }
            let id = inner.next_id;
            inner.next_id += 1;
            inner.entries.push(Entry {
                direction: Direction::Outgoing,
                id,
                name: safe_file_name(name),
                size,
                done: 0,
                state: State::WaitingForPeer,
                detail: None,
                deadline: None,
                work: Work::None,
            });
            id
        };
        (self.send)(Message::FileOffer {
            id,
            name: name.to_owned(),
            size,
        })?;
        tracing::info!(id, size, "a file was offered to the other side");
        Ok(id)
    }

    /// The state of one of this end's outgoing transfers.
    #[cfg(test)]
    pub fn outgoing_state(&self, id: u32) -> Option<State> {
        lock(&self.inner)
            .find(Direction::Outgoing, id)
            .map(|e| e.state)
    }

    /// Sends the next piece of an accepted outgoing file.
    pub fn chunk(&self, id: u32, data: &[u8]) -> Result<(), String> {
        let pause = {
            let mut inner = lock(&self.inner);
            let entry = inner
                .find(Direction::Outgoing, id)
                .ok_or("That transfer does not exist")?;
            if entry.state != State::Transferring {
                return Err(match entry.state {
                    State::WaitingForPeer => {
                        "The other side has not accepted the file yet".to_owned()
                    }
                    State::Refused | State::Cancelled | State::Failed => entry
                        .detail
                        .clone()
                        .unwrap_or_else(|| "The transfer ended".to_owned()),
                    _ => "The transfer is not in progress".to_owned(),
                });
            }
            if entry.done + data.len() as u64 > entry.size {
                entry.end(State::Failed, "more data than the file's size".to_owned());
                return Err("More data than the size that was offered".to_owned());
            }
            let Work::Sending { hasher, started } = &mut entry.work else {
                return Err("The transfer is not in progress".to_owned());
            };
            hasher.update(data);
            entry.done += data.len() as u64;
            // Hold the average rate to the limit.
            let due = Duration::from_secs_f64(entry.done as f64 / SEND_RATE as f64);
            due.saturating_sub(started.elapsed())
        };
        for piece in data.chunks(MAX_FILE_CHUNK_BYTES) {
            (self.send)(Message::FileChunk {
                id,
                data: piece.to_vec(),
            })?;
        }
        if !pause.is_zero() {
            std::thread::sleep(pause.min(Duration::from_secs(2)));
        }
        Ok(())
    }

    /// Says the whole file has been sent.
    pub fn finish(&self, id: u32) -> Result<(), String> {
        let sha256 = {
            let mut inner = lock(&self.inner);
            let entry = inner
                .find(Direction::Outgoing, id)
                .filter(|e| e.state == State::Transferring)
                .ok_or("That transfer is not in progress")?;
            if entry.done != entry.size {
                entry.end(
                    State::Failed,
                    "fewer bytes were sent than offered".to_owned(),
                );
                return Err("Fewer bytes were sent than the size that was offered".to_owned());
            }
            let Work::Sending { hasher, .. } = std::mem::replace(&mut entry.work, Work::None)
            else {
                return Err("The transfer is not in progress".to_owned());
            };
            // Stays "transferring" until the other end confirms.
            entry.detail = Some("waiting for the other side to confirm".to_owned());
            let mut digest = [0u8; 32];
            digest.copy_from_slice(&hasher.finalize());
            digest
        };
        (self.send)(Message::FileEnd { id, sha256 })
    }

    /// The person cancels one transfer, in either direction.
    pub fn cancel(&self, direction: Direction, id: u32) {
        let message = {
            let mut inner = lock(&self.inner);
            inner
                .find(direction, id)
                .filter(|e| e.state.active())
                .map(|entry| {
                    let waiting_for_us = entry.state == State::WaitingForYou;
                    entry.end(State::Cancelled, "you cancelled it".to_owned());
                    if waiting_for_us {
                        Message::FileAnswer {
                            id,
                            accept: false,
                            reason: "refused".to_owned(),
                        }
                    } else {
                        Message::FileCancel {
                            id,
                            by_sender: direction == Direction::Outgoing,
                            reason: "cancelled".to_owned(),
                        }
                    }
                })
        };
        if let Some(message) = message {
            self.tell(vec![message]);
        }
    }
}

impl Drop for Transfers {
    fn drop(&mut self) {
        // Leave no half-written file behind when a session ends.
        let mut inner = lock(&self.inner);
        for entry in inner.entries.iter_mut().filter(|e| e.state.active()) {
            entry.end(State::Cancelled, "the session ended".to_owned());
        }
    }
}

#[cfg(test)]
mod tests;
