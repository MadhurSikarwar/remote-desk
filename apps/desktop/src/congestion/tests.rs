use std::time::{Duration, Instant};

use super::*;
use crate::link::Baseline;

/// A path with a fixed capacity and a buffer in front of it, the sender's
/// video, and pings that wait in the same buffer: what the controller is up
/// against, without a network.
struct Path {
    capacity_kbps: f64,
    /// The round trip of the path itself, with nothing queued.
    rtt_ms: f64,
    /// Round trips measured since the last step.
    measured: Vec<f64>,
    /// Bytes waiting.
    queued: f64,
    /// Pings on their way: when each was sent and when it comes back.
    pings: Vec<(f64, f64)>,
    last_ping: f64,
    latest_rtt_ms: f64,
    /// Bytes that have come out of the far end.
    delivered: f64,
}

const TICK: f64 = 0.05;
/// As often as the link pings by default.
const PING_EVERY: f64 = 0.25;

impl Path {
    fn new(capacity_kbps: f64) -> Self {
        Self {
            capacity_kbps,
            rtt_ms: 1.0,
            measured: Vec::new(),
            queued: 0.0,
            pings: Vec::new(),
            last_ping: -1.0,
            latest_rtt_ms: 0.0,
            delivered: 0.0,
        }
    }

    /// How far behind the picture is, in milliseconds.
    fn lag_ms(&self) -> f64 {
        self.queued * 8.0 / (self.capacity_kbps * 1000.0) * 1000.0
    }

    /// Advances one tick with the sender putting in `kbps`, and returns the
    /// round trip the sender can know about at time `now`: the latest
    /// answer, or how long the oldest unanswered ping has been waiting.
    fn step(&mut self, now: f64, kbps: f64) -> f64 {
        let per_tick = |kbps: f64| kbps * 1000.0 / 8.0 * TICK;
        self.queued += per_tick(kbps);
        let out = self.queued.min(per_tick(self.capacity_kbps));
        self.queued -= out;
        self.delivered += out;

        if now - self.last_ping >= PING_EVERY {
            self.last_ping = now;
            self.pings
                .push((now, now + (self.rtt_ms + self.lag_ms()) / 1000.0));
        }
        while self.pings.first().is_some_and(|(_, back)| *back <= now) {
            let (sent, back) = self.pings.remove(0);
            self.latest_rtt_ms = (back - sent) * 1000.0;
            self.measured.push(self.latest_rtt_ms);
        }
        let waiting_ms = self
            .pings
            .first()
            .map_or(0.0, |(sent, _)| (now - sent) * 1000.0);
        self.latest_rtt_ms.max(waiting_ms)
    }
}

struct Run {
    worst_lag_ms: f64,
    worst_lag_after_ms: f64,
    final_kbps: u32,
    delivered_kbps_late: f64,
    held_ticks: usize,
    ticks: usize,
}

/// Runs a sender that always has `max_kbps` worth of video to send through a
/// path whose capacity is given per second by `capacity`. `settle` is the
/// number of seconds after which `worst_lag_after_ms` starts counting.
fn run(
    max_kbps: u32,
    seconds: usize,
    settle: usize,
    adaptive: bool,
    capacity: impl Fn(f64) -> f64,
) -> Run {
    run_on(max_kbps, seconds, settle, adaptive, capacity, |_| 1.0)
}

