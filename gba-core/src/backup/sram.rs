use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sram {
    pub data: Vec<u8>,
}

impl Sram {
    pub fn new() -> Self {
        Sram {
            data: vec![0xFF; 32 * 1024],
        }
    }

    pub fn read(&self, addr: u32) -> u8 {
        let index = (addr & 0x7FFF) as usize;
        if index < self.data.len() {
            self.data[index]
        } else {
            0xFF
        }
    }

    pub fn write(&mut self, addr: u32, val: u8) {
        let index = (addr & 0x7FFF) as usize;
        if index < self.data.len() {
            self.data[index] = val;
        }
    }
}
