#![cfg(windows)]
//! These tests move the real mouse pointer and press a real key for a few
//! milliseconds, so they only run when asked for (`--include-ignored`).

use rc_input::mapping::Rect;
use rc_input::Injector;
use rc_protocol::InputEvent;
use windows::Win32::Foundation::POINT;
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SetCursorPos, SM_CXSCREEN, SM_CYSCREEN,
};

/// Physical size of the primary display and the pointer position.
fn physical<T>(read: impl FnOnce() -> T) -> T {
    // SAFETY: switches this thread's DPI context so that the calls inside
    // `read` report physical pixels.
    unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    read()
}

fn cursor() -> (i32, i32) {
    let mut point = POINT::default();
    // SAFETY: the out-pointer references a live local.
    unsafe { GetCursorPos(&mut point).unwrap() };
    (point.x, point.y)
}

fn primary() -> Rect {
    // SAFETY: plain calls without pointers.
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    Rect {
        x: 0,
        y: 0,
        width: width as u32,
        height: height as u32,
    }
}

#[test]
#[ignore = "moves the real mouse pointer"]
fn the_pointer_goes_where_the_viewer_points() {
    physical(|| {
        let display = primary();
        let original = cursor();
        let mut injector = Injector::new(display);

        let cases = [
            ((0u16, 0u16), (0, 0)),
            (
                (65535, 65535),
                (display.width as i32 - 1, display.height as i32 - 1),
            ),
            (
                (32768, 32768),
                (display.width as i32 / 2, display.height as i32 / 2),
            ),
            (
                (16384, 49152),
                (display.width as i32 / 4, display.height as i32 * 3 / 4),
            ),
        ];
        let mut results = Vec::new();
        for ((x, y), expected) in cases {
            // A person using the mouse during the test can move the pointer
            // between the injection and the reading, so each position gets a
            // few tries; injection that does not work fails every one.
            let mut got = (0, 0);
            for _ in 0..5 {
                injector.inject(&InputEvent::MouseMove { x, y }).unwrap();
                std::thread::sleep(std::time::Duration::from_millis(30));
                got = cursor();
                if (got.0 - expected.0).abs() <= 1 && (got.1 - expected.1).abs() <= 1 {
                    break;
                }
            }
            results.push((got, expected));
        }
        // SAFETY: plain call; puts the pointer back where the user had it.
        unsafe { SetCursorPos(original.0, original.1).unwrap() };

        for (got, expected) in results {
            assert!(
                (got.0 - expected.0).abs() <= 1 && (got.1 - expected.1).abs() <= 1,
                "pointer at {got:?}, expected {expected:?}"
            );
        }
    });
}

#[test]
#[ignore = "presses the real Shift key briefly"]
fn a_held_key_is_released_when_the_injector_is_dropped() {
    let shift_down = || {
        // SAFETY: plain call without pointers.
        unsafe { GetAsyncKeyState(i32::from(VK_SHIFT.0)) < 0 }
    };
    let wait = || std::thread::sleep(std::time::Duration::from_millis(30));
    assert!(!shift_down(), "Shift is already held; cannot run this test");

    let mut injector = Injector::new(primary());
    injector
        .inject(&InputEvent::Key {
            scancode: 0x2A,
            extended: false,
            down: true,
        })
        .unwrap();
    wait();
    let held = shift_down();
    drop(injector);
    wait();

    assert!(held, "the injected key press did not register");
    assert!(!shift_down(), "Shift was left held down");
}
