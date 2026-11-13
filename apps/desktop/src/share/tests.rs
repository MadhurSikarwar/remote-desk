use super::test_support::{knock, knock_at, pending, settings, wait_until};
use super::*;
use crate::secure::test_support::{desktop, security};
use crate::secure::SecureReader;

const MS: Duration = Duration::from_millis(1);

#[test]
fn pacer_waits_for_the_rest_of_the_interval() {
    let start = Instant::now();
    let mut pacer = Pacer::new(10, start);
    assert_eq!(pacer.after_frame(start + 30 * MS), 70 * MS);
}

#[test]
fn pacer_makes_up_for_an_overslept_wait() {
    let start = Instant::now();
    let mut pacer = Pacer::new(10, start);
    assert_eq!(pacer.after_frame(start), 100 * MS);
    // The 100 ms sleep took 115 ms; the next wait is 15 ms shorter.
    assert_eq!(pacer.after_frame(start + 115 * MS), 85 * MS);
}

#[test]
fn pacer_restarts_after_an_idle_gap_without_bursting() {
    let start = Instant::now();
    let mut pacer = Pacer::new(10, start);
    assert_eq!(pacer.after_frame(start + 5000 * MS), Duration::ZERO);
    assert_eq!(pacer.after_frame(start + 5000 * MS), 100 * MS);
}

#[test]
fn pacer_tolerates_zero_fps() {
    let start = Instant::now();
    assert_eq!(
        Pacer::new(0, start).after_frame(start),
        Duration::from_secs(1)
    );
}

#[test]
fn idle_controller_reports_inactive_and_stop_is_harmless() {
    let (keys, _dir) = security();
    let controller = ShareController::new(Some(keys));
    assert!(!controller.status().active);
    assert!(!controller.stop().active);
    assert!(controller.answer(1, true, false).is_err());
}

#[test]
fn sharing_is_refused_without_a_device_key() {
    let controller = ShareController::new(None);
    let error = controller.start(settings()).unwrap_err();
    assert!(error.contains("key"), "{error}");
    assert!(!controller.status().active);
}

/// The approval logic on its own, without a screen or a network.
mod approval {
    use super::*;

    fn peer() -> Peer {
        Peer {
            public: [7; 32],
            fingerprint: "AAAA-BBBB-CCCC-DDDD".into(),
            auth_code: "123 456".into(),
        }
    }

    /// Runs `ask` on another thread and hands back the shared state.
    fn asking(shared: Arc<Shared>, timeout: Duration) -> JoinHandle<Verdict> {
        let asker = Arc::clone(&shared);
        let thread = thread::spawn(move || asker.ask(&peer(), "Laptop", Some(42), timeout));
        wait_until("the request", || lock(&shared.approval).is_some());
        thread
    }

    fn decide(shared: &Shared, decision: Decision) {
        if let Some(approval) = lock(&shared.approval).as_mut() {
            approval.decision = Some(decision);
        }
        shared.approval_changed.notify_all();
    }

    #[test]
    fn the_request_shows_who_is_asking_and_is_cleared_once_answered() {
        let shared = Arc::new(Shared::default());
        let thread = asking(Arc::clone(&shared), Duration::from_secs(30));
        let shown = shared.status(1, 0.0, true).pending.unwrap();
        assert_eq!(shown.name, "Laptop");
        assert_eq!(shown.fingerprint, "AAAA-BBBB-CCCC-DDDD");
        assert_eq!(shown.auth_code, "123 456");
        assert_eq!(shown.known_since, Some(42));
        assert!(shown.seconds_left >= 28);
        // Not shown once sharing is off.
        assert!(shared.status(1, 0.0, false).pending.is_none());

        decide(&shared, Decision::Allow { remember: true });
        assert_eq!(thread.join().unwrap(), Verdict::Allowed { remember: true });
        assert!(shared.status(1, 0.0, true).pending.is_none());
    }

