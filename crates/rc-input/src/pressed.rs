//! Tracks what the viewer is holding down.
//!
//! If control ends while a key or button is down — the viewer disconnects
//! mid-drag, or the host withdraws permission — the host would be left with
//! a stuck Shift or a held mouse button. Recording presses makes it possible
//! to release them all.

use rc_protocol::{InputEvent, MouseButton};

#[derive(Debug, Default)]
pub struct Pressed {
    keys: Vec<(u16, bool)>,
    buttons: Vec<MouseButton>,
    position: (u16, u16),
}

impl Pressed {
    /// Notes the effect of an event that is about to be injected.
    pub fn record(&mut self, event: &InputEvent) {
        match *event {
            InputEvent::MouseMove { x, y } => self.position = (x, y),
            InputEvent::MouseButton { button, down, x, y } => {
                self.position = (x, y);
                self.buttons.retain(|held| *held != button);
                if down {
                    self.buttons.push(button);
                }
            }
            InputEvent::Key {
                scancode,
                extended,
                down,
            } => {
                self.keys.retain(|held| *held != (scancode, extended));
                if down {
                    self.keys.push((scancode, extended));
                }
            }
            InputEvent::MouseWheel { .. } => {}
        }
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }

    /// The events that release everything currently held, most recent
    /// first, and forgets them.
    pub fn release_all(&mut self) -> Vec<InputEvent> {
        let (x, y) = self.position;
        let buttons = self
            .buttons
            .drain(..)
            .rev()
            .map(|button| InputEvent::MouseButton {
                button,
                down: false,
                x,
                y,
            });
        let keys = self
            .keys
            .drain(..)
            .rev()
            .map(|(scancode, extended)| InputEvent::Key {
                scancode,
                extended,
                down: false,
            });
        buttons.chain(keys).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(scancode: u16, down: bool) -> InputEvent {
        InputEvent::Key {
            scancode,
            extended: false,
            down,
        }
    }

    #[test]
    fn nothing_held_means_nothing_to_release() {
        let mut pressed = Pressed::default();
        pressed.record(&InputEvent::MouseMove { x: 5, y: 6 });
        pressed.record(&InputEvent::MouseWheel { dx: 0, dy: 120 });
        pressed.record(&key(0x1E, true));
        pressed.record(&key(0x1E, false));
        assert!(pressed.is_empty());
        assert!(pressed.release_all().is_empty());
    }

    #[test]
    fn held_keys_and_buttons_are_released_newest_first_at_the_last_position() {
        let mut pressed = Pressed::default();
        pressed.record(&key(0x2A, true)); // Shift
        pressed.record(&key(0x1E, true)); // A
        pressed.record(&InputEvent::MouseButton {
            button: MouseButton::Left,
            down: true,
            x: 100,
            y: 200,
        });
        pressed.record(&InputEvent::MouseMove { x: 300, y: 400 });

        assert_eq!(
            pressed.release_all(),
            [
                InputEvent::MouseButton {
                    button: MouseButton::Left,
                    down: false,
                    x: 300,
                    y: 400
                },
                key(0x1E, false),
                key(0x2A, false),
            ]
        );
        assert!(pressed.is_empty());
        assert!(pressed.release_all().is_empty());
    }

    #[test]
    fn repeats_do_not_accumulate_and_extended_keys_are_distinct() {
        let mut pressed = Pressed::default();
        // Auto-repeat sends "down" again and again.
        for _ in 0..5 {
            pressed.record(&key(0x1D, true));
        }
        pressed.record(&InputEvent::Key {
            scancode: 0x1D,
            extended: true,
            down: true,
        });
        pressed.record(&key(0x1D, false));

        assert_eq!(
            pressed.release_all(),
            [InputEvent::Key {
                scancode: 0x1D,
                extended: true,
                down: false
            }]
        );
    }
}
