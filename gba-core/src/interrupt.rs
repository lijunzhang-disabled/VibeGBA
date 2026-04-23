use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy)]
pub enum Irq {
    VBlank = 0,
    HBlank = 1,
    VCountMatch = 2,
    Timer0 = 3,
    Timer1 = 4,
    Timer2 = 5,
    Timer3 = 6,
    Serial = 7,
    Dma0 = 8,
    Dma1 = 9,
    Dma2 = 10,
    Dma3 = 11,
    Keypad = 12,
    GamePak = 13,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterruptController {
    /// 0x04000200 - IE: Interrupt Enable
    pub ie: u16,
    /// 0x04000202 - IF: Interrupt Request Flags (write 1 to acknowledge)
    pub ir: u16,
    /// 0x04000208 - IME: Interrupt Master Enable
    pub ime: bool,
}

impl InterruptController {
    pub fn new() -> Self {
        InterruptController {
            ie: 0,
            ir: 0,
            ime: false,
        }
    }

    /// Request an IRQ. Sets the corresponding bit in IF.
    pub fn request_irq(&mut self, irq: Irq) {
        self.ir |= 1 << (irq as u16);
    }

    /// Acknowledge IRQs by writing to IF (writing 1 clears the bit).
    pub fn acknowledge(&mut self, value: u16) {
        self.ir &= !value;
    }

    /// Check if any enabled and requested IRQs are pending.
    pub fn has_pending(&self) -> bool {
        self.ime && (self.ie & self.ir) != 0
    }

    /// Read IE register.
    pub fn read_ie(&self) -> u16 {
        self.ie
    }

    /// Write IE register.
    pub fn write_ie(&mut self, value: u16) {
        self.ie = value;
    }

    /// Read IF register.
    pub fn read_if(&self) -> u16 {
        self.ir
    }

    /// Write IF register (acknowledge).
    pub fn write_if(&mut self, value: u16) {
        self.acknowledge(value);
    }

    /// Read IME register.
    pub fn read_ime(&self) -> u16 {
        self.ime as u16
    }

    /// Write IME register.
    pub fn write_ime(&mut self, value: u16) {
        self.ime = value & 1 != 0;
    }
}