    #[test]
    fn deny_is_deny() {
        let shared = Arc::new(Shared::default());
        let thread = asking(Arc::clone(&shared), Duration::from_secs(30));
        decide(&shared, Decision::Deny);
        assert_eq!(thread.join().unwrap(), Verdict::Denied);
    }

    #[test]
    fn no_answer_in_time_means_no() {
        let shared = Arc::new(Shared::default());
        let before = Instant::now();
        let verdict = shared.ask(&peer(), "Laptop", None, Duration::from_millis(150));
        assert_eq!(verdict, Verdict::TimedOut);
        assert!(before.elapsed() < Duration::from_secs(2));
        assert!(lock(&shared.approval).is_none());
    }

    #[test]
    fn stopping_sharing_withdraws_the_request() {
        let shared = Arc::new(Shared::default());
        let thread = asking(Arc::clone(&shared), Duration::from_secs(30));
        shared.stop.store(true, Ordering::Relaxed);
        shared.approval_changed.notify_all();
        assert_eq!(thread.join().unwrap(), Verdict::Stopped);
    }

    #[test]
    fn each_request_gets_a_new_id() {
        let shared = Arc::new(Shared::default());
        shared.ask(&peer(), "First", None, Duration::from_millis(1));
        let thread = asking(Arc::clone(&shared), Duration::from_secs(5));
        assert_eq!(shared.status(1, 0.0, true).pending.unwrap().id, 2);
        decide(&shared, Decision::Deny);
        thread.join().unwrap();
    }
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn unknown_display_fails_to_start() {
    let _desktop = desktop();
    let (keys, _dir) = security();
    let controller = ShareController::new(Some(keys));
    let error = controller
        .start(ShareSettings {
            display: usize::MAX,
            ..settings()
        })
        .unwrap_err();
    assert!(error.contains("does not exist"), "{error}");
    assert!(!controller.status().active);
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn sharing_listens_on_loopback_only_and_refuses_a_second_start() {
    let _desktop = desktop();
    let (keys, _dir) = security();
    let controller = ShareController::new(Some(keys));
    let status = controller.start(settings()).unwrap();
    let port = status.port.unwrap();
    assert!(status.active && port != 0 && !status.viewer_connected);
    assert!(controller.start(settings()).is_err());

    // Reachable on loopback…
    TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    // …and the port is released once sharing stops.
    assert!(!controller.stop().active);
    assert!(TcpStream::connect((Ipv4Addr::LOCALHOST, port)).is_err());
}

/// Reads until the host says goodbye and returns its reason, failing if any
/// session data arrives first.
fn goodbye_without_video(reader: &mut SecureReader) -> String {
    loop {
        match reader
            .read_message()
            .expect("connection ended without a goodbye")
        {
            Message::Bye { reason } => return reason,
            Message::VideoConfig { .. } | Message::VideoFrame { .. } | Message::Accepted => {
                panic!("the host sent session data without approval")
            }
            _ => {}
        }
    }
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn a_declined_viewer_gets_nothing_and_is_not_remembered() {
    let _desktop = desktop();
    let (host_keys, _dir) = security();
    let (viewer_keys, _dir2) = security();
    let host = ShareController::new(Some(Arc::clone(&host_keys)));
    let port = host.start(settings()).unwrap().port.unwrap();

    let (_writer, mut reader, peer) = knock(port, &viewer_keys, "Stranger");
    let request = pending(&host);
    assert_eq!(request.name, "Stranger");
    assert_eq!(request.fingerprint, viewer_keys.fingerprint());
    assert_eq!(request.auth_code, peer.auth_code);
    assert_eq!(request.known_since, None);
    assert!(!host.status().viewer_connected);

    host.answer(request.id, false, true).unwrap();
    assert_eq!(
        goodbye_without_video(&mut reader),
        "the host declined the request"
    );
    wait_until("the prompt to clear", || host.status().pending.is_none());
    assert!(
        host.answer(request.id, true, false).is_err(),
        "stale answer"
    );
    assert_eq!(host.status().frames, 0);
    assert!(host_keys.trust().devices().is_empty());
    host.stop();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn an_unanswered_request_is_refused_when_its_time_runs_out() {
    let _desktop = desktop();
    let (host_keys, _dir) = security();
    let (viewer_keys, _dir2) = security();
    let host = ShareController::new(Some(host_keys));
    let port = host
        .start(ShareSettings {
            approval_timeout: Duration::from_millis(400),
            ..settings()
        })
        .unwrap()
        .port
        .unwrap();

    let (_writer, mut reader, _) = knock(port, &viewer_keys, "Patient");
    assert_eq!(
        goodbye_without_video(&mut reader),
        "the host did not answer"
    );
    assert_eq!(host.status().frames, 0);
    host.stop();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn an_approved_and_remembered_viewer_is_recognised_next_time() {
    let _desktop = desktop();
    let (host_keys, _dir) = security();
    let (viewer_keys, _dir2) = security();
    let host = ShareController::new(Some(Arc::clone(&host_keys)));
    let port = host.start(settings()).unwrap().port.unwrap();

    let (writer, mut reader, _) = knock(port, &viewer_keys, "Laptop");
    let first = pending(&host);
    assert_eq!(first.known_since, None);
    host.answer(first.id, true, true).unwrap();
    assert_eq!(reader.read_message().unwrap(), Message::Accepted);
    wait_until("the viewer to be connected", || {
        host.status().viewer_connected
    });
    assert!(host_keys
        .trust()
        .get(viewer_keys.identity().public())
        .is_some());
    writer
        .send(&Message::Bye {
            reason: "done".into(),
        })
        .unwrap();
    wait_until("the session to end", || !host.status().viewer_connected);

    // Known now — but still asked, every time.
    let (_writer, _reader, _) = knock(port, &viewer_keys, "Laptop");
    let second = pending(&host);
    assert!(second.known_since.is_some());
    assert_ne!(second.id, first.id);
    assert!(!host.status().viewer_connected);
    host.stop();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn a_connection_that_is_not_encrypted_never_reaches_the_user() {
    use std::io::{Read, Write};

    let _desktop = desktop();
    let (host_keys, _dir) = security();
    let host = ShareController::new(Some(host_keys));
    let port = host.start(settings()).unwrap().port.unwrap();

    // What a viewer from before encryption would have sent.
    let mut plain = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    let mut hello = Vec::new();
    rc_protocol::write_message(
        &mut hello,
        &Message::Hello {
            version: rc_protocol::PROTOCOL_VERSION,
            role: Role::Viewer,
            name: "Old client".into(),
        },
    )
    .unwrap();
    plain.write_all(&hello).unwrap();
    plain.write_all(&[0u8; 64]).unwrap();

    // The host drops it.
    plain
        .set_read_timeout(Some(Duration::from_secs(8)))
        .unwrap();
    let mut received = Vec::new();
    let mut chunk = [0u8; 256];
    while let Ok(n) = plain.read(&mut chunk) {
        if n == 0 {
            break;
        }
        received.extend_from_slice(&chunk[..n]);
    }
    // All it can have received is the host's half of a failed handshake.
    assert!(
        received.len() < 200,
        "{} bytes sent to an unencrypted client",
        received.len()
    );
    assert!(host.status().pending.is_none());
    assert_eq!(host.status().frames, 0);

    // A proper viewer still gets through afterwards.
    let (viewer_keys, _dir2) = security();
    let _knock = knock(port, &viewer_keys, "Proper");
    assert_eq!(pending(&host).name, "Proper");
    host.stop();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn stop_returns_promptly_with_a_request_pending_or_a_silent_viewer() {
    let _desktop = desktop();
    let (host_keys, _dir) = security();
    let (viewer_keys, _dir2) = security();
    let host = ShareController::new(Some(host_keys));

    // A request waiting for an answer.
    let port = host.start(settings()).unwrap().port.unwrap();
    let _waiting = knock(port, &viewer_keys, "Waiting");
    pending(&host);
    let before = Instant::now();
    assert!(!host.stop().active);
    let took = before.elapsed();
    assert!(took < Duration::from_secs(2), "stop took {took:?}");

    // An approved viewer that neither reads, writes nor closes.
    let port = host.start(settings()).unwrap().port.unwrap();
    let _silent = knock(port, &viewer_keys, "Silent");
    host.answer(pending(&host).id, true, false).unwrap();
    wait_until("the viewer to be connected", || {
        host.status().viewer_connected
    });
    let before = Instant::now();
    assert!(!host.stop().active);
    let took = before.elapsed();
    assert!(took < Duration::from_secs(2), "stop took {took:?}");
}

/// Pointer position in physical pixels.
#[cfg(windows)]
fn pointer() -> (i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::HiDpi::{
        SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut point = POINT::default();
    // SAFETY: the out-pointer references a live local; the DPI context
    // change affects this test thread only.
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        GetCursorPos(&mut point).unwrap();
    }
    (point.x, point.y)
}

#[cfg(windows)]
fn shift_is_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
    // SAFETY: plain call without pointers.
    unsafe { GetAsyncKeyState(i32::from(VK_SHIFT.0)) < 0 }
}

/// Reads from the host until it announces permissions.
#[cfg(windows)]
fn next_permissions(reader: &mut SecureReader, host: &ShareController) -> bool {
    loop {
        match reader.read_message() {
            Ok(Message::Permissions(permissions)) => return permissions.input,
            Ok(_) => {}
            Err(e) => panic!("{e}; host status: {:?}", host.status()),
        }
    }
}

#[cfg(windows)]
#[test]
#[ignore = "moves the real mouse pointer and presses Shift briefly"]
fn input_works_only_while_the_host_allows_it() {
    let _desktop = desktop();
    let display = rc_capture::list_displays().unwrap().remove(0);
    let centre = (
        display.x + display.width as i32 / 2,
        display.y + display.height as i32 / 2,
    );
    let to_centre = Message::Input(InputEvent::MouseMove { x: 32768, y: 32768 });
    let to_corner = Message::Input(InputEvent::MouseMove { x: 0, y: 0 });
    let shift = |down| {
        Message::Input(InputEvent::Key {
            scancode: 0x2A,
            extended: false,
            down,
        })
    };
    assert!(
        !shift_is_down(),
        "Shift is already held; cannot run this test"
    );

    let (host_keys, _dir) = security();
    let (viewer_keys, _dir2) = security();
    let controller = ShareController::new(Some(host_keys));
    let port = controller.start(settings()).unwrap().port.unwrap();
    let (writer, mut reader, _) = knock(port, &viewer_keys, "Controller");

    let original = pointer();
    // Park the pointer away from both targets so a move is detectable.
    // SAFETY: plain call; the pointer is restored at the end.
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::SetCursorPos(centre.0 / 2, centre.1 / 2).unwrap();
    }
    // 0. Before approval, input is not even looked at.
    writer.send(&to_centre).unwrap();
    thread::sleep(Duration::from_millis(100));
    let before_approval = (pointer(), controller.status().input_ignored);
    controller
        .answer(pending(&controller).id, true, false)
        .unwrap();

    // 1. Approved, control off by default: the viewer is told, and its
    //    input does nothing. (The event sent before approval is read now
    //    and ignored too.)
    assert!(!next_permissions(&mut reader, &controller));
    writer.send(&to_centre).unwrap();
    writer.send(&shift(true)).unwrap();
    wait_until("ignored input", || controller.status().input_ignored == 3);
    let while_off = (pointer(), shift_is_down(), controller.status().input_events);

    // 2. The host allows control: the viewer is told, and input works.
    assert!(controller.set_input_allowed(true).input_allowed);
    assert!(next_permissions(&mut reader, &controller));
    writer.send(&to_centre).unwrap();
    writer.send(&shift(true)).unwrap();
    wait_until("accepted input", || controller.status().input_events == 2);
    thread::sleep(Duration::from_millis(30));
    let while_on = (pointer(), shift_is_down());

    // 3. The host withdraws control: the held key is let go, and further
    //    input does nothing.
    assert!(!controller.set_input_allowed(false).input_allowed);
    assert!(!next_permissions(&mut reader, &controller));
    thread::sleep(Duration::from_millis(30));
    let shift_after_revoke = shift_is_down();
    writer.send(&to_corner).unwrap();
    wait_until("ignored input", || controller.status().input_ignored == 4);
    let after_revoke = pointer();

    controller.stop();
    // SAFETY: plain call; puts the pointer back where the user had it.
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::SetCursorPos(original.0, original.1).unwrap();
    }

    // "Did not act" is judged by the pointer not having reached the target,
    // not by an exact position: a person using the mouse during the test
    // would otherwise make it fail.
    let near = |a: (i32, i32), b: (i32, i32)| (a.0 - b.0).abs() <= 1 && (a.1 - b.1).abs() <= 1;
    assert!(
        !near(before_approval.0, centre) && before_approval.1 == 0,
        "input acted on before approval: {before_approval:?}"
    );
    assert!(
        !near(while_off.0, centre) && !while_off.1 && while_off.2 == 0,
        "input acted on while off: {while_off:?}"
    );
    assert!(near(while_on.0, centre), "pointer at {:?}", while_on.0);
    assert!(while_on.1, "Shift did not register while control was on");
    assert!(
        !shift_after_revoke,
        "Shift stayed down after control was withdrawn"
    );
    assert!(
        near(after_revoke, centre),
        "pointer moved after control was withdrawn"
    );
}

fn remote_settings() -> ShareSettings {
    ShareSettings {
        allow_remote: true,
        lan_discovery: true,
        ..settings()
    }
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn a_second_viewer_is_told_the_host_is_busy_and_the_first_is_unaffected() {
    let _desktop = desktop();
    let (host_keys, _dir) = security();
    let (first_keys, _d1) = security();
    let (second_keys, _d2) = security();
    let host = ShareController::new(Some(host_keys));
    let port = host.start(settings()).unwrap().port.unwrap();

    let _first = knock(port, &first_keys, "First");
    let request = pending(&host);
    let (_writer, mut reader, _) = knock(port, &second_keys, "Second");
    assert_eq!(
        goodbye_without_video(&mut reader),
        "the host is busy with another viewer"
    );
    // The first is still waiting, and its request is the one shown.
    assert_eq!(host.status().pending.unwrap().id, request.id);
    host.stop();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn after_a_refusal_the_next_viewer_is_asked_to_wait_briefly() {
    let _desktop = desktop();
    let (host_keys, _dir) = security();
    let (a_keys, _d1) = security();
    let (b_keys, _d2) = security();
    let host = ShareController::new(Some(host_keys));
    let port = host.start(settings()).unwrap().port.unwrap();

    let (_writer, mut reader, _) = knock(port, &a_keys, "A");
    host.answer(pending(&host).id, false, false).unwrap();
    assert_eq!(
        goodbye_without_video(&mut reader),
        "the host declined the request"
    );

    let (_writer, mut reader, _) = knock(port, &b_keys, "B");
    assert_eq!(goodbye_without_video(&mut reader), "try again in a moment");
    assert!(host.status().pending.is_none());

    // Once the gap has passed a viewer is asked about again.
    thread::sleep(REFUSAL_GAP + Duration::from_millis(300));
    let _knock = knock(port, &b_keys, "B");
    assert_eq!(pending(&host).name, "B");
    host.stop();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn a_stalled_connection_does_not_hold_up_a_real_viewer() {
    let _desktop = desktop();
    let (host_keys, _dir) = security();
    let (viewer_keys, _d1) = security();
    let host = ShareController::new(Some(host_keys));
    let port = host.start(settings()).unwrap().port.unwrap();

    // Connects and says nothing.
    let _silent = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    let before = Instant::now();
    let _knock = knock(port, &viewer_keys, "Real");
    assert_eq!(pending(&host).name, "Real");
    let took = before.elapsed();
    assert!(took < Duration::from_secs(2), "took {took:?}");
    host.stop();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn too_many_simultaneous_connections_are_refused_and_recovery_follows() {
    let _desktop = desktop();
    let (host_keys, _dir) = security();
    let (viewer_keys, _d1) = security();
    let host = ShareController::new(Some(host_keys));
    let port = host.start(settings()).unwrap().port.unwrap();

    let silent: Vec<_> = (0..MAX_CONNECTIONS)
        .map(|_| TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap())
        .collect();
    // Let the host take them all.
    thread::sleep(Duration::from_millis(300));
    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
    let refused = establish(&stream, viewer_keys.identity(), Side::Initiator);
    assert!(
        refused.is_err(),
        "the host took more connections than its limit"
    );

    // The silent ones give up; then a viewer gets through again.
    drop(silent);
    wait_until("the host to recover", || {
        TcpStream::connect((Ipv4Addr::LOCALHOST, port))
            .ok()
            .and_then(|s| establish(&s, viewer_keys.identity(), Side::Initiator).ok())
            .is_some()
    });
    host.stop();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn other_computers_can_connect_only_when_the_user_has_allowed_it() {
    let _desktop = desktop();
    let Some(lan) = rc_net::local_address() else {
        eprintln!("no network address on this computer; test skipped");
        return;
    };
    let (host_keys, _dir) = security();
    let (viewer_keys, _d1) = security();
    let host = ShareController::new(Some(host_keys));

    // Default: reachable on loopback only, so the LAN address refuses.
    let port = host.start(settings()).unwrap().port.unwrap();
    let status = host.status();
    assert!(!status.remote && status.lan_address.is_none());
    let lan_address = std::net::SocketAddr::new(lan, port);
    assert!(TcpStream::connect_timeout(&lan_address, Duration::from_secs(1)).is_err());
    host.stop();

    // Allowed: reachable at the LAN address, and still asks.
    let port = host.start(remote_settings()).unwrap().port.unwrap();
    let status = host.status();
    assert!(status.remote);
    assert_eq!(status.lan_address, Some(lan.to_string()));
    let _knock = knock_at(std::net::SocketAddr::new(lan, port), &viewer_keys, "Remote");
    let request = pending(&host);
    assert_eq!(request.name, "Remote");
    assert!(!host.status().viewer_connected);
    host.stop();
}

#[test]
#[ignore = "needs an interactive desktop with a monitor"]
fn a_sharing_device_is_discoverable_only_while_it_allows_it() {
    let _desktop = desktop();
    let (host_keys, _dir) = security();
    let key = *host_keys.identity().public();
    let host = ShareController::new(Some(host_keys));
    let find = || {
        rc_net::scan(Duration::from_millis(700))
            .unwrap()
            .into_iter()
            .find(|found| found.announcement.public_key == key)
    };

    // Not remote: no discovery, even though it is sharing.
    host.start(settings()).unwrap();
    assert!(!host.status().discoverable);
    assert!(find().is_none());
    host.stop();

    // Remote, but discovery off.
    host.start(ShareSettings {
        lan_discovery: false,
        ..remote_settings()
    })
    .unwrap();
    assert!(!host.status().discoverable);
    assert!(find().is_none());
    host.stop();

    // Remote with discovery: found, with the right port and name.
    let port = host.start(remote_settings()).unwrap().port.unwrap();
    assert!(host.status().discoverable);
    let found = find().expect("a sharing device was not found");
    assert_eq!(found.announcement.port, port);
    assert_eq!(found.announcement.name, "Test host");
    assert_eq!(
        found.announcement.protocol_version,
        rc_protocol::PROTOCOL_VERSION
    );

    // Gone as soon as sharing stops.
    host.stop();
    assert!(find().is_none());
}
