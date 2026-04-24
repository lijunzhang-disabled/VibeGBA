//! Seiko S-3511 Real-Time Clock (RTC) emulation.
//!
//! The GBA has no built-in RTC, but some cartridges (Pokémon Ruby/Sapphire/Emerald,
//! Boktai, etc.) include an S-3511 chip accessed via GPIO registers on the cartridge:
//!
//!   0x080000C4 — DATA      (bits 0-3 = GPIO pins)
//!   0x080000C6 — DIRECTION (0=input, 1=output per pin)
//!   0x080000C8 — CONTROL   (bit 0: 1=enable GPIO readback, 0=reads return ROM)
//!
//! GPIO pin assignments for the RTC:
//!   Bit 0: SCK  (serial clock)
//!   Bit 1: SIO  (serial data, bidirectional)
//!   Bit 2: CS   (chip select, active high)
//!   Bit 3: unused
//!
//! Serial protocol:
//!   1. CS pulsed high → chip activates
//!   2. Game sends 8-bit command byte (MSB-first per the S-3511 convention):
//!        bits[7:4] = 0110 (magic)
//!        bit[3]    = 0 for write, 1 for read
//!        bits[2:0] = command ID
//!   3. Game sends/receives data bytes depending on command
//!   4. CS dropped → transaction complete
//!
//! Commands we care about:
//!   0x60 → Reset               (write, 0 data bytes)
//!   0x62 → Write Status        (write, 1 data byte)
//!   0x63 → Read Status         (read,  1 data byte)
//!   0x64 → Write Date+Time     (write, 7 BCD bytes)
//!   0x65 → Read Date+Time      (read,  7 BCD bytes)
//!   0x66 → Write Time          (write, 3 BCD bytes)
//!   0x67 → Read Time           (read,  3 BCD bytes)
//!
//! BCD byte order for Date+Time: year, month, day, day-of-week, hour, min, sec.
//!
//! Without this, Pokémon Emerald shows "The internal battery has run dry" because
//! it reads garbage when probing the chip.

use serde::{Deserialize, Serialize};

