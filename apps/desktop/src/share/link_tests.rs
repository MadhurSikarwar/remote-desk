//! Keepalive and dead-link detection in real sessions over loopback. They
//! share the desktop with the other session tests, so they take turns.

use std::sync::mpsc;

use super::test_support::{knock, pending, settings, wait_until};
use super::*;
use crate::link::LinkTiming;
use crate::secure::test_support::{desktop, security};
use crate::viewer::{packet, ConnectOptions, ViewerController};

fn quick() -> LinkTiming {
    LinkTiming {
        ping_every: Duration::from_millis(50),
        timeout: Duration::from_millis(600),
    }
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn a_viewer_that_stops_answering_is_dropped_and_the_host_goes_back_to_waiting() {
    let _desktop = desktop();
    let (host_keys, _d1) = security();
    let (viewer_keys, _d2) = security();
    let host = ShareController::new(Some(host_keys));
    let port = host
        .start(ShareSettings {
            link: quick(),
            ..settings()
        })
        .unwrap()
        .port
        .unwrap();

    // Connected and allowed, but it never reads or answers anything.
    let _silent = knock(port, &viewer_keys, "Silent viewer");
    host.answer(pending(&host).id, true, false).unwrap();
    wait_until("the viewer to be connected", || {
        host.status().viewer_connected
    });
    assert!(
        host.status().link.is_some(),
        "no link stats while connected"
    );

    let started = Instant::now();
    while host.status().viewer_connected {
        assert!(started.elapsed() < Duration::from_secs(20), "never dropped");
        thread::sleep(Duration::from_millis(20));
    }
    eprintln!("dropped after {:?}", started.elapsed());
    let status = host.status();
    assert!(status.active, "sharing must go on");
    assert!(status.link.is_none());
    host.stop();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn both_ends_see_round_trips_and_a_session_outlives_the_timeout() {
    let _desktop = desktop();
    let (host_keys, _d1) = security();
    let (viewer_keys, _d2) = security();
    let host = ShareController::new(Some(host_keys));
    let port = host
        .start(ShareSettings {
            link: quick(),
            ..settings()
        })
        .unwrap()
        .port
        .unwrap();
    let viewer = ViewerController::new(Some(viewer_keys));
    let (tx, packets) = mpsc::channel();
    viewer
        .connect_with(
            &format!("127.0.0.1:{port}"),
            &ConnectOptions {
                link: quick(),
                ..ConnectOptions::default()
            },
            "Viewer PC",
            move |packet| {
                let _ = tx.send(packet);
            },
        )
        .unwrap();
    host.answer(pending(&host).id, true, false).unwrap();

    // Well past the timeout, with pings the only guaranteed traffic.
    thread::sleep(Duration::from_millis(2500));
    let host_link = host.status().link.expect("host has no link stats");
    let viewer_link = viewer.status().expect("viewer session ended").link;
    for (who, link) in [("host", &host_link), ("viewer", &viewer_link)] {
        assert!(link.pongs_received >= 10, "{who}: {link:?}");
        assert!(link.rtt_ms.unwrap() < 500.0, "{who}: {link:?}");
        assert!(link.silent_ms < 600, "{who}: {link:?}");
    }
    assert!(host.status().viewer_connected);
    assert!(
        packets
            .try_iter()
            .all(|p| p[0] != packet::ENDED && p[0] != packet::LOST),
        "the session ended early"
    );

    viewer.disconnect();
    wait_until("the host to see the viewer go", || {
        !host.status().viewer_connected
    });
    host.stop();
}
