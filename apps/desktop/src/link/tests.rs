use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use rc_protocol::Message;

use super::*;
use crate::secure::test_support::secure_pair;
use crate::secure::SecureReader;

fn fast() -> LinkTiming {
    LinkTiming {
        ping_every: Duration::from_millis(50),
        timeout: Duration::from_secs(5),
    }
}

fn wait_until(what: &str, limit: Duration, condition: impl Fn() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < limit, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(10));
    }
}

/// Reads from one end, letting the link answer and count what arrives.
fn pump(
    mut reader: SecureReader,
    link: Arc<Link>,
    stop: Arc<AtomicBool>,
) -> JoinHandle<Result<(), String>> {
    thread::spawn(move || {
        reader.poll(&stop, |message| {
            link.handle(&message);
            ControlFlow::Continue(())
        })
    })
}

#[test]
fn two_ends_measure_a_round_trip() {
    let ((a_writer, a_reader, _), (b_writer, b_reader, _)) = secure_pair();
    let a = Monitor::start(Arc::new(a_writer), fast()).unwrap();
    let b = Monitor::start(Arc::new(b_writer), fast()).unwrap();
    let (a_link, b_link) = (a.link(), b.link());
    let stop = Arc::new(AtomicBool::new(false));
    let threads = [
        pump(a_reader, a.link(), Arc::clone(&stop)),
        pump(b_reader, b.link(), Arc::clone(&stop)),
    ];
    a_link.arm();
    b_link.arm();

    wait_until(
        "three answered pings at each end",
        Duration::from_secs(5),
        || a_link.stats().pongs_received >= 3 && b_link.stats().pongs_received >= 3,
    );
    for link in [&a_link, &b_link] {
        let stats = link.stats();
        let (latest, average, best) = (
            stats.rtt_ms.unwrap(),
            stats.rtt_avg_ms.unwrap(),
            stats.rtt_best_ms.unwrap(),
        );
        assert!(best <= latest && best <= average + 1e-9, "{stats:?}");
        assert!(latest < 500.0, "a loopback round trip took {latest} ms");
        assert!(stats.pings_sent >= stats.pongs_received);
        assert!(link.lost_reason().is_none());
    }
    stop.store(true, Ordering::Relaxed);
    for thread in threads {
        thread.join().unwrap().unwrap();
    }
}

#[test]
fn nothing_is_sent_or_timed_out_until_armed() {
    let ((a_writer, _a_reader, _), (_b_writer, mut b_reader, _)) = secure_pair();
    let timing = LinkTiming {
        ping_every: Duration::from_millis(30),
        timeout: Duration::from_millis(100),
    };
    let monitor = Monitor::start(Arc::new(a_writer), timing).unwrap();
    thread::sleep(Duration::from_millis(500));
    let link = monitor.link();
    assert_eq!(link.stats().pings_sent, 0);
    assert!(link.lost_reason().is_none());
    // Nothing reached the other end either.
    assert!(b_reader.read_message().is_err());
}

#[test]
fn a_silent_peer_is_declared_dead_and_the_connection_closed() {
    let ((a_writer, mut a_reader, _), (_b_writer, mut b_reader, _)) = secure_pair();
    let timing = LinkTiming {
        ping_every: Duration::from_millis(50),
        timeout: Duration::from_millis(400),
    };
    let monitor = Monitor::start(Arc::new(a_writer), timing).unwrap();
    let link = monitor.link();
    // The other end is connected but never answers.
    let waiting = thread::spawn(move || {
        let stop = AtomicBool::new(false);
        a_reader.poll(&stop, |_| ControlFlow::Continue(()))
    });
    let armed = Instant::now();
    link.arm();

    wait_until(
        "the link to be declared dead",
        Duration::from_secs(5),
        || link.lost_reason().is_some(),
    );
    let took = armed.elapsed();
    assert!(
        took >= Duration::from_millis(400),
        "gave up too early: {took:?}"
    );
    assert!(took < Duration::from_secs(3), "gave up too late: {took:?}");
    assert!(link.lost_reason().unwrap().contains("no answer"));
    assert!(link.stats().pings_sent >= 2, "pings were sent meanwhile");
    // Both ends see the connection end, so no session thread is left hanging.
    assert!(waiting.join().unwrap().is_err());
    let mut seen_end = false;
    for _ in 0..200 {
        match b_reader.read_message() {
            Ok(Message::Ping { .. }) => {}
            Ok(other) => panic!("unexpected {other:?}"),
            Err(_) => {
                seen_end = true;
                break;
            }
        }
    }
    assert!(seen_end);
}

