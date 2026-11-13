//! Finding devices that are sharing their screen on the local network.
//!
//! **How it works.** A viewer broadcasts a *query* over UDP. A device that
//! is sharing and has allowed it answers the sender directly with a *reply*:
//! its name, its public key and the TCP port to connect to. Nothing is
//! announced unprompted, so a device that is not sharing sends nothing at
//! all.
//!
//! **What it is not.** Replies are unauthenticated: anyone on the network
//! can send one, with any name. They tell a viewer where to *try*. The
//! viewer then connects, the encrypted handshake proves which key is really
//! at the other end, and the viewer refuses a device whose key differs from
//! the one in the reply. Names are claims, never proof.
//!
//! **Abuse.** Queries must be padded to at least [`MIN_QUERY_LEN`] bytes,
//! which is longer than any reply, so a forged query cannot make a device
//! send more data to a victim than the victim's address was sent. A device
//! also answers at most [`MAX_REPLIES_PER_SECOND`] queries a second.
//!
//! IPv4 only for now.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

/// UDP port that queries are sent to.
pub const DISCOVERY_PORT: u16 = 47_821;

const MAGIC: &[u8; 6] = b"RCDISC";
const VERSION: u8 = 1;
const KIND_QUERY: u8 = 1;
const KIND_REPLY: u8 = 2;

pub const KEY_LEN: usize = 32;
/// Longest device name carried, in bytes.
pub const MAX_NAME_BYTES: usize = 64;
/// A query shorter than this is ignored; see the module documentation.
pub const MIN_QUERY_LEN: usize = 128;
const MAX_REPLY_LEN: usize = 6 + 1 + 1 + 8 + 2 + 2 + KEY_LEN + 1 + MAX_NAME_BYTES;
const _: () = assert!(MAX_REPLY_LEN < MIN_QUERY_LEN);

pub const MAX_REPLIES_PER_SECOND: usize = 20;
/// Most devices a scan will report.
pub const MAX_RESULTS: usize = 64;

const RECEIVE_POLL: Duration = Duration::from_millis(100);

/// What a sharing device says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Announcement {
    /// The device's name, as it chose it. Not verified.
    pub name: String,
    /// The device's public key. Verified later, by the handshake.
    pub public_key: [u8; KEY_LEN],
    /// TCP port to connect to.
    pub port: u16,
    /// Version of the session protocol the device speaks.
    pub protocol_version: u16,
}

/// A device found by [`scan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Where the reply came from.
    pub address: IpAddr,
    pub announcement: Announcement,
}

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("network error: {0}")]
    Io(#[from] io::Error),
}

/// Keeps text printable and within `max` bytes, cutting on a character
/// boundary.
fn clean(text: &str, max: usize) -> String {
    let mut out = String::new();
    for c in text.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// A query: magic, version, kind, a random nonce, zero padding.
pub fn encode_query(nonce: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(MIN_QUERY_LEN);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[VERSION, KIND_QUERY]);
    out.extend_from_slice(&nonce.to_le_bytes());
    out.resize(MIN_QUERY_LEN, 0);
    out
}

/// The nonce of a well-formed query, or `None` if the packet should be
/// ignored (wrong magic or version, not a query, or too short).
pub fn decode_query(packet: &[u8]) -> Option<u64> {
    if packet.len() < MIN_QUERY_LEN || !packet.starts_with(MAGIC) {
        return None;
    }
    if packet[6] != VERSION || packet[7] != KIND_QUERY {
        return None;
    }
    Some(u64::from_le_bytes(packet[8..16].try_into().ok()?))
}

