use serde::{Deserialize, Serialize};

/// GBA button indices in KEYINPUT register (active-low: 0 = pressed)
pub const KEY_A: u16 = 1 << 0;
pub const KEY_B: u16 = 1 << 1;
pub const KEY_SELECT: u16 = 1 << 2;
pub const KEY_START: u16 = 1 << 3;
pub const KEY_RIGHT: u16 = 1 << 4;
pub const KEY_LEFT: u16 = 1 << 5;
pub const KEY_UP: u16 = 1 << 6;
pub const KEY_DOWN: u16 = 1 << 7;
pub const KEY_R: u16 = 1 << 8;
pub const KEY_L: u16 = 1 << 9;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Keypad {
    /// 0x04000130 - KEYINPUT: Key Status (read-only, active-low)
    /// Bits 0-9: A, B, Select, Start, Right, Left, Up, Down, R, L
    /// 0 = pressed, 1 = not pressed
    keyinput: u16,
    /// 0x04000132 - KEYCNT: Key Interrupt Control
    pub keycnt: u16,
}

impl Keypad {
    pub fn new() -> Self {
        Keypad {
            keyinput: 0x03FF, // All buttons released
            keycnt: 0,
        }
    }

    /// Set key state. `keys` is a bitmask where 1 = pressed.
    /// Internally stored as active-low (0 = pressed).
    pub fn set_keys(&mut self, pressed: u16) {
        self.keyinput = !pressed & 0x03FF;
    }

    /// Read KEYINPUT register (active-low).
    pub fn read_keyinput(&self) -> u16 {
        self.keyinput
    }

    /// Check if a keypad IRQ should fire based on KEYCNT settings.
    pub fn check_irq(&self) -> bool {
        if self.keycnt & (1 << 14) == 0 {
            return false; // IRQ not enabled
        }

        let key_mask = self.keycnt & 0x03FF;
        let pressed = !self.keyinput & 0x03FF;

        if self.keycnt & (1 << 15) != 0 {
            // AND mode: all specified keys must be pressed
            (pressed & key_mask) == key_mask
        } else {
            // OR mode: any specified key pressed
            (pressed & key_mask) != 0
        }
    }
}
