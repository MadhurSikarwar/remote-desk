//! The viewer's pointer positions must be mapped onto the display as it is
//! now, not as it was when sharing began.

use rc_input::mapping::Rect;

use super::*;

fn rect(x: i32, y: i32, width: u32, height: u32) -> Rect {
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[test]
fn a_display_that_has_not_changed_leaves_the_injector_alone() {
    let a = rect(0, 0, 1920, 1080);
    let injector = Mutex::new(Injector::new(a));
    let mut applied = a;
    assert!(!follow_display(&mut applied, a, &injector));
    assert_eq!(lock(&injector).display(), a);
}

#[test]
fn a_new_resolution_or_place_is_followed_once() {
    let before = rect(0, 0, 1920, 1080);
    let injector = Mutex::new(Injector::new(before));
    let mut applied = before;

    // The resolution changed.
    let smaller = rect(0, 0, 1280, 720);
    assert!(follow_display(&mut applied, smaller, &injector));
    assert_eq!(lock(&injector).display(), smaller);
    assert_eq!(applied, smaller);
    assert!(
        !follow_display(&mut applied, smaller, &injector),
        "followed twice"
    );

    // The monitors were rearranged: it is somewhere else on the desktop.
    let moved = rect(-1280, 200, 1280, 720);
    assert!(follow_display(&mut applied, moved, &injector));
    assert_eq!(lock(&injector).display(), moved);
}

#[test]
fn a_display_is_described_by_its_own_numbers() {
    let info = DisplayInfo {
        index: 1,
        name: "name".into(),
        adapter: "adapter".into(),
        x: -1920,
        y: 120,
        width: 1920,
        height: 1080,
        rotation: rc_capture::Rotation::None,
        primary: false,
    };
    assert_eq!(rect_of(&info), rect(-1920, 120, 1920, 1080));
}
