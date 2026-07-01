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

    /// Read one serial bit (in bit 0) and advance the output shift register.
    ///
    /// On hardware each read of the EEPROM address returns the next bit and
    /// auto-advances — the read-back is a DMA from EEPROM to RAM, which only
    /// issues reads (no interleaved writes to clock it). So the advance must
    /// live here, not in `write`. Output layout: 4 leading dummy 0s, then the
    /// 64 data bits MSB-first.
    pub fn read_bit(&mut self) -> u8 {
        if self.state == State::ReadOut && self.read_bits_left > 0 {
            let bit = if self.read_bits_left > 64 {
                0 // one of the 4 leading dummy bits
            } else {
                ((self.read_buf >> 63) & 1) as u8
            };
            self.read_bits_left -= 1;
            // Shift only once we're into the 64 data bits (past the dummies).
            if self.read_bits_left < 64 {
                self.read_buf <<= 1;
            }
            if self.read_bits_left == 0 {
                self.state = State::Idle;
            }
            bit
        } else {
            1 // ready / idle
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
                } else if self.bits_in < 64 {
                    // Write command: collect exactly 64 data bits (MSB first).
                    // Store as soon as the 64th arrives — shifting a full u64 by
                    // the trailing dummy bit would overflow bit 63 out and drop
                    // the data MSB (silently corrupted every write).
                    self.bit_buf = (self.bit_buf << 1) | bit;
                    self.bits_in += 1;
                    if self.bits_in == 64 {
                        self.store_data(self.bit_buf);
                    }
                } else {
                    // 65th bit is the trailing dummy; the write is complete.
                    self.state = State::WriteDone;
                    self.bit_buf = 0;
                    self.bits_in = 0;
                }
            }
            State::ReadOut => {
                // Read-back is clocked by reads (see `read_bit`); a stray write
                // here would double-advance, so ignore it.
            }
        }
    }

    /// Infer the EEPROM address width from the length of the DMA that drives a
    /// command. Games pick the width to match their chip; the transfer length
    /// encodes it: a set-read-address command is 2+addr+1 units, a write is
    /// 2+addr+64+1. So 9/73 → 6-bit (512 B) and 17/81 → 14-bit (8 KB). The
    /// 68-unit read-back doesn't select a width and is ignored. Without this,
    /// `effective_addr_width` defaults to 14 and a 6-bit game stalls forever
    /// in the address phase.
    pub fn set_addr_width_from_dma(&mut self, count: u32) {
        if self.size != SizeKind::Unknown {
            return;
        }
        match count {
            9 | 73 => {
                self.size = SizeKind::Small;
                self.addr_width = 6;
                self.data.resize(512, 0xFF);
            }
            17 | 81 => {
                self.size = SizeKind::Large;
                self.addr_width = 14;
            }
            _ => {}
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
            let bit = eeprom.read_bit() as u64; // read_bit advances internally
            result = (result << 1) | bit;
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
        assert_eq!(eeprom.read_bit(), 1);
    }

    // Exercise DMA-length-based width detection + a full round-trip at the
    // large (14-bit / 8 KB) width.
    #[test]
    fn test_eeprom_large_width_from_dma_roundtrip() {
        let mut eeprom = Eeprom::new();
        // A write command on an 8 KB chip is 2+14+64+1 = 81 units.
        eeprom.set_addr_width_from_dma(81);
        assert_eq!(eeprom.size, SizeKind::Large);
        assert_eq!(eeprom.addr_width, 14);

        let data: u64 = 0xDEADBEEF_CAFEF00D;
        // Write block 5: 10 (cmd) + 14-bit addr + 64 data + dummy.
        send_bits(&mut eeprom, 0b10, 2);
        send_bits(&mut eeprom, 5, 14);
        send_bits(&mut eeprom, data, 64);
        send_bits(&mut eeprom, 0, 1);

        // Read block 5 back: 11 (cmd) + 14-bit addr + dummy, then 4+64 out.
        send_bits(&mut eeprom, 0b11, 2);
        send_bits(&mut eeprom, 5, 14);
        send_bits(&mut eeprom, 0, 1);
        let _dummy = read_bits(&mut eeprom, 4);
        assert_eq!(read_bits(&mut eeprom, 64), data);
    }
}
