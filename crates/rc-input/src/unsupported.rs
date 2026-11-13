//! What input injection does on systems where it is not implemented yet: it
//! injects nothing and says so. The same names and signatures as the Windows
//! version, so the application builds everywhere.

use rc_protocol::InputEvent;

use crate::mapping::Rect;
use crate::InputError;

pub struct Injector {
    display: Rect,
}

impl Injector {
    pub fn new(display: Rect) -> Self {
        Self { display }
    }

    pub fn display(&self) -> Rect {
        self.display
    }

    pub fn set_display(&mut self, display: Rect) {
        self.display = display;
    }

    pub fn inject(&mut self, _event: &InputEvent) -> Result<(), InputError> {
        Err(InputError::UnsupportedSystem)
    }

    /// Nothing is ever held down here.
    pub fn release_all(&mut self) -> usize {
        0
    }
}
