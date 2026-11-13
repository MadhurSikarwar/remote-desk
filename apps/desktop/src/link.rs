//! Liveness and round-trip time of a running session.
//!
//! Each end of a session pings the other every few seconds and answers the
//! other's pings. Two things follow:
//!
//! * A link that has silently died (a pulled cable, a NAT that forgot the
//!   connection, a sleeping laptop) is noticed after [`LinkTiming::timeout`]
//!   instead of whenever TCP gives up, which can be hours.
//! * A session with a perfectly still screen still carries traffic, so a
//!   relay does not close it as idle.
//!
//! The pings also give the round-trip time shown in the window.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use rc_protocol::Message;
use serde::Serialize;

use crate::secure::{lock, SecureWriter};

/// How often the monitor looks at the clock.
const TICK: Duration = Duration::from_millis(50);
/// Pings that have not been answered yet and are still remembered. Enough
/// to see a delay of a minute at four pings a second.
const OUTSTANDING: usize = 256;
/// Weight of the newest measurement in the smoothed round-trip time.
const SMOOTHING: f64 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkTiming {
    /// How often to ping.
    pub ping_every: Duration,
    /// How long to hear nothing at all before the link counts as dead.
    pub timeout: Duration,
}

impl Default for LinkTiming {
    fn default() -> Self {
        Self {
            // Four a second: the answers time how long data waits in
            // buffers on the way, which the sender uses to slow down, and
            // the sooner it knows the less piles up. A ping is about 31
            // bytes, so this is about 1 kbit/s each way.
            ping_every: Duration::from_millis(250),
            timeout: Duration::from_secs(15),
        }
    }
}

/// What the window shows about the link.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkStats {
    /// The most recent round trip, in milliseconds.
    pub rtt_ms: Option<f64>,
    /// A smoothed average over the session.
    pub rtt_avg_ms: Option<f64>,
    /// The lowest round trip seen.
    pub rtt_best_ms: Option<f64>,
    /// Time since anything was last heard from the other end.
    pub silent_ms: u64,
    pub pings_sent: u64,
    pub pongs_received: u64,
}

#[derive(Default)]
struct Measurements {
    next_nonce: u64,
    outstanding: VecDeque<(u64, Instant)>,
    latest: Option<f64>,
    average: Option<f64>,
    best: Option<f64>,
    pings_sent: u64,
    pongs_received: u64,
}

pub struct Link {
    writer: Arc<SecureWriter>,
    timing: LinkTiming,
    started: Instant,
    armed: AtomicBool,
    stop: AtomicBool,
    timed_out: AtomicBool,
    /// Milliseconds after `started` at which something was last heard.
    last_heard: AtomicU64,
    measurements: Mutex<Measurements>,
}

impl Link {
    fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    /// Starts watching. Until now nothing is sent and nothing times out: a
    /// viewer waiting for the host's approval is allowed to be silent.
    pub fn arm(&self) {
        self.last_heard.store(self.now_ms(), Ordering::Relaxed);
        self.armed.store(true, Ordering::Relaxed);
    }

    /// Looks at a message from the other end. Anything counts as proof of
    /// life; pings are answered and pongs measured. Returns `true` for the
    /// link's own messages, which nothing else needs to see.
    pub fn handle(&self, message: &Message) -> bool {
        self.last_heard.store(self.now_ms(), Ordering::Relaxed);
        match message {
            Message::Ping { nonce } => {
                // A failed answer is noticed by the next write or read.
                let _ = self.writer.send(&Message::Pong { nonce: *nonce });
                true
            }
            Message::Pong { nonce } => {
                self.record_pong(*nonce, Instant::now());
                true
            }
            _ => false,
        }
    }

    fn record_pong(&self, nonce: u64, now: Instant) {
        let mut m = lock(&self.measurements);
        let Some(index) = m.outstanding.iter().position(|(n, _)| *n == nonce) else {
            return;
        };
        let Some((_, sent)) = m.outstanding.remove(index) else {
            return;
        };
        let rtt = now.duration_since(sent).as_secs_f64() * 1000.0;
        m.pongs_received += 1;
        m.latest = Some(rtt);
        m.average = Some(m.average.map_or(rtt, |a| a + SMOOTHING * (rtt - a)));
        m.best = Some(m.best.map_or(rtt, |b| b.min(rtt)));
    }

