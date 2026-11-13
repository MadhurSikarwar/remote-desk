//! What input injection does on systems where it is not implemented yet: it
//! injects nothing and says so. The same names and signatures as the Windows
//! version, so the application builds everywhere.

use rc_protocol::InputEvent;

use crate::mapping::Rect;
use crate::InputError;

pub struct Injector;

impl Injector {
    pub fn new(_display: Rect) -> Self {
        Self
    }

    pub fn inject(&mut self, _event: &InputEvent) -> Result<(), InputError> {
        Err(InputError::UnsupportedSystem)
    }

    /// Nothing is ever held down here.
    pub fn release_all(&mut self) -> usize {
        0
    }
}