#[test]
fn traffic_of_any_kind_counts_as_alive() {
    let ((a_writer, a_reader, _), (b_writer, _b_reader, _)) = secure_pair();
    let timing = LinkTiming {
        ping_every: Duration::from_millis(50),
        timeout: Duration::from_millis(500),
    };
    let monitor = Monitor::start(Arc::new(a_writer), timing).unwrap();
    let link = monitor.link();
    let stop = Arc::new(AtomicBool::new(false));
    let reading = pump(a_reader, monitor.link(), Arc::clone(&stop));
    link.arm();

    // The other end never answers a ping, but keeps sending something else.
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1500) {
        b_writer.send(&Message::KeyframeRequest).unwrap();
        thread::sleep(Duration::from_millis(50));
    }
    assert!(link.lost_reason().is_none());
    assert_eq!(link.stats().pongs_received, 0);
    stop.store(true, Ordering::Relaxed);
    reading.join().unwrap().unwrap();
}

#[test]
fn messages_other_than_pings_are_left_for_the_session() {
    let ((a_writer, _a_reader, _), _b) = secure_pair();
    let monitor = Monitor::start(Arc::new(a_writer), fast()).unwrap();
    let link = monitor.link();
    assert!(!link.handle(&Message::KeyframeRequest));
    assert!(link.handle(&Message::Pong { nonce: 12345 }));
    assert_eq!(
        link.stats().pongs_received,
        0,
        "an unknown nonce is ignored"
    );
}

#[test]
fn dropping_the_monitor_stops_the_pings() {
    let ((a_writer, _a_reader, _), (_b_writer, _b_reader, _)) = secure_pair();
    let monitor = Monitor::start(Arc::new(a_writer), fast()).unwrap();
    let link = monitor.link();
    link.arm();
    wait_until("a first ping", Duration::from_secs(5), || {
        link.stats().pings_sent >= 1
    });
    drop(monitor);
    thread::sleep(Duration::from_millis(200));
    let sent = link.stats().pings_sent;
    thread::sleep(Duration::from_millis(400));
    assert_eq!(link.stats().pings_sent, sent);
}

#[test]
fn a_writer_stuck_on_a_peer_that_never_reads_is_freed_by_the_write_timeout() {
    let ((a_writer, _a_reader, _), (_b_writer, _b_reader, _)) = secure_pair();
    let writer = Arc::new(a_writer);
    let timing = LinkTiming {
        ping_every: Duration::from_millis(50),
        timeout: Duration::from_millis(400),
    };
    let monitor = Monitor::start(Arc::clone(&writer), timing).unwrap();
    let link = monitor.link();
    // Video that nobody reads fills the connection's buffers, and then the
    // sending thread is stuck in a write.
    let sending = {
        let writer = Arc::clone(&writer);
        thread::spawn(move || {
            let frame = Message::VideoFrame {
                keyframe: false,
                timestamp_us: 0,
                data: vec![7; 200_000],
            };
            while writer.send(&frame).is_ok() {}
            Instant::now()
        })
    };
    thread::sleep(Duration::from_millis(500));
    link.arm();
    let armed = Instant::now();
    let freed = sending.join().unwrap().duration_since(armed);
    // Shutting a socket down does not end a write that is already blocked on
    // Windows; the write timeout (5 s without progress) does. That bounds how
    // long a stuck sender can outlive a dead link.
    assert!(freed < Duration::from_secs(8), "freed only after {freed:?}");
    wait_until("the link to give up", Duration::from_secs(1), || {
        link.lost_reason().is_some()
    });
}