    fn send_ping(&self) -> Result<(), String> {
        let nonce = {
            let mut m = lock(&self.measurements);
            let nonce = m.next_nonce;
            m.next_nonce += 1;
            if m.outstanding.len() == OUTSTANDING {
                m.outstanding.pop_front();
            }
            m.outstanding.push_back((nonce, Instant::now()));
            m.pings_sent += 1;
            nonce
        };
        self.writer.send(&Message::Ping { nonce })
    }

    /// How long data sent now would wait in buffers on the way, beyond the
    /// time the path itself takes, in milliseconds. Pings queue behind the
    /// video in the same buffers, so their round trip shows it. A ping that
    /// has not come back yet counts for as long as it has been waiting, so a
    /// delay is noticed while it grows and not only after it has passed.
    pub fn queue_delay_ms(&self) -> f64 {
        let m = lock(&self.measurements);
        let Some(best) = m.best else {
            return 0.0;
        };
        let waiting = m
            .outstanding
            .front()
            .map_or(0.0, |(_, sent)| sent.elapsed().as_secs_f64() * 1000.0);
        (m.latest.unwrap_or(0.0).max(waiting) - best).max(0.0)
    }

    /// The link was declared dead; why, for the "connection lost" message.
    pub fn lost_reason(&self) -> Option<String> {
        self.timed_out.load(Ordering::Relaxed).then(|| {
            format!(
                "no answer from the other device for {} s",
                self.timing.timeout.as_secs().max(1)
            )
        })
    }

    pub fn stats(&self) -> LinkStats {
        let m = lock(&self.measurements);
        LinkStats {
            rtt_ms: m.latest,
            rtt_avg_ms: m.average,
            rtt_best_ms: m.best,
            silent_ms: self
                .now_ms()
                .saturating_sub(self.last_heard.load(Ordering::Relaxed)),
            pings_sent: m.pings_sent,
            pongs_received: m.pongs_received,
        }
    }

    /// Decides when the link is dead. Never writes, so a write that is stuck
    /// on a dead connection cannot delay the verdict.
    fn watch(&self) {
        while !self.stop.load(Ordering::Relaxed) {
            thread::sleep(TICK);
            if !self.armed.load(Ordering::Relaxed) {
                continue;
            }
            let silent = Duration::from_millis(
                self.now_ms()
                    .saturating_sub(self.last_heard.load(Ordering::Relaxed)),
            );
            if silent >= self.timing.timeout {
                self.give_up("heard nothing");
                return;
            }
        }
    }

    /// Sends the pings. A write can block here for as long as the write
    /// timeout; that is why this is not the thread that decides.
    fn ping(&self) {
        let mut last_ping: Option<Instant> = None;
        while !self.stop.load(Ordering::Relaxed) {
            thread::sleep(TICK);
            if !self.armed.load(Ordering::Relaxed) {
                continue;
            }
            if last_ping.is_none_or(|t| t.elapsed() >= self.timing.ping_every) {
                last_ping = Some(Instant::now());
                if let Err(e) = self.send_ping() {
                    if !self.stop.load(Ordering::Relaxed) {
                        tracing::info!(error = %e, "ping could not be sent");
                        self.give_up("cannot send");
                    }
                    return;
                }
            }
        }
    }

    /// Ends the connection, which the session's reader and writer then see.
    fn give_up(&self, why: &str) {
        if self.timed_out.swap(true, Ordering::Relaxed) {
            return;
        }
        tracing::warn!(
            why,
            timeout_s = self.timing.timeout.as_secs(),
            "link is dead"
        );
        self.writer.shutdown();
    }
}

/// Keeps the monitor thread running; dropping it stops the thread.
pub struct Monitor {
    link: Arc<Link>,
}

impl Monitor {
    pub fn start(writer: Arc<SecureWriter>, timing: LinkTiming) -> Result<Self, String> {
        let link = Arc::new(Link {
            writer,
            timing,
            started: Instant::now(),
            armed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            timed_out: AtomicBool::new(false),
            last_heard: AtomicU64::new(0),
            measurements: Mutex::new(Measurements::default()),
        });
        let watcher = Arc::clone(&link);
        thread::Builder::new()
            .name("link-watch".into())
            .spawn(move || watcher.watch())
            .map_err(|e| e.to_string())?;
        let pinger = Arc::clone(&link);
        thread::Builder::new()
            .name("link-ping".into())
            .spawn(move || pinger.ping())
            .map_err(|e| e.to_string())?;
        Ok(Self { link })
    }

    pub fn link(&self) -> Arc<Link> {
        Arc::clone(&self.link)
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        self.link.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