pub fn encode_reply(nonce: u64, announcement: &Announcement) -> Vec<u8> {
    let name = clean(&announcement.name, MAX_NAME_BYTES);
    let mut out = Vec::with_capacity(MAX_REPLY_LEN);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&[VERSION, KIND_REPLY]);
    out.extend_from_slice(&nonce.to_le_bytes());
    out.extend_from_slice(&announcement.protocol_version.to_le_bytes());
    out.extend_from_slice(&announcement.port.to_le_bytes());
    out.extend_from_slice(&announcement.public_key);
    out.push(name.len() as u8);
    out.extend_from_slice(name.as_bytes());
    out
}

/// The nonce and announcement in a well-formed reply. Every length is
/// checked; trailing bytes are an error.
pub fn decode_reply(packet: &[u8]) -> Option<(u64, Announcement)> {
    const FIXED: usize = 6 + 1 + 1 + 8 + 2 + 2 + KEY_LEN + 1;
    if packet.len() < FIXED || !packet.starts_with(MAGIC) {
        return None;
    }
    if packet[6] != VERSION || packet[7] != KIND_REPLY {
        return None;
    }
    let nonce = u64::from_le_bytes(packet[8..16].try_into().ok()?);
    let protocol_version = u16::from_le_bytes(packet[16..18].try_into().ok()?);
    let port = u16::from_le_bytes(packet[18..20].try_into().ok()?);
    let public_key = packet[20..20 + KEY_LEN].try_into().ok()?;
    let name_len = usize::from(packet[20 + KEY_LEN]);
    if name_len > MAX_NAME_BYTES || packet.len() != FIXED + name_len {
        return None;
    }
    let name = String::from_utf8(packet[FIXED..].to_vec()).ok()?;
    if name.chars().any(char::is_control) || port == 0 {
        return None;
    }
    Some((
        nonce,
        Announcement {
            name,
            public_key,
            port,
            protocol_version,
        },
    ))
}

/// A UDP socket that several programs on this computer may share, so that
/// two instances of the application can both listen for queries.
fn shared_socket(port: u16) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)).into())?;
    Ok(socket.into())
}

/// Allows at most `limit` events per second.
struct RateLimit {
    limit: usize,
    recent: VecDeque<Instant>,
}

impl RateLimit {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            recent: VecDeque::new(),
        }
    }

    fn allow(&mut self, now: Instant) -> bool {
        while self
            .recent
            .front()
            .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(1))
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= self.limit {
            return false;
        }
        self.recent.push_back(now);
        true
    }
}

/// Answers queries until dropped.
pub struct Responder {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    port: u16,
}

impl Responder {
    /// Starts answering queries on [`DISCOVERY_PORT`].
    pub fn start(announcement: Announcement) -> Result<Self, DiscoveryError> {
        Self::start_on(DISCOVERY_PORT, announcement)
    }

