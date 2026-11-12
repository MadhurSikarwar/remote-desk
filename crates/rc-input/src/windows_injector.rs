//! Input injection with `SendInput`.

use rc_protocol::{InputEvent, MouseButton};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
    MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
    MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use crate::mapping::{to_absolute, to_pixel, Rect};
use crate::pressed::Pressed;
use crate::InputError;

/// The bounds of all monitors together, in physical pixels.
fn virtual_screen() -> Rect {
    // SAFETY: plain calls with no pointers. The DPI context is switched for
    // this thread only and restored before returning, so the metrics are
    // physical pixels whatever the process's DPI awareness.
    unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let rect = Rect {
            x: GetSystemMetrics(SM_XVIRTUALSCREEN),
            y: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(0) as u32,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(0) as u32,
        };
        if !previous.0.is_null() {
            SetThreadDpiAwarenessContext(previous);
        }
        rect
    }
}

fn send(input: INPUT) -> Result<(), InputError> {
    // SAFETY: one fully initialised INPUT, with its true size.
    let accepted = unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
    if accepted == 1 {
        Ok(())
    } else {
        Err(InputError::Rejected)
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, position: Option<(i32, i32)>, data: i32) -> INPUT {
    let (dx, dy) = position.unwrap_or_default();
    let positioned = if position.is_some() {
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK
    } else {
        MOUSE_EVENT_FLAGS(0)
    };
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                // Wheel deltas are signed but travel in an unsigned field.
                mouseData: data as u32,
                dwFlags: flags | positioned,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

fn key(scancode: u16, extended: bool, down: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if extended {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VIRTUAL_KEY(0),
                wScan: scancode,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Injects a viewer's input onto one display of this computer.
pub struct Injector {
    display: Rect,
    pressed: Pressed,
}

impl Injector {
    /// `display` is the shared display's rectangle in physical pixels.
    pub fn new(display: Rect) -> Self {
        Self {
            display,
            pressed: Pressed::default(),
        }
    }

    fn absolute(&self, x: u16, y: u16) -> (i32, i32) {
        // Read each time: monitors can be added or rearranged mid-session.
        to_absolute(to_pixel(x, y, self.display), virtual_screen())
    }

    pub fn inject(&mut self, event: &InputEvent) -> Result<(), InputError> {
        self.send_event(event)?;
        self.pressed.record(event);
        Ok(())
    }

    fn send_event(&self, event: &InputEvent) -> Result<(), InputError> {
        match *event {
            InputEvent::MouseMove { x, y } => {
                send(mouse(MOUSE_EVENT_FLAGS(0), Some(self.absolute(x, y)), 0))
            }
            InputEvent::MouseButton { button, down, x, y } => {
                let flag = match (button, down) {
                    (MouseButton::Left, true) => MOUSEEVENTF_LEFTDOWN,
                    (MouseButton::Left, false) => MOUSEEVENTF_LEFTUP,
                    (MouseButton::Right, true) => MOUSEEVENTF_RIGHTDOWN,
                    (MouseButton::Right, false) => MOUSEEVENTF_RIGHTUP,
                    (MouseButton::Middle, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (MouseButton::Middle, false) => MOUSEEVENTF_MIDDLEUP,
                };
                send(mouse(flag, Some(self.absolute(x, y)), 0))
            }
            InputEvent::MouseWheel { dx, dy } => {
                if dy != 0 {
                    send(mouse(MOUSEEVENTF_WHEEL, None, i32::from(dy)))?;
                }
                if dx != 0 {
                    send(mouse(MOUSEEVENTF_HWHEEL, None, i32::from(dx)))?;
                }
                Ok(())
            }
            InputEvent::Key {
                scancode,
                extended,
                down,
            } => send(key(scancode, extended, down)),
        }
    }

    /// Lets go of every key and button the viewer still holds. Returns how
    /// many were released.
    pub fn release_all(&mut self) -> usize {
        let releases = self.pressed.release_all();
        for event in &releases {
            // Best effort: if one fails the rest should still be released.
            let _ = self.send_event(event);
        }
        releases.len()
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        let released = self.release_all();
        if released > 0 {
            tracing::info!(released, "released input still held by the viewer");
        }
    }
}