/// As [`run`], with the path's own round trip given per second by `rtt_ms`.
fn run_on(
    max_kbps: u32,
    seconds: usize,
    settle: usize,
    adaptive: bool,
    capacity: impl Fn(f64) -> f64,
    rtt_ms: impl Fn(f64) -> f64,
) -> Run {
    let start = Instant::now();
    let mut controller = Controller::new(max_kbps);
    let mut path = Path::new(capacity(0.0));
    let mut out = Run {
        worst_lag_ms: 0.0,
        worst_lag_after_ms: 0.0,
        final_kbps: max_kbps,
        delivered_kbps_late: 0.0,
        held_ticks: 0,
        ticks: 0,
    };
    let mut delay = 0.0;
    let mut baseline = Baseline::default();
    let mut delivered_at_settle = 0.0;
    let steps = (seconds as f64 / TICK) as usize;
    for i in 0..steps {
        let now = i as f64 * TICK;
        path.capacity_kbps = capacity(now);
        path.rtt_ms = rtt_ms(now);
        let decision = if adaptive {
            controller.update(start + Duration::from_secs_f64(now), delay)
        } else {
            Pace {
                send: true,
                bitrate_kbps: max_kbps,
            }
        };
        if i == 0 && adaptive {
            assert!(
                decision.bitrate_kbps < max_kbps,
                "a session starts below the maximum"
            );
        }
        let sending = if decision.send {
            f64::from(decision.bitrate_kbps)
        } else {
            0.0
        };
        // As the link does it: queueing is what the round trip shows beyond
        // the path's own.
        let at = start + Duration::from_secs_f64(now);
        let known = path.step(now, sending);
        for rtt in path.measured.drain(..) {
            baseline.record(at, rtt);
        }
        delay = baseline
            .base_ms(at)
            .map_or(0.0, |base| (known - base).max(0.0));
        out.worst_lag_ms = out.worst_lag_ms.max(path.lag_ms());
        if now >= settle as f64 {
            out.worst_lag_after_ms = out.worst_lag_after_ms.max(path.lag_ms());
        } else {
            delivered_at_settle = path.delivered;
        }
        out.held_ticks += usize::from(!decision.send);
        out.ticks += 1;
        out.final_kbps = decision.bitrate_kbps;
    }
    out.delivered_kbps_late =
        (path.delivered - delivered_at_settle) * 8.0 / 1000.0 / (seconds - settle) as f64;
    out
}

#[test]
fn a_path_with_room_to_spare_is_left_alone() {
    let r = run(8000, 60, 0, true, |_| 20_000.0);
    assert_eq!(r.final_kbps, 8000);
    assert_eq!(r.held_ticks, 0);
    assert!(r.worst_lag_ms < 1.0, "{}", r.worst_lag_ms);
}

#[test]
fn without_it_a_slow_path_falls_further_and_further_behind() {
    // The measured case: video wanting 1300 kbps through 800 kbps.
    let r = run(1300, 40, 0, false, |_| 800.0);
    assert!(r.worst_lag_ms > 20_000.0, "{}", r.worst_lag_ms);
}

#[test]
fn a_slow_path_is_kept_close_to_live() {
    for (max, capacity) in [(1300, 800.0), (8000, 800.0), (8000, 3000.0), (4000, 300.0)] {
        let r = run(max, 120, 15, true, |_| capacity);
        eprintln!(
            "{max} kbps into {capacity}: worst lag {:.0} ms, after settling {:.0} ms, \
             delivered {:.0} kbps, held {:.0}% of the time, ends at {} kbps",
            r.worst_lag_ms,
            r.worst_lag_after_ms,
            r.delivered_kbps_late,
            100.0 * r.held_ticks as f64 / r.ticks as f64,
            r.final_kbps
        );
        assert!(
            r.worst_lag_ms < 2500.0,
            "{max}/{capacity}: {}",
            r.worst_lag_ms
        );
        assert!(
            r.worst_lag_after_ms < 1000.0,
            "{max}/{capacity}: {}",
            r.worst_lag_after_ms
        );
        // Not by sending nothing: most of what the path can carry is used.
        assert!(
            r.delivered_kbps_late > capacity * 0.8,
            "{max}/{capacity}: only {:.0} kbps delivered",
            r.delivered_kbps_late
        );
    }
}