const PIN_SCK: u8 = 1 << 0;
const PIN_SIO: u8 = 1 << 1;
const PIN_CS: u8 = 1 << 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum TransferState {
    /// Waiting for CS to go high.
    Idle,
    /// Reading command byte from game.
    ReceivingCommand,
    /// Reading data bytes from game (write commands).
    ReceivingData,
    /// Sending data bytes to game (read commands).
    SendingData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rtc {
    /// Whether RTC is enabled (ROM has the SIIRTC signature).
    pub enabled: bool,
    /// Value last written to DATA register (bits 0-3).
    data_out: u8,
    /// Direction register (1 = output, 0 = input per pin).
    direction: u8,
    /// Control register (bit 0 = enable GPIO readback).
    control: u8,

    // Serial state
    state: TransferState,
    /// Last SCK pin state (for rising-edge detection).
    last_sck: bool,
    /// CS pin state.
    cs: bool,
    /// Bit accumulator for incoming byte (LSB-first).
    in_bits: u8,
    in_bit_count: u8,
    /// Byte currently being sent out.
    out_bits: u8,
    out_bit_count: u8,
    /// Outgoing SIO bit (what the chip is driving on SIO).
    sio_out: bool,

    /// Current command byte received.
    command: u8,
    /// Buffer of bytes collected (for writes) or to send (for reads).
    data_buf: [u8; 8],
    data_idx: usize,
    data_len: usize,

    // Chip state
    /// Status register: bit 1 = 24h mode, bit 7 = POWER (1 = battery failed).
    status: u8,
}

impl Rtc {
    pub fn new() -> Self {
        Rtc {
            enabled: false,
            data_out: 0,
            direction: 0,
            control: 0,
            state: TransferState::Idle,
            last_sck: false,
            cs: false,
            in_bits: 0,
            in_bit_count: 0,
            out_bits: 0,
            out_bit_count: 0,
            sio_out: false,
            command: 0,
            data_buf: [0; 8],
            data_idx: 0,
            data_len: 0,
            status: 0x40, // 24-hour mode; POWER bit clear = battery OK
        }
    }

    /// Returns true if the ROM declares RTC support.
    pub fn detect(rom: &[u8]) -> bool {
        // Scan for "SIIRTC_V" ASCII string
        let needle = b"SIIRTC_V";
        rom.windows(needle.len()).any(|w| w == needle)
    }

    /// Read a GPIO register. `offset` is relative to the GPIO base (0x080000C4).
    ///   offset 0 = DATA
    ///   offset 2 = DIRECTION
    ///   offset 4 = CONTROL
    pub fn read_reg(&self, offset: u32) -> u16 {
        if self.control & 1 == 0 {
            // GPIO readback disabled — reads return ROM data (which is 0 here).
            return 0;
        }
        match offset {
            0 => {
                // Report pins: output pins show what we wrote; input pins show chip's SIO.
                let mut val = 0u8;
                // SCK, CS: reflect whatever the game wrote (they're outputs)
                val |= self.data_out & (PIN_SCK | PIN_CS);
                // SIO: if game set it as output, echo back; else, return chip's sio_out
                if self.direction & PIN_SIO != 0 {
                    val |= self.data_out & PIN_SIO;
                } else if self.sio_out {
                    val |= PIN_SIO;
                }
                val as u16
            }
            2 => self.direction as u16,
            4 => self.control as u16,
            _ => 0,
        }
    }

    /// Write a GPIO register.
    pub fn write_reg(&mut self, offset: u32, value: u16) {
        match offset {
            0 => {
                self.data_out = (value & 0x0F) as u8;
                self.pin_update();
            }
            2 => {
                self.direction = (value & 0x0F) as u8;
            }
            4 => {
                self.control = (value & 1) as u8;
            }
            _ => {}
        }
    }

    /// Called after DATA is written — reacts to pin edges.
    fn pin_update(&mut self) {
        let new_cs = self.data_out & PIN_CS != 0;
        let new_sck = self.data_out & PIN_SCK != 0;
        let sio = self.data_out & PIN_SIO != 0;

        // Handle CS edge
        if new_cs && !self.cs {
            // CS rising edge — new transaction
            self.state = TransferState::ReceivingCommand;
            self.in_bits = 0;
            self.in_bit_count = 0;
            self.out_bit_count = 0;
            self.data_idx = 0;
        } else if !new_cs && self.cs {
            // CS falling edge — end transaction
            self.state = TransferState::Idle;
        }
        self.cs = new_cs;

        // Only process SCK edges when CS is high
        if !self.cs {
            self.last_sck = new_sck;
            return;
        }

        // Process on SCK falling edge (S-3511 samples on falling, drives on rising — we
        // simplify to edges).
        let rising = new_sck && !self.last_sck;
        let falling = !new_sck && self.last_sck;
        self.last_sck = new_sck;

        match self.state {
            TransferState::ReceivingCommand => {
                if rising {
                    // Game drives SIO; we read it.
                    // S-3511 command bits come MSB-first: shift left, OR in new bit.
                    self.in_bits = (self.in_bits << 1) | (sio as u8);
                    self.in_bit_count += 1;
                    if self.in_bit_count == 8 {
                        self.command = self.in_bits;
                        self.execute_command();
                        self.in_bit_count = 0;
                        self.in_bits = 0;
                    }
                }
            }
            TransferState::SendingData => {
                if falling {
                    // Drive next bit on SIO — LSB-first for S-3511 data bytes
                    if self.out_bit_count == 0 {
                        // Start of a new byte
                        if self.data_idx < self.data_len {
                            self.out_bits = self.data_buf[self.data_idx];
                            self.data_idx += 1;
                            self.out_bit_count = 8;
                        } else {
                            self.sio_out = false;
                            return;
                        }
                    }
                    self.sio_out = self.out_bits & 1 != 0;
                    self.out_bits >>= 1;
                    self.out_bit_count -= 1;
                    if self.out_bit_count == 0 && self.data_idx >= self.data_len {
                        self.state = TransferState::Idle;
                    }
                }
            }
            TransferState::ReceivingData => {
                if rising {
                    // Receive bits LSB-first into a byte buffer
                    self.in_bits |= (sio as u8) << self.in_bit_count;
                    self.in_bit_count += 1;
                    if self.in_bit_count == 8 {
                        if self.data_idx < self.data_buf.len() {
                            self.data_buf[self.data_idx] = self.in_bits;
                            self.data_idx += 1;
                        }
                        self.in_bits = 0;
                        self.in_bit_count = 0;
                        if self.data_idx >= self.data_len {
                            self.finish_write();
                            self.state = TransferState::Idle;
                        }
                    }
                }
            }
            TransferState::Idle => {}
        }
    }

    /// Decode the command byte and set up response (for reads) or prepare to receive data.
    fn execute_command(&mut self) {
        // S-3511 command format: bits[7:4]=0110, bit[3]=direction(0=w,1=r), bits[2:0]=cmd
        // But we relax the magic check and just use the low nibble + direction bit.
        let is_read = self.command & 0x04 != 0;  // bit 2 is the read/write flag in LSB-first
        // The classic S-3511 command decoding: MSB-first byte is 0x60..0x67
        // 0x60=reset, 0x62=ws, 0x63=rs, 0x64=wd, 0x65=rd, 0x66=wt, 0x67=rt
        let cmd = self.command & 0xF7; // mask away the direction bit in the middle
        let _ = is_read;

        match self.command {
            0x60 => {
                // Reset: clear status, time
                self.status = 0x40;
                self.state = TransferState::Idle;
            }
            0x62 => {
                // Write status
                self.data_len = 1;
                self.data_idx = 0;
                self.in_bits = 0;
                self.in_bit_count = 0;
                self.state = TransferState::ReceivingData;
            }
            0x63 => {
                // Read status
                self.data_buf[0] = self.status;
                self.data_len = 1;
                self.data_idx = 0;
                self.out_bit_count = 0;
                self.state = TransferState::SendingData;
            }
            0x64 => {
                // Write datetime (7 bytes)
                self.data_len = 7;
                self.data_idx = 0;
                self.in_bits = 0;
                self.in_bit_count = 0;
                self.state = TransferState::ReceivingData;
            }
            0x65 => {
                // Read datetime
                self.fill_datetime(7);
                self.state = TransferState::SendingData;
            }
            0x66 => {
                // Write time (3 bytes)
                self.data_len = 3;
                self.data_idx = 0;
                self.in_bits = 0;
                self.in_bit_count = 0;
                self.state = TransferState::ReceivingData;
            }
            0x67 => {
                // Read time
                self.fill_datetime(3);
                self.state = TransferState::SendingData;
            }
            _ => {
                // Unknown command; stay idle
                self.state = TransferState::Idle;
            }
        }
        let _ = cmd;
    }

    fn finish_write(&mut self) {
        match self.command {
            0x62 => self.status = self.data_buf[0],
            // For date/time writes, we ignore the data — we use system time regardless.
            _ => {}
        }
    }

    /// Populate data_buf with current date/time in BCD.
    /// count=7 → year, month, day, day-of-week, hour, minute, second
    /// count=3 → hour, minute, second
    fn fill_datetime(&mut self, count: usize) {
        let (year, month, day, dow, hour, minute, second) = current_datetime();

        let bcd = |n: u32| -> u8 { ((n / 10) << 4) as u8 | (n % 10) as u8 };

        if count == 7 {
            self.data_buf[0] = bcd(year % 100);
            self.data_buf[1] = bcd(month);
            self.data_buf[2] = bcd(day);
            self.data_buf[3] = bcd(dow);
            self.data_buf[4] = bcd(hour);
            self.data_buf[5] = bcd(minute);
            self.data_buf[6] = bcd(second);
        } else {
            self.data_buf[0] = bcd(hour);
            self.data_buf[1] = bcd(minute);
            self.data_buf[2] = bcd(second);
        }

        self.data_len = count;
        self.data_idx = 0;
        self.out_bit_count = 0;
    }
}

/// Return (year, month, day, day-of-week, hour, minute, second).
/// Uses real wall clock. year is full 4-digit (we return year%100 as BCD later).
fn current_datetime() -> (u32, u32, u32, u32, u32, u32, u32) {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Rough date calculation (no timezone; UTC is fine for RTC).
    let sec = (secs % 60) as u32;
    let min = ((secs / 60) % 60) as u32;
    let hour = ((secs / 3600) % 24) as u32;
    let days_since_epoch = (secs / 86400) as u32;
    let dow = ((days_since_epoch + 4) % 7) as u32; // 1970-01-01 was a Thursday (dow=4)

    // Convert days to year/month/day (Gregorian, treating each 4 years as 1461 days with leap)
    let (year, month, day) = days_to_ymd(days_since_epoch as i64);

    (year as u32, month, day, dow, hour, min, sec)
}

/// Convert days since 1970-01-01 to (year, month, day).
fn days_to_ymd(mut days: i64) -> (i32, u32, u32) {
    // Shift origin to 0000-03-01 for a classic algorithm
    days += 719468; // 1970-01-01 offset from 0000-03-01
    let era = if days >= 0 { days / 146097 } else { (days - 146096) / 146097 };
    let doe = (days - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i32 + (era * 400) as i32;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtc_detect() {
        let rom = b"random data SIIRTC_V001 more data".to_vec();
        assert!(Rtc::detect(&rom));

        let rom2 = b"no signature here".to_vec();
        assert!(!Rtc::detect(&rom2));
    }

    #[test]
    fn test_bcd_conversion() {
        let bcd = |n: u32| -> u8 { ((n / 10) << 4) as u8 | (n % 10) as u8 };
        assert_eq!(bcd(0), 0x00);
        assert_eq!(bcd(9), 0x09);
        assert_eq!(bcd(10), 0x10);
        assert_eq!(bcd(59), 0x59);
        assert_eq!(bcd(99), 0x99);
    }

    #[test]
    fn test_days_to_ymd() {
        // 1970-01-01 = day 0
        let (y, m, d) = days_to_ymd(0);
        assert_eq!((y, m, d), (1970, 1, 1));
        // 2000-01-01 = day 10957
        let (y, m, d) = days_to_ymd(10957);
        assert_eq!((y, m, d), (2000, 1, 1));
    }

    #[test]
    fn test_rtc_status_read() {
        let mut rtc = Rtc::new();
        rtc.enabled = true;
        rtc.control = 1; // Enable readback

        // Initial state: status = 0x40 (24h mode, battery OK)
        assert_eq!(rtc.status, 0x40);
        // POWER bit (bit 7) is clear → game sees "battery OK"
        assert_eq!(rtc.status & 0x80, 0);
    }
}
