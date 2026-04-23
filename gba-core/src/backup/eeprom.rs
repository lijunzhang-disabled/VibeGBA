//! EEPROM backup (512 bytes or 8KB).
//!
//! EEPROM uses a serial bit-banging protocol accessed via DMA3 at 0x0DFFFF00.
//!
//! ## Read:  write [11 + address(6/14) + 0(dummy)] → read [0000(4 dummy) + 64 data bits]
//! ## Write: write [10 + address(6/14) + 64 data bits + 0(dummy)] → read ready bit

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum State {
    Idle,
    /// Receiving the 2-bit command type
    CmdType,
    /// Receiving address bits
    Address,
    /// Receiving 64 data bits (write) or dummy bit (read)
    Data,
    /// Outputting read data (4 dummy + 64 data bits)
    ReadOut,
    /// Write complete, returning ready=1
    WriteDone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum SizeKind {
    Unknown,
    Small,  // 512B, 6-bit address
    Large,  // 8KB, 14-bit address
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Eeprom {
    pub data: Vec<u8>,
    state: State,
    size: SizeKind,
    /// 0=unknown, 2=read, 3=write
    cmd: u8,
    /// Bits collected for the current field
    bit_buf: u64,
    bits_in: u32,
    /// Captured address (block index)
    address: u16,
    /// For reads: 68-bit shift register (4 dummy 0s + 64 data bits)
    read_buf: u64,
    read_bits_left: u32,
    /// Expected address width (set after first complete command if Unknown)
    addr_width: u32,
}

impl Eeprom {
    pub fn new() -> Self {
        Eeprom {
            data: vec![0xFF; 8 * 1024],
            state: State::Idle,
            size: SizeKind::Unknown,
            cmd: 0,
            bit_buf: 0,
            bits_in: 0,
            address: 0,
            read_buf: 0,
            read_bits_left: 0,
            addr_width: 0,
        }
    }

    fn effective_addr_width(&self) -> u32 {
        match self.size {
            SizeKind::Small => 6,
            SizeKind::Large => 14,
            SizeKind::Unknown => {
                if self.addr_width > 0 { self.addr_width } else { 14 }
            }
        }
    }

    pub fn read(&self, _addr: u32) -> u8 {
        match self.state {
            State::ReadOut if self.read_bits_left > 64 => {
                0 // Dummy bits (first 4)
            }
            State::ReadOut if self.read_bits_left > 0 => {
                // Output MSB of read_buf
                ((self.read_buf >> 63) & 1) as u8
            }
            State::WriteDone => 1,
            _ => 1,
        }
    }

    pub fn write(&mut self, _addr: u32, val: u8) {
        let bit = (val & 1) as u64;

        match self.state {
            State::Idle | State::WriteDone => {
                self.bit_buf = bit;
                self.bits_in = 1;
                self.state = State::CmdType;
            }
            State::CmdType => {
                self.bit_buf = (self.bit_buf << 1) | bit;
                self.bits_in += 1;
                if self.bits_in == 2 {
                    self.cmd = self.bit_buf as u8;
                    self.bit_buf = 0;
                    self.bits_in = 0;
                    if self.cmd == 2 || self.cmd == 3 {
                        self.state = State::Address;
                    } else {
                        self.state = State::Idle;
                    }
                }
            }
            State::Address => {
                self.bit_buf = (self.bit_buf << 1) | bit;
                self.bits_in += 1;

                let aw = self.effective_addr_width();
                if self.bits_in >= aw {
                    // Auto-detect size on first complete command
                    if self.size == SizeKind::Unknown {
                        if self.bits_in <= 6 {
                            self.size = SizeKind::Small;
                            self.data.resize(512, 0xFF);
                        } else {
                            self.size = SizeKind::Large;
                        }
                        self.addr_width = self.bits_in;
                    }
                    let mask = (1u64 << aw) - 1;
                    self.address = (self.bit_buf & mask) as u16;
                    self.bit_buf = 0;
                    self.bits_in = 0;
                    self.state = State::Data;
                }
            }
            State::Data => {
                if self.cmd == 3 {
                    // Read command: this bit is the dummy bit → start output
                    self.begin_read();
                    self.state = State::ReadOut;
                } else {
                    // Write command: collect 64 data bits + 1 dummy
                    self.bit_buf = (self.bit_buf << 1) | bit;
                    self.bits_in += 1;
                    if self.bits_in >= 65 {
                        // Last bit is dummy; top 64 are data (first received = MSB)
                        let data = self.bit_buf >> 1;
                        self.store_data(data);
                        self.state = State::WriteDone;
                        self.bit_buf = 0;
                        self.bits_in = 0;
                    }
                }
            }
            State::ReadOut => {
                // Each write during read-out advances to the next bit
                if self.read_bits_left > 0 {
                    self.read_bits_left -= 1;
                    // Only shift the data buffer when we're past the dummy bits
                    if self.read_bits_left < 64 {
                        self.read_buf <<= 1;
                    }
                }
                if self.read_bits_left == 0 {
                    self.state = State::Idle;
                }
            }
        }
    }

    fn begin_read(&mut self) {
        let base = self.address as usize * 8;
        let mut val: u64 = 0;
        for i in 0..8 {
            let byte = if base + i < self.data.len() { self.data[base + i] } else { 0xFF };
            val = (val << 8) | byte as u64;
        }
        // Read output: 4 dummy zeros, then 64 data bits (MSB first).
        // We output from bit 63 downward. For the first 4 reads, output 0 (dummy).
        // Then for the next 64 reads, output the data.
        // Store data shifted right by 0 — the first 4 bits will read as 0 because
        // we handle the dummy count separately.
        self.read_buf = val;
        self.read_bits_left = 68; // 4 dummy + 64 data
    }

    fn store_data(&mut self, data: u64) {
        let base = self.address as usize * 8;
        for i in 0..8 {
            let byte = ((data >> (56 - i * 8)) & 0xFF) as u8;
            if base + i < self.data.len() {
                self.data[base + i] = byte;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn send_bits(eeprom: &mut Eeprom, val: u64, count: u32) {
        for i in (0..count).rev() {
            eeprom.write(0, ((val >> i) & 1) as u8);
        }
    }

    fn read_bits(eeprom: &mut Eeprom, count: u32) -> u64 {
        let mut result = 0u64;
        for _ in 0..count {
            let bit = eeprom.read(0) as u64;
            result = (result << 1) | bit;
            eeprom.write(0, 0); // advance
        }
        result
    }

    #[test]
    fn test_eeprom_write_read_small() {
        let mut eeprom = Eeprom::new();
        eeprom.size = SizeKind::Small;
        eeprom.addr_width = 6;

        let data: u64 = 0x0102030405060708;

        // Write: 10 (cmd) + 000000 (addr=0) + 64 data bits + 0 (dummy)
        send_bits(&mut eeprom, 0b10, 2);     // cmd = write
        send_bits(&mut eeprom, 0, 6);         // addr = 0
        send_bits(&mut eeprom, data, 64);     // data
        send_bits(&mut eeprom, 0, 1);         // dummy

        assert_eq!(eeprom.data[0], 0x01);
        assert_eq!(eeprom.data[7], 0x08);

        // Read: 11 (cmd) + 000000 (addr=0) + 0 (dummy)
        send_bits(&mut eeprom, 0b11, 2);
        send_bits(&mut eeprom, 0, 6);
        send_bits(&mut eeprom, 0, 1); // dummy → triggers read

        // Read out: 4 dummy + 64 data
        let _dummy = read_bits(&mut eeprom, 4);
        let result = read_bits(&mut eeprom, 64);

        assert_eq!(result, data);
    }

    #[test]
    fn test_eeprom_write_done_ready() {
        let mut eeprom = Eeprom::new();
        eeprom.size = SizeKind::Small;
        eeprom.addr_width = 6;

        send_bits(&mut eeprom, 0b10, 2);
        send_bits(&mut eeprom, 0, 6);
        send_bits(&mut eeprom, 0xAAAAAAAAAAAAAAAA, 64);
        send_bits(&mut eeprom, 0, 1);

        // After write, reading should return 1 (ready)
        assert_eq!(eeprom.read(0), 1);
    }
}