#[test]
fn the_rate_comes_back_when_the_path_recovers() {
    let r = run(
        8000,
        150,
        60,
        true,
        |t| if t < 60.0 { 800.0 } else { 50_000.0 },
    );
    assert_eq!(r.final_kbps, 8000);
    assert!(r.worst_lag_after_ms < 2000.0, "{}", r.worst_lag_after_ms);
}

#[test]
fn holding_starts_above_the_high_mark_and_ends_below_the_low_one() {
    let start = Instant::now();
    let at = |ms: u64| start + Duration::from_millis(ms);
    let mut c = Controller::new(4000);
    assert!(c.update(at(0), 0.0).send);
    assert!(c.update(at(10), HIGH_MS - 1.0).send, "below the high mark");
    assert!(!c.update(at(20), HIGH_MS + 1.0).send);
    assert!(
        !c.update(at(30), LOW_MS + 1.0).send,
        "between the marks it keeps holding"
    );
    assert!(c.update(at(40), LOW_MS - 1.0).send);
    assert!(
        c.update(at(50), LOW_MS + 1.0).send,
        "between the marks it keeps sending"
    );
    assert!(
        c.status().bitrate_kbps < 1200,
        "the rate was cut from where it started"
    );
}

#[test]
fn the_rate_never_goes_below_the_minimum_or_above_the_maximum() {
    let start = Instant::now();
    let mut c = Controller::new(4000);
    for second in 0..120 {
        let d = c.update(start + Duration::from_secs(second), 60_000.0);
        assert!(!d.send);
        assert!(d.bitrate_kbps >= MIN_KBPS);
    }
    assert_eq!(c.status().bitrate_kbps, MIN_KBPS);
    for second in 120..400 {
        let d = c.update(start + Duration::from_secs(second), 0.0);
        assert!(d.send && d.bitrate_kbps <= 4000);
    }
    assert_eq!(c.status().bitrate_kbps, 4000);
}

#[test]
fn a_configured_maximum_is_never_exceeded_even_below_the_usual_minimum() {
    // The settings accept 100 kbit/s, which is under the controller's usual
    // 150 kbit/s floor: the person's maximum wins, whatever the path does.
    let start = Instant::now();
    for max in [1u32, 10, 100, 149, 150, 151, 4000] {
        let mut c = Controller::new(max);
        for second in 0..300 {
            // Congested for the first 60 s, clear afterwards.
            let delay = if second < 60 { 60_000.0 } else { 0.0 };
            let d = c.update(start + Duration::from_secs(second), delay);
            assert!(
                d.bitrate_kbps <= max,
                "{max} kbit/s configured, but {} was asked for at {second} s",
                d.bitrate_kbps
            );
            assert!(d.bitrate_kbps >= MIN_KBPS.min(max));
        }
        assert_eq!(c.status().max_kbps, max, "the status shows another maximum");
        assert_eq!(
            c.status().bitrate_kbps,
            max,
            "it never recovered to the maximum"
        );
    }
}

#[test]
fn a_path_that_gets_longer_is_not_mistaken_for_one_that_is_full() {
    // After 20 s every round trip takes 200 ms longer (another route, a
    // relay), and for the first few seconds the path is also nearly blocked,
    // so the video is on hold when the change comes. Seen live before the
    // baseline moved with the path: the hold never ended, because 200 ms
    // over the old baseline is neither clearly congested nor clearly clear.
    let r = run_on(
        4000,
        120,
        50,
        true,
        |t| {
            if (20.0..26.0).contains(&t) {
                100.0
            } else {
                50_000.0
            }
        },
        |t| if t < 20.0 { 1.0 } else { 201.0 },
    );
    eprintln!(
        "longer path: held {:.0}% of the whole run, delivered {:.0} kbps after settling, ends at {} kbps",
        100.0 * r.held_ticks as f64 / r.ticks as f64,
        r.delivered_kbps_late,
        r.final_kbps
    );
    assert_eq!(r.final_kbps, 4000);
    assert!(r.delivered_kbps_late > 3000.0, "{}", r.delivered_kbps_late);
}