    /// As [`Responder::start`] on another port; for tests.
    pub fn start_on(port: u16, announcement: Announcement) -> Result<Self, DiscoveryError> {
        let socket = shared_socket(port)?;
        let port = socket.local_addr()?.port();
        socket.set_read_timeout(Some(RECEIVE_POLL))?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let stop = Arc::clone(&stop);
            thread::Builder::new()
                .name("discovery-responder".into())
                .spawn(move || {
                    let mut limit = RateLimit::new(MAX_REPLIES_PER_SECOND);
                    let mut buffer = [0u8; 512];
                    while !stop.load(Ordering::Relaxed) {
                        let Ok((len, from)) = socket.recv_from(&mut buffer) else {
                            continue;
                        };
                        let Some(nonce) = decode_query(&buffer[..len]) else {
                            continue;
                        };
                        if !limit.allow(Instant::now()) {
                            continue;
                        }
                        let _ = socket.send_to(&encode_reply(nonce, &announcement), from);
                    }
                })?
        };
        Ok(Self {
            stop,
            thread: Some(thread),
            port,
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for Responder {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A random nonce, so replies to some other scan are not mistaken for ours.
fn nonce() -> u64 {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    // The standard library seeds `RandomState` from the operating system.
    RandomState::new().build_hasher().finish()
}

/// Broadcasts a query and collects replies for `wait`.
///
/// The query goes to the limited broadcast address and to this computer
/// itself, so a device sharing on the same computer is found too.
pub fn scan(wait: Duration) -> Result<Vec<Found>, DiscoveryError> {
    scan_on(
        &[
            SocketAddr::from((Ipv4Addr::BROADCAST, DISCOVERY_PORT)),
            SocketAddr::from((Ipv4Addr::LOCALHOST, DISCOVERY_PORT)),
        ],
        wait,
    )
}

/// As [`scan`], sending the query to `targets`; for tests.
pub fn scan_on(targets: &[SocketAddr], wait: Duration) -> Result<Vec<Found>, DiscoveryError> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    socket.set_broadcast(true)?;
    socket.set_read_timeout(Some(RECEIVE_POLL))?;
    let nonce = nonce();
    let query = encode_query(nonce);
    for target in targets {
        // A target that cannot be reached (no network) is not an error.
        let _ = socket.send_to(&query, target);
    }

    let mut found: Vec<Found> = Vec::new();
    let mut buffer = [0u8; 512];
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline && found.len() < MAX_RESULTS {
        let Ok((len, from)) = socket.recv_from(&mut buffer) else {
            continue;
        };
        let Some((reply_nonce, announcement)) = decode_reply(&buffer[..len]) else {
            continue;
        };
        let entry = Found {
            address: from.ip(),
            announcement,
        };
        if reply_nonce == nonce && !found.contains(&entry) {
            found.push(entry);
        }
    }
    Ok(found)
}

/// The address other computers most likely reach this one at: the local
/// address of the interface that would be used to reach the outside. No
/// packet is sent.
pub fn local_address() -> Option<IpAddr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // An address reserved for documentation (RFC 5737); connecting a UDP
    // socket only selects a route.
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    let ip = socket.local_addr().ok()?.ip();
    (!ip.is_unspecified() && !ip.is_loopback()).then_some(ip)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announcement() -> Announcement {
        Announcement {
            name: "Workshop PC".into(),
            public_key: [0xAB; KEY_LEN],
            port: 51_000,
            protocol_version: 3,
        }
    }

    #[test]
    fn queries_are_padded_and_round_trip() {
        let query = encode_query(0xDEAD_BEEF);
        assert_eq!(query.len(), MIN_QUERY_LEN);
        assert_eq!(decode_query(&query), Some(0xDEAD_BEEF));
    }

    #[test]
    fn no_reply_is_ever_larger_than_the_smallest_query() {
        let mut long = announcement();
        long.name = "é".repeat(500);
        // The compile-time assertion next to MAX_REPLY_LEN covers the bound;
        // this checks that real replies stay within it.
        assert!(encode_reply(1, &long).len() <= MAX_REPLY_LEN);
    }

    #[test]
    fn malformed_queries_are_ignored() {
        let good = encode_query(7);
        assert_eq!(decode_query(&good[..MIN_QUERY_LEN - 1]), None);
        assert_eq!(decode_query(&[]), None);
        let mut bad = good.clone();
        bad[0] ^= 1;
        assert_eq!(decode_query(&bad), None);
        let mut version = good.clone();
        version[6] = 9;
        assert_eq!(decode_query(&version), None);
        let mut kind = good;
        kind[7] = KIND_REPLY;
        assert_eq!(decode_query(&kind), None);
    }

    #[test]
    fn replies_round_trip() {
        let reply = encode_reply(42, &announcement());
        assert_eq!(decode_reply(&reply), Some((42, announcement())));
    }

    #[test]
    fn names_are_cleaned_and_cut_on_a_character_boundary() {
        let mut odd = announcement();
        odd.name = format!("Lab\n\t{}", "é".repeat(100));
        let (_, decoded) = decode_reply(&encode_reply(1, &odd)).unwrap();
        assert!(decoded.name.starts_with("Lab"));
        assert!(decoded.name.len() <= MAX_NAME_BYTES);
        assert!(decoded.name.chars().all(|c| !c.is_control()));
    }

    #[test]
    fn malformed_replies_are_rejected() {
        let good = encode_reply(5, &announcement());
        for cut in 0..good.len() {
            assert_eq!(decode_reply(&good[..cut]), None, "cut at {cut}");
        }
        let mut trailing = good.clone();
        trailing.push(0);
        assert_eq!(decode_reply(&trailing), None);

        let name_at = 20 + KEY_LEN;
        let mut wrong_length = good.clone();
        wrong_length[name_at] += 1;
        assert_eq!(decode_reply(&wrong_length), None);
        let mut control = good.clone();
        control[name_at + 1] = 0x07;
        assert_eq!(decode_reply(&control), None);
        let mut utf8 = good.clone();
        utf8[name_at + 1] = 0xFF;
        assert_eq!(decode_reply(&utf8), None);
        let mut no_port = good;
        no_port[18] = 0;
        no_port[19] = 0;
        assert_eq!(decode_reply(&no_port), None);
    }

    #[test]
    fn the_rate_limit_allows_a_burst_then_recovers() {
        let start = Instant::now();
        let mut limit = RateLimit::new(3);
        assert!([0, 1, 2]
            .iter()
            .all(|ms| limit.allow(start + Duration::from_millis(*ms))));
        assert!(!limit.allow(start + Duration::from_millis(500)));
        assert!(limit.allow(start + Duration::from_millis(1001)));
    }

    #[test]
    fn a_responder_is_found_and_stops_answering_when_dropped() {
        let responder = Responder::start_on(0, announcement()).unwrap();
        let target = SocketAddr::from((Ipv4Addr::LOCALHOST, responder.port()));

        let found = scan_on(&[target], Duration::from_millis(600)).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].announcement, announcement());
        assert!(found[0].address.is_loopback());

        drop(responder);
        assert!(scan_on(&[target], Duration::from_millis(300))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_short_query_gets_no_answer() {
        let responder = Responder::start_on(0, announcement()).unwrap();
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(400)))
            .unwrap();
        let target = SocketAddr::from((Ipv4Addr::LOCALHOST, responder.port()));

        socket
            .send_to(&encode_query(1)[..MIN_QUERY_LEN - 1], target)
            .unwrap();
        socket.send_to(b"hello", target).unwrap();
        let mut buffer = [0u8; 512];
        assert!(
            socket.recv_from(&mut buffer).is_err(),
            "answered a short query"
        );

        // A proper one is answered.
        socket.send_to(&encode_query(2), target).unwrap();
        let (len, _) = socket.recv_from(&mut buffer).unwrap();
        assert_eq!(decode_reply(&buffer[..len]).unwrap().0, 2);
    }

    #[test]
    fn replies_to_another_scan_are_ignored() {
        let responder = Responder::start_on(0, announcement()).unwrap();
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        socket
            .send_to(&encode_query(99), (Ipv4Addr::LOCALHOST, responder.port()))
            .unwrap();
        let mut buffer = [0u8; 512];
        let (len, _) = socket.recv_from(&mut buffer).unwrap();
        // The nonce is echoed, so a scan using another nonce discards this.
        assert_eq!(decode_reply(&buffer[..len]).unwrap().0, 99);
        assert_ne!(nonce(), 99);
    }

    #[test]
    fn two_responders_can_share_a_port_for_two_instances_on_one_computer() {
        let first = Responder::start_on(0, announcement()).unwrap();
        // Bind the same port a second time, as a second instance would.
        let second = Responder::start_on(first.port(), announcement());
        assert!(second.is_ok(), "{:?}", second.err());
    }

    #[test]
    fn the_local_address_is_a_real_interface_or_nothing() {
        if let Some(ip) = local_address() {
            assert!(!ip.is_loopback() && !ip.is_unspecified());
        }
    }
}
